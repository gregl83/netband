# Configuration and providers

Netband reads one optional TOML file, then applies CLI overrides. Scalar CLI values
replace TOML values. Repeated `--interface` and `--ping-target` values replace, rather
than extend, their TOML lists. Relative paths are resolved from the current directory.
Unknown TOML keys, zero durations, duplicate interfaces/targets, and conflicting output
settings are errors. `config check` resolves all values and validates paths/interfaces
without sending a probe.

```sh
netband --config examples/netband.toml config check
netband --config examples/netband.toml --ping-interval 10s config check
```

The complete default-oriented file is [examples/netband.toml](../examples/netband.toml).
The service file is [packaging/netband.toml](../packaging/netband.toml). `output` and
`output_dir` are mutually exclusive.

## CLI options and defaults

Durations accept values such as `250ms`, `5s`, `36m`, and `2h`.

| CLI option | TOML key | Default / behavior |
| --- | --- | --- |
| `--config FILE` | n/a | No file; load the named TOML before CLI overrides |
| `--console auto\|human\|jsonl\|off` | `console` | `auto` for `run`/`config check`; `human` for `once`; `auto` is human on a TTY and off otherwise |
| `--verbosity error\|warn\|info\|debug\|trace` | `verbosity` | `info` |
| `--interface NAME` (repeatable) | `interfaces` | Empty; use the default route |
| `--ping-target IP` (repeatable) | `ping.targets` | `1.1.1.1`, `8.8.8.8`, `9.9.9.9` |
| `--ping-interval DURATION` | `ping.interval` | `5s` |
| `--ping-timeout DURATION` | `ping.timeout` | `2s` per probe |
| `--no-bandwidth` | n/a | False; disable automatic bandwidth work for this `run` |
| `--force` | n/a | False; for `once bandwidth`, bypass configured cap, spacing, and cooldown for this attempt; M-Lab consent and its hard four-start daily cap still apply |
| `--output FILE` | `output` | No fixed file; create a timestamped CSV in the current directory |
| `--output-dir DIR` | `output_dir` | Current directory when neither output option is set |
| `--state-file FILE` | `state_file` | Platform user state directory, file `scheduler.json` |
| `--shutdown-grace DURATION` | `shutdown_grace` | `30s` |
| `--ndt-provider mlab\|direct` | `bandwidth.provider` | `mlab` |
| `--mlab-locate-url URL` | `bandwidth.mlab.locate_url` | M-Lab Locate v2 NDT7 URL; override is intended for testing |
| `--ndt-target HOST[:PORT]` | `bandwidth.direct.target` | None; direct provider only; generates standard NDT7 paths |
| `--ndt-download-url URL` | `bandwidth.direct.download_url` | None; requires upload URL and excludes target |
| `--ndt-upload-url URL` | `bandwidth.direct.upload_url` | None; requires download URL and excludes target |
| `--ndt-tls-server-name DNS_NAME` | `bandwidth.direct.tls_server_name` | None; TLS identity for an IP connection |
| `--ndt-ca-cert FILE` | `bandwidth.direct.ca_cert` | System/WebPKI roots only; add the named private CA bundle |
| `--allow-insecure-ndt` | `bandwidth.direct.allow_insecure` | False; required for plain `ws://` on a trusted private network |
| `--bandwidth-daily-max COUNT` | `bandwidth.daily_max` | `4`; `0` disables bandwidth; M-Lab rejects values above 4 |
| `--bandwidth-min-spacing DURATION` | `bandwidth.min_spacing` | `36m`; direct minimum is at least timeout + shutdown margin and 60s |
| `--bandwidth-slot-jitter-pct PERCENT` | `bandwidth.slot_jitter_pct` | `50`, range 0-100 |
| `--bandwidth-timeout DURATION` | `bandwidth.whole_test_timeout` | `55s` for discovery, download, and upload together |
| `--bandwidth-shutdown-margin DURATION` | `bandwidth.shutdown_margin` | `15s` reserved for clean cancellation |
| `--loss-window-rounds ROUNDS` | `bandwidth.trigger.window_rounds` | `6` rounds |
| `--loss-min-samples COUNT` | `bandwidth.trigger.min_samples` | `6` probes |
| `--loss-threshold-pct PERCENT` | `bandwidth.trigger.loss_threshold_pct` | `50` percent |
| `--rtt-threshold-ms MILLISECONDS` | `bandwidth.trigger.rtt_threshold_ms` | Disabled; positive p95 RTT threshold when set |
| `--recovery-loss-pct PERCENT` | `bandwidth.trigger.recovery_loss_pct` | `10` percent; must be below loss trigger |
| `--recovery-rounds ROUNDS` | `bandwidth.trigger.recovery_rounds` | `3` consecutive healthy rounds |
| `--pending-trigger-ttl DURATION` | `bandwidth.trigger.pending_ttl` | `30m` |
| `--cooldown-initial DURATION` | `bandwidth.cooldown.initial` | `60s` |
| `--cooldown-max DURATION` | `bandwidth.cooldown.max` | `16m` |
| `--accept-mlab-policy` | `bandwidth.accept_mlab_policy` | False; explicit M-Lab AUP/privacy acknowledgement |

