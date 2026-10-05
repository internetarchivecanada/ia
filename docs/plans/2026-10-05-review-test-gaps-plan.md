# Test gaps from the series review

Date: 2026-10-05. Branch `chore/review-test-gaps`. The independent review of
PRs #26–#49 (main at 179b503) mapped every behavior change of the series to
the test that pins it. Three gaps are closed here; the rest are recorded
below.

## 1. `#[non_exhaustive]` has no test

PR #47 put `#[non_exhaustive]` on `IaError`, `UploadStatus`, `DownloadOpts`
and `UploadOpts`. Nothing fails if the attribute is removed: inside the
crate it has no effect, and the repo has no compile-fail harness.

Fix: a `compile_fail` doctest on each type. Doctests compile as an external
crate, so the attribute is in force there. For the two structs the test is
a struct expression, which is exactly what the attribute forbids
(`E0639`). For the two enums it is a `match` that names every variant and
has no wildcard; the only reason it cannot compile is the attribute
(`E0004`). The enum tests must list every variant: a variant added later
without a line in the test still fails to compile (a missing arm is
`E0004` too), so the test keeps passing but no longer proves the
attribute. The doc comment says so, and it is the one maintenance cost.

No new crate: `trybuild` would do the same with a dependency.

## 2. A part's spent budget on BadDigest has no test of its own

PR #49 made `BadDigest` retryable on both paths. The single PUT has
`upload_exhausted_bad_digest_names_the_attempt_count`; the part path's
spent-budget message is pinned only through a `SlowDown` test.

Fix: `part_exhausted_bad_digest_budget_is_a_kept_upload` in
ia-core/tests/upload_multipart.rs: every send of part 2 gets `400
BadDigest`, `retries = 2`; the error must read "failed after 3 attempts
(BadDigest ...)", not "refused by IA", carry status 400, keep the upload
and name `ia upload cleanup`.

## 3. A stale comment

`stalled_file_is_not_retried_by_the_outer_loop` in
ia-core/src/download/mod.rs says the outer loop's first retry delay is
2 s. Since PR #32 the first wait is random up to 1 s. The 3 s quiet period
the test uses is therefore three times the longest first wait, not one and
a half. Comment corrected; the test is unchanged.

## Recorded, not changed

- `grace_restarts_on_each_stream` (download/mod.rs): a 600 ms silence
  against a 1 s test grace. The test's point is a fresh grace per stream;
  the slack is 400 ms on a loopback socket. Left.
- `openai_500_with_retry_after_zero_resends_at_once` (ai/client.rs): two
  local requests must finish in 900 ms. Left.
- The `ResumeFailed` arm's "removing the .part file failed" branch has no
  test: a `.part` that is a directory never reaches the arm (it is not
  resumed), and no other way to make the removal fail is portable.
- The CLI's wildcard arms for an unknown `UploadStatus` are dead code
  until a variant is added; untestable without one.
- The download per-file wait's randomness and the self-updater's own
  retry behavior are covered by the shared `retry` module's unit tests,
  not by command-level tests. Left.

## Review record

Filled in after the code-reviewer pass.
