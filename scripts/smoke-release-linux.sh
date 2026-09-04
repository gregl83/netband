#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: release smoke requires Linux" >&2
  exit 2
fi
for command in cargo python3 stat; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "error: required command not found: $command" >&2
    exit 2
  }
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
cargo build --release --locked
target_dir="${CARGO_TARGET_DIR:-$root/target}"
binary="$target_dir/release/netband"

"$binary" --help >/dev/null
"$binary" --version >/dev/null
"$binary" once --help >/dev/null
"$binary" config --help >/dev/null

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
(
  cd "$work"
  "$binary" config check >config.txt 2>config.err
  test ! -s config.err
  grep -q '^configuration=valid$' config.txt
)

python3 - "$binary" "$root/tests/fixtures/v1-events.csv" <<'PY'
import csv
import pathlib
import subprocess
import sys
import time

binary = sys.argv[1]
started = time.monotonic()
result = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=2)
assert result.returncode == 0 and result.stdout.startswith("netband ")
assert time.monotonic() - started < 2

with pathlib.Path(sys.argv[2]).open(newline="", encoding="utf-8") as stream:
    rows = list(csv.DictReader(stream))
assert rows
kinds = {row["event_kind"] for row in rows}
assert {"ping_probe", "bandwidth", "request_failure", "scheduler"} <= kinds
assert len(rows[0]) == 42
PY

size="$(stat -c '%s' "$binary")"
maximum=$((25 * 1024 * 1024))
if (( size > maximum )); then
  echo "error: release binary is $size bytes (limit $maximum)" >&2
  exit 1
fi

cargo test --release --locked --test bandwidth_contract \
  direct_download_and_upload_produce_attributed_bandwidth_result
cargo test --release --locked --test provider_contract \
  locate_follows_redirect_identifies_client_and_parses_multiple_results
echo "release smoke: passed (binary_bytes=$size)"
