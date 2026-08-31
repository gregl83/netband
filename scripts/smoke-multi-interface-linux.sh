#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--help" ]]; then
  cat <<'EOF'
Usage: scripts/smoke-multi-interface-linux.sh

Builds Netband and runs the ignored Linux two-interface integration smoke test.
The test temporarily creates two veth pairs and network namespaces, then removes them.

Requirements: Linux, cargo, iproute2, coreutils timeout, and sudo or root access.
EOF
  exit 0
fi

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "error: the multi-interface smoke test requires Linux" >&2
  exit 2
fi

for command in cargo ip timeout id; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "error: required command not found: $command" >&2
    exit 2
  fi
done

if [[ "$(id -u)" != "0" ]]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "error: sudo is required when not running as root" >&2
    exit 2
  fi
  sudo -v
fi

cargo test --test linux_multi_interface_smoke -- --ignored --nocapture
