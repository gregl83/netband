#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: the service subprocess smoke test requires Linux" >&2
  exit 2
fi
for command in cargo kill id ip; do
  command -v "$command" >/dev/null 2>&1 || {
    echo "error: required command not found: $command" >&2
    exit 2
  }
done

if [[ "$(id -u)" != "0" ]]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "error: sudo is required when not running as root" >&2
    exit 2
  fi
  sudo -v
  cargo test --test linux_service_contract --no-run
  test_binary="$(find "${CARGO_TARGET_DIR:-target}/debug/deps" -maxdepth 1 -type f -name 'linux_service_contract-*' -perm -111 | head -n 1)"
  exec sudo -n "$test_binary" --ignored --nocapture
fi

cargo test --test linux_service_contract -- --ignored --nocapture
