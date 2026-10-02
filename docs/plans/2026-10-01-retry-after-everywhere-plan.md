# Retry-After Everywhere Implementation Plan

Follow-up to PR #31 (standard backoff and Retry-After on IA-S3 upload retries).

**Decision (Jake, 2026-10-01):** "We must always adhere to Retry-After. Everywhere anything is retried." Every retry site honors the header as given, in both forms (seconds, or an HTTP date), over the computed wait. The wait when there is no header is the standard schedule from PR #31: truncated exponential backoff with full jitter, 1 s to 60 s, from `retry-policies` (already in the tree via `reqwest-retry`).

**Audit (2026-10-01).** Honored today: tasks (rate-limit pause), metadata writes (`concurrency.rs`), metadata reads on a 429 (`read.rs` → `RateLimited`; the concurrent path pauses every worker). Not honored, fixed here:

1. **Transport middleware** (`client.rs:225-256`, `retry.rs` strategies). `reqwest-retry` 0.9.1 (the newest release) computes the wait in `RetryPolicy::should_retry(start_time, n_past_retries)`, which never sees the response, and `RetryableStrategy::handle` sees the response but can only answer retry or not. A 5xx retried by this middleware therefore cannot follow the header by construction of the library's trait boundary.
2. **Search** (`search.rs`, all six backends): a 429 is a plain `IaError::Http` and ends the stream. No retry, no wait, header never read. (A 5xx is retried by the middleware, so it gets fixed by 1.)
3. **Download per-file loop** (`download/mod.rs`): `fetch_response` → `http_error_from` builds `IaError::Http { status, message }` and drops the headers, so the loop cannot read Retry-After even on a 429 it does retry. The wait is `2^attempt` seconds capped at 60, no jitter, not the library.
4. **AI client** (`ai/client.rs::send_request`): a fixed table `[1, 2, 4, 8, 16]` seconds indexed by attempt; headers never read.
5. **Self-updater** (`update.rs`): the same library middleware as 1, 1 s to 30 s, for GitHub's API. "Everywhere" includes it.

Not applicable: the download stream re-request after a body error (no response to read; the Range re-request's own failure comes back through the per-file loop, which is fixed here), metadata-read decode retries.

**Architecture.**

- **Shared helpers move to `crate::retry`:** `backoff_policy`, `backoff_wait`, `retry_after_wait`, `wait_before_retry` (today in `upload::retry`), plus `pub(crate) const STANDARD_MIN_DELAY = 1 s`, `STANDARD_MAX_DELAY = 60 s`. `upload::retry` calls them through `crate::retry`. `extract_retry_after` stays where it is.
- **`RetryMiddleware`** (new, `crate::retry`): a `reqwest_middleware::Middleware` with the same loop as the library's `RetryTransientMiddleware` (clone the request, run it, classify with a `RetryableStrategy`, sleep, repeat up to the policy's budget), except the wait is `wait_before_retry(retry_after_from_response, policy, n)`. It takes any `RetryableStrategy`, so `LoggingRetryStrategy` and `ConnectOnlyRetryStrategy` are unchanged and the 429 rule is unchanged: the strategies return `None` for a 429, which keeps surfacing to the application layer where the shared `RateLimiter` pauses every worker. `client.rs` and `update.rs` build it instead of the library's middleware. The error on a failed final attempt is returned as is (nothing in the tree matches on `reqwest_retry::RetryError`).
- **`IaError::Http` gains `retry_after: Option<u64>`.** The only way for a loop that retries on an error object to honor the header is for the error to carry it (see [[retry-after-everywhere]]). `IaError::retry_after(&self) -> Option<u64>` reads it from `Http` and `RateLimited`. `to_json_error` adds `retry_after` to `http_error` when present. Every construction site sets it (`None` where no response is at hand; the header's value where one is). Public API change; version bump stays with the release process.
- **Download:** `http_error_from` fills `retry_after` from the response. The per-file loop waits `wait_before_retry(e.retry_after(), &policy, attempt - 1)` with the standard policy built once per file for `--retries`. `--retries` keeps its meaning (attempts after the first, and the stall budget).
- **Search:** each backend's page request goes through one helper, `send_page`, which retries a 429 up to 3 times (the transport's own budget for a 5xx; per page request) waiting `wait_before_retry(retry_after, policy, n)`; on the last failure it returns `IaError::RateLimited { retry_after }` the way metadata reads do (0 when the header is absent). `reqwest_middleware::RequestBuilder::try_clone` rebuilds the request per attempt. The three `*_num_found` probes use the same helper.
- **AI client:** the delay table becomes the standard policy with 5 retries; `retry_after_wait` is read from a 429 or 5xx response before the body is consumed.
- **Self-updater:** `RetryMiddleware` with its existing bounds (1 s to 30 s, 3 retries).

