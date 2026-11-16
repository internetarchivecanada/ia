# Release an Intel macOS binary, `ia-x86_64-apple-darwin`

**The gap.** Releases ship binaries for Linux (x86_64 musl), macOS on Apple Silicon and Windows (x86_64). Staff on Intel Macs, some on older macOS versions and without Homebrew or a Rust toolchain, have no binary to download. The next release carries one.

**Decision.** The build matrix in `.github/workflows/build.yml` gains `x86_64-apple-darwin` on the same `macos-latest` runner as the Apple Silicon build, cross-compiled with the extra Rust target. This is the standard route for Rust release pipelines: Apple's toolchain on an Apple Silicon host builds x86_64 binaries with no extra setup, and it does not depend on an Intel runner image that GitHub is retiring. The release job already renames every `ia-<target>` artifact to `ia-<target>`, writes `SHA256SUMS` over `ia-*` and attests `release/ia-*`, so the new asset gets the same treatment with no change to `release.yml`.

**Minimum macOS version, set on purpose.** The Intel build sets `MACOSX_DEPLOYMENT_TARGET`, which rustc and the C compiler used by `aws-lc-sys` both read:

- `x86_64-apple-darwin`: 10.14. Rust's own floor for the target is 10.12, but `rustls-platform-verifier` verifies certificates with `SecTrustEvaluateWithError` (through `security-framework`'s default `OSX_10_14` feature), a Security.framework function that first appeared in macOS 10.14. The call is linked directly, so on 10.12 or 10.13 the binary cannot make an HTTPS request. Declaring 10.14 states the real floor.
- `aarch64-apple-darwin`: 11.0, the first macOS release for Apple Silicon and Rust's default for the target, left unset. Setting it would change the environment that cargo's build-script fingerprints (`aws-lc-sys` reruns when the variable changes) without changing the rust-cache key, which does not hash it; the main push would then restore its cache by exact match, rebuild, and never save, so every release would rebuild those crates. The build.yml comment records the 11.0 floor; the shipped 0.26.0 asset's `LC_BUILD_VERSION` reads `minos 11.0`.

**`ia update`.** The asset name is not mapped by hand: `ia-cli/build.rs` exports the compile target as `IA_TARGET`, and `ia_core::update::find_matching_asset` looks for `ia-{IA_TARGET}`. A binary built for `x86_64-apple-darwin` therefore asks for `ia-x86_64-apple-darwin`. A test pins that with both macOS assets present, each target picks its own.

**Documentation.** README: the install table lists the asset, and a macOS example picks the asset from `uname -m` (`arm64` or `x86_64`), downloads it with `curl -L -o`, checks it against `SHA256SUMS` with `shasum -a 256`, and only then moves it to `/usr/local/bin/ia-cli`. `docs/why-rust.md` names Intel macOS among the platforms. Workflow comments and `CONTRIBUTING.md` that say "three" builds say four. The release note for the next release is the release manager's; the pull request carries a one-line note for it.

**Out of scope.** Signing and notarization, a universal binary, Homebrew, an install script.

**Proof without a tag.** The release job runs only on a tag. A local `cargo build --locked -p ia-cli --release --target x86_64-apple-darwin` with the same deployment target, inspected with `file` and `otool -l`, shows the architecture and the minimum version. `release.yml` also has a `workflow_dispatch` trigger whose run builds and tests every target and skips the release job (`if: startsWith(github.ref, 'refs/tags/')`), so it publishes nothing.

## Tasks
- [x] Plan committed first.
- [x] Test: `find_matching_asset` picks `ia-x86_64-apple-darwin` for `x86_64-apple-darwin` and `ia-aarch64-apple-darwin` for `aarch64-apple-darwin` from a list holding both.
- [x] build.yml matrix entry and deployment targets; comments in build.yml, ci.yml, release.yml; CONTRIBUTING.md.
- [x] README table and macOS example; `docs/why-rust.md`.
- [x] Local cross-build, `file`, `otool -l`.
- [x] `cargo test --locked -p ia-core -p ia-cli`, `cargo clippy --locked -p ia-core -p ia-cli -- -D warnings`.
- [x] Code review; pull request.

## Review (2026-11-16)

Taken: the README install blocks continued after a failed checksum (each step now runs only if the previous one succeeded, in the Linux block too); the macOS block had no branch for an unknown `uname -m`; this plan said the aarch64 build sets the deployment target, which the code does not. Not taken: a `#` comment line pasted into interactive zsh prints an error (the macOS block's comment became a sentence above it; the Linux block keeps its comment line as before). Checked and fine: `if: matrix.macos-deployment-target` is skipped for entries without the key; a `GITHUB_ENV` write after rust-cache does not change the key; the 10.14 floor; the release job's rename, `SHA256SUMS` and attestation pick the new artifact up with no change to `release.yml`.
