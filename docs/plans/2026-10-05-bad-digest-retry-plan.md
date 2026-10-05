# `BadDigest` is retried

**Decision (Jake, 2026-10-05):** retry `BadDigest` within the existing `--retries` budget on both paths, single PUT and multipart part.

**Why.** `BadDigest` means IA's md5 of the body it received differs from the `Content-MD5` we sent. Over https the bytes cannot change on the wire (TLS fails the connection first), so the cases left are: IA received a truncated body (a connection cut mid-body, hashed as received) or an IA-side fault, both fixed by a re-send; and, on the single PUT only, a file edited between the md5 pass and the send, which a re-send cannot fix (the file is streamed again but the first md5 is reused) and which then spends the budget and ends with the same error naming `BadDigest`. On a part the body is one in-memory buffer, hashed then streamed, so that case cannot occur. Today `BadDigest` is a permanent refusal: a part fails for good with "refused by IA (BadDigest ...): fix the cause and rerun", naming a cause the user cannot fix.

**Change:** `BadDigest` joins the retryable codes in `s3_error::is_retryable_code`, which both paths use. The exhausted-budget wording ("failed after N attempts (BadDigest: ...)") already fits. usage.md's "Retries" lists it among the transient failures; "Multipart part failures" no longer lists it among the refusals.

**Not changed:** the single PUT does not re-hash the file per attempt (a separate question; a changed file is a user action, and the final error still says what IA saw).

## Tasks
- [x] Red: `upload_400_bad_digest_is_not_retried` (upload_single.rs) becomes `upload_400_bad_digest_is_retried` (a BadDigest then a 200 → Uploaded, two PUTs); the two `s3_error.rs` tests pinning it non-retryable flip; a part-PUT test: part 1 BadDigest once, then 200, upload completes. Run: all four failed.
- [x] Green: the classifier; the doc list on `S3Error::is_retryable`; the `KeptUpload::describe` comment's example code; usage.md.
- [ ] `just ci`; code-reviewer pass; PR; merge after checks; `scripts/ia-cleanup bad-digest-retry` after a confirmed merge.

## Review (2026-10-05), closed before merge

Suggestion, taken: on the single PUT a spent budget on a non-503 retryable code returned the bare S3 text ("BadDigest: ..."), so eleven sends read like a first-try refusal; the message now carries "(after N attempts)" through the shared `describe_attempts`, pinned by `upload_exhausted_bad_digest_names_the_attempt_count`. The usage.md sentence says "the body is sent again" (the single PUT re-reads the file; a part re-sends the same bytes).

Noted, no change: `S3Error::is_retryable` is public, so the classification is observable to external consumers (in the PR body); a 503 carrying `<Code>BadDigest</Code>` would now poll check_limit, theoretical since IA sends it as 400; the cost of the change is up to ten extra sends before the same error in the one case a retry cannot fix; the 2026-03-05 design doc's "400 BadDigest | No" row is a dated record.