**Help and docs.** `ia download --help`: `--retries` says how the wait is chosen and that Retry-After is honored; `long_about` mentions it in the retry sentence; an example. `ia search --help` (the three backends' `long_about`): one sentence on 429 handling. `ia ai --help`: one sentence on LLM request retries. `docs/usage.md`: download `--retries` row and the retry paragraph, a sentence in the search section, a sentence in the ai section. `docs/design-philosophy.md` "Retry with backoff" bullet: one schedule, Retry-After everywhere, the middleware is the crate's own and why.

---

### Task 1: shared helpers and the error field

**Files:** `ia-core/src/retry.rs`, `ia-core/src/upload/retry.rs`, `ia-core/src/upload/single.rs`, `ia-core/src/upload/multipart.rs`, `ia-core/src/upload/types.rs`, `ia-core/src/error.rs`, every `IaError::Http {` construction site (62 across `tasks.rs`, `error.rs`, `scandata.rs`, `search.rs`, `download/zip.rs`, `download/mod.rs`, `ai/ia_config.rs`, `metadata/write.rs`, `metadata/schema.rs`, `metadata/read.rs`), the full destructurings (`tasks.rs:862, 1135`, `download/mod.rs:2679, 2716`).

- [x] **Step 1: Failing tests.** `crate::retry` unit tests move with the helpers (the four from `upload::retry`). `error.rs`: `IaError::Http { retry_after: Some(7), .. }.retry_after() == Some(7)`; `RateLimited { retry_after: 3 }.retry_after() == Some(3)`; `Http { retry_after: None }` → `None`; `to_json_error` on an `Http` with `retry_after: Some(7)` has `"retry_after": 7` and without it has no such key.
- [x] **Step 2: Run**; compile failure on the new field and method.
- [x] **Step 3: Implement.** The 62 construction sites are mechanical: a `sonnet` subagent adds `retry_after: None` everywhere, then the sites that have a response in hand are revisited by hand in Tasks 3 and 4. Commit: `refactor(retry): shared backoff helpers; IaError::Http carries Retry-After`.

### Task 2: the middleware

**Files:** `ia-core/src/retry.rs`, `ia-core/src/client.rs`, `ia-core/src/update.rs`, `ia-core/tests/retry.rs` (the existing middleware tests)

- [x] **Step 1: Failing tests.** Through `IaClient::from_config` against wiremock, a GET on `client.http()`: (a) 503 with `Retry-After: 1` once then 200 → Ok, two requests, at least 1 s elapsed; (b) 503 with an HTTP date 3 s ahead (computed right before the request) then 200 → at least 1 s; (c) 500 without a header, four times → the error surfaces after 4 requests (3 retries); (d) 429 → surfaces at once, one request (the strategy's rule is unchanged); (e) `client.api_no_retry()` on a 503 with `Retry-After: 1` → surfaces at once, one request (connect-only strategy). `RetryStats` counters still count the retries.
- [x] **Step 2: Run**; all three new tests failed: the seconds form waited 959 ms (the backoff's draw), the date form 117 ms, and `Retry-After: 0` took 3 s of backoff. (d) was already pinned by `request_429_passes_through_without_retry`; (e) is not reachable from an integration test (`api_no_retry` is crate-private) and the connect-only strategy is unchanged and unit-tested.
- [x] **Step 3: Implement.** `RetryMiddleware<S: RetryableStrategy>` with `new(policy, strategy)`; `client.rs` and `update.rs` switch to it; `reqwest_retry::RetryTransientMiddleware` is no longer imported anywhere. Commit: `fix(client): the transport retry middleware honors Retry-After`.

Observations from Task 2, no change: (1) `RetryStats::requests_total` counts outer calls, not attempts, because `TimingMiddleware` wraps the retry middleware; its doc comment says "including retried ones". Pre-existing; for #17 or a later fix. (2) A request whose body cannot be cloned is sent once by `RetryMiddleware` instead of failing with the library's "not cloneable" error; ia-core has no `anyhow` to build that error, and one attempt is the right behavior for a streaming body anyway.

### Task 3: download

**Files:** `ia-core/src/download/mod.rs` (its `tests` module holds the per-file tests), `ia-cli/src/commands/download.rs`, `ia-cli/tests/cli.rs`

- [x] **Step 1: Failing tests.** (a) the file GET answers 429 with `Retry-After: 3` once then 200 → file downloaded, at least 3 s; (b) 503 with `Retry-After: 3` once → same (3 s, not 1 s: the old fixed first wait was 2 s, so a 1 s header could not tell old from new); (c) 503 without a header once, `--retries 1` → file downloaded and the wait was under 2 s (the jittered first retry is at most 1 s; before the fix it was exactly 2 s); (d) `IaError::Http` from `http_error_from` carries the header (unit); (e) `ia download --help` mentions `Retry-After` and the wait rule.
- [x] **Step 2: Run**; all five failed (a, b: 2.02 s waits; c: 2.02 s; d: `retry_after: None`; e: wording).
- [x] **Step 3: Implement.** The redirect and body-read `Http` errors in `download/mod.rs` and `download/zip.rs` keep `retry_after: None`: no response header applies to them. Commit: `fix(download): per-file retries back off on the standard schedule and honor Retry-After`.

### Task 4: search

**Files:** `ia-core/src/search.rs` (its `tests` module), `ia-cli/src/commands/search.rs`, `docs/usage.md`

- [x] **Step 1: Failing tests.** For scrape (and one each for advanced and fts): (a) first page 429 with `Retry-After: 1` then 200 → full results, two requests, at least 1 s; (b) 429 four times (`Retry-After: 0`, so the budget is what is measured) → the stream yields `IaError::RateLimited { retry_after: 0 }`; (c) `num_found` on a 429 then 200 → the count; (d) `ia search <backend> --help` mentions 429 and Retry-After for all three backends.
- [x] **Step 2: Run**; all six failed (the 429 surfaced as `Http { status: 429, retry_after: None }` at once).
- [x] **Step 3: Implement.** `send_page` is the one sender for all six request sites; `http_error` builds the non-success error with the header. Commit: `fix(search): retry a 429 with Retry-After instead of failing the stream`.

### Task 5: AI client and updater

**Files:** `ia-core/src/ai/client.rs`, its tests, `ia-core/src/update.rs`, `ia-cli/src/commands/ai.rs`

- [ ] **Step 1: Failing tests.** AI: 429 with `Retry-After: 1` then 200 → response, at least 1 s; 500 without a header then 200 → response, under 2 s (the table's first wait was exactly 1 s, so this one may pass already; recorded). Updater: covered by Task 2's middleware tests; one test through `update.rs`'s client builder if a seam exists, otherwise recorded as covered by construction.
- [ ] **Step 2: Run**; fail.
- [ ] **Step 3: Implement.** Commit: `fix(ai,update): LLM and release requests honor Retry-After`.

### Task 6: docs

- [ ] `docs/usage.md`, `docs/design-philosophy.md`, `--help` text per the Architecture section. Commit: `docs: Retry-After is honored everywhere anything is retried`.

### Task 7: verification and review

- [ ] `just ci`; code-reviewer pass (default model); fix or record findings; PR against `main`; squash-merge after checks pass; `scripts/ia-cleanup retry-after-everywhere` only after a confirmed merge.
