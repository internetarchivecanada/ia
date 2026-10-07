# Multipart fallback for an unreliable upload path (#21)

**Rule.** Every file is sent as a single PUT. When the single PUT of a file larger than 2 MiB ends in a dead send, an unanswered body, or a transport error, and at least one retry remains, the file continues as a multipart upload with the retries it has left, and every file larger than 2 MiB that starts afterwards in the same run is sent as a multipart upload from the beginning. Files of 2 MiB or less are always sent as a single PUT. `--multipart` still sends every file as multipart. No flag turns the fallback off.

**Why this shape.** IA ops advised against a size threshold: multipart stays off by default, so that a run which dies does not leave many unfinished multipart uploads on IA, and is used only once the upload path has proven unreliable. A dead send, an unanswered body and a transport error are the outcomes that say the path is unreliable. A 503 throttle says IA is busy, and a multipart request would be throttled the same way; a non-retryable S3 refusal and a spam rejection say the request is wrong, and multipart would not change that. None of those switch.

**What does not change.** The retry schedule, the stall rules, the single PUT's headers, the multipart protocol, resume (#19), completion (#20, #44), part failures (#18), and `ia upload cleanup` (#24). A switched file that still fails is left on IA for resume, with the existing message.

## Design

- `upload::multipart::MULTIPART_FALLBACK_MIN_SIZE: u64 = 2 * 1024 * 1024`. A file switches only when its size is strictly greater than this.
- `upload::MultipartFallback`: a cloneable handle around an `Arc<AtomicBool>` with `new()`, `is_on()`, and a crate-private `turn_on()`. `Default` gives a fresh handle; clones share the state.
- `UploadOpts.multipart_fallback: MultipartFallback`, a new public field (additive under `#[non_exhaustive]`). Every clone of the options made during a run (`upload_item` for `test_item`, the concurrent path, `upload_batch` per item) shares the handle, so one handle spans the run without a change to any public function signature. A fresh `UploadOpts` carries a fresh handle.
- `upload_file` decides the path once, from `opts.multipart || (opts.multipart_fallback.is_on() && file_size > MULTIPART_FALLBACK_MIN_SIZE)`, and uses that decision both for the hashing pass (per-part md5s only on the multipart path) and for the dispatch to `upload_file_multipart`.
- In the single PUT loop, the arm for a stalled or unanswered send and the arm for a transport error gain the same step before their `continue`: when `retries < opts.retries` and the file is larger than the threshold, turn the handle on, log a warning, and return the result of `upload_file_multipart` called with a clone of the options whose `retries` is the remaining budget. The single PUT has made `retries + 1` attempts of the `opts.retries + 1` allowed, so the multipart requests get `opts.retries - retries - 1` retries each. The returned result's `retries` adds the single PUT's attempts, and `elapsed_ms` is measured from the file's start. The hashes already computed for the skip check are passed on; without per-part md5s the multipart path hashes parts as it sends them and, on a resume, hashes the file itself.
- The warning, at `warn` level (shown by default on the console): `single PUT of <identifier>/<key> <what happened>; the file continues as a multipart upload, as does every later file larger than 2 MiB in this run`.
- A failure on the last allowed attempt does not switch; the file fails as it does today.

## Public API

Additive: `upload::MultipartFallback`, `UploadOpts.multipart_fallback`, `upload::multipart::MULTIPART_FALLBACK_MIN_SIZE`. No signature changes. The version bump stays with the release process.

## Tests

Hermetic. A transport error is produced by a loopback proxy in `ia-core/tests/support/mod.rs` that accepts its first N connections and closes them at once, and forwards every later connection byte for byte to the wiremock server. A closed connection on its first use is not retried by the HTTP client, so the single PUT sees a transport error. A dead send is produced in the crate's own tests with the shrunk stall policy and a proxy that reads the first kilobyte of its first connection and then stops reading, forwarding later connections.

