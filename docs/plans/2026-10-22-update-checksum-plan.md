# `ia update` checks the asset against the release's `SHA256SUMS`

**The gap.** `download_and_replace` in `ia-core/src/update.rs` writes whatever the asset URL returns to a temporary file and renames it over the running binary. The only check is that the new binary prints the expected version. A truncated download, a corrupted CDN object, or an asset replaced on the release page after publication is installed as is. The 2026-10-22 supply-chain audit rated this the highest-impact finding in the shipped binary.

**What the release now carries.** Since PR #73 the release job writes `SHA256SUMS` over every `ia-*` asset (the `sha256sum` format: 64 hex digits, two spaces, the asset name, one line per asset) and attaches it as a release asset, and records a Sigstore build-provenance attestation for the binaries. This plan uses the sums. Verifying the attestation in the binary is a later, separate change: it needs a Sigstore client and a policy for key rotation.

**What the sums protect against, and what they do not.** The sums travel the same path as the asset list (the GitHub releases API over TLS), so they catch a transfer that went wrong and an asset that no longer matches what the release workflow published. They do not catch an attacker who controls the repository and re-uploads both. That is what the attestation is for.

**Decision.** `download_and_replace` fetches the release's `SHA256SUMS` asset, finds the line for the asset it is installing, hashes the downloaded bytes, and refuses the install on a mismatch. The hash is computed before anything is written next to the binary, so a bad download never lands on disk. SHA-256 comes from `aws-lc-rs` (`digest::digest` with `digest::SHA256`), which is already compiled into every binary as rustls's crypto provider; it becomes a direct dependency of `ia-core` with the feature set rustls already enables, so nothing new is built.

**Releases without sums.** Every release up to and including 0.22.x has no `SHA256SUMS`, and `ia update install <version>` can install any of them. `MIN_CHECKSUMMED_VERSION`, set to the first release that ships the file, sits next to `MIN_INSTALLABLE_VERSION` and decides what a missing file means:

- At or above it: a release without `SHA256SUMS`, or a `SHA256SUMS` without a line for this asset, is `IaError::UpdateChecksumMissing { version }`. The install stops.
- Below it: the install proceeds and a line goes to stderr: `warning: release v{version} predates SHA256SUMS; installed without checksum verification`. Refusing would break `ia update install` for every existing release for no gain, since those assets were never summed.

**Errors.** Two new `IaError` variants, both permanent (never retried) and both with a JSON code for `--json`:

- `UpdateChecksumMismatch { asset, expected, actual }`, code `update_checksum_mismatch`, message `checksum mismatch for {asset}: expected {expected}, got {actual}`. The temporary file is removed; the running binary is untouched.
- `UpdateChecksumMissing { version }`, code `update_checksum_missing`, message `release v{version} has no SHA256SUMS entry for this asset`.

The doc-test lists of variants in `error.rs` gain both.

**Shape of the change.** `download_asset` keeps its signature for library callers and gains a sibling, `download_asset_checked(url, dest, current_version, expected_sha256: Option<&str>)`, which downloads, hashes, compares when an expectation is given, and only then writes. `download_asset` calls it with `None`. A small `parse_sha256sums(text) -> Vec<(String, String)>` handles the file; it accepts both the `  ` and the ` *` separators `sha256sum` can emit and ignores blank lines. `expected_checksum_for(release, asset_name, current_version)` fetches and looks up the line, returning `Ok(None)` for a pre-sums release and `Err(UpdateChecksumMissing)` for a post-sums one. `download_and_replace` calls it before downloading the asset.

**User-visible text.** The `update` help gains one sentence: "Assets are checked against the release's SHA256SUMS before they are installed." `docs/usage.md` under `ia update` gains the same sentence plus the two error codes, and names the pre-sums warning. README already documents verifying a manual download.

## Tasks
- [x] Plan committed first.
- [x] Red: unit tests for `parse_sha256sums` (two-space and asterisk separators, blank lines, a line for a different asset); wiremock tests through `install_version` for a matching sum (installs), a mismatching sum (error, temp file gone, target untouched), a post-sums release with no `SHA256SUMS` asset (`UpdateChecksumMissing`), a post-sums release whose file lacks this asset's line (`UpdateChecksumMissing`), and a pre-sums release without the file (installs); `error.rs` tests for display, JSON code and non-retryability of both variants.
- [x] Green: `aws-lc-rs` dependency, `parse_sha256sums`, `expected_checksum_for`, `download_asset_checked`, the two variants, the `MIN_CHECKSUMMED_VERSION` constant.
- [x] Docs: `update` help, `docs/usage.md`.
- [x] `cargo test --locked -p ia-core -p ia-cli`, `cargo clippy --locked -p ia-core -p ia-cli -- -D warnings`.
- [ ] Code-reviewer pass; PR.