`run`, `once ping`, `once bandwidth`, and `config check` are subcommands, not TOML
values. CLI help is authoritative for spelling: `netband --help`.

## M-Lab provider

M-Lab uses its Locate API to select NDT7 servers. Automated operation is disabled until
`accept_mlab_policy = true` or `--accept-mlab-policy` is supplied. Before enabling it,
read [Netband privacy](../PRIVACY.md), the
[M-Lab acceptable-use policy](https://www.measurementlab.net/aup/), and the
[M-Lab privacy policy](https://www.measurementlab.net/privacy/).

The M-Lab profile has a hard global maximum of four starts per UTC day and a 36-minute
minimum spacing. Manual commands, triggered tests, interfaces, and restarts share that
allowance. `--force` never bypasses consent or the hard four-start limit. Netband does
not silently fall back between M-Lab and direct providers.

## Direct provider

Direct mode bypasses M-Lab Locate. The operator is responsible for endpoint
authorization, capacity, privacy notices, retention, and rate policy. M-Lab's four-run
limit applies only to the M-Lab profile. Direct mode may set a different cap and spacing,
while retaining persisted admission, trigger latching, and adaptive rate-limit backoff:

```sh
netband --ndt-provider direct \
  --ndt-target ndt.operator.example.invalid:443 \
  --bandwidth-daily-max 8 --bandwidth-min-spacing 90m \
  config check
```

All names below use the reserved `.invalid` suffix and all IPs are documentation
ranges. Replace them with an authorized operator endpoint.

```sh
# DNS name with standard /ndt/v7/download and /ndt/v7/upload paths
netband --ndt-provider direct --ndt-target ndt.operator.example.invalid:443 config check

# IPv4 and bracketed IPv6 targets
netband --ndt-provider direct --ndt-target 192.0.2.40:443 config check
netband --ndt-provider direct --ndt-target '[2001:db8::40]:443' config check

# Explicit nonstandard paths
netband --ndt-provider direct \
  --ndt-download-url wss://ndt.operator.example.invalid/custom/download \
  --ndt-upload-url wss://ndt.operator.example.invalid/custom/upload config check

# Connect to an IP while validating the operator's DNS certificate identity
netband --ndt-provider direct --ndt-target 192.0.2.40:443 \
  --ndt-tls-server-name ndt.operator.example.invalid config check

# Add an operator-controlled private CA; WebPKI validation remains enabled
netband --ndt-provider direct --ndt-target 192.0.2.40:443 \
  --ndt-tls-server-name ndt.operator.example.invalid \
  --ndt-ca-cert /etc/netband/operator-ca.pem config check

# Explicitly insecure private/LAN operation only
netband --ndt-provider direct \
  --ndt-download-url ws://192.168.50.10:8080/ndt/v7/download \
  --ndt-upload-url ws://192.168.50.10:8080/ndt/v7/upload \
  --allow-insecure-ndt config check
```

A CDN-hosted endpoint uses the same DNS form, for example an operator-controlled
`ndt.customer.example.invalid` name whose DNS is placed behind that operator's CDN.
Netband does not supply, discover, endorse, or imply a public Akamai NDT7 endpoint.
Using Akamai or another CDN requires an authorized server deployment and compliance
with that provider's terms and traffic policy.

URLs may contain operator query parameters, but credentials embedded in URL userinfo
are rejected. Query values are removed from logs, stdout, provider fingerprints, and
the CSV `server` field. Prefer a protected config file if an endpoint requires a token.
