# NDT7 reference-client validation

This validation asks three separate questions:

1. **Reliability:** do both clients produce complete results for both NDT7 directions,
   and do any auxiliary diagnostics qualify those results?
2. **Repeatability:** how tightly does each client cluster while the endpoint and test
   environment remain fixed?
3. **Agreement:** how close are Netband and the reference client's reported throughput
   distributions?

It does not claim to establish absolute accuracy. That would require calibrated traffic
generation or a separately measured path with a known bottleneck. NDT7 measures the
end-to-end path to one server, and ordinary network conditions can change between tests.

## Test environment

The measurements were collected on September 3, 2026 from 23:24–23:47 PDT
(September 4, 06:24–06:47 UTC).

| Component | Configuration |
| --- | --- |
| Client host | x86-64 Linux 7.1.9, connected through `wlp0s20f3` (Wi-Fi) |
| Netband | 0.3.0 installed from crates.io; release source at commit [`7531961`](https://github.com/gregl83/netband/commit/753196116cf52e4f8295e619a42fe51e3e599bf8) |
| Reference | M-Lab [`ndt7-client-go`](https://github.com/m-lab/ndt7-client-go), v0.10.1 program code built at [`1f6adcf`](https://github.com/m-lab/ndt7-client-go/commit/1f6adcf81a3f29cae933a2e317e86e24826e2900) with Go 1.27.1 |
| Server | `ndt.example.com` in this report, standing in for an operator-managed NDT7 server on a nearby Akamai Cloud compute instance |
| Transport | Direct `wss://` connections with normal WebPKI certificate verification |

The reference build's commit follows v0.10.1 by one documentation-only change; its
program code is identical to the v0.10.1 tag. The actual server endpoint and client
address are deliberately withheld. `ndt.example.com` is the reserved documentation
placeholder used by the [Akamai deployment guide](akamai-ndt-server.md).

## Method

The run contained 20 pairs, or 40 complete NDT7 tests. Each test performed download and
upload. Netband ran first in odd-numbered pairs and the reference client ran first in
even-numbered pairs, controlling for a simple first/second or warming effect. There was
a 10-second cooldown after every individual run.

Each program's native final throughput was retained rather than recomputing both from a
shared counter. This matters for upload:

- both clients calculate download at the receiver from client-observed application bytes
  and elapsed time;
- the [reference summary](https://github.com/m-lab/ndt7-client-go/blob/v0.10.1/internal/runner/runner.go)
  calculates upload from the server's TCP `BytesReceived` and `ElapsedTime` values; and
- Netband calculates upload from application payload bytes accepted by its WebSocket sink
  and client-observed elapsed time. See [`throughput_mbps`](../src/bandwidth.rs).

Those are measurements of the same transfer from different observation points. WebSocket
framing, buffering, the final in-flight data, and slightly different timing boundaries can
produce a stable offset without either implementation being erratic.

There is a second implementation difference at the end of upload. The reference client's
upload worker stops on its own 10-second context and does not propagate its writer
goroutine's return value. Netband continues sending until the server ends the test and
preserves a writer-side error if the socket closes before it processes the WebSocket close
frame. That behavior is conservative and can attach a diagnostic to an otherwise complete
measurement.

For each client and direction, the analysis reports the median, linearly interpolated p10
and p90, and coefficient of variation (sample standard deviation divided by the mean).
Paired differences use `(Netband - reference) / reference × 100`. Because paired tests are
sequential, a paired difference includes real network movement during the gap as well as
implementation differences.

## Results

| Client | Complete runs | Download median (p10–p90) | Download CV | Upload median (p10–p90) | Upload CV |
| --- | ---: | ---: | ---: | ---: | ---: |
| NDT7 reference | 20/20 (100%) | 22.24 (19.75–23.17) Mbit/s | 6.28% | 16.62 (16.05–17.33) Mbit/s | 3.53% |
| Netband | 20/20 (100%) | 21.84 (19.77–23.99) Mbit/s | 9.93% | 17.97 (16.93–19.07) Mbit/s | 4.80% |

| Direction | Median difference between distribution medians | Median paired signed difference | Median paired absolute difference | p90 paired absolute difference |
| --- | ---: | ---: | ---: | ---: |
| Download | -1.83% | -0.73% | 5.90% | 12.73% |
| Upload | +8.13% | +7.21% | 7.21% | 15.30% |

Nine of the 20 Netband journals contained an auxiliary upload-end diagnostic: four
`EPIPE` (`os error 32`) and five `ECONNRESET` (`os error 104`). Every one still contained
both throughput values and a terminal bandwidth outcome of `success`. The warned runs had
a 17.99 Mbit/s median upload versus 17.96 Mbit/s for the 11 clean runs, so the diagnostic
did not correspond to a material shift in the recorded throughput in this sample. The
reference client returned exit code 0 for all 20 runs.

The observations support the following bounded conclusions:

- **Completion reliability:** both clients produced complete results for all 20 runs. This
  is a 100% observed completion rate for this sample, not a claim that future runs cannot
  fail. Netband's 9 auxiliary warnings mean the sample is not warning-free.
- **Repeatability:** all four per-client/direction coefficients of variation remained below
  10%. Netband's download CV includes one 14.57 Mbit/s result; the following reference run
  was also below its median at 19.09 Mbit/s, consistent with a transient path change.
- **Download agreement:** the clients' central 80% ranges almost coincide, their medians
  differ by 1.83%, and the median signed paired difference is -0.73%.
- **Upload agreement:** both clients produce tight distributions, but Netband reports a
  systematic 7–8% increase under these conditions. The counter and timing distinction
  above makes the values useful for the same trend analysis but not exact substitutes.

The sanitized [paired measurements](benchmarks/2026-09-03-akamai.csv) contain the values
and Netband upload-end status behind these tables. The original JSON events, Netband
journals, logs, timestamps, and addresses remain uncommitted because they include client
network identifiers.

## Limitations

- This is one 23-minute sample from one Wi-Fi client, access connection, route, and server.
- Paired runs were close in time but necessarily sequential because both saturate the same
  connection.
- No server CPU or interface telemetry was captured with the client results, so the test
  does not independently rule out a server-side bottleneck.
- Twenty observations describe this session but are not a long-term availability study.
- Agreement with a reference implementation is not a calibrated ground-truth measurement.

## Reproduce the comparison

Install Netband and M-Lab's Go reference client, then run:

```sh
NDT7_CLIENT_BIN=/path/to/ndt7-client \
NETBAND_BIN=/path/to/netband \
./scripts/benchmark-ndt7-clients.sh \
  ndt.example.com 20 10
```

The arguments are server, pair count, and cooldown seconds. A fourth argument can select
the output directory. By default the raw results and generated summary are placed below
`.netband/benchmarks/`, which Git ignores. Treat that directory as private: the reference
client's raw JSON and Netband's CSV can contain public and local client addresses.

Run at different times of day before generalizing the result into a long-term baseline,
and monitor the server's CPU and network limit as described in
[Self-hosted NDT7 on Akamai Cloud](akamai-ndt-server.md#5-verify-cost-and-accuracy).