`ia-core/tests/upload_single.rs`:
- a 3 MiB file whose first connection is closed completes as a multipart upload (list, initiate, one part, complete observed; no single PUT reaches the server), the result is `Uploaded` with `retries` 1, and the handle is on;
- a 1 MiB file whose first connection is closed is re-sent as a single PUT (one PUT observed, no initiate) and the handle stays off;
- a 3 MiB file that gets a 503 `SlowDown` then a 200 completes as a single PUT with no initiate;
- a 3 MiB file refused with 403 fails with no initiate;
- a 3 MiB file rejected as spam fails with `SpamDetected` and no initiate;
- a 3 MiB file with `retries` 0 whose first connection is closed fails with no initiate;
- a 3 MiB file with `retries` 2 whose first connection is closed continues as multipart with one retry per request: a part answered 500 every time is tried exactly twice and the error says "after 2 attempts";
- a file with the handle already on is hashed per part and sent as multipart from the start without any single PUT.

`ia-core/src/upload/single.rs` (crate tests): a 16 MiB file whose first send dies under the shrunk policy continues as multipart; the proxy forwards the multipart requests to a wiremock server that completes them.

`ia-core/tests/upload_item.rs`:
- sequential: files [3 MiB, 3 MiB, 1 MiB], first connection closed: the first file completes as multipart, the second is initiated without a single PUT attempt, the third is a single PUT;
- concurrent (`file_concurrency` 2, four files [3 MiB, 1 MiB, 3 MiB, 3 MiB]), first connection closed: the first file completes as multipart, the 1 MiB middle file is a single PUT, the 3 MiB middle file and the last file are multipart from the start.

`ia-core/src/upload/types.rs`: a fresh `UploadOpts` has the handle off; clones share it; `Default` handles are independent.

`ia-cli/tests/cli.rs`: `ia upload --help` describes the fallback ("larger than 2 MiB", "continues as a multipart upload").

## Documentation

- `ia upload --help`: the `long_about` paragraph on single PUT versus `--multipart` gains the fallback sentence; the `--multipart` help says the flag forces what the fallback does on its own after a failure; the `--retries` help says the remaining attempts carry over to the multipart requests. Both upload argument structs.
- `docs/usage.md`: the `--multipart` row; a "Falling back to multipart" section under the upload stall and multipart sections stating the rule, the trigger, the non-triggers, the carried budget, the warning text, and that `--multipart` still forces it; the "Resuming uploads" paragraph notes that a switched file resumes like a `--multipart` file.
- `README.md`: the upload row.

## Finding during implementation: requests without a body have no answer bound

The multipart control requests (initiate, complete, abort, the two listings) carry no body, so neither the dead-send rule nor the 120 s answer bound from #40 applies to them, and the upload transport has no read timeout. Against a server that accepts a connection and then goes silent, a single PUT was abandoned after 60 s; with the fallback, the same file now continues with an initiate request to the same server, and that request waits indefinitely. Before this change the behavior was reachable only with `--multipart`; the fallback reaches it on the default path. The pre-existing crate test that sent a 16 MiB file to such a server with one retry, and expected the file to fail as stalled after two sends, hung for this reason; it now covers the no-retry case, and the retry case is covered by the fallback test. The bound for bodiless requests is a separate change (an answer clock on every IA-S3 request, not only after a body) and is filed as its own issue.

## Tasks

- [x] Plan committed first.
- [x] Red: the tests above.
- [x] Green: `MULTIPART_FALLBACK_MIN_SIZE`, `MultipartFallback`, the `UploadOpts` field, the decision in `upload_file`, the switch in the two arms, the warning.
- [x] Docs: help text on both structs, `usage.md`, `README.md`.
- [ ] `just ci`; code-reviewer pass; findings closed and recorded below; PR; squash-merge after green checks; `scripts/ia-cleanup multipart-fallback` after the merge is confirmed; close #21 naming the PR and commit; file the bodiless-request issue.
