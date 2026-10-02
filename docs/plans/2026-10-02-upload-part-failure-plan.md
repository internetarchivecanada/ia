# Upload Part Failure Implementation Plan (#18)

**Issue:** #18. When a multipart part fails for good, `upload_file_multipart` aborts the whole upload, which tells IA to delete every part already uploaded. Multipart exists so a failure costs at most one part; the abort throws that away.

**Decision (Jake, 2026-10-01):** a part that fails with a permanent S3 refusal (`AccessDenied`, `InvalidAccessKeyId`, `BadDigest`, any non-retryable S3 code) does not abort the multipart upload. Leave it for `ia upload cleanup`; print the upload ID and the two ways forward (rerun to resume, or cleanup to discard). An exhausted transient budget also leaves the upload for resume. `upload_part_with_retry` returns the `S3Failure` (code, attempts) so the loop can tell the two apart.

**Scope, after PR #10:** every multipart request already retries transient failures, 429 and retryable S3 codes through `send_with_retry`, and since PR #31 on the standard schedule honoring Retry-After. What remains is only the `Err` arm of the part loop, which still calls `abort_upload_with_ctx`. Killing the process never aborted, so resume already works for that case.

**This reverses two recorded decisions:** `docs/plans/2026-03-05-upload-design.md:741` ("`--multipart` flag. Off by default to avoid orphaned uploads on IA") and `docs/plans/2026-03-06-upload-phase2-implementation-plan.md:1654` ("On permanent part failure, aborts the upload for cleanup"). Both get a dated note pointing here.

**Architecture.**

