[![Build](https://github.com/gregl83/netband/actions/workflows/ci.yml/badge.svg)](https://github.com/gregl83/netband/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/netband.svg)](https://crates.io/crates/netband)
[![Apache 2.0 licensed](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

<p align="center"><img src="/assets/netband.svg" alt="netband" width="250" /></p>

# netband

Netband is a Linux-first command-line monitor that records latency, packet loss, and
NDT7 bandwidth measurements in a durable CSV journal. During automatic bandwidth tests,
it keeps measuring and classifies latency by NDT7 phase for loaded-latency analysis. It
is built for unattended home lab and Raspberry Pi monitoring, especially when
intermittent failures disappear before a manual speed test can capture them.

**One speed test is a snapshot; Netband builds the timestamped evidence to show when your ISP falls short.**

## Five-minute quick start

Prerequisites are Linux x86_64 or aarch64, `curl`, `tar`, `sha256sum`, and permission
to create ICMP sockets. The installer and its checksum-verified pre-built binaries are
hosted entirely on GitHub. No bandwidth traffic is sent until M-Lab consent is accepted
or a direct NDT7 provider is configured.

```sh
curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
  https://github.com/gregl83/netband/releases/latest/download/netband-installer.sh | sh
export PATH="$HOME/.local/bin:$PATH"
netband config check
```

Prefer to compile it yourself? See [Build from source](docs/service.md#build-from-source).

Run one ping round and inspect the authoritative CSV:

```sh
netband --output netband.csv once ping
head -n 3 netband.csv
```

Start foreground ping monitoring without bandwidth tests:

```sh
netband --output netband.csv --no-bandwidth run
```

Stop it with `Ctrl-C`. Netband flushes completed measurements before exiting. To test
bandwidth through M-Lab, first review [Netband privacy](PRIVACY.md), the
[M-Lab acceptable-use policy](https://www.measurementlab.net/aup/), and the
[M-Lab privacy policy](https://www.measurementlab.net/privacy/). Consent is explicit:

```sh
netband --output netband.csv --accept-mlab-policy once bandwidth
```

The command consumes one of M-Lab's maximum four automated runs per UTC day. Netband
persists that allowance across restarts and manual commands.

For an authorized manual diagnostic, `once bandwidth --force` bypasses configured
spacing, cooldown, and provider-specific local caps. It still requires M-Lab consent,
records the attempt, and cannot exceed M-Lab's hard four-start daily maximum.

## Output channels

| Command/mode | Measurement CSV | stdout presentation | Operational stderr |
| --- | --- | --- | --- |
| `run` default (`auto`) | Authoritative | Human on a TTY; off when redirected | Logs |
| `once ...` default (`human`) | Authoritative | Concise human result | Logs |
| `--console human` | Authoritative | Concise human result | Logs |
| `--console jsonl` | Authoritative | Versioned JSON Lines | Logs |
| `--console off` | Authoritative | Disabled | Logs |
| systemd example | Authoritative | Explicitly disabled | journald |

CSV is the source of truth. Human and JSONL stdout are independent, best-effort live
views. JSONL uses `schema_version=1`, but records may be dropped or the stream may stop
under backpressure or a broken pipe without affecting CSV or service health.

```sh
# Interactive human output
netband --output netband.csv --console human once ping

# Independently parse the live JSONL view
netband --output netband.csv --console jsonl once ping | jq -c .

# Quiet measurement with only the CSV and operational log retained
netband --output netband.csv --console off once ping 2>netband.log

# Keep all three channels separate
netband --output netband.csv --console jsonl once ping >events.jsonl 2>netband.log
```

## Configuration

Copy [examples/netband.toml](examples/netband.toml), adjust its relative output paths,
and validate it without sending probes:

```sh
netband --config examples/netband.toml config check
```

CLI values override TOML values; repeated CLI targets and interfaces replace their
TOML lists. The complete option/default table and direct-provider examples are in
[Configuration and providers](docs/configuration.md).

Scheduler state uses the operating system's per-user state directory and remains
independent of the directory where Netband is launched. Use `--state-file` only when
an explicit portable or service-managed location is required.

Netband supports M-Lab discovery and operator-supplied NDT7 servers. Direct endpoints
use verified TLS by default. Plain `ws://` requires `--allow-insecure-ndt` and is only
appropriate on an explicitly trusted private network. Netband does not provide or
imply a public Akamai NDT7 endpoint; CDN-hosted servers must be authorized and supplied
by their operator.

## Validation against the NDT7 reference client

On September 3, 2026, Netband 0.3.0 and M-Lab's Go NDT7 reference client were each
run 20 times from the same Wi-Fi-connected Linux host to the same nearby Akamai Cloud
server. The order alternated within each pair, with a 10-second cooldown between runs.

| Client | Complete runs | Download median (p10–p90) | Download CV | Upload median (p10–p90) | Upload CV |
| --- | ---: | ---: | ---: | ---: | ---: |
| NDT7 reference | 20/20 | 22.24 (19.75–23.17) Mbit/s | 6.28% | 16.62 (16.05–17.33) Mbit/s | 3.53% |
| Netband | 20/20 | 21.84 (19.77–23.99) Mbit/s | 9.93% | 17.97 (16.93–19.07) Mbit/s | 4.80% |

All 40 runs produced complete download and upload results. Download distribution medians
differed by 1.8%, and their central 80% ranges were nearly identical. Netband's median
upload was 8.1% higher; the clients intentionally summarize upload from different
observation points, so their upload values are comparable but not numerically interchangeable.
Netband also preserved a late upload `EPIPE` or `ECONNRESET` diagnostic in 9 runs while
still producing complete results; the full report explains this shutdown behavior.
This is evidence of completion reliability, repeatability, and agreement under the
measured conditions—not proof of warning-free operation or absolute path capacity.

See [NDT7 reference-client validation](docs/ndt7-validation.md) for the method,
per-pair data, interpretation, limitations, and reproduction command.

## Running as a service

The reviewed [systemd unit](packaging/netband.service) uses a non-root dynamic user,
keeps measurements out of journald, and grants only `CAP_NET_RAW`. Installation,
ICMP permission setup, exit codes, state recovery, and troubleshooting are documented
in [Service operation](docs/service.md).

## Reference

- [Configuration and providers](docs/configuration.md)
- [NDT7 reference-client validation](docs/ndt7-validation.md)
- [Self-hosted NDT7 on Akamai Cloud](docs/akamai-ndt-server.md)
- [CSV schema and outcomes](docs/data-format.md)
- [Scheduling, triggers, cooldowns, and fairness](docs/scheduling.md)
- [Privacy and provider data](PRIVACY.md)
- [Service operation and recovery](docs/service.md)
- [Release validation](docs/release.md)

## License

[Apache 2.0](LICENSE)
