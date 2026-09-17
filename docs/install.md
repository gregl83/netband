# Install Netband

Install a release binary or build Netband v1.0.0 from source.
Neither path requires setting up a service.

## Release binary

Supported release targets are Linux x86_64 and aarch64. You need `curl`, `tar`, and
`sha256sum`. The installer verifies the archive checksum and installs into
`~/.local/bin` by default:

```sh
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
  https://github.com/gregl83/netband/releases/latest/download/netband-installer.sh | sh
export PATH="$HOME/.local/bin:$PATH"
netband --version
netband config check
```

Add `~/.local/bin` to your shell's PATH permanently if it is not already included.
You can also download archives from [GitHub Releases](https://github.com/gregl83/netband/releases).

The installer selects the latest published release. See
[release verification](release.md#release-assets) for archive checksums and provenance.

## Build from source

Building v1.0.0 requires Git and Rust 1.98 or newer. Clone the release tag:

```sh
git clone --branch v1.0.0 https://github.com/gregl83/netband.git
cd netband
```

From that directory, or from your existing checkout:

```sh
cargo build --release --locked
./target/release/netband config check
./target/release/netband once ping
```

To put the built binary on your user PATH:

```sh
mkdir -p "$HOME/.local/bin"
install -m0755 target/release/netband "$HOME/.local/bin/netband"
export PATH="$HOME/.local/bin:$PATH"
```

Ping needs ICMP socket permissions; see [ICMP setup](service.md#icmp-permissions)
if probes report permission errors. A standalone bandwidth test does not require
ICMP sockets. M-Lab bandwidth tests require explicit policy acceptance.

Continue with [quick tests](../README.md#try-a-quick-test). To run unattended later,
follow [service operation](service.md).
