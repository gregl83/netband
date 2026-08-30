# netband

Network bandwidth monitoring utility.

Netband is a planned Linux-first daemon that records network health over time, making
intermittent connection problems easier to identify and analyze.

## Features

- Continuous latency and packet-loss measurements against multiple ping targets.
- NDT7 bandwidth tests using M-Lab or a directly configured server.
- Bandwidth tests triggered by degraded ping results or a jittered schedule.
- Provider-aware daily limits, cooldowns, and rate-limit handling.
- Fair, non-overlapping monitoring across multiple network interfaces.
- Detailed CSV history with optional human-readable or JSON Lines console output.
- Graceful command-line and systemd daemon operation.

Designed for home labs, Raspberry Pis, and networks where failures disappear before a
manual speed test can capture them.

## License

[Apache 2.0](LICENSE)