- `upload_part_with_retry` returns `std::result::Result<(String, u32), S3Failure>`; the public `upload_part` wrapper maps the failure into `IaError` as before, so its signature is unchanged.
- The part loop's `Err(f)` arm no longer aborts. It builds one `IaError::UploadFailed` whose message carries: the part number, what went wrong (the S3 code and message, or the transport error, with the attempt count when there was more than one), the upload ID, how many parts IA holds, and the two ways forward. Two wordings, chosen on `f.code`:
  - permanent refusal (a parsed S3 code that is not retryable): `part 3 of 7 refused by IA (AccessDenied: ...): multipart upload <id> is kept with 2 parts on IA; fix the cause and rerun the same command to resume, or discard it with 'ia upload cleanup <item> <key>'`
  - exhausted budget (retryable code, status fallback, or transport failure): `part 3 of 7 failed after 11 attempts (503 SlowDown: ...): multipart upload <id> is kept with 2 parts on IA; rerun the same command to resume, or discard it with 'ia upload cleanup <item> <key>'`
  The message is the `Failed` detail in `--json` and the joblog, as for every per-file failure (structured fields are #17's call).
- `UploadResult.retries` is unaffected. `status` on the error is the HTTP status of the final attempt when there was a response.
- `abort_upload_with_ctx` stays for `ia upload cleanup` and the public `abort_upload`. The "failed to abort ... run `ia upload cleanup`" warning goes with the abort.
- Help: `--multipart` (both arg structs) says a part that fails for good leaves the upload on IA, the error names the upload ID, rerunning resumes from the parts already there, and `ia upload cleanup` discards it; an example of the rerun. `docs/usage.md` upload section: a "Multipart part failures" paragraph with the message shapes. The module doc and `upload_file_multipart`'s doc comment drop "aborts the upload".

**Open for IA ops (not blocking, for the final ping):** how long IA keeps an unfinished multipart upload before garbage-collecting it; a part PUT to a server that stops reading can hang because the upload transport has no read timeout (#18 says this should be its own issue).

---

### Task 1: the failure reaches the loop

**Files:** `ia-core/src/upload/multipart.rs`, `ia-core/tests/upload_multipart.rs`

- [x] **Step 1: Failing tests.**
  - `part_exhausted_budget_leaves_the_upload_for_resume`: initiate OK; part 1 OK; part 2 answers `503 SlowDown` with `Retry-After: 0` on every attempt; `retries: 2`; a `DELETE ?uploadId=` mock with `expect(0)`. Result: `Err(UploadFailed)` whose message contains the upload ID, "after 3 attempts", "kept with 1 part", "rerun" and "ia upload cleanup"; `server.verify()` proves no abort.
  - `part_permanent_refusal_leaves_the_upload_for_cleanup`: part 2 answers `403` with an `AccessDenied` S3 body once; `DELETE` `expect(0)`. Message contains "AccessDenied", the upload ID, "refused", "ia upload cleanup"; the part PUT was sent once.
  - `rerun_after_part_failure_resumes_from_existing_parts`: `list_uploads` returns the upload; `list_parts` returns part 1; part 1 PUT `expect(0)`; part 2 PUT 200; complete 200 → `Uploaded`, `retries == 0`.
  - `transport_failure_past_the_budget_leaves_the_upload`: a part PUT that never gets a response. wiremock cannot drop a connection, but the crate's tests already use a raw `tokio::net::TcpListener` that accepts and closes (`client.rs` and `download/mod.rs` tests); here the whole upload must go to one server, so this test is feasible only if the retry context can target the part URL alone. If not, record it: the exhausted-budget path is the same code for a transport failure and a `503`, and the first test covers it.
  - The existing `upload_file_multipart_aborts_on_permanent_error` asserts the abort (`DELETE` `expect(1)`); it becomes `part_permanent_refusal_leaves_the_upload_for_cleanup` (`expect(0)`), so the red is a failing existing test turned around, not only new tests.
- [x] **Step 1** written: the existing abort test became `part_permanent_refusal_leaves_the_upload_for_cleanup`; `part_exhausted_budget_leaves_the_upload_for_resume` and `rerun_after_part_failure_resumes_from_existing_parts` added. The transport-failure test was not written: every upload request goes to one wiremock server and the crate's accept-and-close listener trick cannot target one part URL; the spent-budget path is the same code for a transport failure and a `503`, which the second test covers.
- [x] **Step 2: Run**; the two new failure tests failed on the message (no upload ID, no ways forward); the rerun test passed already (resume existed) and pins the scenario.
- [x] **Step 3: Implement.** `upload_part_with_retry` returns `S3Failure`; `s3_error::is_retryable_code` is the code-only form of `S3Error::is_retryable`; `KeptUpload::describe` builds the message in two wordings. Commit: `fix(upload): a failed part leaves the multipart upload on IA for resume or cleanup`.

### Task 2: help and docs

- [x] `--multipart` help on both arg structs; an example; `docs/usage.md` (`--multipart` row and a "Multipart part failures" section with both message shapes); dated notes in the two design docs; function doc comment. CLI test `upload_help_describes_kept_multipart_upload_on_part_failure` asserts the wording (red first). Commit: `docs(upload): a failed part no longer aborts the multipart upload`.

### Review findings (2026-10-02), closed before the PR

Important, fixed: `KeptUpload::describe` reworded every error from the part request, including IA's spam rejection (`SpamDetected`), which the item loop treats as fatal and which on main passed through unchanged; wrapped into `UploadFailed` it became a per-file failure telling the user to rerun a permanent rejection. Now only an `UploadFailed` is reworded; anything else passes through. Test: `spam_rejection_on_a_part_stays_fatal_and_does_not_abort` (red first), plus `describe_passes_other_errors_through_unchanged` (unit).

Suggestions, taken: unit tests of `describe` pin every wording branch without a network (transport failure past the budget, a single attempt, no parts on IA, a refusal); "kept with 0 parts on IA" became "is kept on IA with no parts yet"; the suggested cleanup command is `discard it with: ia upload cleanup <item> <key>` with the key single-quoted (shell-style) when it holds whitespace or quote characters, instead of the whole command in quotes around a raw key. For the #24 plan: the message string, the usage.md section and the two help examples name `ia upload cleanup ITEM FILE` as the way to discard; #24 changes that invocation and must update all four.

Noted, no change: `abort_upload_with_ctx` stays reachable through the public `abort_upload` used by `ia upload cleanup`; the `require_auth` wrap in `upload_part_with_retry` (attempts 0) is unreachable from the loop because `try_resume` already required auth, and it now passes through `describe` unchanged anyway.

### Task 3: verification and review

- [ ] `just ci`; code-reviewer pass (default model); fix or record findings; PR; squash-merge after checks pass; `scripts/ia-cleanup upload-part-failure` only after a confirmed merge.

> **2026-10-02 (#24):** the discard command named in the message, usage.md and the help examples is now `ia upload cleanup <item> <key> --abort`; a bare `cleanup ITEM FILE` lists. See `docs/plans/2026-10-02-upload-cleanup-safety-plan.md`.
