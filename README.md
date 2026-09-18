[![Build](https://github.com/gregl83/netband/actions/workflows/ci.yml/badge.svg)](https://github.com/gregl83/netband/actions/workflows/ci.yml)
[![Coverage Status](https://codecov.io/gh/gregl83/netband/graph/badge.svg?token=CL93O7DW9C)](https://codecov.io/gh/gregl83/netband)
[![Crates.io](https://img.shields.io/crates/v/netband.svg)](https://crates.io/crates/netband)
[![Apache 2.0 licensed](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

<p align="center"><img src="assets/netband.svg" alt="Netband network pulse logo" width="250" /></p>

# netband

**Ping, packet loss, and download/upload tests from your terminal.**

Run a quick network check, or keep Netband running to catch intermittent problems.
Get readable results in your terminal and a CSV record you can compare later.
Built for Linux, from your workstation to a Raspberry Pi.

- **Test now:** one ping round or one NDT7 bandwidth test, then exit.
- **Catch intermittent problems:** continuous pings, scheduled bandwidth tests, and tests triggered by degraded connectivity.
- **See latency under load:** continuous monitoring keeps pinging during bandwidth tests and labels the active phase.

## Install

For Linux x86_64 and aarch64 with glibc 2.35+ ([requirements](docs/install.md#runtime-requirements)),
install a checksum-verified release to `~/.local/bin`:

```sh
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
  https://github.com/gregl83/netband/releases/latest/download/netband-installer.sh | sh
export PATH="$HOME/.local/bin:$PATH"
```

Requires `curl`, `tar`, and `sha256sum`. Prefer to compile it yourself?
See [installation and source builds](docs/install.md).

## Try a quick test

No configuration file or background service is needed.

**Check latency and packet loss:**

```sh
netband once ping
```

This sends one probe to each default target: `1.1.1.1`, `8.8.8.8`, and `9.9.9.9`.
To check a specific address:

```sh
netband --ping-target 1.1.1.1 once ping
```

Ping needs permission to open ICMP sockets. If you get a permission error, follow the
[ICMP setup instructions](docs/service.md#icmp-permissions).

**Check download and upload bandwidth:**

Review [privacy](PRIVACY.md) and M-Lab's
[acceptable-use](https://www.measurementlab.net/aup/) and
[privacy](https://www.measurementlab.net/privacy/) policies, then explicitly accept:

```sh
netband --accept-mlab-policy once bandwidth
```

Netband finds an NDT7 server through M-Lab and runs download followed by upload.
Netband enforces a four-start UTC daily cap for M-Lab, shared by manual and scheduled
tests through scheduler state. Spacing and cooldowns also apply. You can instead use an
[operator-supplied NDT7 server](docs/configuration.md#direct-provider).

Illustrative terminal results (one ping target shown):

```text
2026-09-17T12:00:00.000Z ping interface=default-route target=1.1.1.1 outcome=success rtt_ms=12.5 loss_pct=0
2026-09-17T12:01:00.000Z bandwidth interface=default-route provider=mlab server_name=ndt.example.net outcome=success download=94.2 Mbps upload=20.1 Mbps
```

Both commands exit when finished and save a unique CSV under
`~/.local/share/netband/journals/once/` (or `$XDG_DATA_HOME/netband/journals/once/`).
The full results path is printed to stderr at startup. Use `--output netband.csv`
to save in your current directory instead.

## Keep monitoring

Leave ping monitoring running in your terminal:

```sh
netband --no-bandwidth run
```

After accepting the provider policies, enable scheduled and health-triggered bandwidth tests:

```sh
netband --accept-mlab-policy run
```

Results rotate daily under the same data directory in `journals/run/`. Netband
prints the output directory and each new CSV path to stderr. Stop with `Ctrl-C`;
completed measurements are flushed before exit.
For unattended monitoring, see [systemd setup](docs/service.md).

## Use your results

CSV is the durable record. For scripts, select JSONL output and keep logs separate:

```sh
netband --console jsonl once ping >events.jsonl 2>netband.log
```

JSONL is a best-effort live view; use CSV when you need the complete journal.
See [quick tests and output](docs/usage.md) for console modes and troubleshooting,
or inspect the [example records](docs/examples/README.md).

Netband has been [compared with M-Lab's Go reference client](docs/ndt7-validation.md).
The recorded dataset, measurement definitions, and limitations are available for inspection.

## Explore further

- [Configuration and providers](docs/configuration.md): targets, interfaces, TOML, and all CLI options.
- [Data format](docs/data-format.md): field definitions and parsing examples.
- [Documentation index](docs/README.md): scheduling, service operation, and research use.
- [Issues and feedback](https://github.com/gregl83/netband/issues): report a problem or suggest an improvement.

## License

[Apache 2.0](LICENSE)
