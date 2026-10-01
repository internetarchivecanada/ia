# Upload Retry Backoff Implementation Plan

Follow-up to PR #10 (one retry policy for every IA-S3 multipart request). Jake, 2026-10-01: exponential backoff is objectively better than a constant sleep; do the standard thing.

**Goal:** IA-S3 upload retries use truncated exponential backoff with jitter and honor `Retry-After`, with `--retry-sleep` as the base delay. This is what every HTTP client library does.

**History, stated plainly:** the application-level `--retry-sleep` was always a constant sleep, before and after PR #10. What #10 removed was the transport middleware's exponential backoff (1 s to 60 s over three attempts) that sat underneath the application loop on IA-S3 calls and multiplied its attempts. This plan gives the application loop the backoff itself.

**Architecture:** one pure function, `retry_delay(base, attempt, retry_after, unit)` in `ia-core/src/upload/retry.rs`, used by both sleep sites in `send_with_retry` (multipart) and by the retry loop in `single.rs`. The check-limit poll in `single.rs` keeps its plain interval: it is a poll, not a retry.

**Rule:**

- delay for retry `n` (1-based) = `min(base × 2^(n-1), 60 s)`, then full jitter: a uniform random fraction of that in `[0, 1)`.
- A `Retry-After` header on the failed response (seconds, or an HTTP date) overrides the computed delay, as given, without jitter: the server said how long.
- `--retry-sleep` is the base; default stays 30 s. Default sequence of upper bounds: 30, 60, 60, ... s; `--retry-sleep 1` gives 1, 2, 4, 8, 16, 32, 60, 60, ... s.
- The 60 s ceiling is a constant (`MAX_RETRY_DELAY`), documented, not a flag.

**Engineering calls, not Jake decisions:**

- Jitter source: a small splitmix64 step seeded from the clock, in `retry.rs`. No new crate (AGENTS.md: ask before adding one); jitter does not need a cryptographic source.
- `Retry-After` is honored as given. Libraries that honor it (urllib3, Google's clients) do not cap it; the server's number is an instruction.
- `--retry-sleep 0` keeps meaning no sleep: the base is zero, every computed delay is zero, `Retry-After` still applies.
- `UploadOpts::retry_sleep` keeps its name and type; its doc says "base" now. No public API change.

**Out of scope:** the download side's per-file retry delay (`2^attempt` capped at 60 s, already exponential, no jitter) and the stall/stream re-request delays (#11); the metadata-write `Retry-After` handling in `concurrency.rs`.

---

### Task 1: the delay function

**Files:** `ia-core/src/upload/retry.rs`

- [ ] **Step 1: Failing unit tests** for `retry_delay(base, attempt, retry_after, unit)` with `unit` the jitter fraction:
  - `unit = 1.0`, base 30 s: attempts 1, 2, 3 → 30, 60, 60 s.
  - `unit = 1.0`, base 1 s: attempts 1..=8 → 1, 2, 4, 8, 16, 32, 60, 60 s.
  - `unit = 0.5`, base 30 s, attempt 1 → 15 s; `unit = 0.0` → 0.
  - base 0 → 0 for every attempt and unit.
  - `retry_after = Some(90 s)` overrides regardless of base, attempt and unit (also when 90 > 60: the cap is for the computed delay only).
  - attempt 40 does not overflow (saturates at the cap).
  - `parse_retry_after("120")` → 120 s; an HTTP date 30 s ahead → about 30 s; a date in the past → 0; `"soon"` → `None`.
  - `jitter_unit()` returns values in `[0, 1)` across many calls and is not constant.
- [ ] **Step 2: Run**; compile failure.
- [ ] **Step 3: Implement** `MAX_RETRY_DELAY`, `retry_delay`, `parse_retry_after`, `jitter_unit`.
- [ ] **Step 4: Run**; green. Commit: `feat(upload): truncated exponential backoff with jitter for IA-S3 retries (pure function)`.

### Task 2: use it

**Files:** `ia-core/src/upload/retry.rs`, `ia-core/src/upload/single.rs`

- [ ] **Step 1: Failing tests** (wiremock, in `ia-core/tests/upload_multipart.rs` and `upload_single.rs`):
  - `retry_after_header_is_honored`: a part PUT answers 503 with `Retry-After: 1` once, then 200; `retry_sleep` 1 ms; the upload completes and takes at least 1 s.
  - `retry_delay_grows_with_the_attempt`: `retry_sleep` 200 ms, two 503s then 200; total elapsed at least 300 ms is not provable with full jitter, so instead assert through a probe: `send_with_retry` is private, so test the delay sequence indirectly by setting `retry_sleep` to 0 and checking that three attempts complete quickly (the zero base path), and rely on the unit tests for the growth. (If a cleaner seam appears while implementing, use it.)
  - `single_file_retry_honors_retry_after`: same as the first, through `upload_file`.
- [ ] **Step 2: Run**; fail.
- [ ] **Step 3: Implement.** In `send_with_retry`, read `Retry-After` before the body is consumed; both sleep sites call `retry_delay(ctx.retry_sleep, attempt, retry_after, jitter_unit())`; log the chosen delay. In `single.rs`, the non-503 retry sleep does the same with its `retries` counter.
- [ ] **Step 4: Run**; green. Commit: `fix(upload): back off exponentially between IA-S3 retries and honor Retry-After`.

### Task 3: help and docs

- [ ] `--retry-sleep` help: "Base delay between retries in seconds; doubles each retry up to 60 s, with jitter; a Retry-After header overrides it". Both `UploadArgs` copies. `docs/usage.md` row and a sentence in the upload section's retry paragraph. `UploadOpts::retry_sleep` doc.
- [ ] Commit: `docs: describe the upload retry backoff`.

### Task 4: verification and review

- [ ] `just ci`; code-reviewer pass; fix findings; PR against `main` ("Follow-up to #10"); squash-merge after checks pass; `scripts/ia-cleanup retry-sleep-backoff` only after a confirmed merge.
