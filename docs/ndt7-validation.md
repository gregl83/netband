# NDT7 measurement validation

Netband is an independent Rust client for the [NDT7 protocol](https://github.com/m-lab/ndt-server/blob/main/spec/ndt7-protocol.md).
It uses M-Lab discovery or an operator-supplied server. This guide explains what its
rates measure, what local tests cover, and how to compare a build with
[M-Lab's Go reference client](https://github.com/m-lab/ndt7-client-go).

## Measurement behavior

Each direction uses one TCP/WebSocket connection. Download and upload run
sequentially. Netband performs asynchronous I/O and batches TCP reads through a
64 KiB buffer beneath TLS. TLS certificate validation, WebSocket framing and
control responses remain active throughout the test.

Download throughput uses client-received binary application bytes and elapsed time.
Go's download summary uses its last periodic client snapshot and includes text
measurement bytes. These are both client-side observations, with different sampling
boundaries. Netband's upload throughput uses application payload bytes accepted by
the local WebSocket sink during the active upload window, including any buffered
tail. Go summarizes upload from server-side TCP measurements. Neither WebSocket nor
TLS overhead is counted in Netband's application-byte rates.

Upload accepts payloads for ten seconds after the handshake, or until the peer closes
or a transport error occurs. Adaptive outbound payload sizing starts at 8 KiB and
caps at 1 MiB; the inbound limit is 16 MiB. The close handshake has a
separate two-second allowance, subject to earlier cancellation or the whole-test
timeout. Cleanup adds neither measurement bytes nor measurement time. Incoming
control messages remain responsive while writes are blocked.

A terminal bandwidth `success` means both rates are available, not that shutdown was
clean. Unexpected transport errors and cleanup timeouts remain diagnostics. Whole-test
timeout or cancellation retains completed direction measurements while preserving the
terminal outcome; unfinished direction counters remain unavailable. See
[Data format](data-format.md#outcomes).

## Protocol behavior and automated coverage

Netband requests and verifies the `net.measurementlab.ndt.v7` WebSocket subprotocol
and identifies itself as `netband/<version>`. M-Lab discovery follows redirects,
preserves paired download/upload endpoints, and handles capacity and rate-limit
responses. Direct endpoints bypass discovery. Public M-Lab tests require consent;
see [provider data and privacy](../PRIVACY.md).

| Area | Coverage |
| --- | --- |
| Discovery and consent | [Provider contracts](../tests/provider_contract.rs): client identification, redirects, endpoint pairs, no capacity, rate limits, and consent before network access. |
| Transport and measurement | [Bandwidth contracts](../tests/bandwidth_contract.rs): TLS validation, large messages, byte counts, matching Pong replies, adaptive upload payloads, and control messages under backpressure. |
| Deadlines and failures | [Bandwidth contracts](../tests/bandwidth_contract.rs): bounded cleanup, cancellation, timeouts, retained direction results, and diagnostics. |
| Scheduling | [Scheduler contracts](../tests/scheduler_contract.rs): admission, persisted budgets, and cooldowns. |
| Comparison tooling | [Harness tests](../scripts/test_benchmark.py): binary identity, exact commands, and failed-run retention. |

These tests use local fixtures; they do not establish interoperability with every
server deployment. Live comparisons evaluate identified binaries and network paths;
numerical agreement alone does not prove protocol conformance or absolute accuracy.

## v1 reference-client comparison

Netband **1.0.0** and M-Lab's Go reference client completed twenty alternating pairs
on **2026-09-18 UTC**, against the same operator-authorized Akamai Cloud NDT7 server.
Both directions ran sequentially in each test, with ten seconds between tests. The
client host used 5 GHz Wi-Fi; no builds or test suites ran during measurement.

| Client | Complete runs | Download median | Download CV | Upload median | Upload CV |
| --- | ---: | ---: | ---: | ---: | ---: |
| Netband | 20/20 | 209.76 Mb/s | 16.18% | 114.50 Mb/s | 5.40% |
| Go reference | 20/20 | 208.42 Mb/s | 12.09% | 110.20 Mb/s | 9.14% |

All forty runs returned both rates, with no recorded diagnostics. Download
variability was greater for Netband; upload variability was greater for the reference.
The lowest upload samples were 97.31 Mb/s for Netband and 74.00 Mb/s for the reference.
All samples are included in the summaries; these observations do not establish the
cause of individual rate changes.

| Direction | Median paired difference | Exploratory 95% interval |
| --- | ---: | ---: |
| Download | -0.36% | -6.99% to +3.68% |
| Upload | +2.81% | +1.34% to +7.23% |

The paired download difference differs from the comparison of client medians because
it compares adjacent measurements. The download interval includes zero. Upload rates
were higher for Netband in the paired analysis, but the clients' upload observation
points differ. This single-host comparison does not establish a speed advantage,
measurement accuracy, or general equivalence.

Inspect the [measurements](benchmarks/2026-09-18-v1-akamai/measurements.csv),
[full summary](benchmarks/2026-09-18-v1-akamai/summary.md),
[machine-readable summary](benchmarks/2026-09-18-v1-akamai/summary.json),
[paired and run-order analysis](benchmarks/2026-09-18-v1-akamai/paired-analysis.json), and
[versions, revisions, binary hashes, and environment](benchmarks/2026-09-18-v1-akamai/metadata.json).
Router/AP model, firmware, test location, and server version were not recorded.
Raw journals and exact executables are retained privately; published data excludes
client addresses and raw paths.

## Reproduce the comparison

Build Netband and run both clients sequentially against an authorized endpoint:

```sh
cargo build --release --locked
NDT7_CLIENT_BIN=/path/to/ndt7-client \
NETBAND_BIN="$PWD/target/release/netband" \
NETBAND_BENCHMARK_NOTES="router/AP model; wired or Wi-Fi; band; location" \
./scripts/benchmark-ndt7-clients.sh \
  ndt.example.com 20 10
```

The arguments are server, pair count and cooldown seconds. A fourth argument selects
the output directory. The default is twenty pairs; any positive pair count is
accepted, so `ndt.example.com 4 10` gives a quick four-pair screen. Use
`ndt.example.com` as a documentation placeholder, not a public testing endpoint.

Raw output and generated summaries default to the Git-ignored `.netband/benchmarks/`
directory. Raw journals can contain client addresses; publish only whitelisted
measurement fields. The harness alternates which client runs first in each pair,
with the specified cooldown between individual runs.

Before measurements begin, the harness saves both executable SHA-256 hashes,
Netband's version, pair count, cooldown, route, and supplied network notes in
`metadata.txt`. `netband-config.txt` records resolved shared settings; each raw
measurement has a `.command` file with its exact shell-quoted invocation, including
Netband's per-attempt output path and `--force`. Metadata collection failure stops
preparation; client measurement failures are still recorded for analysis.

Keep the binaries unchanged during a comparison. Supply router/AP model, firmware,
wired/Wi-Fi connection, band, and location in `NETBAND_BENCHMARK_NOTES`; these are
operator observations, not automatically detected facts. The route is a startup
snapshot, not proof of the interface used throughout the run. Default-route journal
rows leave `interface` empty; use `--interface` for deliberately bound standalone
tests. Record the source revision alongside the executable hash when available.
Keep this metadata private until reviewed for publication.

The benchmark harness runs both shared analysis scripts automatically. Each accepts
a measurements CSV and an optional output directory, defaulting to the CSV's
directory. Paired medians support small runs too; bootstrap intervals require at
least two complete adjacent blocks with alternating run order and no unmatched
successful pairs. Otherwise the intervals are `null`.

## Analyze and interpret results

The harness generates `measurements.csv`, `summary.json`, `summary.md`, and
`paired-analysis.json`. To regenerate summaries without network traffic:

```sh
python3 scripts/summarize-ndt7-benchmark.py /path/to/measurements.csv
python3 scripts/analyze-ndt7-benchmark.py /path/to/measurements.csv
```

Compare completion and diagnostic counts, rate distributions, paired differences,
and run order. Paired differences are `(Netband / reference - 1) × 100`: a positive
value means Netband reported a higher rate. It does not establish greater accuracy
or higher server-received throughput, particularly for upload, where the clients
use different observation points.

The summary's p10–p90 range contains the central 80% of observed rates; it is not a
confidence interval. CV is sample standard deviation divided by the mean. Paired
analysis uses 20,000 bootstrap resamples of adjacent two-pair blocks, preserving both
run orders within each block. Its intervals are exploratory and do not remove
longer-term network variation.

Retain exact binaries, hashes, source revisions, and measurement settings. Record
the server version when available. Repeat across hosts, servers, and network
conditions before generalizing; agreement on one path does not establish absolute
accuracy. That requires calibrated traffic generation or a separately measured path
with a known bottleneck. For an operator-controlled endpoint, see
[Self-hosted NDT7 on Akamai Cloud](akamai-ndt-server.md).

## Research use

Preserve the executable hash, source revision, effective configuration, and measurement
environment with each study's CSV journals. The journal records session and measurement-run lifecycles,
results, request failures, and scheduler decisions. UUIDs and explicit parent references
link related events; see the [data contract](data-format.md).
Review addresses and diagnostics before sharing data;
token redaction does not anonymize a journal.

Netband measures ICMP latency/loss and one TCP/WebSocket stream per NDT7 direction.
These observations do not isolate an ISP as a cause or establish cluster interconnect,
MPI, or RDMA performance.

Provider limits are enforced per scheduler state file. Separate hosts or independent
state files do not share a budget. Coordinate targets, aggregate traffic, and provider
authorization with the network operator before deploying across shared infrastructure.
