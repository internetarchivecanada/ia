# The 416 delete arm says what happened (Part 2, PR 3)

**Source:** `docs/reviews/2026-10-02-series-26-43-review.md` §5 and `docs/plans/2026-09-28-download-size-check-plan.md` Task F observations: when a resume `Range` request gets `416` and the `.part` is deleted so the download restarts, `range_not_satisfiable` returns `DownloadSizeMismatch { expected: server_size, received: offset }`. With a `.part` exactly as long as the server's copy but no metadata size to confirm it (the literal reading Jake kept), that renders as `download size mismatch for <name>: expected 30 bytes, received 30 bytes`, a contradiction. The variant was also the wrong one: nothing was received; a resume was refused and the file restarts.

**Change:** that arm returns the existing retryable `IaError::ResumeFailed { file, reason }` (`--json` code `resume_failed`), whose reason states the two lengths and what was done: `the .part file is 40 bytes and the server's copy is 32 bytes; it cannot be resumed, so it was removed and the download restarts from byte 0`; when the metadata gives no size, `... is 32 bytes and the server's copy is 32 bytes, and the item metadata gives no size to confirm it; it was removed and the download restarts from byte 0`. `DownloadSizeMismatch` keeps its one meaning: the body ended with the wrong byte count. No public API change (both variants exist).

**Also:** the `range_not_satisfiable` doc comment, usage.md's 416 paragraph (the message it names), and a resolution note under the 2026-09-28 plan's observation.

## Tasks
- [ ] Red: the six 416 tests that assert `DownloadSizeMismatch` on the delete arm assert `ResumeFailed` with a reason naming both lengths; the equal-length case asserts the reason does not read "expected N bytes, received N bytes". Run; fail.
- [ ] Green: the arm, the doc comment, usage.md, the plan note.
- [ ] `just ci`; code-reviewer pass; PR; merge after checks; `scripts/ia-cleanup size-mismatch-message` after a confirmed merge.
