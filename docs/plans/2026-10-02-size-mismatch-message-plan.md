# The 416 delete arm says what happened (Part 2, PR 3)

**Source:** `docs/reviews/2026-10-02-series-26-43-review.md` §5 and `docs/plans/2026-09-28-download-size-check-plan.md` Task F observations: when a resume `Range` request gets `416` and the `.part` is deleted so the download restarts, `range_not_satisfiable` returns `DownloadSizeMismatch { expected: server_size, received: offset }`. With a `.part` exactly as long as the server's copy but no metadata size to confirm it (the literal reading Jake kept), that renders as `download size mismatch for <name>: expected 30 bytes, received 30 bytes`, a contradiction. The variant was also the wrong one: nothing was received; a resume was refused and the file restarts.

**Change:** that arm returns the existing retryable `IaError::ResumeFailed { file, reason }` (`--json` code `resume_failed`), whose reason states the two lengths and what was done: `the server answered 416 to a resume from byte 40 of its 32-byte copy; the .part file is longer than the file, so the .part file was removed`; with equal lengths, `...; the .part file is as long as the file but the item metadata has no size that can confirm it, so the .part file was removed`; with a shorter .part (the server refused anyway), `...; the .part file cannot be resumed from there, so ...`; when the removal fails, `..., and removing the .part file failed: <error>`. `DownloadSizeMismatch` keeps its one meaning: the body ended with the wrong byte count. No public API change (both variants exist).

**Also:** the `range_not_satisfiable` doc comment, usage.md's 416 paragraph (the message it names), and a resolution note under the 2026-09-28 plan's observation.

## Tasks
- [x] Red: the eight 416 tests (six found by their asserted values first, two more by the full download test run) that assert `DownloadSizeMismatch` on the delete arm assert `ResumeFailed` with a reason naming both lengths; every case asserts the reason does not promise a restart. Run; all failed with the old variant.
- [x] Green: the arm, the doc comment, usage.md, the plan note. 148 download tests pass.
- [ ] `just ci`; code-reviewer pass; PR; merge after checks; `scripts/ia-cleanup size-mismatch-message` after a confirmed merge.

## Review (2026-10-02), closed before merge

Important, fixed: the reason promised "the download restarts from byte 0", which is false when the arm fires on the last attempt (the text is then the file's failure); it also said "was removed" without checking the removal. The reason now states facts only: the 416, the resume offset, the server's length, why the `.part` cannot be resumed (longer than the file / as long but no metadata size can confirm it / cannot be resumed from there, when the server refused a shorter `.part`), and whether the removal succeeded. The restart is the retry loop's business and usage.md's.

Suggestions, taken: the "no size to confirm" wording applies only to the equal-length case (a longer `.part` cannot be resumed regardless of metadata); "has no size that can confirm it" is true for `_files.xml` too, whose metadata size exists but is not final; the odd shorter-`.part` case is explained by naming the 416; usage.md's quote uses N for the server's length as the rest of the paragraph does. Not taken: "see --min-speed" in download's `--retries` help is correct, download still has `--min-speed` (only upload's was removed in #42).
