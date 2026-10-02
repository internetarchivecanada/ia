# Multipart completion is the upload (Part 2, PR 1)

**Source:** `docs/reviews/2026-10-02-series-26-43-review.md` §5 (local file; `docs/reviews/` is gitignored), Jake's decision 2026-10-02: the post-completion metadata poll from #35 (PR #35) is wrong. IA checks every part at completion: the manifest carries each part's md5 as its ETag (IA returns no ETag on a part PUT, so the local md5 goes in; `upload_part_with_retry`), and IA compares the manifest against the parts it holds before answering 2xx. A 2xx on complete therefore means every byte landed as sent. Polling the item's metadata afterwards added a five-minute wait, a third outcome (`uploaded, not yet verified`, `uploaded_unverified`) that the joblog had to record as `ok`, and a `--no-resume` instruction nobody should need.

**Change:** a 2xx on multipart complete is `Uploaded`. `--delete-after-upload` deletes the local file then, as the single-PUT path does after its synchronous Content-MD5 check.

**Removed:** `verify_assembled`, the `Assembled` enum and the second `Verifying` progress event in `upload/multipart.rs`; `UploadOpts.verify_timeout`; `UploadStatus::UploadedUnverified` (public API; recorded for the 0.21.0 bump in PR 4); every match arm on it (CLI result line, joblog, summary, TUI, `collection.rs`, `item.rs`); the "not yet verified" text in `--multipart` and `--delete-after-upload` help, usage.md and the README row; the `mount_assembled` metadata fixture every completing multipart test had to mount.

**Content-MD5 on part PUTs:** not sent today. IA's S3 help (`archive.org/help/abouts3.txt`) says only "ias3 has support for multipart uploads"; whether IA honors Content-MD5 on a part PUT can be learned only by a live write to archive.org, which this project does not do from code. Not added; the manifest check remains the verification. A manual check by Jake against a test item would settle it.

## Tasks

### Task 1: red
- [x] `ia-core/tests/upload_multipart.rs`: `multipart_complete_2xx_is_uploaded_without_reading_metadata` — fresh single-part upload mocked, `GET /metadata/test-item` `expect(0)`, `delete_after_upload: true`, `clobber_opts()` (no skip check) → `Uploaded`, local file gone, `server.verify()`.
- [x] `ia-cli/tests/cli.rs`: `upload_help_describes_multipart_verification` becomes `upload_help_does_not_promise_a_post_completion_check` — help contains neither "not yet verified" nor "5 minutes".
- [x] Run; both failed: the library test sat in the poll until a 90 s cap killed it (the default deadline is 5 minutes and the field that shortens it is the one being removed), the help test failed on "not yet verified".

### Task 2: green
- [x] Remove the poll, the enum, the second Verifying event, the field, the variant and every arm; delete the nine tests that pinned the poll (`multipart_polls_metadata_until_the_object_appears`, `multipart_fails_when_the_assembled_md5_differs`, `..._reports_unverified_at_the_deadline_and_keeps_the_file`, `..._deletes_the_local_file_only_once_verified`, `..._no_verify_checks_size_only`, `..._polls_through_a_404_before_the_object_appears`, `..._verification_honors_retry_after_on_the_metadata_api`, `..._does_not_poll_past_a_retry_after_beyond_the_deadline`, `..._floors_the_wait_between_polls`) and the three unit tests on the variant; drop `mount_assembled` and its callers.
- [x] Help, usage.md ("Completing a multipart upload"), README row, the #35 plan doc (dated reversal note). The `--clobber --no-verify` test went back to `expect(0)` on the metadata endpoint: nothing reads it now.
- [ ] `just ci`; code-reviewer pass; PR; merge after checks; `scripts/ia-cleanup multipart-complete-is-uploaded` after a confirmed merge.
