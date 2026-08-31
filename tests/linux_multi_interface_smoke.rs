use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::tempdir;

const SMOKE_DURATION: &str = "6s";

#[derive(Debug)]
struct Privilege {
    root: bool,
}

impl Privilege {
    fn detect() -> Self {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .expect("the Linux smoke test requires the id command");
        assert!(output.status.success(), "id -u failed: {}", stderr(&output));
        let root = String::from_utf8_lossy(&output.stdout).trim() == "0";
        if !root {
            let output = Command::new("sudo")
                .args(["-n", "true"])
                .output()
                .expect("the Linux smoke test requires sudo when not run as root");
            assert!(
                output.status.success(),
                "sudo credentials are unavailable; run `sudo -v` before this test: {}",
                stderr(&output)
            );
        }
        Self { root }
    }

    fn command(&self, program: &str) -> Command {
        if self.root {
            Command::new(program)
        } else {
            let mut command = Command::new("sudo");
            command.args(["-n", program]);
            command
        }
    }

    fn ip(&self, args: &[&str]) {
        let output = self
            .command("ip")
            .args(args)
            .output()
            .expect("the Linux smoke test requires iproute2");
        assert!(
            output.status.success(),
            "ip {} failed: {}",
            args.join(" "),
            stderr(&output)
        );
    }

    fn try_ip(&self, args: &[&str]) {
        let _ = self.command("ip").args(args).output();
    }
}

struct NetworkFixture {
    privilege: Privilege,
    interfaces: [String; 2],
    peers: [String; 2],
    namespaces: [String; 2],
    source_ips: [String; 2],
    target_ips: [String; 2],
}

impl NetworkFixture {
    fn create() -> Self {
        let privilege = Privilege::detect();
        let suffix = format!("{:x}", std::process::id());
        let octet = 1 + std::process::id() % 250;
        let fixture = Self {
            privilege,
            interfaces: [format!("nb{suffix}a"), format!("nb{suffix}b")],
            peers: [format!("np{suffix}a"), format!("np{suffix}b")],
            namespaces: [
                format!("netband-smoke-{suffix}-a"),
                format!("netband-smoke-{suffix}-b"),
            ],
            source_ips: [format!("198.18.{octet}.1"), format!("198.18.{octet}.5")],
            target_ips: [format!("198.18.{octet}.2"), format!("198.18.{octet}.6")],
        };

        for namespace in &fixture.namespaces {
            fixture.privilege.ip(&["netns", "add", namespace]);
            fixture
                .privilege
                .ip(&["-n", namespace, "link", "set", "lo", "up"]);
        }
        for index in 0..2 {
            fixture.privilege.ip(&[
                "link",
                "add",
                &fixture.interfaces[index],
                "type",
                "veth",
                "peer",
                "name",
                &fixture.peers[index],
            ]);
            fixture.privilege.ip(&[
                "link",
                "set",
                &fixture.peers[index],
                "netns",
                &fixture.namespaces[index],
            ]);
            fixture.privilege.ip(&[
                "addr",
                "add",
                &format!("{}/30", fixture.source_ips[index]),
                "dev",
                &fixture.interfaces[index],
            ]);
            fixture
                .privilege
                .ip(&["link", "set", &fixture.interfaces[index], "up"]);
            fixture.privilege.ip(&[
                "-n",
                &fixture.namespaces[index],
                "addr",
                "add",
                &format!("{}/30", fixture.target_ips[index]),
                "dev",
                &fixture.peers[index],
            ]);
            fixture.privilege.ip(&[
                "-n",
                &fixture.namespaces[index],
                "link",
                "set",
                &fixture.peers[index],
                "up",
            ]);
        }
        fixture
    }
}

impl Drop for NetworkFixture {
    fn drop(&mut self) {
        for interface in &self.interfaces {
            self.privilege.try_ip(&["link", "del", interface]);
        }
        for namespace in &self.namespaces {
            self.privilege.try_ip(&["netns", "del", namespace]);
        }
    }
}

#[derive(Debug)]
struct ProbeRow {
    interface: String,
    source_ip: String,
    target: String,
    outcome: String,
    started_at: String,
    finished_at: String,
}

