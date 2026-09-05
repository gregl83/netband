# NDT7 measurement validation

Validation addresses three separate questions:

1. **Reliability:** are both direction measurements available, and do diagnostics
   qualify their completion?
2. **Repeatability:** how tightly do measurements cluster under fixed conditions?
3. **Agreement:** how close are Netband and the NDT7 reference client's throughput
   distributions?

Agreement does not establish absolute accuracy. That requires calibrated traffic
generation or a separately measured path with a known bottleneck.

## Measurement behavior

Download throughput uses client-received application bytes and elapsed time. Upload
throughput uses application payload bytes accepted by the local WebSocket sink during
the active upload window. These bytes include any buffered tail, not confirmed server
receipt. WebSocket and TLS overhead are excluded.

Upload accepts payloads for ten seconds after the handshake, or until the peer closes
or a transport error occurs. Reads remain responsive while writes are blocked. Adaptive
outbound payload sizing starts at 8 KiB and caps at 1 MiB; the inbound limit is 16 MiB.
The close handshake has a separate two-second allowance, subject to earlier cancellation
or the whole-test timeout. Cleanup adds neither measurement bytes nor measurement time.

Normal expiration of the active window is not a failure. Unexpected transport errors
and cleanup timeouts remain diagnostics. A terminal bandwidth `success` means both
rates are available, not that shutdown was clean. See [Data format](data-format.md).

## Reference-client benchmark

M-Lab's Go `ndt7-client` was used alongside Netband to check comparable throughput
and interoperability, complementing specification-oriented implementation tests.
The [recorded paired measurements](benchmarks/2026-09-03-akamai.csv) contain twenty
runs per client against the same nearby Akamai Cloud NDT7 server from one Wi-Fi-connected
Linux host. Client order alternated within each pair, with a ten-second cooldown
between individual runs.

| Client | Complete runs | Download median (p10–p90) | Upload median (p10–p90) |
| --- | ---: | ---: | ---: |
| NDT7 reference client | 20/20 | 22.24 (19.75–23.17) Mb/s | 16.62 (16.05–17.33) Mb/s |
| Netband | 20/20 | 21.84 (19.77–23.99) Mb/s | 17.97 (16.93–19.07) Mb/s |

Download medians differed by approximately 1.8%, with closely overlapping central
80% ranges. Netband's upload median was approximately 8.1% higher. Different upload
observation points, buffering, timing boundaries, and conditions between sequential
runs can contribute to that offset; the results are similar, not interchangeable.
Nine Netband runs retained upload-end diagnostics while still producing both rates.

These measurements support comparable throughput under the tested conditions.
Specification consistency is assessed separately through protocol behavior, message
sizing, timing, and control-frame tests; numerical agreement alone does not prove
conformance or absolute accuracy. The upload lifecycle's live reference-client agreement
requires a dedicated paired run; the local checks below do not establish it.

## Automated coverage

Upload unit and contract tests exercise:

- the active deadline and bounded cleanup under blocked writes;
- incoming Ping, measurement, and Close messages during backpressure;
- automatic Pong responses and close acknowledgement;
- partial-frame integrity, exact byte accounting, and adaptive payload limits;
- early disconnects, no-data closure, and retained diagnostics;
- exclusion of connection setup and close-handshake waiting from the upload window;
- cancellation, whole-test timeout, and load-phase retention during cleanup.

Monitoring contracts also verify concurrent pings and serialized bandwidth execution.

## Local throughput checks

A localhost-only WebSocket fixture exercised two runs per profile, three-second
downloads, ten-second server upload windows, periodic Ping/measurement messages, and
a fifteen-second hard disconnect. The throttled fixture paced application I/O at
44 Mb/s down and 17.6 Mb/s up; the fast fixture had no rate limit.

| Profile | Download median | Upload median |
| --- | ---: | ---: |
| Throttled | 43.987 Mb/s | 21.395 Mb/s |
| Fast loopback | 46.700 Gb/s | 26.229 Gb/s |

All four upload measurement windows ended at approximately 10.001 seconds. One
throttled run exhausted the close allowance and reported a cleanup timeout; the other
three had no diagnostics. Buffered data cannot always drain within two seconds.

These are small functional/performance checks, not a calibrated path test or an
external reference-client comparison. Sender-side buffering makes the throttled upload
estimate exceed the fixture's receive pacing. Fast-loopback results are sensitive to
host and fixture overhead. Neither profile establishes absolute accuracy or a general
throughput guarantee. Treat these checks separately from the recorded reference-client
benchmark and from a dedicated live comparison of the upload lifecycle.

## Reference-client comparison procedure

Run Netband and M-Lab's Go reference client sequentially against the same authorized
endpoint, alternating their order and allowing a cooldown between runs:

```sh
NDT7_CLIENT_BIN=/path/to/ndt7-client \
NETBAND_BIN=/path/to/netband \
./scripts/benchmark-ndt7-clients.sh \
  ndt.example.com 20 10
```

The arguments are server, pair count, and cooldown seconds. A fourth argument selects
the output directory. Raw results and the generated summary default to the Git-ignored
`.netband/benchmarks/` directory. Keep raw journals private: they can contain public and
local client addresses. Use `ndt.example.com` as a documentation placeholder, not a
public testing endpoint.

Retain each client's native counters and timing boundaries. The reference client
summarizes upload from server-side TCP measurements, whereas Netband uses local payload
acceptance. Compare distributions rather than treating individual values as equivalent.
Report medians, p10/p90, coefficients of variation, paired differences, and diagnostic
counts separately. Repeat at different times and monitor server CPU and network limits
before generalizing results. See [Self-hosted NDT7 on Akamai Cloud](akamai-ndt-server.md).
