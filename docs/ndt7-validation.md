# NDT7 measurement validation

Validation addresses reliability, repeatability and agreement with the Go reference
client. Agreement does not establish absolute accuracy: that requires calibrated
traffic generation or a separately measured path with a known bottleneck.

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
timeout. Cleanup adds neither
measurement bytes nor measurement time. Incoming control messages remain responsive
while writes are blocked.

A terminal bandwidth `success` means both rates are available, not that shutdown was
clean. Unexpected transport errors and cleanup timeouts remain diagnostics. Whole-test
timeout or cancellation retains completed direction measurements while preserving the
terminal outcome; unfinished direction counters remain unavailable. See
[Data format](data-format.md#outcomes).

## Reference-client benchmark

The Netband build identified below was compared with M-Lab's Go `ndt7-client` on
**2026-09-06 UTC**: twenty pairs, forty sequential measurements, against the same
operator-authorized Akamai Cloud NDT7 server from one Wi-Fi-connected Linux host.
Netband ran first in odd-numbered pairs and Go ran first in even-numbered pairs.
There was a ten-second cooldown between individual runs. Both directions ran in
every measurement, with normal production binaries and no receive instrumentation.
No compilation ran during measurement.

The [sanitized measurements](benchmarks/2026-09-06-akamai/measurements.csv),
[generated summary](benchmarks/2026-09-06-akamai/summary.md),
[machine-readable summary](benchmarks/2026-09-06-akamai/summary.json),
[paired analysis](benchmarks/2026-09-06-akamai/paired-analysis.json), and
[build metadata and binary hashes](benchmarks/2026-09-06-akamai/metadata.json)
are checked in. Raw journals and client addresses are kept private.

Netband revision: `8c214f5e810726fcd024174f3ee55dbf185f5487`. Go reference revision:
`1f6adcf81a3f29cae933a2e317e86e24826e2900`. Netband was built with `cargo build --release --locked`
and rustc 1.98.0 (88d9e12ae 2026-08-18); the reference used go version go1.27.1 linux/amd64.

| Client | Complete runs | Diagnostic runs | Download median (p10–p90) Mb/s | Download CV | Upload median (p10–p90) Mb/s | Upload CV |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Go reference | 20/20 | 0 | 49.72 (47.58–54.49) | 6.52% | 19.44 (17.08–20.77) | 10.86% |
| Netband | 20/20 | 0 | 53.17 (48.79–56.19) | 6.23% | 20.56 (19.30–22.65) | 6.63% |

CV is the sample standard deviation divided by the mean. p10–p90 is the central
80% of observed rates, not a confidence interval.

| Direction | Complete pairs | Median paired difference | Median absolute paired difference | p90 absolute paired difference |
| --- | ---: | ---: | ---: | ---: |
| Download | 20 | +7.28% | 9.15% | 17.51% |
| Upload | 20 | +6.68% | 7.32% | 25.17% |

Paired differences are `(Netband / reference - 1) × 100`; positive values favor
Netband. The median paired difference is distinct from the percentage difference
between the two clients' median rates.

### Run order and uncertainty

| Direction | Netband first: median paired difference | Netband second: median paired difference | Exploratory 95% interval |
| --- | ---: | ---: | ---: |
| Download | -4.96% | +11.48% | +2.43% to +12.17% |
| Upload | +7.32% | +5.78% | +5.29% to +9.08% |

Intervals use 20,000 bootstrap resamples of adjacent two-pair blocks, preserving
both run orders within each block. They are exploratory: longer-term path
variation is not controlled. The results describe the clients as a whole and do
not isolate the effect of TCP buffering.

All forty runs completed with both direction rates and no recorded diagnostics.
Netband's download median was 6.94% higher than Go's, and its median paired
advantage was 7.28%. Netband was faster in 13 of 20 download pairs.

The order effect was substantial: Netband's median paired difference was
-4.96% when it ran first and +11.48% when it ran second. The positive aggregate
result therefore does not establish an order-independent speed advantage. The
exploratory download interval was +2.43% to +12.17% under the stated resampling
method; its assumptions do not remove longer-term network variation.

Upload medians were 20.56 Mb/s for Netband and 19.44 Mb/s for Go; the median paired
difference was +6.68%. Those native measurements are not interchangeable.

Native upload values describe different observation points. A higher Netband upload
number does not by itself establish higher server-received throughput. Reported
network variation and diagnostic counts are part of the result. One server and
one Wi-Fi path cannot establish universal equivalence or a general speed advantage;
numerical agreement alone does not prove protocol conformance or absolute accuracy.

## Automated coverage

Download contracts verify TLS validation, exact application-byte counts across large
messages, automatic matching Pong replies, and continued reception when control
writes are backpressured. Upload unit and contract tests exercise:

- active deadlines and bounded cleanup under blocked writes;
- incoming Ping, measurement and Close messages during backpressure;
- partial-frame integrity, exact byte accounting and adaptive payload limits;
- early disconnects, no-data closure and retained diagnostics;
- cancellation and whole-test timeout across setup and transfer stages;
- completed-direction and diagnostic retention, including interrupted upload cleanup;
- unchanged reservation accounting and load-phase retention during cleanup.

Monitoring contracts verify concurrent pings and serialized bandwidth execution.
Published-data contracts verify the paired dataset, sanitized fields, and agreement
between the dataset, generated summary and documented medians.

## Analyze recorded data offline

Python 3 can regenerate the recorded summaries without installing Netband or contacting
a measurement server. Run from the repository root, writing results separately from
the checked-in evidence:

```sh
python3 scripts/summarize-ndt7-benchmark.py \
  docs/benchmarks/2026-09-06-akamai/measurements.csv \
  .netband/research-example
python3 scripts/analyze-ndt7-benchmark.py \
  docs/benchmarks/2026-09-06-akamai/measurements.csv \
  .netband/research-example
```

Compare the generated `summary.json` and `paired-analysis.json` with the checked-in
files. This reproduces the analysis of the recorded data, not a new network experiment
or validation of a different executable. Keep the accompanying build metadata with
the dataset.

## Reproduce the comparison

Build Netband and run both clients sequentially against an authorized endpoint:

```sh
cargo build --release --locked
NDT7_CLIENT_BIN=/path/to/ndt7-client \
NETBAND_BIN="$PWD/target/release/netband" \
./scripts/benchmark-ndt7-clients.sh \
  ndt.example.com 20 10
```

The arguments are server, pair count and cooldown seconds. A fourth argument selects
the output directory. The default is twenty pairs; any positive pair count is
accepted, so `ndt.example.com 4 10` gives a quick four-pair screen. Use
`ndt.example.com` as a documentation placeholder, not a public testing endpoint.

Raw output and generated summaries default to the Git-ignored `.netband/benchmarks/`
directory. Raw journals can contain client addresses; publish only whitelisted
measurement fields. To regenerate the checked-in summary from the sanitized data,
use the [offline analysis commands](#analyze-recorded-data-offline).

The benchmark harness runs both shared analysis scripts automatically. Each accepts
a measurements CSV and an optional output directory, defaulting to the CSV's
directory. Paired medians support small runs too; bootstrap intervals require at
least two complete adjacent blocks with alternating run order and no unmatched
successful pairs. Otherwise the intervals are `null`.

Keep shared tooling in `scripts/`. Publish each retained run under
`docs/benchmarks/<UTC-date>-<label>/` with sanitized `measurements.csv`,
`metadata.json`, and generated `summary.json`, `summary.md`, and
`paired-analysis.json`. Use a distinct label for multiple runs on the same date.
Keep raw output and binaries in `.netband/benchmarks/`, and update this page to
link to the current findings. Future runs reuse the tools without copying scripts.

Retain exact binaries, hashes, source revisions, native counters and timing
boundaries. Compare distributions, paired differences, run order and diagnostic
counts. Repeat under other conditions before generalizing. See
[Self-hosted NDT7 on Akamai Cloud](akamai-ndt-server.md).
