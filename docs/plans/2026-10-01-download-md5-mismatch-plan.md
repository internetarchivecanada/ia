# Download md5 Mismatch Handling Implementation Plan

Closes #14 (keep the file on md5 mismatch, stop re-downloading a bad source) and #25 (a symlink `.part` is followed when computing the resume offset).

**Goal:** when a download fails its md5 check, the bytes are kept next to the file as `<name>.md5-mismatch` and the error names that path, so a corrupt transfer can be told from a bad source file or wrong metadata. A second download that produces the same wrong md5 stops the retries: the source is the problem. A symlink `.part` is removed before any `Range` request is computed, so a planted link can never shape a resume.

**Architecture:** `finish_part` renames `.part` to `.md5-mismatch` instead of deleting it and returns `ChecksumMismatch` with the kept path; on a verified completion it removes any earlier `.md5-mismatch`. The per-file retry loop in `download_item_with_metadata` remembers the last wrong md5; a repeat becomes the permanent `SourceChecksumMismatch`. The `.part` symlink check moves ahead of the resume-offset read in `download_file`.

**Tech Stack:** existing crates only. `wiremock` for HTTP.

**Decisions (Jake, 2026-10-01):**

- Kept-file name: `<name>.md5-mismatch`, beside the file. Nothing scans for it: the skip check looks at `<name>` and resume looks at `<name>.part`.
- Disk use: one bad copy per file, overwritten on each mismatch. When a later attempt verifies, the bad copy is deleted and the deletion logged. It stays only when the file ends up failing.
- Retry rule: the first mismatch always gets one retry from byte 0. The same wrong md5 twice in a row is a permanent failure that says the source file or its metadata is likely wrong. A different wrong md5 keeps retrying up to `--retries`.
- The too-large abort and the oversize `ServerSizeMismatch` keep deleting `.part`. A body of the wrong length is by construction not the file; the sizes are already in the message; the mid-stream abort is disk protection. An md5-mismatch file is a complete candidate copy, and comparing it is the evidence.
- Joblog and `--json`: the kept path goes in the error message text, which both already carry. No structured field (revisit in #17).
- #25: the symlink check runs before the resume offset is read; the later check stays as a guard against a link planted between the two steps.

**Engineering calls, not Jake decisions (flagged in the PR):**

- `ChecksumMismatch` gains a `kept: String` field (the path as displayed), so the message can name it everywhere the error is shown. `SourceChecksumMismatch { file, expected, actual, kept }` is a new permanent variant with JSON code `source_checksum_mismatch`. Both are public-API additions to `IaError`.
- The repeat check compares the `actual` md5 of consecutive mismatches within one run of the per-file loop. A mismatch followed by a different error and then the same mismatch again does not count as "twice in a row"; the previous md5 is forgotten on any other error.
- The rename to `.md5-mismatch` replaces an existing one (`std::fs::rename` semantics on Unix and Windows). If a `.md5-mismatch` path is a symlink, the rename replaces the link itself and writes through nothing.
- On a verified completion the earlier `.md5-mismatch` is removed with `remove_file`, which removes a link rather than its target. A missing file is not an error.
- With `--retries 0` the first mismatch fails the file; the kept copy stays.
- The 416 shortcut's md5 compare runs through `finish_part`, so it keeps the file the same way.

**Out of scope:** md5 verification by default (#13); comparing the kept copy automatically; any change to `ia verify`.

---

## File Structure

| Action | Path | Responsibility |
|--------|------|----------------|
| Modify | `ia-core/src/error.rs` | `kept` on `ChecksumMismatch`; new `SourceChecksumMismatch`; Display, retryability, JSON codes |
| Modify | `ia-core/src/download/mod.rs` | `mismatch_path`; `finish_part` keeps the file and removes an earlier one on success; repeat rule in the per-file loop; symlink check before resume; tests |
| Modify | `ia-cli/src/commands/download.rs` | `--checksum` long help; `long_about` sentence |
| Modify | `ia-cli/tests/cli.rs` | help text test |
| Modify | `docs/usage.md` | "Checksum mismatches" subsection; `-C` row |

---

### Task 1: error variants

**Files:** `ia-core/src/error.rs`

- [x] **Step 1: Failing tests:** `checksum_mismatch_names_the_kept_file` (Display contains the kept path); `json_checksum_mismatch` gains `kept`; `source_checksum_mismatch_is_not_retryable`; `source_checksum_mismatch_displays_details` (both md5s, "twice", "likely wrong", the kept path); `json_source_checksum_mismatch` (code `source_checksum_mismatch`, `file`, `expected`, `actual`, `kept`).
- [x] **Step 2: Run**; compile failure.
- [x] **Step 3: Implement.**
  ```rust
  #[error("checksum mismatch for {file}: expected {expected}, got {actual}; kept the download at {kept}")]
  ChecksumMismatch { file: String, expected: String, actual: String, kept: String },

  /// Two downloads in a row produced the same wrong md5: the transfer is
  /// not corrupting the data, the source file or its metadata is wrong.
  #[error("checksum mismatch for {file} twice in a row (expected {expected}, got {actual}): the source file or its metadata is likely wrong; kept the download at {kept}")]
  SourceChecksumMismatch { file: String, expected: String, actual: String, kept: String },
  ```
  `is_retryable`: `ChecksumMismatch` stays `true`; `SourceChecksumMismatch` is `false`.
- [x] **Step 4: Run**; green. Commit: `feat(core): name the kept file in ChecksumMismatch and add SourceChecksumMismatch`.

### Task 2: keep the file on mismatch, remove it on success

**Files:** `ia-core/src/download/mod.rs`

- [x] **Step 1: Failing tests** (wiremock, `checksum: true`):
  - Rework `checksum_inline_hash_detects_mismatch_and_removes_part` into `checksum_mismatch_keeps_the_download_as_md5_mismatch`: `Err(ChecksumMismatch { kept, .. })` where `kept` ends with `b.txt.md5-mismatch`; that file holds the body; no `.part`; no `b.txt`.
  - `checksum_mismatch_overwrites_an_earlier_kept_copy`: an existing `b.txt.md5-mismatch` with other bytes; after the mismatch it holds the new body.
  - `verified_download_removes_an_earlier_kept_copy`: an existing `b.txt.md5-mismatch`; the download verifies; the kept copy is gone and `b.txt` is right.
  - `checksum_mismatch_through_a_symlinked_kept_path_replaces_the_link`: `b.txt.md5-mismatch` is a symlink to another file; after the mismatch the path is a regular file with the body and the target is untouched.
  - Rework `range_not_satisfiable_at_part_length_with_checksum_mismatch_deletes_part` into `..._keeps_md5_mismatch`: the 32 `A` bytes end up in `disk.img.md5-mismatch`.
  - `unverified_download_leaves_an_earlier_kept_copy`: without `--checksum` nothing was verified, so an existing `.md5-mismatch` stays.
- [x] **Step 2: Run**; fail.
- [x] **Step 3: Implement** `fn mismatch_path(file_path: &Path) -> PathBuf` (`format!("{}.md5-mismatch", display)`), used in `finish_part`: on mismatch `fs::rename(part_path, &kept).await?` then return the error with `kept: kept.display().to_string()`; after the successful rename into place, `if fs::symlink_metadata(&kept).await.is_ok() { fs::remove_file(&kept).await?; info!(...) }`. Update the `warn!`.
- [x] **Step 4: Run**; green. Commit: `fix(download): keep a failed md5 download as <name>.md5-mismatch`.

### Task 3: stop on a repeated wrong md5

**Files:** `ia-core/src/download/mod.rs`

- [x] **Step 1: Failing tests** through `download_item_with_metadata` (`checksum: true`, wrong md5 in metadata):
  - `same_wrong_md5_twice_stops_retrying`: the server always serves the same bytes; `retries: 5`; expect exactly two requests, `files_failed == 1`, the failure message contains "twice in a row", "likely wrong", and `.md5-mismatch`; the kept file holds the body; no `.part`.
  - `different_wrong_md5_keeps_retrying`: three mocks with `up_to_n_times(1)` serving bodies A, B, C; `retries: 2`; three requests; fails with the plain `ChecksumMismatch` text; the kept file holds C.
  - `different_wrong_md5_then_success_completes_and_removes_kept_copy`: body A (wrong), then the right body (md5 matches); `files_downloaded == 1`; no `.md5-mismatch`.
  - `retries_zero_keeps_the_mismatch_and_stops`: `retries: 0`; one request; `files_failed == 1`; kept file present.
  - `a_different_error_between_mismatches_resets_the_repeat_check`: A (wrong), then a 500 (retryable), then A again; `retries: 3`; the third mismatch is not "twice in a row" (the 500 reset it), so a fourth request follows and then the repeat stops it: four requests total.
- [x] **Step 2: Run**; fail.
- [x] **Step 3: Implement** in the per-file loop: `let mut last_wrong_md5: Option<String> = None;`. On `Err(IaError::ChecksumMismatch { file, expected, actual, kept })`: if `last_wrong_md5.as_deref() == Some(&actual)` then `last_err = Some(IaError::SourceChecksumMismatch { .. }); break;` else `last_wrong_md5 = Some(actual.clone())` and continue as a retryable error. On any other error set `last_wrong_md5 = None`.
- [x] **Step 4: Run**; green. Commit: `fix(download): stop retrying when a second download has the same wrong md5`.

### Task 4: symlink `.part` removed before the resume offset (#25)

**Files:** `ia-core/src/download/mod.rs`

- [x] **Step 1: Failing tests:**
  - Rework `part_file_symlink_works_with_206_response` into `part_file_symlink_is_removed_before_the_range_request`: no request carries `Range`; the first call completes with the full body; the target is untouched; the link is gone.
  - Rework `range_not_satisfiable_at_part_length_through_symlink_part_restarts` into `..._never_reaches_the_shortcut`: the link is removed up front, the plain GET completes on the first call, one request without `Range`, target untouched.
  - `part_file_symlink_skips_resume` keeps passing.
  - PR #29's `range_not_satisfiable_past_part_length_through_symlink_removes_only_the_link` lost its premise (a symlink can no longer reach the delete arm) and became `symlink_part_longer_than_the_file_is_removed_and_the_file_downloaded`: plain GET, complete, target untouched.
- [x] **Step 2: Run**; fail (today a `Range` is sent).
- [x] **Step 3: Implement:** before opening `.part` for its length, `if let Ok(meta) = fs::symlink_metadata(&part_path).await { if meta.file_type().is_symlink() { warn!; fs::remove_file(&part_path).await?; } }`. The later check (before opening for writing) and the one inside the 416 shortcut stay as guards, with their comments saying so.
- [x] **Step 4: Run**; green. Commit: `fix(download): remove a symlink .part before computing the resume offset`.

### Task 5: help and docs

**Files:** `ia-cli/src/commands/download.rs`, `ia-cli/tests/cli.rs`, `docs/usage.md`

- [x] `--checksum` gets a long help paragraph: what a mismatch does (kept as `<name>.md5-mismatch`, path in the error, one retry from byte 0, same md5 twice stops with "source file or metadata is likely wrong", different md5 retries up to `--retries`, the kept copy is deleted when a later attempt verifies). `long_about` gains one sentence. Test `download_help_describes_md5_mismatch_handling`.
- [x] `docs/usage.md`: "Checksum mismatches" subsection after "Slow and stalled downloads"; `-C` row mentions the kept file.
- [x] Commit: `docs: describe md5 mismatch handling`.

### Task 6: verification and review

- [ ] `just ci`.
- [ ] Code-reviewer pass; fix findings; re-run.
- [ ] PR against `main` with `Closes #14` and `Closes #25`; note the `IaError` additions; squash-merge after checks pass; `scripts/ia-cleanup md5-mismatch-keep` only after a confirmed merge.
