# Netband documentation

Start with the [project quick start](../README.md#five-minute-quick-start) to install
Netband and collect your first measurements.

| Document | Contents |
| --- | --- |
| [Configuration and providers](configuration.md) | CLI options, TOML defaults, state paths, M-Lab consent, and direct endpoints |
| [Service operation](service.md) | Installation, systemd, CSV locations, journal logs, permissions, and recovery |
| [CSV schema and outcomes](data-format.md) | Measurement fields, units, outcomes, and parsing examples |
| [Scheduling](scheduling.md) | Daily opportunities, health triggers, provider cooldowns, and interface selection |
| [NDT7 measurement validation](ndt7-validation.md) | Measurement methods, reference-client comparison, offline analysis, and reproduction |
| [Self-hosted NDT7 on Akamai Cloud](akamai-ndt-server.md) | Deploying an operator-controlled measurement server |
| [Release maintenance](release.md) | Maintainer validation commands, publication workflow, and release assets |
| [Privacy and provider data](../PRIVACY.md) | Collected data, provider policies, and operator responsibilities |

## Recorded benchmarks

| Dataset | Supporting files |
| --- | --- |
| [2026-09-06 Akamai comparison](benchmarks/2026-09-06-akamai/summary.md) | [Measurements](benchmarks/2026-09-06-akamai/measurements.csv), [metadata](benchmarks/2026-09-06-akamai/metadata.json), [summary JSON](benchmarks/2026-09-06-akamai/summary.json), [paired analysis](benchmarks/2026-09-06-akamai/paired-analysis.json) |

Benchmark records describe the identified binaries and measurement environment.
See the validation guide for interpretation and limitations.
