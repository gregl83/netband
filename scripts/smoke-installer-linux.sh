#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
  echo "installer smoke: skipped (requires Linux x86_64)"
  exit 0
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/fixture" "$work/fake-bin" "$work/install"
system_path="$PATH"

asset="netband-x86_64-unknown-linux-gnu.tar.gz"
printf '#!/bin/sh\necho netband-installer-smoke\n' >"$work/fixture/netband"
chmod 0755 "$work/fixture/netband"
tar -C "$work/fixture" -czf "$work/fixture/$asset" netband
checksum="$(sha256sum "$work/fixture/$asset" | cut -d ' ' -f 1)"
printf '%s  %s\n' "$checksum" "$asset" >"$work/fixture/$asset.sha256"

cat >"$work/fake-bin/curl" <<'EOF'
#!/bin/sh
set -eu
output=""
url=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output) output="$2"; shift 2 ;;
        --*) shift ;;
        *) url="$1"; shift ;;
    esac
done
case "$url" in
    "$INSTALLER_EXPECTED_URL/$INSTALLER_ASSET")
        cp "$INSTALLER_FIXTURE_DIR/$INSTALLER_ASSET" "$output"
        ;;
    "$INSTALLER_EXPECTED_URL/$INSTALLER_ASSET.sha256")
        cp "$INSTALLER_FIXTURE_DIR/$INSTALLER_ASSET.sha256" "$output"
        ;;
    *)
        echo "unexpected installer URL: $url" >&2
        exit 1
        ;;
esac
EOF
chmod 0755 "$work/fake-bin/curl"

cat >"$work/fake-bin/getconf" <<'EOF'
#!/bin/sh
[ "$1" = GNU_LIBC_VERSION ] || exit 1
printf '%s\n' "${INSTALLER_TEST_LIBC:-glibc 2.35}"
EOF
chmod 0755 "$work/fake-bin/getconf"

run_installer() {
  local version="$1"
  local expected_url="$2"
  local destination="$work/install/$version"
  PATH="$work/fake-bin:$system_path" \
    INSTALLER_FIXTURE_DIR="$work/fixture" \
    INSTALLER_EXPECTED_URL="$expected_url" \
    INSTALLER_ASSET="$asset" \
    NETBAND_VERSION="$version" \
    NETBAND_INSTALL_DIR="$destination" \
    sh "$root/install.sh" || return $?
  cmp "$work/fixture/netband" "$destination/netband"
}

run_installer latest "https://github.com/gregl83/netband/releases/latest/download"
run_installer 0.1.0 "https://github.com/gregl83/netband/releases/download/v0.1.0"
run_installer v1.2.3-alpha.1 \
  "https://github.com/gregl83/netband/releases/download/v1.2.3-alpha.1"

if PATH="$work/fake-bin:$system_path" \
  NETBAND_VERSION=1garbage \
  NETBAND_INSTALL_DIR="$work/install/invalid-version" \
  sh "$root/install.sh"; then
  echo "error: installer accepted an invalid version" >&2
  exit 1
fi
test ! -e "$work/install/invalid-version/netband"

if NETBAND_INSTALL_DIR=/ sh "$root/install.sh"; then
  echo "error: installer accepted / as the install directory" >&2
  exit 1
fi

(
  export PATH="$work/fake-bin:$system_path"
  export NETBAND_INSTALL_DIR="$work/install/truncated"
  sed '$d' "$root/install.sh" | sh
)
test ! -e "$work/install/truncated/netband"

for runtime in 'glibc 2.34' 'musl 1.2.5' 'unknown' 'glibc 35' 'glibc 2.bad'; do
  if INSTALLER_TEST_LIBC="$runtime" run_installer latest \
    "https://github.com/gregl83/netband/releases/latest/download"; then
    echo "error: installer accepted unsupported runtime: $runtime" >&2
    exit 1
  fi
done
run_installer latest "https://github.com/gregl83/netband/releases/latest/download"
INSTALLER_TEST_LIBC='glibc 2.40' run_installer latest \
  "https://github.com/gregl83/netband/releases/latest/download"

# A valid checksum does not imply the executable can run. Keep an existing
# destination intact and clean up the staged file when the startup check fails.
printf '#!/bin/sh\nexit 127\n' >"$work/fixture/netband"
tar -C "$work/fixture" -czf "$work/fixture/$asset" netband
(cd "$work/fixture" && sha256sum "$asset" >"$asset.sha256")
mkdir "$work/install/unusable"
printf 'existing binary\n' >"$work/install/unusable/netband"
if PATH="$work/fake-bin:$system_path" \
  INSTALLER_FIXTURE_DIR="$work/fixture" \
  INSTALLER_EXPECTED_URL="https://github.com/gregl83/netband/releases/latest/download" \
  INSTALLER_ASSET="$asset" \
  NETBAND_INSTALL_DIR="$work/install/unusable" \
  sh "$root/install.sh"; then
  echo "error: installer accepted an unusable executable" >&2
  exit 1
fi
test "$(cat "$work/install/unusable/netband")" = 'existing binary'
test "$(ls -A "$work/install/unusable")" = netband

printf 'tampered' >>"$work/fixture/$asset"
if PATH="$work/fake-bin:$system_path" \
  INSTALLER_FIXTURE_DIR="$work/fixture" \
  INSTALLER_EXPECTED_URL="https://github.com/gregl83/netband/releases/latest/download" \
  INSTALLER_ASSET="$asset" \
  NETBAND_INSTALL_DIR="$work/install/tampered" \
  sh "$root/install.sh"; then
  echo "error: installer accepted an invalid checksum" >&2
  exit 1
fi
test ! -e "$work/install/tampered/netband"
echo "GitHub release installer smoke: passed"
