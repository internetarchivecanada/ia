# Upload Retry Backoff Implementation Plan

Follow-up to PR #10 (one retry policy for every IA-S3 multipart request).

**Decision (Jake, 2026-10-01):** remove `--retry-sleep`. Retries follow the industry standard: the number of attempts is the only knob; the schedule is truncated exponential backoff with jitter from the library already in the tree (`reqwest-retry` re-exports `retry_policies::ExponentialBackoff`); a `Retry-After` header from the server overrides the computed wait. "We must always adhere to Retry-After. Everywhere anything is retried."

**Definitions.** Retry: the same request again after a failure. Backoff: the wait before a retry. Exponential: each wait doubles. Truncated: no single wait exceeds a cap. Jitter: each wait is randomized so clients do not retry in lockstep. Retry-After: the HTTP header in which the server says how long to wait (seconds, or an HTTP date).

**Schedule.** Library `ExponentialBackoff` with bounds 1 s to 60 s, base 2, full jitter (a uniform random wait between 0 and the computed one). The upper bounds for retries 1, 2, 3, ... are 1, 2, 4, 8, 16, 32, 60, 60, ... s. Whole-run worst case over the default 10 retries is about 4 minutes. These are the bounds the transport middleware used before PR #10.

**History, plainly:** the application-level `--retry-sleep` was always a constant sleep. PR #10 removed the middleware's exponential backoff from under it. This plan puts the standard schedule into the application loop and drops the flag.

**Architecture:**

- `UploadOpts` loses `retry_sleep` and gains `retry_min_delay` (1 s) and `retry_max_delay` (60 s): the schedule's bounds, with builder methods. They are library configuration, not CLI flags; tests shrink them to milliseconds. `retries` stays.
- `S3RetryCtx` carries the built `ExponentialBackoff` instead of a sleep. `send_with_retry` reads `Retry-After` from a failed response before consuming its body and sleeps that, or else the policy's wait for the retry number.
- `single.rs`: the non-503 retry sleep uses the same policy; the check-limit poll waits with the same policy between polls (a poll with backoff, standard) and honors `Retry-After` from the 503 that sent it there.
- `crate::retry::extract_retry_after` learns the HTTP-date form (`httpdate` is already a dependency); a date in the past is 0.
- CLI: `--retry-sleep` removed from both `UploadArgs`; the `--retries` help says how the waits grow and that `Retry-After` is honored.

**Public API (ia-core):** `UploadOpts::retry_sleep` field and builder method removed; `retry_min_delay`/`retry_max_delay` added. `S3RetryCtx` is crate-private. CLI: `--retry-sleep` removed (was in the Python CLI as `--sleep`; the parity is deliberately dropped).