#[test]
#[ignore = "requires Linux, iproute2, timeout, and root network-namespace access"]
fn two_real_interfaces_are_bound_attributed_fair_and_serialized() {
    if std::env::consts::OS != "linux" {
        panic!("this smoke test requires Linux");
    }

    let fixture = NetworkFixture::create();
    let directory = tempdir().unwrap();
    let output_path = directory.path().join("multi-interface-smoke.csv");

    let mut command = fixture.privilege.command("timeout");
    command
        .args(["-s", "KILL", SMOKE_DURATION])
        .arg(env!("CARGO_BIN_EXE_netband"))
        .args(["--console", "off", "--verbosity", "error"])
        .args(["--interface", &fixture.interfaces[0]])
        .args(["--interface", &fixture.interfaces[1]])
        .args(["--ping-target", &fixture.target_ips[0]])
        .args(["--ping-target", &fixture.target_ips[1]])
        .args(["--ping-interval", "100ms", "--ping-timeout", "300ms"])
        .arg("--no-bandwidth")
        .arg("--output")
        .arg(&output_path)
        .arg("run");
    let output = command
        .output()
        .expect("netband smoke process should start");

    let rows = read_probe_rows(&output_path).unwrap_or_else(|error| {
        panic!(
            "cannot read smoke output ({error}); process status={} stderr={}",
            output.status,
            stderr(&output)
        )
    });
    assert!(
        !rows.is_empty(),
        "no ping probe rows were written; process status={} stderr={}",
        output.status,
        stderr(&output)
    );

    for index in 0..2 {
        assert!(
            rows.iter().any(|row| {
                row.interface == fixture.interfaces[index]
                    && row.source_ip == fixture.source_ips[index]
                    && row.target == fixture.target_ips[index]
                    && row.outcome == "success"
            }),
            "{} never successfully reached {} from {}; rows={rows:#?}",
            fixture.interfaces[index],
            fixture.target_ips[index],
            fixture.source_ips[index]
        );
    }

    for row in &rows {
        let index = fixture
            .interfaces
            .iter()
            .position(|interface| interface == &row.interface)
            .unwrap_or_else(|| panic!("unexpected interface attribution: {row:#?}"));
        assert_eq!(
            row.source_ip, fixture.source_ips[index],
            "probe used or reported the wrong source address: {row:#?}"
        );
    }

    let turns = rows.iter().fold(Vec::<&str>::new(), |mut turns, row| {
        if turns.last().copied() != Some(row.interface.as_str()) {
            turns.push(&row.interface);
        }
        turns
    });
    assert!(turns.len() >= 2, "both interfaces did not receive a turn");
    assert_eq!(turns[0], fixture.interfaces[0]);
    assert_eq!(turns[1], fixture.interfaces[1]);

    for (index, left) in rows.iter().enumerate() {
        for right in rows.iter().skip(index + 1) {
            if left.interface == right.interface {
                continue;
            }
            let overlaps =
                left.started_at < right.finished_at && right.started_at < left.finished_at;
            assert!(
                !overlaps,
                "different interfaces overlapped: left={left:#?} right={right:#?}"
            );
        }
    }
}

fn read_probe_rows(path: &Path) -> Result<Vec<ProbeRow>, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() == 0 {
        return Err("CSV file is empty".to_owned());
    }
    let mut reader = csv::Reader::from_path(path).map_err(|error| error.to_string())?;
    let headers = reader
        .headers()
        .map_err(|error| error.to_string())?
        .iter()
        .enumerate()
        .map(|(index, name)| (name.to_owned(), index))
        .collect::<HashMap<_, _>>();
    let field = |name: &str| {
        headers
            .get(name)
            .copied()
            .unwrap_or_else(|| panic!("missing CSV field {name}"))
    };
    let mut rows = Vec::new();
    for result in reader.records() {
        let record = result.map_err(|error| error.to_string())?;
        if record.get(field("event_kind")) != Some("ping_probe") {
            continue;
        }
        rows.push(ProbeRow {
            interface: record
                .get(field("interface"))
                .unwrap_or_default()
                .to_owned(),
            source_ip: record
                .get(field("source_ip"))
                .unwrap_or_default()
                .to_owned(),
            target: record.get(field("target")).unwrap_or_default().to_owned(),
            outcome: record.get(field("outcome")).unwrap_or_default().to_owned(),
            started_at: record
                .get(field("started_at_utc"))
                .unwrap_or_default()
                .to_owned(),
            finished_at: record
                .get(field("finished_at_utc"))
                .unwrap_or_default()
                .to_owned(),
        });
    }
    Ok(rows)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_owned()
}
