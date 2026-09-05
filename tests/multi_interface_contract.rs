use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
use netband::cli::Cli;
use netband::config::{ResolveContext, resolve};
use netband::console::ConsoleOff;
use netband::health::HealthConfig;
use netband::interfaces::{
    FairInterfaceSelector, InterfaceError, InterfaceResolver, ResolvedInterface,
};
use netband::journal::{JournalError, JournalSink, OutputCoordinator};
use netband::model::{EventKind, MeasurementEvent, Outcome};
use netband::monitor::{
    PingMonitorConfig, PingTransportFactory, cancellation_channel, monitor_multi_interface,
};
use netband::ping::{PingTransport, ProbeAttemptResult, ProbeBinding, ProbeReply, ProbeRequest};
use tempfile::tempdir;

type ProbeFuture<'a> = Pin<Box<dyn Future<Output = ProbeAttemptResult> + Send + 'a>>;

#[test]
fn deficit_round_robin_stays_balanced_and_compensates_for_triggered_attempts() {
    let names = vec!["eth-a".to_owned(), "eth-b".to_owned(), "eth-c".to_owned()];
    let eligible = names.iter().cloned().collect::<HashSet<_>>();
    let mut selector = FairInterfaceSelector::new(&names);

    for _ in 0..2 {
        let selected = selector.select(&eligible).unwrap().to_owned();
        selector.record_attempt(&selected);
    }
    let counts = names
        .iter()
        .map(|name| selector.attempts(name))
        .collect::<Vec<_>>();
    assert_eq!(counts, [1, 1, 0], "a budget smaller than N remains fair");

    selector.record_attempt("eth-a");
    for _ in 0..7 {
        let selected = selector.select(&eligible).unwrap().to_owned();
        selector.record_attempt(&selected);
    }
    let counts = names
        .iter()
        .map(|name| selector.attempts(name))
        .collect::<Vec<_>>();
    assert!(counts.iter().max().unwrap() - counts.iter().min().unwrap() <= 1);
    assert!(selector.attempts("eth-b") >= selector.attempts("eth-a") - 1);
}

#[derive(Clone)]
struct FlappingResolver {
    calls: Arc<Mutex<HashMap<String, usize>>>,
}

impl InterfaceResolver for FlappingResolver {
    fn resolve(&self, name: &str) -> Result<ResolvedInterface, InterfaceError> {
        let mut calls = self.calls.lock().unwrap();
        let count = calls.entry(name.to_owned()).or_default();
        *count += 1;
        if name == "eth-b" && *count == 1 {
            return Err(InterfaceError::Down {
                name: name.to_owned(),
            });
        }
        let address = match name {
            "eth-a" => "192.0.2.10",
            "eth-b" => "192.0.2.20",
            "eth-c" => "192.0.2.30",
            _ => "192.0.2.40",
        };
        Ok(ResolvedInterface {
            name: name.to_owned(),
            addresses: vec![address.parse().unwrap()],
        })
    }
}

#[derive(Clone)]
struct RecordingFactory {
    order: Arc<Mutex<Vec<String>>>,
    active: Arc<Mutex<HashSet<String>>>,
    max_interfaces: Arc<AtomicUsize>,
}

impl PingTransportFactory for RecordingFactory {
    fn create(&self, interface: &str, _targets: &[IpAddr]) -> Arc<dyn PingTransport> {
        self.order.lock().unwrap().push(interface.to_owned());
        let address = match interface {
            "eth-a" => "192.0.2.10",
            "eth-b" => "192.0.2.20",
            "eth-c" => "192.0.2.30",
            _ => "192.0.2.40",
        };
        Arc::new(InterfaceTransport {
            interface: interface.to_owned(),
            source_ip: address.parse().unwrap(),
            active: Arc::clone(&self.active),
            max_interfaces: Arc::clone(&self.max_interfaces),
        })
    }
}

struct InterfaceTransport {
    interface: String,
    source_ip: IpAddr,
    active: Arc<Mutex<HashSet<String>>>,
    max_interfaces: Arc<AtomicUsize>,
}