**Other retry sites (audit, 2026-10-01):** honored today: tasks API (rate-limit pause), metadata writes (`concurrency.rs`), metadata reads on a 429 (`read.rs` turns it into `RateLimited` with the header's seconds; the concurrent path pauses every worker for that long). Not honored, for the next PR, fixed once at the transport layer so every API call gets it: 5xx on any API call (the reqwest-retry 0.9 middleware ignores the header), search (a 429 or 503 is a plain `Http` error: no retry, no pause, header never read), the download per-file loop (headers are dropped in `fetch_response` before the loop sees the error), AI client LLM retries. Not applicable: stream re-request after a body error, metadata-read decode retries (no response to read).

---

### Task 1: the schedule and the header

**Files:** `ia-core/src/upload/types.rs`, `ia-core/src/upload/retry.rs`, `ia-core/src/retry.rs`

- [x] **Step 1: Failing tests.**
  - `types.rs`: the defaults test asserts `retry_min_delay == 1 s`, `retry_max_delay == 60 s`, and no `retry_sleep`.
  - `retry.rs` (new `tests` module): `backoff_policy(min, max, retries)` + `backoff_wait(&policy, n_past_retries)`: with 1 s/60 s, retry 1 waits at most 1 s, retry 7 and 20 at most 60 s, never more than the upper bound, and 1000 draws are not all equal (jitter). With retries = 3, `n_past_retries = 3` → zero wait (the policy says do not retry; the loop's own budget check is what ends the loop).
  - `crate::retry`: `extract_retry_after("120")` → 120; an HTTP date 30 s ahead → 29..=31; a date in the past → 0; `"soon"` → None. (Tasks 1 and 2 were committed together: the schedule helpers are unused until the loops call them, and every commit must pass clippy.)
- [x] **Step 2: Run**; compile failure.
- [x] **Step 3: Implement.** Commit: `feat(upload): standard exponential backoff schedule; Retry-After parses HTTP dates`.

### Task 2: use them, drop the flag

**Files:** `ia-core/src/upload/retry.rs`, `single.rs`, `multipart.rs`, `ia-cli/src/commands/upload.rs`, tests

- [x] **Step 1: Failing tests.**
  - `ia-core/tests/upload_multipart.rs`: `part_retry_honors_retry_after`: part PUT answers 503 with `Retry-After: 1` once, then 200; bounds 1 ms/2 ms; the upload completes and takes at least 1 s.
  - `ia-core/tests/upload_single.rs`: `retry_honors_retry_after`: same through `upload_file` on a 503 whose check-limit clears at once.
  - `ia-cli/tests/cli.rs`: `upload --help` has no `--retry-sleep`; `ia upload x f --retry-sleep 5` exits 2 with "unexpected argument"; `--retries` help mentions `Retry-After`.
  - Existing tests: every `retry_sleep: Duration::from_millis(..)` becomes `retry_min_delay: Duration::from_millis(1), retry_max_delay: Duration::from_millis(2)`.
- [x] **Step 2: Run**; fail.
- [x] **Step 3: Implement.** Commit: `fix(upload): remove --retry-sleep; back off exponentially and honor Retry-After on IA-S3 retries`.

### Task 3: docs

- [x] `docs/usage.md`: drop the `--retry-sleep` row and its mention in the batch-options list; the `--retries` row says "waits grow from 1 s to 60 s with jitter; a Retry-After header is honored". Commit: `docs: retries back off; --retry-sleep is gone`.

### Task 4: verification and review

- [x] `just ci`; code-reviewer pass; fix findings. [ ] PR against `main` ("Follow-up to #10"); squash-merge after checks pass; `scripts/ia-cleanup retry-sleep-backoff` only after a confirmed merge.

### Review findings (2026-10-01), fixed before the PR

Important:
1. `backoff_policy` panics when `retry_min_delay > retry_max_delay` (the library asserts it), reachable from public `UploadOpts` fields. Validate once: add `UploadOpts::backoff(&self) -> ExponentialBackoff` (crate-visible) that clamps `max = max.max(min)` with a `warn!`, and use it at the four construction sites (`single.rs`, two in `multipart.rs`, `default_ctx`). Test: min > max does not panic and uses min as the cap.
2. Stale docs: `docs/design-philosophy.md:68` still describes `--retry-sleep` and "three retries one second apart"; the `extract_retry_after` doc comment says HTTP dates are ignored.
3. `single.rs`: no `WaitingRateLimit` progress event before the Retry-After sleep that precedes the check-limit poll; emit it before sleeping (as `send_with_retry` does with `report_backoff`).

Suggestions, take them:
4. `backoff_wait`: clamp the computed wait to `policy.max_retry_interval` so a wall-clock step backwards between the two `SystemTime::now()` calls cannot inflate it.
5. `warn!` when a Retry-After exceeds `retry_max_delay` (honored as given, but visible in `--log`); document that `Retry-After: 0` means an immediate re-send.
6. `poll_check_limit` sleeps after its final poll before failing; skip the sleep on the last iteration.
7. Wording in `--retries` help and usage.md: "waits are random, up to a cap that doubles from 1 s to 60 s" (full jitter can shrink a wait; "doubling" describes the cap).
8. Add tests: a 500 with Retry-After through `single.rs`; a 429 with Retry-After through `send_with_retry`; the HTTP-date form through an upload; min > max clamp.

How they were closed (same day, test-first):
- 1: `backoff_policy` clamps `max` up to `min` with a `warn!`; `UploadOpts::backoff()` (crate-visible) is the one builder used by `single.rs` and both opts-derived sites in `multipart.rs`; `default_ctx` keeps the constants. Tests: `min_above_max_does_not_panic_and_caps_at_min` (retry.rs), `backoff_is_built_from_the_opts`, `backoff_clamps_max_up_to_min` (types.rs); red was a compile failure on the missing method.
- 2: `design-philosophy.md` and the `extract_retry_after` doc comment rewritten.
- 3: `WaitingRateLimit` emitted before the Retry-After sleep in `single.rs`. Test `upload_503_retry_after_reports_waiting_before_the_sleep` (red: first event at 1.005 s; green: before 700 ms).
- 4: `backoff_wait` clips to `policy.max_retry_interval`. No deterministic test: the clock step cannot be injected; `waits_never_exceed_the_schedule` covers the normal path.
- 5: `retry::retry_after_wait` wraps `extract_retry_after`, warns above the cap, and is the single Retry-After reader for `send_with_retry` and `single.rs`. `Retry-After: 0` documented in `--retries` help and usage.md; pinned by `upload_503_retry_after_zero_retries_at_once`.
- 6: `poll_check_limit` skips the sleep after its last poll. Test `check_limit_exhaustion_does_not_sleep_after_the_last_poll` (red: 8.9 s; green: under 2 s; the red is probabilistic because the sleep was a jittered draw).
- 7: help and usage.md say "random, up to a cap that doubles from 1 s to 60 s"; `upload_help_has_no_retry_sleep_and_describes_backoff` asserts the phrase.
- 8: `upload_500_retry_honors_retry_after`, `upload_503_retry_after_http_date_is_honored` (single), `upload_file_multipart_part_429_retry_honors_retry_after` (multipart). These pin behavior already on the branch and passed on their first run.

Second review (same day, on the fix commit): all eight closed, no Important findings. Taken: the above-cap warning moved from the header read to the sleep sites (`retry::wait_before_retry`), so it cannot fire for a response that is never retried; the WaitingRateLimit test asserts the event's key (`file.txt`, which only the pre-sleep emission carries; the poll's is empty) instead of a 700 ms timing threshold; the Retry-After: 0 test uses a 500 so a dropped header would fall back to the 10 s draw and fail it; the `--retries` example in `after_long_help`. Noted, no change: the new tests add about 5 s of real sleeping to the suite; `poll_check_limit`'s own WaitingRateLimit carries an empty key and so never reaches the TUI's per-file row (pre-existing).

Noted, no change: metadata read/write now pause 0 s on a past HTTP-date Retry-After instead of the 30 s fallback (that is what the header says); `..UploadOpts::default()` in `run_bare_upload` matches `run_import`.
