#!/usr/bin/env bash
# Run the extracted release binary with the baseline user-space libraries.
set -euo pipefail

target="${1:?target required}"
binary="$(realpath "${2:?extracted binary required}")"
version="${3:?release version required}"
image=ubuntu:22.04
mounts=(--volume "$binary:/netband:ro")
case "$target" in
  x86_64-unknown-linux-gnu)
    platform=linux/amd64
    executable=(--entrypoint /netband "$image")
    ;;
  aarch64-unknown-linux-gnu)
    platform=linux/arm64
    emulator="$(command -v qemu-aarch64-static)"
    mounts+=(--volume "$emulator:/qemu:ro")
    executable=(--entrypoint /qemu "$image" /netband)
    ;;
  *) echo "Unsupported target: $target" >&2; exit 1 ;;
esac

docker pull --platform "$platform" "$image"
run=(docker run --rm --network none --platform "$platform" --workdir /tmp
     "${mounts[@]}" "${executable[@]}")
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
"${run[@]}" --version | grep -Fx "netband $version"
"${run[@]}" --help >/dev/null
"${run[@]}" config check >"$work/config.txt" 2>"$work/config.err"
test ! -s "$work/config.err"
grep -Fx 'configuration=valid' "$work/config.txt"
echo "$target baseline runtime ($image): passed"