impl PingTransport for InterfaceTransport {
    fn probe(&self, request: ProbeRequest) -> ProbeFuture<'_> {
        Box::pin(async move {
            {
                let mut active = self.active.lock().unwrap();
                active.insert(self.interface.clone());
                self.max_interfaces
                    .fetch_max(active.len(), Ordering::SeqCst);
            }
            tokio::time::sleep(Duration::from_secs(7)).await;
            self.active.lock().unwrap().remove(&self.interface);
            ProbeAttemptResult {
                binding: ProbeBinding {
                    interface: Some(self.interface.clone()),
                    source_ip: Some(self.source_ip),
                },
                sent: true,
                result: Ok(ProbeReply {
                    target: request.target,
                    identifier: Some(request.identifier),
                    sequence: request.sequence,
                    rtt: Duration::from_millis(10),
                    icmp_type: 0,
                    icmp_code: 0,
                }),
            }
        })
    }
}

#[derive(Clone, Default)]
struct RecordingJournal {
    events: Arc<Mutex<Vec<MeasurementEvent>>>,
}

impl JournalSink for RecordingJournal {
    fn append_batch(&mut self, events: &[MeasurementEvent]) -> Result<(), JournalError> {
        self.events.lock().unwrap().extend_from_slice(events);
        Ok(())
    }
}

fn resolved(root: PathBuf) -> netband::config::ResolvedConfig {
    let cli = Cli::try_parse_from([
        "netband",
        "--interface",
        "eth-a",
        "--interface",
        "eth-b",
        "--interface",
        "eth-c",
        "--ping-target",
        "198.51.100.1",
        "--ping-interval",
        "5s",
        "--no-bandwidth",
        "run",
    ])
    .unwrap();
    resolve(
        &cli,
        &ResolveContext {
            stdout_is_terminal: false,
            current_dir: root.clone(),
            state_dir: root.join("state"),
        },
    )
    .unwrap()
}

