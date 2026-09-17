#![cfg(target_os = "linux")]

use std::fs;
use std::process::Command;

use tempfile::tempdir;

#[test]
fn service_smoke_waits_for_the_restarted_process_segment() {
    assert_smoke_waits_for_new_segments(false);
}

#[test]
fn service_smoke_replaces_stale_output_from_an_already_running_service() {
    assert_smoke_waits_for_new_segments(true);
}

fn assert_smoke_waits_for_new_segments(already_running: bool) {
    let dir = tempdir().unwrap();
    if already_running {
        fs::write(dir.path().join("previous.csv"), "header\n").unwrap();
        fs::write(dir.path().join(".netband-active"), "previous.csv\n").unwrap();
    }
    let script = include_str!("../scripts/smoke-systemd-linux.sh");
    // Exercise the real readiness function and its lifecycle callers without the
    // installer preamble or host systemd. Keep all measurement paths in the fixture.
    let start = script.find("wait_ready() {").unwrap();
    let end = script[start..].find("\npython3 - ").unwrap() + start;
    let lifecycle =
        script[start..end].replace("/var/lib/netband/measurements", "$TEST_MEASUREMENTS");
    let harness = r#"
set -euo pipefail
running=$TEST_SERVICE_RUNNING
generation=0
pending=''

publish_segment() {
  printf 'header\n' >"$TEST_MEASUREMENTS/segment-$generation.csv"
  printf 'segment-%s.csv\n' "$generation" >"$TEST_MEASUREMENTS/.netband-active"
  printf 'published %s\n' "$generation" >>"$TEST_MEASUREMENTS/trace"
  pending=''
}

systemctl() {
  case "$1" in
    start|restart)
      if [[ "$1" == start && "$running" == 1 ]]; then
        return 0
      fi
      generation=$((generation + 1))
      running=1
      printf '%s %s\n' "$1" "$generation" >>"$TEST_MEASUREMENTS/trace"
      if [[ "$generation" == 1 && ! -s "$TEST_MEASUREMENTS/.netband-active" ]]; then
        publish_segment
      else
        # Type=simple is already active, but recovery has not replaced the marker.
        pending=1
      fi
      ;;
    is-active) [[ "$running" == 1 ]] ;;
    show) printf '%s\n' "$generation" ;;
    stop) running=0 ;;
    status) printf 'mock service status: generation=%s\n' "$generation" >&2 ;;
    *) printf 'unexpected systemctl call: %s\n' "$*" >&2; return 1 ;;
  esac
}

sleep() {
  # Advance startup deterministically at the next readiness poll, with no wall-clock
  # timing dependency. An early readiness return never reaches this publication.
  if [[ -n "$pending" ]]; then
    publish_segment
  fi
}
"#;
    let output = Command::new("bash")
        .args(["-c", &format!("{harness}\n{lifecycle}")])
        .env("TEST_MEASUREMENTS", dir.path())
        .env(
            "TEST_SERVICE_RUNNING",
            if already_running { "1" } else { "0" },
        )
        .output()
        .unwrap();
    let trace = fs::read_to_string(dir.path().join("trace")).unwrap();
    assert!(
        output.status.success(),
        "service smoke accepted a stale segment before restart initialization completed:\n{trace}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        trace,
        "start 1\npublished 1\nstart 2\npublished 2\nrestart 3\npublished 3\n"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join(".netband-active")).unwrap(),
        "segment-3.csv\n"
    );
}
