# 0.21.0 and `#[non_exhaustive]` (Part 2, PR 4)

**Source:** `docs/reviews/2026-10-02-series-26-43-review.md` §2 (local file; `docs/reviews/` is gitignored) and Jake's decision 2026-10-02: bump ia-core and ia-cli to 0.21.0 (still alpha, no 1.0) and mark `IaError`, `UploadStatus`, `DownloadOpts` and `UploadOpts` `#[non_exhaustive]`, so that the next added variant or field is not a breaking change for external consumers.

**What the series changed in ia-core's public API since 0.20.1** (from the review, updated through Part 2): `IaError::Http.retry_after`, `IaError::retry_after()`, `IaError::ChecksumMismatch.kept`, new variants `SourceChecksumMismatch`, `DownloadStalled`, `UploadStalled`; `DownloadOpts.min_speed`; `UploadOpts.retry_sleep` removed, `retry_min_delay`/`retry_max_delay` added; `upload_file_multipart(..., hashes)`; `checksum::FileHashes` and `hash_file_and_parts`; `retry::RetryMiddleware`; the raw transport error after a spent budget; and in Part 2 the removal of `UploadOpts.verify_timeout` and `UploadStatus::UploadedUnverified`. The `--json` wire gained `retry_after`, `kept`, codes `source_checksum_mismatch`, `download_stalled`, `upload_stalled`, and lost status `uploaded_unverified`.

**Effect of `#[non_exhaustive]`:** outside ia-core, a `match` on `IaError` or `UploadStatus` needs a wildcard arm, and `DownloadOpts`/`UploadOpts` can no longer be built by a struct expression at all, `..Default::default()` included: start from `Default::default()` and assign fields, or use `UploadOptsBuilder`. ia-cli and the ia-core integration tests are outside the crate, so the compiler lists every site; each is fixed the way an external consumer would fix it. Inside ia-core nothing changes.

**Release:** the version lives once, in the workspace `Cargo.toml`; `Cargo.lock` follows. Tagging and publishing are the release workflow's, not this PR's.

## Tasks
- [x] Red: `ia --version` prints `0.21.0` (failed at 0.20.1). The attribute's effect is a compile-time contract, pinned by the compiler on every outside site, not by a test; no new crate for compile-fail tests.
- [x] Green: version, four attributes, every outside site the compiler named (two `UploadOpts` and one `DownloadOpts` built in ia-cli, four `UploadStatus` matches in ia-cli, 80 `UploadOpts` literals in the ia-core integration tests), `Cargo.lock`. Outside the crate a non-exhaustive struct cannot be built by a struct expression even with `..Default::default()`, so every site builds a default and assigns fields. The wildcard arms treat an unknown status as not a success: the joblog records an error (a later run must not skip the file), the summaries count it as failed, the result line prints its Debug form. 1968 tests pass.
- [ ] `just ci`; code-reviewer pass; PR; merge after checks; `scripts/ia-cleanup non-exhaustive-0-21` after a confirmed merge.

## Review (2026-10-02), closed before merge

Important, fixed: the new doc comments on `UploadOpts` and `DownloadOpts` told external consumers to build one with `..Default::default()`, the one form that does not compile for them; they now say to start from `Default::default()` and assign fields (or use `UploadOptsBuilder`), and the "Effect" paragraph above says the same.

Suggestions: `DownloadOpts` has no builder; the assign-on-default pattern is documented instead of adding one (nothing asked for a builder). The version test hard-codes `0.21.0` on purpose: it is this PR's red test; a later bump edits it. Noted, no change: the reviewer re-derived all 80 rewritten literals against main (same base, fields, values, order, zero differences); two "fast for tests" comments moved to their own line; `cargo clippy --tests` has 39 pre-existing lints outside this PR's code and is not the CI recipe.
