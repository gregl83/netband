# Release maintenance

This guide describes the repository's release checks, artifacts, and publication
workflow. For installation and service operation, see [Service operation](service.md).
For recorded measurement evidence, see [NDT7 validation](ndt7-validation.md).

## Source validation

Run from the repository root with Rust 1.98 and `cargo-deny` installed:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-targets --all-features --locked
cargo deny check
bash scripts/smoke-installer-linux.sh
bash scripts/smoke-readme-linux.sh
bash scripts/smoke-release-linux.sh
cargo package --locked
```

The [CI workflow](../.github/workflows/ci.yml) runs source checks and Linux x86_64
release smoke tests. A separate job cross-builds aarch64 and runs startup/configuration
checks under QEMU. The release smoke uses local NDT7 and Locate fixtures, checks the
CSV fixture with Python, and enforces binary size and startup-time limits.

These checks do not establish physical Raspberry Pi compatibility or qualify every
GNU/Linux runtime. Record the source commit, toolchain, platform, binary hash, and
results when reporting validation.

## Service and hardware validation

On a disposable Linux test host, run:

```sh
sudo bash scripts/smoke-service-linux.sh
```

After installing the binary, configuration, and unit using the
[service instructions](service.md), exercise systemd integration:

```sh
sudo bash scripts/smoke-systemd-linux.sh
```

The systemd smoke replaces the installed example unit and starts, restarts, and stops
`netband.service`. Use a test configuration with bandwidth disabled unless a live
provider run has been explicitly authorized. For a Raspberry Pi hardware result,
record the board model, OS/kernel, architecture, and command results from the board.

Live NDT7 comparisons are separate from local smoke tests. Follow the
[comparison procedure](ndt7-validation.md#reproduce-the-comparison) with an authorized
endpoint and retain the measured executable's identity with the results.

## Publication workflow

The [CD workflow](../.github/workflows/cd.yml) starts when a GitHub release is
published. The tag must be `v` followed by the package version in `Cargo.toml`.
The workflow then:

1. Checks the tag/version, source quality, installer fixtures, dependency policy,
   and crate packaging.
2. Builds GNU/Linux x86_64 and aarch64 binaries with Rust 1.98 on Ubuntu 24.04.
3. Packages and attests the release assets, then uploads them to the GitHub release.
4. Publishes the crate through the `crates-io` environment, unless that version
   already exists in the registry.

Because publication triggers the build, the release can be visible before its assets
are available. Check workflow completion and both publication destinations before
announcing availability. CD builds its binaries separately from CI; it does not run
runtime smoke tests against the extracted release archives.

## Release assets

| Asset | Contents |
| --- | --- |
| `netband-x86_64-unknown-linux-gnu.tar.gz` | x86_64 `netband` executable |
| `netband-aarch64-unknown-linux-gnu.tar.gz` | aarch64 `netband` executable |
| Each archive's `.sha256` file | SHA-256 checksum for that archive |
| `netband-installer.sh` | Shell installer |
| `netband.toml` | Example service configuration |
| `netband.service` | Example systemd unit |

With an archive and its checksum downloaded into the same directory, verify them:

```sh
sha256sum --check netband-x86_64-unknown-linux-gnu.tar.gz.sha256
gh attestation verify netband-x86_64-unknown-linux-gnu.tar.gz \
  --repo gregl83/netband \
  --signer-workflow gregl83/netband/.github/workflows/cd.yml
```

The attestation command requires GitHub access. Keep the version, source identity,
and artifact hash with any measurements made using a released binary.
