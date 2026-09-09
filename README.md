[![Build](https://github.com/gregl83/netband/actions/workflows/ci.yml/badge.svg)](https://github.com/gregl83/netband/actions/workflows/ci.yml)
[![Coverage Status](https://codecov.io/gh/gregl83/netband/graph/badge.svg?token=CL93O7DW9C)](https://codecov.io/gh/gregl83/netband)
[![Crates.io](https://img.shields.io/crates/v/netband.svg)](https://crates.io/crates/netband)
[![Apache 2.0 licensed](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

<p align="center"><img src="/assets/netband.svg" alt="netband" width="250" /></p>

# netband

Netband is a Linux-first command-line monitor that records latency, packet loss, and
NDT7 bandwidth measurements in a durable CSV journal. During automatic bandwidth tests,
it keeps measuring and classifies latency by NDT7 phase for loaded-latency analysis. It
is built for unattended home lab and Raspberry Pi monitoring, especially when
intermittent failures disappear before a manual speed test can capture them.

**Track network performance over time with timestamped measurements you can inspect and compare.**

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

A twenty-pair comparison recorded on 2026-09-06 against M-Lab's Go reference client
completed all forty runs without diagnostics. Download medians were **53.17 Mb/s
for Netband and 49.72 Mb/s for Go**, with a **+7.28% median paired difference**.
Results were sensitive to run order.

Upload medians were 20.56 and 19.44 Mb/s respectively. Netband measures locally
accepted payload bytes, while Go uses server-side upload measurements, so these
values describe different observation points.

See [NDT7 measurement validation](docs/ndt7-validation.md) for the recorded dataset,
build identities, uncertainty, protocol coverage and reproduction commands.

## Research use

Start with the [recorded-data analysis](docs/ndt7-validation.md#analyze-recorded-data-offline)
to inspect the benchmark without sending network traffic. Preserve the executable
hash, source revision, effective configuration, and measurement environment with
each study's CSV journals. Review addresses and diagnostics before sharing data;
token redaction does not anonymize a journal.

Netband measures ICMP latency/loss and one TCP/WebSocket stream per NDT7 direction.
These observations do not isolate an ISP as a cause or establish cluster interconnect,
MPI, or RDMA performance. The recorded comparison covers one host and one server.

Provider limits are enforced per scheduler state file. Separate hosts or independent
state files do not share a budget. Coordinate targets, aggregate traffic, and provider
authorization with the network operator before deploying across shared infrastructure.

## Running as a service

The reviewed [systemd unit](packaging/netband.service) uses a non-root dynamic user,
keeps measurements out of journald, and grants only `CAP_NET_RAW`. Installation,
ICMP permission setup, exit codes, state recovery, and troubleshooting are documented
in [Service operation](docs/service.md).

## Documentation

See the [documentation directory](docs/README.md) for configuration, service operation,
data formats, scheduling, measurement validation, and release maintenance.

## License

[Apache 2.0](LICENSE)
