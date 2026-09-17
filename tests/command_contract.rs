//! Public command contracts using disposable files and loopback-only providers.
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tempfile::{TempDir, tempdir};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

struct CommandFixture {
    root: TempDir,
    listener: TcpListener,
}

impl CommandFixture {
    async fn new() -> Self {
        Self {
            root: tempdir().unwrap(),
            listener: TcpListener::bind("127.0.0.1:0").await.unwrap(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_netband"));
        command
            .current_dir(self.root.path())
            .args([
                "--output",
                "results.csv",
                "--state-file",
                "scheduler.json",
                "--console",
                "off",
                "--ndt-provider",
                "direct",
                "--allow-insecure-ndt",
                "--ndt-download-url",
            ])
            .arg(format!(
                "ws://{}/download",
                self.listener.local_addr().unwrap()
            ))
            .arg("--ndt-upload-url")
            .arg(format!(
                "ws://{}/upload",
                self.listener.local_addr().unwrap()
            ))
            .args(["--bandwidth-timeout", "5s", "once", "bandwidth"]);
        command
    }

    fn spawn(&self) -> ChildGuard {
        let stderr = self.root.path().join("stderr.log");
        let child = self
            .command()
            .stdout(Stdio::from(
                File::create(self.root.path().join("stdout.log")).unwrap(),
            ))
            .stderr(Stdio::from(File::create(&stderr).unwrap()))
            .spawn()
            .unwrap();
        ChildGuard { child, stderr }
    }

    async fn accept_request(&self) -> tokio::net::TcpStream {
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), self.listener.accept())
            .await
            .expect("command must connect")
            .unwrap();
        let mut request = [0; 4096];
        assert!(
            tokio::time::timeout(Duration::from_secs(5), socket.read(&mut request))
                .await
                .expect("command must send handshake")
                .unwrap()
                > 0
        );
        socket
    }

