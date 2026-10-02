# Multipart Skip Check and Post-Completion Verification Implementation Plan (#20)

**Issue:** #20. `upload_file` branches to `upload_file_multipart` before the two checks a single PUT gets by default: the skip-if-already-uploaded md5 comparison (`opts.checksum`, on unless `--clobber`) and `Content-MD5` verification. A rerun of a completed `--multipart` upload sends the whole file again, `--clobber` and `--no-verify` are silently ignored, and nothing confirms the assembled object: IA assembles it asynchronously, so a 200 on completion proves nothing about the object.

**Decisions (Jake, 2026-10-01):**
- After completion, verify the assembled file through the metadata API (`/metadata/{id}` file entry: size and md5), polling with the standard backoff schedule for up to 5 minutes. Verified → `Uploaded`. Timeout → a distinct status "uploaded, not yet verified", exit 0 with a warning.
- Move the checksum-skip check above the multipart branch so both paths share it: one full read gives the local md5 and the per-part md5s (#19's `hash_file_and_parts`). Set `UploadResult.md5` for multipart.
- `--delete-after-upload` deletes the local file only after verification passes; on timeout keep it and say so.
- `--clobber --no-verify` skips the pre-upload full read, as single PUT does.

**Architecture.**

- **One read, shared.** `upload_file` computes `FileHashes` (whole-file md5 and per-part md5s at `DEFAULT_PART_SIZE`) when `opts.checksum || opts.verify`, before the multipart branch, honoring `opts.checksum_file` for the whole-file md5 when a precomputed value exists (then the per-part md5s are computed only if multipart needs them for a resume). The skip check runs for both paths. `upload_file_multipart` gains an `hashes: Option<&FileHashes>` parameter (public API change: a new parameter; the alternative, a second entry point, would leave the old one doing the wrong thing). `try_resume` uses the given per-part md5s and hashes the file itself only when none were given (`--clobber --no-verify` with an unfinished upload on IA).
- **Verification after completion.** `verify_assembled(client, identifier, key, size, md5, deadline)`: poll `client.get_item` until the file entry has `size == file_size` and `md5 == local md5`; between polls wait on the standard schedule (`crate::retry::backoff_policy`, 1 s to 60 s, full jitter) with a generous retry count, bounded by a 5-minute deadline; a `429` from the metadata API honors its `Retry-After` through the existing `RateLimited` path. An entry with the right size but a different md5 is a mismatch, not "not yet": the file fails (`UploadFailed`, "assembled object md5 X does not match local Y") so the user knows the object on IA is wrong. An entry whose md5 is absent while the size matches is treated as not yet assembled.
- **Statuses.** New `UploadStatus::UploadedUnverified` (serde: `uploaded_unverified`), reported when the deadline passes: the parts all landed and completion returned 200, but the object has not appeared in the metadata with the expected size and md5 within 5 minutes. The CLI prints it as a success with a warning line, exits 0, and the joblog records it as `ok` (the joblog has no note field; `ok` is what makes a rerun skip the file) (the skip check will then find the md5 once IA has it; if it never does, the next rerun's skip check fails and uploads again, which is the right recovery). `--json` shows `"status":"uploaded_unverified"`.
- **`--delete-after-upload`** moves after verification: deleted on `Uploaded`, kept on `UploadedUnverified` with the warning saying so. The single-PUT path is unchanged (IA verified `Content-MD5` synchronously).
- **`UploadResult.md5`** is set for multipart from the local hash; `bytes` unchanged.
- **Skip of the read.** `--clobber --no-verify` → no hashes computed, no skip check, no post-completion md5 check (size is still checked; IA's metadata size is free), `UploadResult.md5: None`.
- **Dry run** is unchanged (no read).
- **Help:** `--multipart` (both structs) says the skip check and `--clobber` apply, that the assembled object is checked by size and md5 for up to 5 minutes, what "uploaded, not yet verified" means, and that `--no-verify` skips the md5 part of both; `--delete-after-upload` says the file is deleted only once verified; `--clobber`/`--no-verify` help loses nothing. `docs/usage.md`: the upload section's integrity text covers multipart; a "Verifying a multipart upload" paragraph with the three outcomes. `after_long_help` "Integrity & Skip Behavior" block gains a multipart line.

**Engineering calls (flagged in the PR):** the poll interval is the standard schedule rather than a fixed period; a size-match with a different md5 fails rather than waits; `UploadedUnverified` is a success for exit-code purposes (Jake: exit 0 with a warning); a precomputed `--checksums` md5 is trusted for the skip check and the post-completion check, with per-part md5s computed only on a resume.

---

### Task 1: skip check and one read before the branch

**Files:** `ia-core/src/upload/single.rs`, `ia-core/src/upload/multipart.rs`, `ia-core/tests/upload_multipart.rs`, `ia-core/tests/upload_single.rs`

- [ ] **Step 1: Failing tests.** `multipart_skips_a_file_whose_md5_matches` (metadata lists the key with the local md5 → `Skipped`, no S3 request); `multipart_clobber_uploads_despite_a_matching_md5`; `multipart_sets_result_md5`; `multipart_clobber_no_verify_reads_nothing_before_uploading` (observable: no metadata GET; the resume path still hashes when an unfinished upload exists); existing single-PUT skip tests unchanged.
- [ ] **Step 2: Run**; fail.
- [ ] **Step 3: Implement.** Commit: `fix(upload): the skip check and one read of the file apply to --multipart too`.

### Task 2: verify the assembled object

**Files:** `ia-core/src/upload/multipart.rs`, `ia-core/src/upload/types.rs`, `ia-core/tests/upload_multipart.rs`, `ia-cli/src/commands/upload.rs`, `ia-cli/src/output.rs`, `ia-cli/src/tui/upload_app.rs`, `ia-core/src/collection.rs` (exhaustive matches on `UploadStatus`)

- [ ] **Step 1: Failing tests.** After a 200 on complete: metadata shows the file with the right size and md5 on the first poll → `Uploaded`, `md5` set; a 404 then a placeholder entry (right name, wrong size, no md5) then the right entry → `Uploaded` after three polls, waits between them (test deadline shrunk through a `cfg(test)` seam or an opts field, as the stall tests did); right size but a different md5 → `UploadFailed` naming both md5s; never appears before the deadline → `UploadedUnverified`, local file kept with `--delete-after-upload`; `--no-verify` → size checked only; the CLI prints a warning line and the joblog records `ok`; `--json` status string; the TUI and `collection.rs` matches count it as uploaded.
- [ ] **Step 2: Run**; fail.
- [ ] **Step 3: Implement.** Commit: `feat(upload): confirm a multipart upload's assembled object by size and md5`.

### Task 3: docs, verification, review

- [ ] Help on both structs, `after_long_help`, usage.md; `just ci`; code-reviewer pass; fix or record findings; PR; squash-merge after checks pass; `scripts/ia-cleanup upload-multipart-verify` only after a confirmed merge.
