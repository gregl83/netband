# Quick tests and output

[Install Netband](install.md), then choose a one-shot command. Each prints human-readable
results, saves a CSV journal, and exits. No TOML file or service is required.

[Quick tests](#one-ping-round) · [Find results](#where-results-go) ·
[Scripts](#output-channels) · [Monitoring](#continuous-monitoring) ·
[Troubleshooting](#configure-or-troubleshoot)

## One ping round

```sh
netband once ping
```

To select targets instead:

```sh
netband --ping-target 1.1.1.1 --ping-target 9.9.9.9 once ping
```

Without explicit targets, Netband probes `1.1.1.1`, `8.8.8.8`, and `9.9.9.9`.
Each target gets one attempt. A failed probe produces a result too; a one-round loss
percentage describes only that attempt, not a long-term loss estimate. Ping requires
[ICMP permissions](service.md#icmp-permissions).

## One bandwidth test

After reviewing [Netband privacy](../PRIVACY.md), the
[M-Lab acceptable-use policy](https://www.measurementlab.net/aup/), and the
[M-Lab privacy policy](https://www.measurementlab.net/privacy/):

```sh
netband --accept-mlab-policy once bandwidth
```

This runs NDT7 download and upload measurements sequentially. It does not also run
ping probes; loaded ping measurements are available during continuous monitoring.
The whole-test timeout defaults to 55 seconds. Completed directions remain available
if the attempt is interrupted. See [measurement semantics](data-format.md#timing-and-throughput).

Manual tests share provider accounting with continuous monitoring. A start is
reserved before the first NDT7 connection. Netband enforces a four-start UTC daily
cap for M-Lab, persisted in scheduler state. A failed discovery before admission
does not consume a start. A suppressed command can finish without sending test traffic; inspect its journal
record or stderr explanation.

For an authorized manual diagnostic, `once bandwidth --force` bypasses configured
spacing, cooldown, and local caps. It still requires consent, records the attempt,
and cannot exceed M-Lab's hard four-start daily maximum. Separate hosts/state files
do not share a budget. See [scheduling](scheduling.md) and
[direct NDT7 providers](configuration.md#direct-provider).

## Where results go

Netband prints the absolute CSV path to stderr when it opens the file.

| Command | Default directory | Behavior |
| --- | --- | --- |
| `once` | `~/.local/state/netband/journals/once/` | A new timestamped CSV for each invocation |
| `run` | `~/.local/state/netband/journals/run/` | A new segment at startup, then daily rotation |

`$XDG_STATE_HOME` overrides `~/.local/state`. Default directories are created as
needed. Earlier results are preserved; files are not automatically deleted.
To save in your current directory instead:

```sh
netband --output netband.csv once ping
```

Paths remain visible with `--console off` or `--verbosity error`. Directory output
also reports its directory at startup and each new CSV on rotation. `config check`
shows the resolved `run` destination without creating files. See
[storage and recovery](data-format.md#journal-storage) for locking and interrupted runs.

The CSV includes lifecycle and diagnostic records as well as measurements. Filter
`event_kind=ping_probe` or `event_kind=bandwidth` when extracting results; the first
rows are run starts. See [parsing examples](data-format.md#reading-examples).

## Output channels

Measurements always go to CSV. Stderr carries result paths and operational logs;
stdout follows the selected presentation mode:

| Command/mode | stdout |
| --- | --- |
| `run` default (`auto`) | Human on a terminal; off when redirected |
| `once ...` default (`human`) | Concise human results |
| `--console human` | Concise human results |
| `--console jsonl` | Versioned JSON Lines |
| `--console off` | Disabled |
| systemd example | Disabled; stderr goes to journald |
| `config check` | Resolved `key=value` configuration; no measurement CSV |

CSV is the source of truth. Human and JSONL stdout are independent, best-effort live
views. JSONL uses `schema_version=1`, but records may be dropped or the stream may stop
under backpressure or a broken pipe without affecting CSV or service health.
Human bandwidth output selects bps, Kbps, Mbps, Gbps, or Tbps with up to three
significant digits (for example, `download=94.2 Mbps upload=1.25 Gbps`). Extreme
values use scientific notation; missing results display `-`. Ping values use up to
three decimal places. CSV and JSONL retain full precision and fixed Mbps fields.

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

## Continuous monitoring

Start with pings only, then add bandwidth once the provider policies are accepted:

```sh
netband --no-bandwidth run
# Alternative: scheduled and health-triggered bandwidth, with concurrent pings.
netband --accept-mlab-policy run
```

These are alternatives; stop the first command with `Ctrl-C` before starting the second.
Default `run` output already rotates daily. To choose a directory and add a size limit:

```sh
mkdir -p measurements
netband --output-dir measurements --rotate-max-bytes 67108864 --no-bandwidth run
```

Rotation is daily at UTC midnight or at a soft 64 MiB limit. Complete batches stay
together; segments remain until you archive or remove them. See
[rotation and recovery](data-format.md#rotating-directory-output) and
[systemd operation](service.md).

## Configure or troubleshoot

| What you see | What to do |
| --- | --- |
| No console output after redirecting `run` | Select `--console human` or `--console jsonl`; `auto` disables redirected stdout |
| A second command reports locked scheduler state | Stop the bandwidth-enabled monitor before a manual bandwidth test; a quick ping can run alongside it |
| No CSV in your working directory | Use the path printed to stderr, or choose `--output results.csv` |
| Bandwidth is deferred or suppressed | Check stderr or scheduler records for the [cap, spacing, or cooldown](scheduling.md) |


CLI values override TOML. Copy [the example configuration](../examples/netband.toml),
adjust its paths, and validate without sending probes:

```sh
netband --config examples/netband.toml config check
```

- Permission errors: [ICMP setup](service.md#icmp-permissions).
- Deferred or suppressed bandwidth: [provider limits and cooldowns](scheduling.md).
- Scripts and exit status: [exit codes](service.md#exit-codes).
- Targets, interfaces, TLS and state paths: [configuration reference](configuration.md).
- Other failures: [troubleshooting](service.md#troubleshooting).
