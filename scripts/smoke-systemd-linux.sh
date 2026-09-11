#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--help" ]]; then
  cat <<'EOF'
Usage: sudo bash scripts/smoke-systemd-linux.sh

Validates packaging/netband.service, then starts, stops, starts, restarts, and stops the
installed netband.service. Run only on a disposable/test Linux system with systemd as
PID 1 after installing /usr/local/bin/netband and /etc/netband/netband.toml.
EOF
  exit 0
fi

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: the systemd smoke test requires Linux" >&2
  exit 2
fi
if [[ "$(id -u)" != "0" ]]; then
  echo "error: run this smoke test as root" >&2
  exit 2
fi
for command in systemctl systemd-analyze journalctl python3; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "error: required command not found: $command" >&2
    exit 2
  }
done
if [[ "$(cat /proc/1/comm)" != "systemd" ]]; then
  echo "error: systemd must be PID 1" >&2
  exit 2
fi
test -x /usr/local/bin/netband
test -r /etc/netband/netband.toml

systemd-analyze verify packaging/netband.service
install -Dm0644 packaging/netband.service /etc/systemd/system/netband.service
systemctl daemon-reload

wait_ready() {
  for _ in $(seq 1 100); do
    if systemctl is-active --quiet netband.service && [[ -s /var/lib/netband/measurements/.netband-active ]]; then
      local segment
      read -r segment </var/lib/netband/measurements/.netband-active
      if [[ -s "/var/lib/netband/measurements/$segment" ]]; then
        return 0
      fi
    fi
    sleep 0.1
  done
  systemctl status netband.service --no-pager >&2 || true
  return 1
}

systemctl start netband.service
wait_ready
first_segment="$(cat /var/lib/netband/measurements/.netband-active)"
systemctl stop netband.service
systemctl is-active --quiet netband.service && exit 1 || true
systemctl start netband.service
wait_ready
second_segment="$(cat /var/lib/netband/measurements/.netband-active)"
[[ "$first_segment" != "$second_segment" ]]
before_restart="$(systemctl show netband.service -p MainPID --value)"
systemctl restart netband.service
wait_ready
after_restart="$(systemctl show netband.service -p MainPID --value)"
[[ "$before_restart" != "$after_restart" ]]
third_segment="$(cat /var/lib/netband/measurements/.netband-active)"
[[ "$second_segment" != "$third_segment" ]]
systemctl stop netband.service

python3 - "$first_segment" "$second_segment" "$third_segment" <<'PYCSV'
import csv
import pathlib
import sys

root = pathlib.Path("/var/lib/netband/measurements")
identifiers = set()
for name in sys.argv[1:]:
    with (root / name).open(newline="", encoding="utf-8") as stream:
        rows = csv.DictReader(stream)
        assert rows.fieldnames and rows.fieldnames[0] == "schema_version"
        for row in rows:
            assert None not in row and None not in row.values()
            assert row["schema_version"] == "1"
            identity = (row["run_id"], row["event_id"])
            assert identity not in identifiers
            identifiers.add(identity)
print("systemd segment restart and CSV validation: passed")
PYCSV

journal_output="$(mktemp)"
trap 'rm -f "$journal_output"' EXIT
journalctl -u netband.service --no-pager >"$journal_output"
grep -q "Netband starting" "$journal_output"
grep -q "graceful shutdown complete" "$journal_output"
echo "systemd start/stop/restart smoke: passed"
