use std::fs::{self, File};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

struct ChildGuard {
    child: Child,
    stderr_path: PathBuf,
    name: String,
}

struct DummyLink(String);

impl DummyLink {
    fn create() -> Self {
        let name = format!("nbg{:x}", std::process::id());
        run_ip(&["link", "add", &name, "type", "dummy"]);
        run_ip(&["addr", "add", "198.19.250.1/24", "dev", &name]);
        run_ip(&["link", "set", &name, "up"]);
        Self(name)
    }
}

impl Drop for DummyLink {
    fn drop(&mut self) {
        let _ = Command::new("ip").args(["link", "del", &self.0]).status();
    }
}

impl ChildGuard {
    fn spawn(args: &[&str], root: &Path, name: &str, stdout: Stdio) -> Self {
        let stderr_path = root.join(format!("{name}.stderr"));
        let stderr = File::create(&stderr_path).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_netband"))
            .args(args)
            .stdout(stdout)
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("netband subprocess should start");
        Self {
            child,
            stderr_path,
            name: name.to_owned(),
        }
    }

    fn spawn_closed_stdout(args: &[&str], root: &Path, name: &str) -> Self {
        let stderr_path = root.join(format!("{name}.stderr"));
        let stderr = File::create(&stderr_path).unwrap();
        let child = Command::new("sh")
            .args([
                "-c",
                "exec \"$0\" \"$@\" 1>&-",
                env!("CARGO_BIN_EXE_netband"),
            ])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("netband subprocess should start with stdout closed");
        Self {
            child,
            stderr_path,
            name: name.to_owned(),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn wait(mut self, timeout: Duration) -> (ExitStatus, String) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                let stderr = fs::read_to_string(&self.stderr_path).unwrap_or_default();
                return (status, stderr);
            }
            assert!(
                Instant::now() < deadline,
                "subprocess {} did not exit in time; stderr={}",
                self.name,
                fs::read_to_string(&self.stderr_path).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
#[ignore = "requires Linux and permission to create ICMP sockets"]
fn signals_locks_restart_and_stdout_backpressure_preserve_durable_state() {
    if std::env::consts::OS != "linux" {
        panic!("this service integration test requires Linux");
    }
    assert_eq!(
        String::from_utf8_lossy(&Command::new("id").arg("-u").output().unwrap().stdout).trim(),
        "0",
        "run the service integration test as root"
    );
    let root = tempdir().unwrap();
    graceful_signal_and_restart(&root);
    graceful_signal_during_bandwidth(&root);
    competing_state_lock(&root);
    second_signal_forces_nonzero_exit(&root);
    grace_timeout_forces_nonzero_exit(&root);
}

fn graceful_signal_during_bandwidth(root: &TempDir) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted_tx, accepted_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        accepted_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(10));
    });

    let output = root.path().join("bandwidth-signal.csv");
    let state = root.path().join("bandwidth-signal-state.json");
    let download = format!("ws://{address}/ndt/v7/download");
    let upload = format!("ws://{address}/ndt/v7/upload");
    let args = [
        "--console".to_owned(),
        "off".to_owned(),
        "--output".to_owned(),
        output.to_string_lossy().into_owned(),
        "--state-file".to_owned(),
        state.to_string_lossy().into_owned(),
        "--ndt-provider".to_owned(),
        "direct".to_owned(),
        "--ndt-download-url".to_owned(),
        download,
        "--ndt-upload-url".to_owned(),
        upload,
        "--allow-insecure-ndt".to_owned(),
        "--bandwidth-timeout".to_owned(),
        "30s".to_owned(),
        "--shutdown-grace".to_owned(),
        "2s".to_owned(),
        "once".to_owned(),
        "bandwidth".to_owned(),
    ];
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    let process = ChildGuard::spawn(&args, root.path(), "bandwidth-signal", Stdio::null());
    accepted_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("bandwidth subprocess should connect to the local NDT7 endpoint");

    send_signal(process.pid(), "TERM");
    let (status, stderr) = process.wait(Duration::from_secs(5));
    release_tx.send(()).unwrap();
    server.join().unwrap();

    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("scheduler state flushed"));
    assert!(stderr.contains("measurement journal flushed"));
    assert!(stderr.contains("graceful shutdown complete"));
    assert!(state.exists());
    assert!(csv::Reader::from_path(output).is_ok());
}

