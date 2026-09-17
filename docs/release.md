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
python3 -m unittest discover -s scripts -p test_release.py
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

Before publishing, check that README installation commands lead to binaries with the
advertised CLI and that linked reference docs match the release. Keep the source-build
tag in [installation](install.md) aligned with the package version.

The [CD workflow](../.github/workflows/cd.yml) separates preparation from publication.
The workflow must be present on the default branch to enable manual dispatch.

### Prepare a draft

In **Actions → CD → Run workflow**, select the release branch and enter the package
version, such as `1.0.0`. Equivalently:

```sh
gh workflow run cd.yml --ref release/v1.0.0 -f version=1.0.0
```

The workflow captures the selected ref's commit when dispatched. Every checkout uses
that exact commit, even if the branch advances. Preparation:

1. Checks the version, formatting, lint, tests, installer fixtures, dependency policy,
   and crate package.
2. Builds GNU/Linux x86_64 and aarch64 binaries with Rust 1.98 on Ubuntu 24.04.
   Checksums and extracts each archive, then tests its executable's version, help,
   and configuration (aarch64 runs under QEMU).
3. Attests all seven assets and verifies their source commit and workflow identity.
4. Creates `v1.0.0` at the validated commit and a **draft** release, uploads the assets,
   then downloads and verifies them again.

A successful run leaves a public Git tag and an unpublished draft release. An existing
tag must point to the same commit; preparation never moves it. A draft for that commit
can be retried, replacing its assets while preserving release notes. A published
release is rejected. Failed uploads can leave a partial draft: wait for a successful
preparation run before publishing it.

### Publish the prepared draft

Review the successful preparation run, all seven assets, and the release notes, then
publish the draft in GitHub's Releases page. Publishing triggers CD to verify the
asset checksums and attestations against the released commit, then publish the crate
through the `crates-io` environment. It does not rebuild or replace the binary assets.
An already published crate version is skipped.

Do not create a separate release or publish from automation using `GITHUB_TOKEN`;
that token's release events do not start another workflow. Configure the `crates-io`
environment and its `CARGO_REGISTRY_TOKEN` secret before publication.

The binaries are available when the draft becomes public; crates.io publication
follows. Confirm both destinations before announcing availability. These archive
checks do not establish physical Raspberry Pi compatibility or a minimum glibc version.

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
