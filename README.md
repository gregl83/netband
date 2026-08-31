[![Build](https://github.com/gregl83/netband/actions/workflows/ci.yml/badge.svg)](https://github.com/gregl83/netband/actions/workflows/ci.yml)
[![Coverage Status](https://codecov.io/gh/gregl83/netband/graph/badge.svg?token=S9vGTwnOw6)](https://codecov.io/gh/gregl83/netband)
[![Crates.io](https://img.shields.io/crates/v/netband.svg)](https://crates.io/crates/netband)
[![Apache 2.0 licensed](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/gregl83/netband/blob/master/LICENSE)


<p align="center"><img src="/assets/netband.svg" alt="netband" width="400" /></p>

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

Direct NDT7 connections use verified TLS by default. `--allow-insecure-ndt` permits
unencrypted `ws://` only for explicitly trusted private networks.

## License

[Apache 2.0](LICENSE)