async fn wait_for_rounds(order: &Mutex<Vec<String>>, expected: usize) {
    for _ in 0..100 {
        if order.lock().unwrap().len() >= expected {
            return;
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    panic!("expected {expected} interface rounds");
}

#[tokio::test(start_paused = true, flavor = "current_thread")]
async fn failed_interface_does_not_starve_rotation_and_recovers_without_relabelling() {
    let root = tempdir().unwrap();
    let config = resolved(root.path().to_path_buf());
    let resolver = FlappingResolver {
        calls: Arc::new(Mutex::new(HashMap::new())),
    };
    let order = Arc::new(Mutex::new(Vec::new()));
    let max_interfaces = Arc::new(AtomicUsize::new(0));
    let factory = RecordingFactory {
        order: Arc::clone(&order),
        active: Arc::new(Mutex::new(HashSet::new())),
        max_interfaces: Arc::clone(&max_interfaces),
    };
    let journal = RecordingJournal::default();
    let mut coordinator = OutputCoordinator::new(journal.clone(), ConsoleOff);
    let settings = PingMonitorConfig {
        run_id: "multi-run".into(),
        targets: config.ping.targets.clone(),
        interval: config.ping.interval,
        timeout: Duration::from_secs(30),
        identifier: 42,
        health: HealthConfig {
            window_rounds: 6,
            min_samples: 6,
            loss_threshold_pct: 50.0,
            rtt_threshold_ms: None,
            recovery_loss_pct: 10.0,
            recovery_rounds: 3,
        },
    };
    let (shutdown_tx, shutdown) = cancellation_channel();
    let task = tokio::spawn(async move {
        monitor_multi_interface(
            &config,
            &resolver,
            &factory,
            settings,
            None,
            &mut coordinator,
            shutdown,
        )
        .await
    });

    wait_for_rounds(&order, 4).await;
    shutdown_tx.send(true).unwrap();
    tokio::time::advance(Duration::from_secs(7)).await;
    let stats = task.await.unwrap().unwrap();

    assert_eq!(*order.lock().unwrap(), ["eth-a", "eth-c", "eth-a", "eth-b"]);
    assert_eq!(stats.interface_failures, 1);
    assert_eq!(stats.rounds_started, 4);
    assert_eq!(stats.rounds_completed, 4);
    assert_eq!(max_interfaces.load(Ordering::SeqCst), 1);
    let events = journal.events.lock().unwrap();
    assert!(events.iter().any(|event| {
        event.event_kind == EventKind::Scheduler
            && event.interface.as_deref() == Some("eth-b")
            && event.outcome == Outcome::Deferred
            && event.source_ip.is_none()
    }));
    assert!(events.iter().any(|event| {
        event.event_kind == EventKind::Scheduler
            && event.interface.as_deref() == Some("eth-b")
            && event.outcome == Outcome::Success
    }));
    for event in events
        .iter()
        .filter(|event| event.event_kind == EventKind::PingProbe)
    {
        let expected = match event.interface.as_deref().unwrap() {
            "eth-a" => "192.0.2.10",
            "eth-b" => "192.0.2.20",
            "eth-c" => "192.0.2.30",
            other => panic!("unexpected interface {other}"),
        };
        assert_eq!(event.source_ip, Some(expected.parse().unwrap()));
    }
}

#[cfg(target_os = "linux")]
mod loaded_tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use netband::model::LoadPhase;
    use netband::ping::ProbeFailure;
    use netband::scheduler::Scheduler;
    use tokio::net::TcpListener;
    use tokio::sync::watch;
    use tokio_tungstenite::accept_hdr_async;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
    use tokio_tungstenite::tungstenite::http::HeaderValue;

    const NDT7_PROTOCOL: &str = "net.measurementlab.ndt.v7";

    #[derive(Clone, Copy)]
    struct StableResolver;

    impl InterfaceResolver for StableResolver {
        fn resolve(&self, name: &str) -> Result<ResolvedInterface, InterfaceError> {
            let address = match name {
                "lo" => "127.0.0.1",
                "test-other" => "192.0.2.20",
                _ => "192.0.2.40",
            };
            Ok(ResolvedInterface {
                name: name.to_owned(),
                addresses: vec![address.parse().unwrap()],
            })
        }
    }

    #[derive(Clone)]
    struct LoadedPingFactory {
        order: Arc<Mutex<Vec<String>>>,
    }

    impl PingTransportFactory for LoadedPingFactory {
        fn create(&self, interface: &str, _targets: &[IpAddr]) -> Arc<dyn PingTransport> {
            self.order.lock().unwrap().push(interface.to_owned());
            Arc::new(LoadedPingTransport {
                interface: interface.to_owned(),
            })
        }
    }

    struct LoadedPingTransport {
        interface: String,
    }

    impl PingTransport for LoadedPingTransport {
        fn probe(&self, request: ProbeRequest) -> ProbeFuture<'_> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(2)).await;
                let source_ip = if self.interface == "lo" {
                    "127.0.0.1"
                } else {
                    "192.0.2.20"
                }
                .parse()
                .unwrap();
                let result = if self.interface == "lo" && request.target.to_string().ends_with(".2")
                {
                    Err(ProbeFailure::Timeout)
                } else {
                    Ok(ProbeReply {
                        target: request.target,
                        identifier: Some(request.identifier),
                        sequence: request.sequence,
                        rtt: Duration::from_millis(10),
                        icmp_type: 0,
                        icmp_code: 0,
                    })
                };
                ProbeAttemptResult {
                    binding: ProbeBinding {
                        interface: Some(self.interface.clone()),
                        source_ip: Some(source_ip),
                    },
                    sent: true,
                    result,
                }
            })
        }
    }

    #[allow(clippy::result_large_err)]
    fn accept_ndt7(_request: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
        response.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static(NDT7_PROTOCOL),
        );
        Ok(response)
    }

    async fn loaded_ndt_server(listener: TcpListener, shutdown: watch::Sender<bool>) {
        let (download, _) = listener.accept().await.unwrap();
        let mut download = accept_hdr_async(download, accept_ndt7).await.unwrap();
        for _ in 0..6 {
            download
                .send(Message::Binary(vec![1_u8; 16 * 1024].into()))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        download.close(None).await.unwrap();

        let (upload, _) = listener.accept().await.unwrap();
        let mut upload = accept_hdr_async(upload, accept_ndt7).await.unwrap();
        let upload_window = tokio::time::sleep(Duration::from_millis(60));
        tokio::pin!(upload_window);
        loop {
            tokio::select! {
                _ = &mut upload_window => break,
                message = upload.next() => {
                    if message.is_none() {
                        break;
                    }
                }
            }
        }
        upload.close(None).await.unwrap();
        while let Some(message) = upload.next().await {
            if matches!(message.unwrap(), Message::Close(_)) {
                break;
            }
        }
        drop(upload);
        tokio::time::sleep(Duration::from_millis(60)).await;
        let _ = shutdown.send(true);
    }

    fn resolved_with_bandwidth(
        root: PathBuf,
        address: std::net::SocketAddr,
    ) -> netband::config::ResolvedConfig {
        let download = format!("ws://{address}/ndt/v7/download");
        let upload = format!("ws://{address}/ndt/v7/upload");
        let cli = Cli::try_parse_from([
            "netband",
            "--interface",
            "lo",
            "--interface",
            "test-other",
            "--ndt-provider",
            "direct",
            "--ndt-download-url",
            &download,
            "--ndt-upload-url",
            &upload,
            "--allow-insecure-ndt",
            "--ping-target",
            "192.0.2.1",
            "--ping-target",
            "192.0.2.2",
            "--ping-interval",
            "10ms",
            "--loss-window-rounds",
            "1",
            "--loss-min-samples",
            "2",
            "--bandwidth-timeout",
            "2s",
            "run",
        ])
        .unwrap();
        resolve(
            &cli,
            &ResolveContext {
                stdout_is_terminal: false,
                current_dir: root.clone(),
                state_dir: root.join("state"),
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn loaded_rounds_stay_on_the_bandwidth_interface_then_rotation_resumes() {
        let root = tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config =
            resolved_with_bandwidth(root.path().to_path_buf(), listener.local_addr().unwrap());
        let order = Arc::new(Mutex::new(Vec::new()));
        let factory = LoadedPingFactory {
            order: Arc::clone(&order),
        };
        let journal = RecordingJournal::default();
        let mut coordinator = OutputCoordinator::new(journal.clone(), ConsoleOff);
        let (shutdown_tx, shutdown) = cancellation_channel();
        let server = tokio::spawn(loaded_ndt_server(listener, shutdown_tx));
        let scheduler =
            Scheduler::open(&config.state_file, &config.bandwidth, chrono::Utc::now()).unwrap();
        let settings = PingMonitorConfig {
            run_id: "multi-loaded-run".into(),
            targets: config.ping.targets.clone(),
            interval: config.ping.interval,
            timeout: Duration::from_secs(1),
            identifier: 44,
            health: HealthConfig {
                window_rounds: 1,
                min_samples: 2,
                loss_threshold_pct: 50.0,
                rtt_threshold_ms: None,
                recovery_loss_pct: 10.0,
                recovery_rounds: 3,
            },
        };

        let stats = tokio::time::timeout(
            Duration::from_secs(5),
            monitor_multi_interface(
                &config,
                &StableResolver,
                &factory,
                settings,
                Some(scheduler),
                &mut coordinator,
                shutdown,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();

        assert_eq!(stats.bandwidth_attempts, 1);
        let events = journal.events.lock().unwrap();
        let loaded = events
            .iter()
            .filter(|event| event.event_kind == EventKind::PingProbe && event.load_run_id.is_some())
            .collect::<Vec<_>>();
        assert!(!loaded.is_empty());
        assert!(loaded.iter().all(|event| {
            event.interface.as_deref() == Some("lo")
                && event.source_ip == Some("127.0.0.1".parse().unwrap())
        }));
        assert!(
            loaded
                .iter()
                .any(|event| event.load_phase == Some(LoadPhase::Download))
        );
        assert!(
            loaded
                .iter()
                .any(|event| event.load_phase == Some(LoadPhase::Upload))
        );
        assert!(events.iter().any(|event| {
            event.event_kind == EventKind::PingProbe
                && event.load_run_id.is_none()
                && event.interface.as_deref() == Some("test-other")
        }));
        assert_eq!(&order.lock().unwrap()[..2], ["lo", "lo"]);
        assert!(
            order
                .lock()
                .unwrap()
                .iter()
                .skip(2)
                .any(|name| name == "test-other")
        );
    }
}
