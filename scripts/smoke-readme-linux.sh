#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: README smoke requires Linux" >&2
  exit 2
fi
for command in cargo python3; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "error: required command not found: $command" >&2
    exit 2
  }
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
cargo build --locked
target_dir="${CARGO_TARGET_DIR:-$root/target}"
binary="$target_dir/debug/netband"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

(
  cd "$work"
  "$binary" config check >defaults.txt 2>defaults.err
  test ! -s defaults.err
  grep -q '^configuration=valid$' defaults.txt
  grep -q '^bandwidth.automatic_enabled=false$' defaults.txt
)

"$binary" --config examples/netband.toml config check >"$work/example.txt"
"$binary" --config examples/mlab.toml config check >"$work/mlab.txt"
"$binary" --config examples/direct.toml config check >"$work/direct.txt"
grep -q '^bandwidth.provider=mlab$' "$work/mlab.txt"
grep -q '^bandwidth.provider=direct$' "$work/direct.txt"

"$binary" --ndt-provider direct --ndt-target 192.0.2.40:443 config check >/dev/null
"$binary" --ndt-provider direct --ndt-target '[2001:db8::40]:443' config check >/dev/null
"$binary" --ndt-provider direct \
  --ndt-download-url wss://ndt.operator.example.invalid/custom/download \
  --ndt-upload-url wss://ndt.operator.example.invalid/custom/upload \
  config check >/dev/null
"$binary" --ndt-provider direct \
  --ndt-download-url ws://192.168.50.10:8080/ndt/v7/download \
  --ndt-upload-url ws://192.168.50.10:8080/ndt/v7/upload \
  --allow-insecure-ndt config check >/dev/null

python3 - "$root/docs/examples/console.jsonl" <<'PY'
import json
import pathlib
import sys

lines = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
events = [json.loads(line) for line in lines]
assert len(events) == 2
assert all(event["schema_version"] == 1 for event in events)
assert {event["event_kind"] for event in events} == {"ping_probe", "bandwidth"}
PY

cargo test --locked --test documentation_contract
cargo test --locked --test ping_contract one_shot_cli_pipeline_records_all_rows_and_separates_console_modes
echo "README command and channel smoke: passed"
