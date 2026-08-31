#!/bin/sh
set -eu
umask 077

repository="gregl83/netband"
work_dir=""
staged_binary=""

die() {
    printf 'netband: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    [ -z "$work_dir" ] || rm -rf "$work_dir"
    [ -z "$staged_binary" ] || rm -f "$staged_binary"
}

require_commands() {
    for command in curl mktemp sha256sum tar uname; do
        command -v "$command" >/dev/null 2>&1 || \
            die "required command not found: $command"
    done
}

validate_version() {
    candidate="${1#v}"
    case "$candidate" in
        '' | *[!0-9A-Za-z.+-]* | *..* | *[-+.])
            die "invalid release version: $1"
            ;;
    esac

    core="${candidate%%[-+]*}"
    major="${core%%.*}"
    remainder="${core#*.}"
    [ "$remainder" != "$core" ] || die "invalid release version: $1"
    minor="${remainder%%.*}"
    patch="${remainder#*.}"
    [ "$patch" != "$remainder" ] || die "invalid release version: $1"
    case "$patch" in
        *.*) die "invalid release version: $1" ;;
    esac

    for component in "$major" "$minor" "$patch"; do
        case "$component" in
            0 | [1-9] | [1-9][0-9]*) ;;
            *) die "invalid release version: $1" ;;
        esac
    done
}

select_release() {
    case "$version" in
        latest)
            release_url="https://github.com/${repository}/releases/latest/download"
            ;;
        v*)
            validate_version "$version"
            release_url="https://github.com/${repository}/releases/download/${version}"
            ;;
        *)
            validate_version "$version"
            release_url="https://github.com/${repository}/releases/download/v${version}"
            ;;
    esac
}

select_target() {
    [ "$(uname -s)" = "Linux" ] || die "pre-built binaries support Linux only"
    case "$(uname -m)" in
        x86_64 | amd64) target="x86_64-unknown-linux-gnu" ;;
        aarch64 | arm64) target="aarch64-unknown-linux-gnu" ;;
        *) die "unsupported architecture: $(uname -m)" ;;
    esac
}

download() {
    curl --fail --location --silent --show-error \
        --proto '=https' --proto-redir '=https' \
        --connect-timeout 15 --max-time 300 --max-filesize 52428800 \
        --retry 3 --retry-delay 1 \
        --output "$2" "$1"
}

verify_and_extract() (
    cd "$work_dir"
    sha256sum --check --strict "$checksum_asset"

    entries="$(tar -tzf "$asset")"
    [ "$entries" = "netband" ] || die "release archive contains unexpected entries"
    tar -xzf "$asset" netband
    if [ ! -f netband ] || [ -L netband ]; then
        die "release archive does not contain a regular netband binary"
    fi
)

install_binary() {
    mkdir -p "$install_dir"
    staged_binary="$(mktemp "$install_dir/.netband-install.XXXXXX")"
    cp "$work_dir/netband" "$staged_binary"
    chmod 0755 "$staged_binary"
    mv -f "$staged_binary" "$install_dir/netband"
    staged_binary=""
}

netband_install() {
    version="${NETBAND_VERSION:-latest}"
    install_dir="${NETBAND_INSTALL_DIR:-${HOME:?HOME is not set}/.local/bin}"
    case "$install_dir" in
        /) die "refusing to install into /" ;;
        /*) ;;
        *) die "NETBAND_INSTALL_DIR must be an absolute path" ;;
    esac

    require_commands
    select_target
    select_release

    asset="netband-${target}.tar.gz"
    checksum_asset="${asset}.sha256"
    work_dir="$(mktemp -d)"
    trap cleanup EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    download "$release_url/$asset" "$work_dir/$asset"
    download "$release_url/$checksum_asset" "$work_dir/$checksum_asset"
    verify_and_extract
    install_binary
    printf 'Installed netband to %s/netband\n' "$install_dir"
}

netband_install
