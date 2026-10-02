# Upload Resume Validation Implementation Plan (#19)

**Issue:** #19. `try_resume` takes the most recent unfinished multipart upload for the key and marks its listed part numbers as done without comparing anything to the local file. A changed local file, a changed part size, or a different file under the same remote name yields a completed object made of stale parts mixed with new ones, and IA's completion check passes because the ETags came from IA's own list.

**Decision (Jake, 2026-10-01, "no decision needed"):** every listed part must match the local file: its `size` must equal the expected size at the current part size, and its ETag must equal the md5 of the matching local range. Any mismatch → do not reuse that upload, initiate a new one, warn with the old upload ID. Handle pagination in `list_parts` and `list_uploads` if IA uses it.

**Designed together with #20 (cross-issue decision):** the per-part md5s and the whole-file md5 come from one read pass. This PR adds that pass, `checksum::hash_file_and_parts(path, part_size)`, returning the whole-file md5 and one md5 per part, and uses the part md5s to validate a resume. #20 then moves the pass above the multipart branch for the skip check and hands its result in, so the file is read once per upload. On its own, this PR reads the whole file only when there is an unfinished upload to validate.

**Architecture.**

- `checksum::hash_file_and_parts(path, part_size) -> Result<FileHashes { md5: String, parts: Vec<String> }>`: one sequential read, feeding a whole-file hasher and a per-part hasher that is finalized at every part boundary. `part_size == 0` is an error. An empty file yields the empty md5 and one part. Hex, unquoted, lowercase.
- `try_resume(client, ctx, file, file_size, part_size)`: lists the uploads for the item; the candidates are the uploads whose `key` matches, newest first (the list is chronological; `rfind` order). For each candidate: `list_parts`, then `validate_parts(&parts, file_size, part_size, &hashes.parts)`. The first candidate that validates is resumed. A candidate that does not validate is left in place (never aborted here; #18's rule) and logged at warn with its upload ID, the reason, and that `ia upload cleanup` discards it. If none validates (or none exists), return `None` so the loop initiates a fresh upload. The local hashes are computed once, lazily, the first time a candidate needs them.
- `validate_parts`: for each listed part, `part_number` must be in `1..=part_count`; `size` must equal the expected size for that part (`part_size`, or `file_size - offset` for the last part) when the listing gave a size (IA's `<Size>` is parsed with a fallback of 0 today; a missing size is not held against the part, the md5 check is the stronger one); the ETag, with surrounding quotes stripped and compared case-insensitively, must equal the local md5 of that range. A duplicate part number is a mismatch. The function returns `Ok(())` or `Err(reason: String)` naming the first offending part.
- **Pagination**, S3 semantics, cheap to do right whether or not IA paginates: `list_parts_with_ctx` follows `<IsTruncated>true</IsTruncated>` with `<NextPartNumberMarker>` as `part-number-marker`; `list_uploads_with_ctx` follows `<IsTruncated>` with `<NextKeyMarker>` and `<NextUploadIdMarker>` as `key-marker` and `upload-id-marker`. A truncated page without a marker stops the loop (what has been read is returned) rather than spinning. The public wrappers' signatures are unchanged.
- `PartInfo.size` stays `u64` (0 when absent); the validation treats 0 as "not given" only when the expected size is not 0.
- Help: `--multipart` (both structs) adds one sentence: a resume is used only when every part IA holds matches the local file by size and md5; otherwise a fresh upload starts and the stale one is left for `ia upload cleanup`. `docs/usage.md`: a "Resuming a multipart upload" paragraph with what is checked and what happens on a mismatch. (`docs/plans/2026-03-18-upload-auto-resume-design.md` is about the joblog, not multipart parts; the unchecked resume is described in `2026-03-06-upload-phase2-implementation-plan.md`, which gets a dated note.)

**Engineering calls (flagged in the PR):** newest-first and first-valid-wins among several unfinished uploads for one key; a missing `<Size>` does not fail validation; a stale upload is left in place rather than aborted (#18's rule).

---

### Task 1: one read pass, whole-file and per-part md5s

**Files:** `ia-core/src/upload/checksum.rs` (+ unit tests)

- [x] **Step 1: Failing tests.** `hash_file_and_parts` on 2500 bytes with `part_size` 1024 → three part md5s equal to `md5` of each range and a whole-file md5 equal to `compute_file_md5`; a file exactly 2 × 1024 → two parts; an empty file → one part with the empty md5; `part_size == 0` → `Err`.
- [x] **Step 2: Run**; compile failure.
- [x] **Step 3: Implement.** `FileHashes { md5, parts }`, `hash_file_and_parts`, and `hash_file_and_parts_async` (blocking thread, as `compute_file_md5_async`). Commit: `feat(upload): one read pass yields the whole-file md5 and every part's md5`.

### Task 2: validate before reusing

**Files:** `ia-core/src/upload/multipart.rs`, `ia-core/tests/upload_multipart.rs`, `ia-cli/src/commands/upload.rs`, `ia-cli/tests/cli.rs`, `docs/usage.md`

- [x] **Step 1: Failing tests.**
  - `resume_with_matching_parts_skips_them` (the existing `upload_file_multipart_resumes_from_existing` and `..._resume_non_contiguous_parts` with ETags that are the real md5s of the local ranges and real sizes; today they use placeholder ETags, so they must be updated, and that is the red for the validation).
  - `resume_rejects_a_part_whose_md5_differs`: part 1 listed with a wrong ETag → no part skipped, a fresh initiate, both parts PUT, the old upload never aborted (`DELETE expect(0)`), and the warning names the old upload ID (observed through the new upload ID in the complete call).
  - `resume_rejects_a_part_whose_size_differs`: right ETag, wrong size.
  - `resume_rejects_a_part_number_past_the_count`: a listed part 5 on a 2-part file.
  - `resume_picks_the_newest_valid_upload`: two unfinished uploads for the key, the newer invalid, the older valid → the older is resumed.
  - `resume_with_a_missing_size_relies_on_the_md5`: `<Part>` without `<Size>`, right ETag → reused.
  - `validate_parts` unit tests for each rule, including a duplicate part number.
  - `upload --help` mentions the check.
- [x] **Step 2: Run**; the four rejection tests failed (every listed part was reused); the `validate_parts` unit tests failed to compile; the help test failed. The two existing resume tests and the #18 rerun test, given real md5 ETags, kept passing and pin "a valid resume still skips". The missing-size test passed already (nothing checked sizes); it pins the engineering call.
- [x] **Step 3: Implement.** `try_resume(client, ctx, file, file_size, part_size)` hashes the file once (only when there is a candidate), checks candidates newest first with `validate_parts`, warns with the upload ID on a mismatch, and never aborts. The warning is not asserted (no log capture in the tree); the fresh initiate and the `DELETE expect(0)` are. Commit: `fix(upload): resume a multipart upload only when every part on IA matches the local file`.

### Task 3: pagination

**Files:** `ia-core/src/upload/multipart.rs`, `ia-core/tests/upload_multipart.rs`

- [x] **Step 1: Failing tests.** `list_parts` over two pages (`IsTruncated` + `NextPartNumberMarker`; the second request carries `part-number-marker`) returns both pages' parts; `list_uploads` over two pages likewise with the key and upload-id markers; a truncated page with no marker returns what was read.
- [x] **Step 2: Run**; the two two-page tests failed (one request each, one entry returned); the no-marker guard passed already (no loop existed) and pins the guard.
- [x] **Step 3: Implement.** `next_page_marker` reads `IsTruncated` and the named marker; both listings loop, appending `part-number-marker` or `key-marker` + `upload-id-marker` (url-encoded). Commit: `fix(upload): follow S3 pagination when listing multipart uploads and parts`.

### Review findings (2026-10-02), closed before the PR

Important, fixed: in the hasher, `(part_size - in_part) as usize` would truncate on a 32-bit target (a 4 GiB part gives a room of 0 and the inner loop never advances); now `usize::try_from(..).unwrap_or(usize::MAX)`. Production passes 100 MiB; the function is a public entry point.

Suggestions, taken: a listing page whose marker repeats the one just sent ends the walk (the red for that test was an infinite loop); candidates are sorted newest first by `Initiated` (stable, so ties and missing times keep the reversed listing order) instead of trusting the listing order; `FileHashes` carries `size`, the bytes actually hashed, so #20 can hand one struct to the skip check and `try_resume` and the sizes and hashes always describe the same bytes; the hasher is split into `hash_reader_and_parts(reader, part_size, chunk_len)` so tests cover both shapes without large files (a part carrying across reads, the production shape; several boundaries in one chunk).

Second batch (findings 6 to 11): the not-resuming warning now names the upload ID in its text and gives the full `ia upload cleanup <item> <key>` command through the same quoting helper as #18's message, and usage.md quotes it exactly; the reason string says "ETag" (IA could return a non-md5 ETag; it is rejected by the md5 rule either way); three pinning unit tests (a 15-byte part under a 10-byte part size, the empty file where an expected size of 0 makes a listed size a real mismatch, a composite ETag); the rerun example in `after_long_help` says the parts are checked first. Noted, no change: a `NoSuchBucket` on a later page discards earlier pages (cannot happen in practice; the result is a fresh upload); a `list_parts` failure for one candidate fails the upload (pre-existing); both hashers run over every byte, well above disk throughput.

### Task 4: docs, verification, review

- [ ] usage.md, help, the phase-2 plan's dated note; `just ci`; code-reviewer pass; fix or record findings; PR; squash-merge after checks pass; `scripts/ia-cleanup upload-resume-validation` only after a confirmed merge.
