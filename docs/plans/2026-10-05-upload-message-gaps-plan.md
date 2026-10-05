# Upload message and progress gaps from the series review

Date: 2026-10-05. Branch `fix/upload-message-gaps`. Found by the independent
review of PRs #26–#49 (main at 179b503, version 0.21.0) before the v0.21.0
tag. Three small gaps in what the user sees during an upload, each one
with a test that fails on main first.

## 1. The check_limit poll's rate-limit event names no file

`upload::single::poll_check_limit` emits `WaitingRateLimit` with an empty
key and `total_bytes: 0` (ia-core/src/upload/single.rs:553-559). The TUI
and the console progress keep per-file rows keyed by `key`, so the event
reaches none of them. When the 503 carried a `Retry-After`, the caller
already emits the event with the key before its sleep (single.rs:218-227),
so the row shows "waiting"; when it did not, the poll's event is the only
one and the row sits on "uploading" for the whole poll.

Fix: the poll takes the key and the file size and puts them on the event.
The poll is still per item (check_limit is per bucket); only the event's
addressing changes.

Test: `check_limit_poll_reports_waiting_for_the_file` in
ia-core/tests/upload_single.rs: a 503 with no `Retry-After`, a check_limit
that clears, then a 200; a `WaitingRateLimit` event with the file's key
and size must be seen. The doc comment on
`upload_503_retry_after_reports_waiting_before_the_sleep`, which described
the empty key, is corrected.

## 2. The single PUT's transport failure has no attempt count

PR #49 gave the single PUT's spent budget on an S3 error the
"(after N attempts)" suffix through `describe_attempts`. The transport
branch (a reset, a connection closed before the response) still returns
the bare error chain (single.rs:507-512). The part path says "failed after
N attempts" for the same failure.

Fix: `describe_attempts(&full_message, retries + 1)`.

Test: `upload_exhausted_transport_error_names_the_attempt_count`: a
loopback listener that accepts each connection and closes it, `retries =
2`; the error must say "after 3 attempts".

## 3. Cleanup's listing says "this upload" for several

`ia upload cleanup ITEM FILE` with two unfinished uploads of FILE lists
both and ends with "Add --abort to abort this upload." `--abort` aborts
every upload of FILE (ia-cli/src/commands/upload.rs:1252-1258).

Fix: "this upload" for one, "these uploads" for more.

Test: `cleanup_with_file_and_two_uploads_says_these_uploads` in
ia-cli/tests/cli.rs, on a fixture listing two uploads of one key.

## Not changed, recorded

- The poll's `WaitingRateLimit` goes out once per poll attempt; unchanged.
- A `.part` removal that fails with NotFound in the 416 arm renders as a
  failed removal although the state is the wanted one (download side,
  PR #46's note); left as is.

## Review record

Filled in after the code-reviewer pass.
