# Netband documentation

Start with [a quick ping or bandwidth test](../README.md#try-a-quick-test).
Choose the next guide for what you want to do:

| I want to… | Start here |
| --- | --- |
| Install a binary or build from source | [Installation](install.md) |
| Run a single test or use JSONL in a script | [Quick tests and output](usage.md) |
| Find saved CSVs or choose where to write them | [Result locations](usage.md#where-results-go) |
| Diagnose missing output, permission errors, or blocked tests | [Troubleshooting](usage.md#configure-or-troubleshoot) |
| Change targets, interfaces, providers, or defaults | [Configuration and providers](configuration.md) |
| Leave Netband running unattended | [Service operation](service.md) |
| Understand automatic tests, limits, or deferrals | [Scheduling](scheduling.md) |
| Parse results or inspect example records | [Data format](data-format.md) · [Example session](examples/README.md) |
| Understand measurement methods and validation | [NDT7 validation](ndt7-validation.md) |
| Plan a study and preserve measurement provenance | [Research use](ndt7-validation.md#research-use) |
| Run an operator-controlled NDT7 server | [Self-hosted NDT7 on Akamai Cloud](akamai-ndt-server.md) |
| Understand what data is collected and shared | [Privacy and provider data](../PRIVACY.md) |

## Recorded benchmarks

The [reference-client comparison](benchmarks/2026-09-06-akamai/summary.md) includes
[measurements](benchmarks/2026-09-06-akamai/measurements.csv),
[build metadata](benchmarks/2026-09-06-akamai/metadata.json), and
[paired analysis](benchmarks/2026-09-06-akamai/paired-analysis.json).
Records describe the identified binaries and environment; see the validation guide
for interpretation and limitations.

## Maintainers

[Release maintenance](release.md) covers validation, packaging, publication, and artifacts.
These reference docs describe their source checkout. For an installed release, use
the documentation at its matching Git tag; see [installation](install.md).
