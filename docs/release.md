# Release validation

Netband is not tagged `v1.0.0` until every item below has current evidence. Tests use
local NDT7 and Locate servers unless a human explicitly opts into a live provider run.

## Automated gates

Run from a clean checkout with Rust 1.98:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo deny check
bash scripts/smoke-installer-linux.sh
bash scripts/smoke-readme-linux.sh
bash scripts/smoke-release-linux.sh
```

The release smoke builds with `--release --locked`, enforces a 25 MiB binary ceiling,
requires `--version` startup in under two seconds, verifies help/config commands, parses
the v1 fixture with Python's CSV implementation, and executes local NDT7 and Locate
mocks. CI runs the quality/MSRV/release job on Linux x86_64 and cross-builds aarch64 with
the GNU linker before executing help/version/config under QEMU. A separate CI job runs
the test suite with `cargo-llvm-cov`, uploads LCOV to Codecov using `CODECOV_TOKEN`, and
retains the raw report as a workflow artifact.

## Release publication

Publishing a GitHub release is the only trigger for `.github/workflows/cd.yml`. Its tag
must be exactly `v<crate-version>`; for example, crate version `1.0.0` requires tag
`v1.0.0`. The workflow checks formatting, Clippy, tests, dependency policy, and package
construction before the publishing job starts. Only the final step receives the
`CARGO_REGISTRY_TOKEN` secret and runs `cargo publish` against crates.io.

The workflow also builds GNU/Linux x86_64 and aarch64 archives and publishes a SHA-256
file beside each archive. The static `netband-installer.sh` downloads both files from
the same GitHub release, verifies the archive before extraction, rejects unexpected
archive entries, and installs through a temporary file in the destination directory.
Release jobs check out the event's commit SHA, and third-party actions in the publishing
workflow are pinned to complete commit SHAs.

Every release asset receives a GitHub artifact attestation from the pinned CD workflow.
After downloading an asset, verify both its checksum and build provenance:

```sh
sha256sum --check netband-x86_64-unknown-linux-gnu.tar.gz.sha256
gh attestation verify netband-x86_64-unknown-linux-gnu.tar.gz \
  --repo gregl83/netband \
  --signer-workflow gregl83/netband/.github/workflows/cd.yml
```

The workflow does not overwrite existing assets. A failed or partially published
release must be replaced with a new release version rather than repaired in place.

The `published` event includes GitHub prereleases. Publish a prerelease only when the
matching Cargo version also contains the intended SemVer prerelease suffix.

## Architecture evidence

| Target | Required evidence | Current result |
| --- | --- | --- |
| Linux x86_64 | Clean release build, README/release smoke, privileged service/systemd smoke | Passed 2026-08-31; Phase 8 systemd PID 1 smoke passed 2026-08-30 |
| Linux aarch64 | Release build and QEMU startup/config smoke | Passed under QEMU 2026-08-31 |
| Raspberry Pi hardware | 64-bit Pi build, ICMP, SIGTERM, CSV parse, optional authorized NDT7 | Required before a hardware-qualified release claim; QEMU does not replace it |

On real Raspberry Pi hardware run:

```sh
cargo build --release --locked
bash scripts/smoke-release-linux.sh
sudo bash scripts/smoke-service-linux.sh
```

Then install the example unit, run `scripts/smoke-systemd-linux.sh`, and independently
parse the resulting CSV. Record model, OS, architecture, Rust version, command results,
and whether NDT7 used M-Lab consent or an authorized direct endpoint.

## Live session gate

A release operator must explicitly choose one provider:

1. Review [PRIVACY.md](../PRIVACY.md) and both M-Lab policies, then run one manual M-Lab
   NDT7 test with `--accept-mlab-policy`; or
2. Use an authorized operator-owned direct NDT7 endpoint and its documented policy.

Use one journal for a default-target ping and bandwidth run:

```sh
netband --output release-live.csv once ping
netband --output release-live.csv --accept-mlab-policy once bandwidth
```

Do not run the second command without consent. Parse `release-live.csv` with the Python
snippet in [CSV schema and outcomes](data-format.md) and require valid `ping_probe`,
`ping_summary`, and `bandwidth` rows. Record the date, provider kind (not credentials),
row count, exit statuses, and parser result in the Phase 9 decision log. Do not commit
the live journal because it can contain public/source IP and endpoint data.

The 2026-08-31 validation used the default timestamped output paths, producing one ping
journal and one bandwidth journal instead of the single explicitly named journal above.
Both were parsed independently and are equivalent evidence for this gate; neither is
committed because they contain network metadata.

## Final checklist

- [x] All nine implementation phases and decision entries are current.
- [x] Formatting, Clippy, tests, MSRV, dependency/license, and release builds pass.
- [x] Linux x86_64 and aarch64 evidence is current.
- [x] Raspberry Pi hardware evidence is current or the release is explicitly described
  as CI/QEMU-qualified rather than hardware-qualified.
- [x] Consented live CSV journals contain independently parsed ping and bandwidth rows.
- [x] `Cargo.toml` metadata, `Cargo.lock`, README, privacy, examples, and service files
  are included in `cargo package --list`.
- [ ] Package version is changed to `1.0.0`, `cargo package --locked` succeeds, and only
  then is the signed `v1.0.0` tag created.

Current development version remains `0.2.0` until this checklist passes. Creating a tag
or claiming a completed v1 without the live/provider and hardware evidence would make
the checklist meaningless.
