#!/usr/bin/env bash
set -uo pipefail

usage() {
  cat >&2 <<'EOF'
usage: benchmark-ndt7-clients.sh SERVER [PAIRS [COOLDOWN_SECONDS [OUTPUT_DIR]]]

Runs alternating, paired NDT7 measurements with M-Lab's Go reference client and
Netband. Raw output may contain client addresses; keep OUTPUT_DIR private.

Environment:
  NDT7_CLIENT_BIN  reference client binary (default: ndt7-client on PATH)
  NETBAND_BIN      Netband binary (default: netband on PATH)
EOF
}

if [[ $# -lt 1 || $# -gt 4 ]]; then
  usage
  exit 2
fi

server="$1"
pairs="${2:-20}"
cooldown="${3:-10}"
output_dir="${4:-.netband/benchmarks/$(date -u +%Y%m%dT%H%M%SZ)}"
ndt7_client="${NDT7_CLIENT_BIN:-$(command -v ndt7-client || true)}"
netband="${NETBAND_BIN:-$(command -v netband || true)}"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
route_address="$(getent ahosts "$server" | awk 'NR == 1 { print $1 }')"

if [[ ! "$pairs" =~ ^[1-9][0-9]*$ ]]; then
  echo "PAIRS must be a positive integer" >&2
  exit 2
fi
if [[ ! "$cooldown" =~ ^[0-9]+$ ]]; then
  echo "COOLDOWN_SECONDS must be a non-negative integer" >&2
  exit 2
fi
for dependency in "$ndt7_client" "$netband" python3; do
  if [[ -z "$dependency" ]] || ! command -v "$dependency" >/dev/null 2>&1; then
    echo "missing required command: ${dependency:-unset binary path}" >&2
    exit 2
  fi
done
if [[ -z "$route_address" ]]; then
  echo "cannot resolve an address for $server" >&2
  exit 2
fi

mkdir -p "$output_dir/raw"
measurements="$output_dir/measurements.csv"
state_file="$output_dir/netband-state.json"
printf '%s\n' 'pair,position,client,started_at_utc,finished_at_utc,exit_code,outcome,download_mbps,upload_mbps,diagnostic,raw_file' >"$measurements"

{
  printf 'started_at_utc=%s\n' "$(date -u --iso-8601=seconds)"
  printf 'server=%s\n' "$server"
  printf 'resolved_addresses=%s\n' "$(getent ahosts "$server" | awk '{print $1}' | sort -u | paste -sd, -)"
  printf 'kernel=%s\n' "$(uname -srmo)"
  printf 'route=%s\n' "$(ip route get "$route_address" 2>/dev/null | head -n 1 || true)"
  printf 'netband_binary=%s\n' "$netband"
  printf 'netband_version=%s\n' "$($netband --version)"
  printf 'ndt7_client_binary=%s\n' "$ndt7_client"
  if command -v go >/dev/null 2>&1; then
    go version -m "$ndt7_client" 2>/dev/null || true
  fi
} >"$output_dir/metadata.txt"

append_record() {
  python3 - "$measurements" "$@" <<'PY'
import csv
import sys

with open(sys.argv[1], "a", newline="", encoding="utf-8") as stream:
    csv.writer(stream).writerow(sys.argv[2:])
PY
}

run_reference() {
  local pair="$1" position="$2" base started finished status parsed outcome download upload
  base="$output_dir/raw/pair-$(printf '%02d' "$pair")-${position}-reference"
  started="$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)"
  "$ndt7_client" -server "$server" -format json -quiet >"$base.jsonl" 2>"$base.stderr"
  status=$?
  finished="$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)"
  parsed="$(python3 - "$base.jsonl" <<'PY'
import json
import sys

summary = {}
with open(sys.argv[1], encoding="utf-8") as stream:
    for line in stream:
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if "ServerFQDN" in value:
            summary = value
download = summary.get("Download", {}).get("Throughput", {}).get("Value", "")
upload = summary.get("Upload", {}).get("Throughput", {}).get("Value", "")
print(download, upload, sep="\t")
PY
)"
  IFS=$'\t' read -r download upload <<<"$parsed"
  if [[ $status -eq 0 && -n "$download" && -n "$upload" ]]; then
    outcome="success"
  else
    outcome="error"
  fi
  append_record "$pair" "$position" reference "$started" "$finished" "$status" "$outcome" "$download" "$upload" "" "${base#"$output_dir/"}.jsonl"
  printf 'pair=%02d position=%s client=reference outcome=%s download=%s upload=%s\n' "$pair" "$position" "$outcome" "${download:-n/a}" "${upload:-n/a}"
}

run_netband() {
  local pair="$1" position="$2" base started finished status parsed outcome download upload diagnostic
  base="$output_dir/raw/pair-$(printf '%02d' "$pair")-${position}-netband"
  started="$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)"
  "$netband" \
    --ndt-provider direct \
    --ndt-target "$server" \
    --bandwidth-daily-max "$((pairs * 2 + 10))" \
    --bandwidth-min-spacing 70s \
    --force \
    --output "$base.csv" \
    --state-file "$state_file" \
    --console off \
    once bandwidth >"$base.stdout" 2>"$base.stderr"
  status=$?
  finished="$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)"
  parsed="$(python3 - "$base.csv" <<'PY'
import csv
import sys

measurement = {}
diagnostics = []
try:
    with open(sys.argv[1], newline="", encoding="utf-8") as stream:
        for row in csv.DictReader(stream):
            if row.get("event_kind") == "bandwidth":
                measurement = row
            elif row.get("event_kind") == "request_failure":
                kind = row.get("error_kind", "request_failure")
                code = row.get("os_error_code", "")
                diagnostics.append(f"{kind}:{code}" if code else kind)
except FileNotFoundError:
    pass
print(
    measurement.get("outcome", "error"),
    measurement.get("download_mbps", ""),
    measurement.get("upload_mbps", ""),
    ";".join(diagnostics),
    sep="\t",
)
PY
)"
  IFS=$'\t' read -r outcome download upload diagnostic <<<"$parsed"
  append_record "$pair" "$position" netband "$started" "$finished" "$status" "${outcome:-error}" "$download" "$upload" "$diagnostic" "${base#"$output_dir/"}.csv"
  printf 'pair=%02d position=%s client=netband outcome=%s download=%s upload=%s\n' "$pair" "$position" "${outcome:-error}" "${download:-n/a}" "${upload:-n/a}"
}

completed=0
total=$((pairs * 2))
cool_down() {
  completed=$((completed + 1))
  if ((completed < total && cooldown > 0)); then
    sleep "$cooldown"
  fi
}

echo "Writing private raw measurements to $output_dir"
for ((pair = 1; pair <= pairs; pair++)); do
  if ((pair % 2 == 1)); then
    run_netband "$pair" first
    cool_down
    run_reference "$pair" second
  else
    run_reference "$pair" first
    cool_down
    run_netband "$pair" second
  fi
  cool_down
done

printf 'finished_at_utc=%s\n' "$(date -u --iso-8601=seconds)" >>"$output_dir/metadata.txt"
python3 "$script_dir/summarize-ndt7-benchmark.py" "$measurements" "$output_dir"