    fn rows(&self) -> Vec<HashMap<String, String>> {
        csv::Reader::from_path(self.root.path().join("results.csv"))
            .unwrap()
            .deserialize()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

struct ChildGuard {
    child: Child,
    stderr: PathBuf,
}

impl ChildGuard {
    async fn wait(&mut self) -> ExitStatus {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "command did not exit: {}",
                fs::read_to_string(&self.stderr).unwrap()
            )
        })
    }

    #[cfg(unix)]
    fn signal(&self, name: &str) {
        assert!(
            Command::new("kill")
                .args([name, &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn provider_failure_is_journaled_and_suppression_does_not_reconnect() {
    let fixture = CommandFixture::new().await;
    let mut child = fixture.spawn();
    let mut socket = fixture.accept_request().await;
    socket
        .write_all(
            b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    drop(socket);
    assert_eq!(child.wait().await.code(), Some(1));
    let rows = fixture.rows();
    assert_complete_hierarchy(&rows);
    assert_eq!(
        rows.iter()
            .filter(|row| row["event_kind"] == "bandwidth")
            .count(),
        1
    );
    assert!(rows.iter().any(|row| row["event_kind"] == "bandwidth"
        && row["outcome"] == "rate_limited"
        && row["provider_daily_starts"] == "1"));
    let bandwidth = rows
        .iter()
        .find(|row| row["event_kind"] == "bandwidth")
        .unwrap();
    assert!(bandwidth["scheduled_at_utc"].is_empty());
    assert!(!bandwidth["requested_at_utc"].is_empty());
    let decision = rows
        .iter()
        .find(|row| row["scheduler_action"] == "rate_limit")
        .unwrap();
    assert_eq!(decision["run_id"], bandwidth["run_id"]);
    assert_eq!(decision["parent_run_id"], bandwidth["parent_run_id"]);
    assert_eq!(decision["root_run_id"], bandwidth["parent_run_id"]);
    assert_eq!(decision["run_kind"], "bandwidth");
    let before = rows.len();
    let mut second = fixture.spawn();
    assert_eq!(second.wait().await.code(), Some(1));
    let rows = fixture.rows();
    assert_complete_hierarchy(&rows);
    assert_eq!(rows.len(), before + 3);
    let blocked = &rows[before + 1];
    assert_eq!(blocked["run_kind"], "session");
    assert_eq!(blocked["root_run_id"], blocked["run_id"]);
    assert!(blocked["parent_run_id"].is_empty());
    assert_eq!(rows.last().unwrap()["outcome"], "suppressed");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), fixture.listener.accept())
            .await
            .is_err()
    );
    assert!(
        fs::read(fixture.root.path().join("stdout.log"))
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[allow(clippy::result_large_err)] // The WebSocket handshake callback fixes the error type.
async fn successful_command_writes_one_terminal_result_and_returns_zero() {
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
    let fixture = CommandFixture::new().await;
    let mut child = fixture.spawn();
    tokio::time::timeout(Duration::from_secs(10), async {
        for download in [true, false] {
            let (stream, _) =
                tokio::time::timeout(Duration::from_secs(5), fixture.listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                stream,
                |_: &Request, mut response: Response| {
                    response.headers_mut().insert(
                        "sec-websocket-protocol",
                        "net.measurementlab.ndt.v7".parse().unwrap(),
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
            if download {
                socket
                    .send(Message::Binary(vec![0; 1024].into()))
                    .await
                    .unwrap();
            } else {
                assert!(matches!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Binary(_)
                ));
            }
            socket.close(None).await.unwrap();
        }
    })
    .await
    .expect("command must complete both transfer stages");
    assert_eq!(child.wait().await.code(), Some(0));
    let rows = fixture.rows();
    assert_complete_hierarchy(&rows);
    let bandwidth: Vec<_> = rows
        .iter()
        .filter(|row| row["event_kind"] == "bandwidth")
        .collect();
    assert_eq!(bandwidth.len(), 1);
    assert_eq!(bandwidth[0]["outcome"], "success");
    assert!(bandwidth[0]["download_mbps"].parse::<f64>().unwrap() > 0.0);
    assert!(bandwidth[0]["upload_mbps"].parse::<f64>().unwrap() > 0.0);
}

#[tokio::test]
async fn durable_state_errors_stop_before_provider_activity() {
    for corrupt_state in [false, true] {
        let fixture = CommandFixture::new().await;
        let path = fixture.root.path().join(if corrupt_state {
            "scheduler.json"
        } else {
            "results.csv"
        });
        fs::write(&path, b"invalid fixture").unwrap();
        let mut child = fixture.spawn();
        assert_eq!(child.wait().await.code(), Some(4));
        assert_eq!(fs::read(path).unwrap(), b"invalid fixture");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), fixture.listener.accept())
                .await
                .is_err()
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn interrupt_and_terminate_flush_cancelled_results_and_release_ownership() {
    use netband::config::OutputTarget;
    use netband::journal::JournalWriter;
    for signal in ["-INT", "-TERM"] {
        let fixture = CommandFixture::new().await;
        let mut child = fixture.spawn();
        let mut socket = fixture.accept_request().await;
        child.signal(signal);
        assert_eq!(child.wait().await.code(), Some(0));
        let rows = fixture.rows();
        assert_complete_hierarchy(&rows);
        assert_eq!(
            rows.iter()
                .filter(|row| row["event_kind"] == "bandwidth")
                .count(),
            1
        );
        assert!(rows.iter().any(|row| row["event_kind"] == "bandwidth"
            && row["outcome"] == "cancelled"
            && row["provider_daily_starts"] == "1"));
        let output = fixture.root.path().join("results.csv");
        let (_journal, _) =
            JournalWriter::open_at(&OutputTarget::File(output), chrono::Utc::now()).unwrap();
        let mut tail = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut tail))
            .await
            .unwrap()
            .unwrap();
        assert!(fixture.root.path().join("scheduler.json").exists());
        let lock = File::options()
            .read(true)
            .write(true)
            .open(fixture.root.path().join("scheduler.lock"))
            .unwrap();
        lock.try_lock()
            .expect("scheduler ownership must be released");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn starts_are_durable_before_network_work_and_kill_leaves_runs_unfinished() {
    let fixture = CommandFixture::new().await;
    let mut child = fixture.spawn();
    let _socket = fixture.accept_request().await;
    let starts = fixture.rows();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0]["event_kind"], "run_started");
    assert_eq!(starts[0]["run_kind"], "session");
    assert_eq!(starts[0]["command"], "once bandwidth");
    assert!(starts[0]["parent_run_id"].is_empty());
    assert_eq!(starts[1]["event_kind"], "run_started");
    assert_eq!(starts[1]["run_kind"], "bandwidth");
    assert_eq!(starts[1]["parent_run_id"], starts[0]["run_id"]);
    assert_ne!(starts[1]["run_id"], starts[0]["run_id"]);
    for (index, row) in starts.iter().enumerate() {
        assert_eq!(row["event_sequence"], (index + 1).to_string());
        for field in ["run_id", "event_id"] {
            assert_eq!(
                uuid::Uuid::parse_str(&row[field])
                    .unwrap()
                    .get_version_num(),
                4
            );
        }
    }
    child.signal("-KILL");
    assert!(!child.wait().await.success());
    assert_eq!(fixture.rows(), starts);
}

fn assert_complete_hierarchy(rows: &[HashMap<String, String>]) {
    use std::collections::HashSet;
    let mut active = HashMap::new();
    let mut ids = HashSet::new();
    let mut sequence = 0;
    let mut root = None;
    for row in rows {
        assert!(ids.insert(&row["event_id"]));
        if row["event_kind"] == "run_started" && row["run_kind"] == "session" {
            assert!(active.is_empty());
            sequence = 0;
            root = Some(&row["run_id"]);
        }
        assert_eq!(Some(&row["root_run_id"]), root);
        sequence += 1;
        assert_eq!(row["event_sequence"], sequence.to_string());
        let run = &row["run_id"];
        let parent = &row["parent_run_id"];
        if !parent.is_empty() {
            assert!(active.contains_key(parent));
        }
        if row["event_kind"] == "run_started" {
            assert!(active.insert(run, (&row["run_kind"], parent)).is_none());
        }
        assert_eq!(active.get(run), Some(&(&row["run_kind"], parent)));
        if row["event_kind"] == "run_finished" {
            assert!(!active.values().any(|(_, parent)| *parent == run));
            active.remove(run);
        }
    }
    assert!(active.is_empty());
}
