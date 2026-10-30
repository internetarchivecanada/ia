# Verify downloads by default, split from the `--checksum` skip check (#13)

**The gap.** `-C`/`--checksum` on `ia download` does two unrelated things, and `DownloadOpts.checksum` (default `false`) carries both:

1. **Skip check.** A local file is skipped when its md5 matches the item metadata, instead of when its size and mtime match. This reads every local file in full.
2. **Verification.** The md5 of the downloaded bytes is computed from the stream as it is written and compared with the metadata before the `.part` is renamed into place. On a fresh download this costs CPU only; nothing is read back from disk.

Without `-C` nothing is ever compared with the md5, so a corrupt transfer of the right length is renamed into place and reported as a success. The size check (#12) catches truncation, not corruption.

**Decision.** Verification is on by default and turned off with `--no-verify`. `-C` controls only the skip check. `DownloadOpts` carries the two as separate fields, named as on `UploadOpts`, where the same split already exists:

- `checksum: bool` (default `false`): decide skips by md5 instead of size and mtime. The field keeps its name; it loses its second meaning.
- `verify: bool` (default `true`): compare the md5 of the downloaded bytes with the metadata before the rename.

An `ia-core` consumer that set `checksum = true` keeps both behaviors; one that left it `false` now gets verification and sets `verify = false` to keep the old behavior. The struct is `#[non_exhaustive]`, so the new field is not a breaking change in itself.

## Behavior

| Mode | Existing local file | Downloaded bytes |
|---|---|---|
| default | skipped when its size and mtime match the metadata; never read | md5 compared with the metadata before the rename |
| `--no-verify` | as default | not compared (the size check of #12 still applies) |
| `-C` | skipped when its md5 matches the metadata (the file is read in full); otherwise downloaded | md5 compared, as default |
| `-C --no-verify` | as `-C` | not compared |

In every mode, a file is not verified, and the download completes on the size check alone, when:

- **The metadata has no md5, or an empty one.** Verification is skipped for that file and a debug-level log line says so. It is not an error: the file was never verifiable, and failing it would turn downloads that succeed today into failures.
- **The file is `{identifier}_files.xml`.** It lists every file in the item, itself included, so its recorded md5 (like its size, #12) is written before its final bytes exist and never matches. It is exempt from verification for the same reason it is exempt from the size check. No other file is exempt.

What `-C` does with a file that has no md5 in the metadata, and with `_files.xml`, is unchanged: neither can match, so the file is downloaded again.

## Open questions from the issue, resolved

**Resume cost.** A resumed download seeds the md5 by reading the bytes already in the `.part` before the stream continues (`seed_hasher_from_part`); the 416 shortcut (a `.part` that is already the whole file) does the same. With verification on by default, every resume now pays that read.

Measured on an Apple M1 Pro (16 GB, internal SSD), with the same loop the code runs (1 MiB reads into md-5 0.11), over a 4 GiB file: 383 to 614 MB/s over three runs, on a machine also running other builds. A plain read of the same file ran at 4.2 GB/s, so the md5 is the limit, not the disk. A resume of a 7 GB file therefore spends roughly 11 to 18 s hashing what is already on disk, with a `Verifying` progress event every 16 MiB. The same machine hashes faster than any download it can make, so a fresh download is not slowed.

The re-read stays. A resume happens because a download already ran for minutes or hours; tens of seconds of local hashing is small next to the transfer it continues, and less than a second fresh download from byte 0 would cost. The alternatives are worse here:

- Persisting the md5 state beside the `.part`: md-5's state is not serializable, and a saved state that does not match the `.part` exactly (a crash between write and save) would make verification fail on good bytes or pass on bad ones.
- A separate full pass after the download: reads the whole file instead of the prefix, so every resumed download reads more, and fresh downloads lose their read-free verification.

For #16 (parallel `Range` downloads, parked): parts arrive out of order, so a rolling hash cannot work and a post-download full pass is the only option there. That path stays possible: `finish_part` takes the md5 state as an argument, and a full pass can build it from the finished `.part` with the same seeding function. Whether the sequential path should then share the full pass is a decision for #16, with its measurements.

**Mismatch handling.** Unchanged, following the #14 rules: the bytes are kept as `<name>.md5-mismatch`, nothing is left as `.part`, the first mismatch is retried from byte 0, the same wrong md5 twice in a row fails the file for good, a different wrong md5 keeps retrying up to `--retries`, and a later verified download removes the kept copy. The only change is that these now apply by default.

**Unreliable md5s.** Only `_files.xml` is exempt (above). A file with no md5 is not verified. A derivative whose md5 is wrong in the metadata now fails after two downloads with "the source file or its metadata is likely wrong", where it used to be accepted silently; `--no-verify` is the way to download it anyway.

**Release notes.** Yes: this is a user-visible change of default. `docs/releases/unreleased.md` gets a "Changed" entry, carried into the next version's notes at release time.

## Tasks

- [x] Plan committed first.
- [x] Red: the tests below, failing against the current code where they test new behavior (16 of the download unit tests failed before the change: every verification test now on default options, and `-C` without verification).
- [x] Green: `DownloadOpts.verify` (default `true`); `checksum` documented as the skip check only; one function decides whether a file is verified (`verify` on, not `_files.xml`, md5 present and non-empty, debug log otherwise), used at both places the md5 state is created; `--no-verify` on `ia download`; `-C` help rewritten.
- [x] Docs: `ia download` `long_about` and flag help, `docs/usage.md` (option table, "After an interruption", 416 paragraph, stall paragraph, a new "Verification" section before "Checksum mismatches"), the `--help` examples, README feature row, `docs/releases/unreleased.md`.
- [x] `cargo test` green. `cargo clippy --all-targets -- -D warnings` with the local rustc 1.95 reports 59 errors, the same count in the same files as on main before this change, none in lines this change adds; `cargo clippy -p ia-core -p ia-cli -- -D warnings` is clean.
- [ ] Code-review pass on the full diff; findings fixed or recorded below; PR.

## Tests

Hermetic, wiremock (`ia-core/src/download/mod.rs` test module, and `ia-cli/tests/download_search_list.rs` for the flag):

1. Default options, md5 matches: the file completes. (Previously needed `checksum: true`.)
2. Default options, md5 wrong: `ChecksumMismatch`, bytes kept as `.md5-mismatch`, no `.part`. Fails on the current code, which accepts the file.
3. `verify: false`, md5 wrong: the file completes; an earlier `.md5-mismatch` copy is left alone.
4. Default options, a local file with matching size and mtime but different bytes: skipped without being read (no `Verifying` event) and without a request. `-C` is not needed for the size and mtime skip and verification does not read local files.
5. `checksum: true`, local file whose md5 matches: skipped with no request (existing test, kept).
6. `checksum: true, verify: false`, no local file, md5 wrong: the file completes. Fails on the current code, where `-C` also verifies.
7. Default options, md5 absent, and md5 empty: the file completes.
8. Default options, `{identifier}_files.xml` with a wrong md5: the file completes.
9. Default options, resume: a `.part` holding a wrong prefix and a 206 with the right tail fails as a mismatch (the seeded md5 covers the `.part`); with the right prefix it completes.
10. Default options, a 416 shortcut over a `.part` with the wrong bytes: kept as `.md5-mismatch` (existing test, moved to default options).
11. Item level, default options: the #14 retry rules (same wrong md5 twice stops; a different one retries up to `--retries`; a later good download removes the kept copy). The existing tests switch from `checksum: true` to the default.
12. CLI: `ia download` against a wiremock item whose md5 is wrong exits 1 by default and 0 with `--no-verify`; `--help` describes `--no-verify` and `-C` as above.

The test helper `test_file_meta` carried the md5 of the empty file as a placeholder; with verification on by default every test serving a non-empty body would have failed on it, so it now carries no md5, and tests that verify use `test_file_meta_with_md5`. The CLI fixtures in `ia-cli/tests/download_search_list.rs` carry the real md5 of the body each one serves.