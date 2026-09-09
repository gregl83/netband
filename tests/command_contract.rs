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
    assert_eq!(
        rows.iter()
            .filter(|row| row["event_kind"] == "bandwidth")
            .count(),
        1
    );
    assert!(rows.iter().any(|row| row["event_kind"] == "bandwidth"
        && row["outcome"] == "rate_limited"
        && row["daily_runs_used"] == "1"));
    let before = rows.len();
    let mut second = fixture.spawn();
    assert_eq!(second.wait().await.code(), Some(1));
    let rows = fixture.rows();
    assert_eq!(rows.len(), before + 1);
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
    use netband::journal::Journal;
    for signal in ["-INT", "-TERM"] {
        let fixture = CommandFixture::new().await;
        let mut child = fixture.spawn();
        let mut socket = fixture.accept_request().await;
        child.signal(signal);
        assert_eq!(child.wait().await.code(), Some(0));
        let rows = fixture.rows();
        assert_eq!(
            rows.iter()
                .filter(|row| row["event_kind"] == "bandwidth")
                .count(),
            1
        );
        assert!(rows.iter().any(|row| row["event_kind"] == "bandwidth"
            && row["outcome"] == "cancelled"
            && row["daily_runs_used"] == "1"));
        let output = fixture.root.path().join("results.csv");
        let (_journal, _) =
            Journal::open_at(&OutputTarget::File(output), chrono::Utc::now()).unwrap();
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
