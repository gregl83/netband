#!/usr/bin/env bash
set -euo pipefail

binary="${1:-target/aarch64-unknown-linux-gnu/release/netband}"
test -x "$binary"
binary="$(cd "$(dirname "$binary")" && pwd)/$(basename "$binary")"

case "$(uname -m)" in
  aarch64|arm64)
    runner=()
    ;;
  *)
    command -v qemu-aarch64 >/dev/null 2>&1 || {
      echo "error: qemu-aarch64 is required on a non-aarch64 host" >&2
      exit 2
    }
    runner=(qemu-aarch64 -L "${QEMU_LD_PREFIX:-/usr/aarch64-linux-gnu}")
    ;;
esac

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
"${runner[@]}" "$binary" --help >/dev/null
"${runner[@]}" "$binary" --version | grep -q '^netband '
(
  cd "$work"
  "${runner[@]}" "$binary" config check >config.txt 2>config.err
  test ! -s config.err
  grep -q '^configuration=valid$' config.txt
)
echo "aarch64 help/version/config smoke: passed"