fn graceful_signal_and_restart(root: &TempDir) {
    let output = root.path().join("service.csv");
    let output_text = output.to_str().unwrap();
    let common = [
        "--console",
        "human",
        "--verbosity",
        "info",
        "--output",
        output_text,
        "--ping-target",
        "127.0.0.1",
        "--ping-interval",
        "20ms",
        "--ping-timeout",
        "200ms",
        "--shutdown-grace",
        "2s",
        "--no-bandwidth",
        "run",
    ];
    let first = ChildGuard::spawn_closed_stdout(&common, root.path(), "term");
    wait_for_rows(&output, 4, Duration::from_secs(10));

    let contender = Command::new(env!("CARGO_BIN_EXE_netband"))
        .args([
            "--console",
            "off",
            "--output",
            output_text,
            "--ping-target",
            "127.0.0.1",
            "once",
            "ping",
        ])
        .output()
        .unwrap();
    assert_eq!(contender.status.code(), Some(4));
    assert!(
        String::from_utf8_lossy(&contender.stderr).contains("already locked"),
        "{}",
        String::from_utf8_lossy(&contender.stderr)
    );

    send_signal(first.pid(), "TERM");
    let (status, stderr) = first.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("measurement journal flushed"));
    assert!(stderr.contains("graceful shutdown complete"));
    let rows_after_first = csv_rows(&output);

    let slow_stdout_args = [
        "--console",
        "jsonl",
        "--verbosity",
        "info",
        "--output",
        output_text,
        "--ping-target",
        "127.0.0.1",
        "--ping-interval",
        "1ms",
        "--ping-timeout",
        "100ms",
        "--shutdown-grace",
        "2s",
        "--no-bandwidth",
        "run",
    ];
    let restarted = ChildGuard::spawn(&slow_stdout_args, root.path(), "interrupt", Stdio::piped());
    wait_for_rows(&output, rows_after_first + 300, Duration::from_secs(15));
    send_signal(restarted.pid(), "INT");
    let (status, stderr) = restarted.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("graceful shutdown complete"));

    let contents = fs::read_to_string(&output).unwrap();
    assert_eq!(contents.matches(netband::journal::CSV_HEADER).count(), 1);
    assert!(csv_rows(&output) > rows_after_first);
}

fn competing_state_lock(root: &TempDir) {
    let state = root.path().join("scheduler.json");
    let first_output = root.path().join("automatic.csv");
    let second_output = root.path().join("manual.csv");
    let first = ChildGuard::spawn(
        &[
            "--console",
            "off",
            "--output",
            first_output.to_str().unwrap(),
            "--state-file",
            state.to_str().unwrap(),
            "--ping-target",
            "127.0.0.1",
            "--accept-mlab-policy",
            "run",
        ],
        root.path(),
        "state-owner",
        Stdio::null(),
    );
    wait_for_path(&state, Duration::from_secs(10));

    let contender = Command::new(env!("CARGO_BIN_EXE_netband"))
        .args([
            "--console",
            "off",
            "--output",
            second_output.to_str().unwrap(),
            "--state-file",
            state.to_str().unwrap(),
            "--accept-mlab-policy",
            "once",
            "bandwidth",
        ])
        .output()
        .unwrap();
    assert_eq!(contender.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&contender.stderr).contains("already locked"));

    send_signal(first.pid(), "TERM");
    let (status, stderr) = first.wait(Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("scheduler state flushed"));
}

fn second_signal_forces_nonzero_exit(root: &TempDir) {
    let output = root.path().join("forced.csv");
    let process = ChildGuard::spawn(
        &[
            "--console",
            "off",
            "--output",
            output.to_str().unwrap(),
            "--ping-target",
            "192.0.2.1",
            "--ping-timeout",
            "30s",
            "--shutdown-grace",
            "10s",
            "--no-bandwidth",
            "run",
        ],
        root.path(),
        "forced",
        Stdio::null(),
    );
    wait_for_path(&output, Duration::from_secs(10));
    thread::sleep(Duration::from_millis(100));
    send_signal(process.pid(), "STOP");
    send_signal(process.pid(), "TERM");
    send_signal(process.pid(), "INT");
    send_signal(process.pid(), "CONT");
    let (status, stderr) = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(6), "{stderr}");
    assert!(stderr.contains("second shutdown signal"));
    assert!(csv::Reader::from_path(output).is_ok());
}

fn grace_timeout_forces_nonzero_exit(root: &TempDir) {
    let link = DummyLink::create();
    let output = root.path().join("grace-expired.csv");
    let process = ChildGuard::spawn(
        &[
            "--console",
            "off",
            "--output",
            output.to_str().unwrap(),
            "--interface",
            &link.0,
            "--ping-target",
            "198.19.250.2",
            "--ping-timeout",
            "30s",
            "--shutdown-grace",
            "50ms",
            "--no-bandwidth",
            "run",
        ],
        root.path(),
        "grace-expired",
        Stdio::null(),
    );
    wait_for_path(&output, Duration::from_secs(10));
    thread::sleep(Duration::from_millis(100));
    send_signal(process.pid(), "TERM");
    let (status, stderr) = process.wait(Duration::from_secs(5));
    assert_eq!(status.code(), Some(6), "{stderr}");
    assert!(stderr.contains("shutdown grace period expired"));
    assert!(csv::Reader::from_path(output).is_ok());
}

fn send_signal(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status()
        .expect("the Linux service test requires kill");
    assert!(status.success(), "cannot send SIG{signal} to {pid}");
}

fn wait_for_rows(path: &Path, minimum: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if csv_rows(path) >= minimum {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("CSV did not reach {minimum} rows: {}", path.display());
}

fn wait_for_path(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("path was not created: {}", path.display());
}

fn csv_rows(path: &Path) -> usize {
    csv::Reader::from_path(path)
        .map(|mut reader| reader.records().filter(Result::is_ok).count())
        .unwrap_or(0)
}

fn run_ip(args: &[&str]) {
    let output = Command::new("ip")
        .args(args)
        .output()
        .expect("the Linux service test requires iproute2");
    assert!(
        output.status.success(),
        "ip {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}
