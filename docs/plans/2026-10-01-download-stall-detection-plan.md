# Download Stall Detection Implementation Plan

Closes #11: a slow-trickling datanode can hang a download indefinitely.

**Goal:** a download whose throughput stays below a floor for a sustained window is abandoned and re-requested with `Range` from the bytes already on disk, through the body-stream retry path that already exists in `download_file`. The check runs on a timer, so a stream that sends nothing at all is caught just as a stream that drips is.

**Architecture:** a small pure `StallDetector` (new file `ia-core/src/download/stall.rs`) keeps a sliding window of bytes received per second and answers "is this stream stalled right now?". `download_file` feeds it from the chunk loop and asks it once a second from a `tokio::time::interval` branch in a `tokio::select!` alongside `stream.next()`. A stall takes the same exit as a body-stream error: flush the `.part`, drop the stream, re-request with `Range: bytes={bytes_downloaded}-`. Stalls have their own counter, capped at `opts.retries`; body-stream errors keep `MAX_STREAM_RETRIES` (3). When the stall budget is spent the file fails with a new, permanent `IaError::DownloadStalled`.

**Tech Stack:** existing crates only. `tokio::select!` and `tokio::time::interval` are already available (`macros`, `time` via `rt`). Tests: `wiremock` for HTTP, a raw `tokio::net::TcpListener` for the drip-feed body, `tokio::time::pause` (the `test-util` dev-dependency is already present) so a 60-second window runs in milliseconds.

**Decisions (Jake, 2026-10-01):**

- Stall rule: absolute throughput floor over a sliding window. Default floor 10 KiB/s averaged over the last 60 s, after a 30 s grace period at stream start.
- On a stall: flush, abandon the stream, re-request with `Range` from the bytes on disk, the same path as a body-stream error.
- Budget: stalls get their own counter capped at `--retries` (default 5); body-stream errors keep their existing cap of 3.
- Flag: `--min-speed <RATE>` (`10K`, `1M`, a plain number of bytes; `0` disables). Window and grace are fixed and documented, not flags.
- `DownloadOpts` gains a `min_speed` field (public API addition).
- The behavior is described in detail in `--help`, `docs/usage.md`, and tested thoroughly, including a raw-TCP drip-feed test.

**Engineering calls, not Jake decisions (flagged in the PR):**

- **Units.** `K` and `M` in `--min-speed` are powers of 1024, matching the `KiB`/`MiB` labels `ia` prints. The default `10K` is 10240 bytes per second.
- **Averaging span.** "The last 60 s" means the last 60 s or the whole stream when it is younger than that. At the end of the 30 s grace the average is over 30 s; from 60 s on it is over 60 s. Dividing 30 s of bytes by 60 would halve the measured rate and stall healthy streams.
- **Each stream has its own grace.** A re-request after a stall or a body-stream error is a new stream: the detector restarts, so the new connection gets its 30 s grace too.
- **No backoff before a stall re-request.** The body-stream error path sleeps 0.5 s, 1.5 s, 4.5 s; a stall has already waited at least 30 s and the server is answering, slowly. The re-request goes out at once.
- **Spent budget is permanent.** `DownloadStalled` is not retryable, so the outer per-file loop in `download_item_with_metadata` does not start over. `--retries` is the number of stall re-requests the file gets; with `--retries 0` the first stall fails it. If the error were retryable, a trickling file could take `retries × (retries + 1)` streams of 30 s or more before failing.
- **No new progress status.** A stall logs a `warn!` and the download continues; adding a `DownloadStatus::Stalled` would break exhaustive matches in the external GUI and is not needed for the fix.
- **Check interval.** One second. The `select!` branch is disabled when `min_speed` is 0, so a disabled detector costs nothing.

**Out of scope:** switching datanode on a stall (#15); per-part streams (#16); relative floors (a fraction of the best observed rate). The `READ_TIMEOUT` of 60 s on the transport stays; it catches a fully silent connection after 60 s on its own, which arrives as a body-stream error and takes that path and that budget.

---

## File Structure

| Action | Path | Responsibility |
|--------|------|----------------|
| Create | `ia-core/src/download/stall.rs` | `StallDetector`: per-second buckets over a 60 s window, 30 s grace, `record` and `check`; the fixed `WINDOW`, `GRACE` constants; unit tests |
| Modify | `ia-core/src/download/mod.rs` | `min_speed` on `DownloadOpts`; the `select!` chunk loop; stall counter and shared re-request tail; raw-TCP drip tests |
| Modify | `ia-core/src/error.rs` | `DownloadStalled` variant: Display, `is_retryable` false, JSON code `download_stalled` |
| Modify | `ia-cli/src/commands/download.rs` | `--min-speed` flag with `parse_rate`, long help, `long_about` paragraph, example; pass through to `DownloadOpts` |
| Modify | `ia-cli/tests/cli.rs` | help text shows the flag and the rule; `parse_rate` rejections surface as usage errors |
| Modify | `docs/usage.md` | flag table row; a "Slow and stalled downloads" subsection |

---

### Task 1: `DownloadStalled` error variant

**Files:** `ia-core/src/error.rs`

- [ ] **Step 1: Failing tests** in the `tests` module of `error.rs`:
  - `download_stalled_is_not_retryable`
  - `download_stalled_displays_details`: the message names the file, the observed rate, the floor, and the number of re-requests.
  - `json_download_stalled`: code `download_stalled`; `file`, `observed_bytes_per_sec`, `min_bytes_per_sec`, `stalls` in the JSON.
- [ ] **Step 2: Run** `cargo test -p ia-core download_stalled`; compile failure.
- [ ] **Step 3: Implement.**
  ```rust
  /// The stream stayed below the `--min-speed` floor for the whole window
  /// `stalls` times in a row, so the stall budget (`--retries`) is spent.
  /// Each stall re-requested the file with `Range`; the `.part` is kept.
  #[error("download of {file} stalled {stalls} times: {observed_bytes_per_sec} B/s over the last {window_secs} s is below the --min-speed floor of {min_bytes_per_sec} B/s")]
  DownloadStalled { file: String, observed_bytes_per_sec: u64, min_bytes_per_sec: u64, window_secs: u64, stalls: usize },
  ```
  `is_retryable` false, with a comment: the budget was the retries.
- [ ] **Step 4: Run**; green. Commit: `feat(core): add the DownloadStalled error for spent stall budgets`.

### Task 2: `StallDetector`

**Files:** `ia-core/src/download/stall.rs`, `mod stall;` in `download/mod.rs`

The detector is pure: every method takes `now: tokio::time::Instant` so unit tests pick the clock and the drip test can run under `tokio::time::pause`.

```rust
pub(crate) const WINDOW: Duration = Duration::from_secs(60);
pub(crate) const GRACE: Duration = Duration::from_secs(30);

pub(crate) struct StallDetector {
    min_bytes_per_sec: u64,
    started: Instant,
    buckets: [u64; 60],   // bytes received in each of the last 60 whole seconds
    current_second: u64,  // seconds since `started` of the bucket last written
}

impl StallDetector {
    pub(crate) fn new(min_bytes_per_sec: u64, now: Instant) -> Self;
    /// Credit `bytes` to the bucket for `now`, zeroing any seconds skipped since the last call.
    pub(crate) fn record(&mut self, now: Instant, bytes: u64);
    /// `Some(observed_bytes_per_sec)` when the stream is stalled at `now`: the grace has passed and the
    /// average over the window (or the whole stream, if shorter) is below the floor. `None` otherwise.
    pub(crate) fn check(&mut self, now: Instant) -> Option<u64>;
}
```

- [ ] **Step 1: Failing unit tests** in `stall.rs` (clock is `start + Duration`, no runtime needed):
  - `no_check_during_grace`: 0 bytes, `check` at 29 s → `None`.
  - `silent_stream_stalls_at_end_of_grace`: 0 bytes, `check` at 30 s → `Some(0)`.
  - `steady_stream_above_floor_never_stalls`: 20 KiB every second for 120 s, floor 10 KiB → `None` at every second.
  - `average_uses_stream_age_before_window_fills`: 15 KiB/s for 30 s then `check` at 30 s → `None` (450 KiB / 30 s = 15 KiB/s, not 450 KiB / 60 s = 7.5 KiB/s).
  - `burst_then_silence_stalls_once_the_burst_leaves_the_window`: 2 MiB at t=1, nothing after; `check` at 60 s → `None` (2 MiB / 60 s ≈ 34 KiB/s), at 62 s → `Some(0)`.
  - `drip_below_floor_stalls`: 1 byte every 10 s; `check` at 30 s → `Some(0)` (3 bytes / 30 s rounds to 0).
  - `fast_then_slow_stalls_when_the_window_average_drops`: 100 KiB/s for 60 s, then 1 KiB/s; stalls at the second the window average first falls below 10 KiB/s (compute the expected second in the test).
  - `gap_longer_than_window_clears_every_bucket`: 1 MiB at t=1, `record` 1 byte at t=200, `check` at 200 → `Some(0)`.
  - `observed_rate_is_reported`: 5 KiB/s for 40 s, `check` at 40 s → `Some(5120)`.
- [ ] **Step 2: Run** `cargo test -p ia-core stall::`; compile failure.
- [ ] **Step 3: Implement** with `roll_to(second)` zeroing skipped buckets (all of them when the gap is ≥ 60), `sum / span_secs` where `span_secs = min(elapsed_secs, 60).max(1)`.
- [ ] **Step 4: Run**; green. Commit: `feat(core): add a sliding-window stall detector for download streams`.

### Task 3: wire the detector into `download_file`

**Files:** `ia-core/src/download/mod.rs`

- [ ] **Step 1: Failing tests.** Raw-TCP servers under `#[tokio::test(start_paused = true)]`, modelled on `stream_error_retries_with_range_and_completes`. Every server sleeps on tokio timers, so the paused clock drives both sides and each test finishes in well under a second of wall time. `DownloadOpts { min_speed: 10 * 1024, retries: 5, .. }` unless stated.
  - `drip_feed_stalls_then_resumes_with_range`: the first connection sends headers for a 64 KiB file, then 1 byte every 5 s. At 30 s the detector fires (6 bytes / 30 s). The second connection must carry `Range: bytes=6-` and serves the remaining bytes at once. Result `Complete`, final file equals the 64 KiB body, exactly two connections.
  - `silent_stream_stalls_at_the_end_of_grace`: headers then nothing. The `select!` ticker, not a chunk, must trigger the check; the second connection (`Range: bytes=0-`) completes the file. Assert the first connection was abandoned at about 30 s of virtual time (the server records `Instant::now()` when its socket closes), well before the 60 s `READ_TIMEOUT`.
  - `stall_budget_is_separate_from_stream_error_budget`: `retries: 1`. Connection 1 drips (stall 1 of 1), connection 2 drops mid-body (stream error 1 of 3), connection 3 completes. `Complete`; three connections. Shows a stall does not consume a stream-error retry and vice versa.
  - `spent_stall_budget_fails_permanently`: `retries: 2`; every connection drips. Three connections (the first stream plus two re-requests), then `Err(DownloadStalled { stalls: 2, min_bytes_per_sec: 10240, .. })`, `is_retryable()` false, `.part` holds the bytes received so far.
  - `min_speed_zero_disables_detection`: `min_speed: 0`; the server sends headers, waits 45 s of virtual time, then the whole body. `Complete` on a single connection.
  - `grace_restarts_on_each_stream`: connection 1 drips and stalls at 30 s; connection 2 sends nothing for 25 s then the whole body. `Complete`, two connections: the second stream was not judged by the first stream's clock.
  - `stall_keeps_rolling_md5_correct`: `checksum: true` with the real md5; drip then resume; `Complete` and the final file verifies (the hasher is not reset by the re-request).
  - Through the outer loop: `download_item_with_metadata` with `retries: 1` and a server that always drips → `files_failed == 1` and exactly two connections. The permanent error is not retried from the top.
- [ ] **Step 2: Run**; fail (today every drip test hangs; give them a `tokio::time::timeout` of 10 virtual minutes so a hang is a failure, not a stuck suite).
- [ ] **Step 3: Implement.**
  - `pub min_speed: u64` on `DownloadOpts` with a doc comment stating the rule, the units, the fixed window and grace, and `0` disables. Default `10 * 1024`.
  - In `download_file`, before `'stream_retry`: `let mut stall_attempt = 0usize;`. At the top of each `'stream_retry` iteration: `let mut detector = (opts.min_speed > 0).then(|| StallDetector::new(opts.min_speed, Instant::now()));` and `let mut ticker = tokio::time::interval(Duration::from_secs(1));` with `MissedTickBehavior::Delay`.
  - The inner loop becomes a `select!`:
    ```rust
    enum StreamEnd { Done, Error(reqwest_middleware::Error), Stalled { observed: u64 } }
    let end = loop {
        tokio::select! {
            next = stream.next() => match next {
                Some(Ok(chunk)) => { write, hash, count, size guard, progress; detector.record(now, len) }
                Some(Err(e)) => break StreamEnd::Error(e.into()),
                None => break StreamEnd::Done,
            },
            _ = ticker.tick(), if detector.is_some() => {
                if let Some(observed) = detector.check(now) { break StreamEnd::Stalled { observed } }
            }
        }
    };
    ```
    `stream.next()` is cancel-safe, so a tick that loses the race drops nothing.
  - After the loop: `Done` → `break 'stream_retry`. `Error` → the existing budget check and backoff (`MAX_STREAM_RETRIES`, `Network` on exhaustion). `Stalled` → `if stall_attempt >= opts.retries { drop(output); return Err(DownloadStalled {..}) }`, else `stall_attempt += 1`, `warn!` with the observed rate, floor, attempt and budget. Both then fall into one shared re-request tail: flush, `fetch_response` with `Range`, the 416 handling, `check_content_range`, the 200-on-resume `ResumeFailed`, `response = new_resp; continue 'stream_retry`.
- [ ] **Step 4: Run** `cargo test -p ia-core -p ia-cli`; green. Commit: `fix(download): abandon and resume a stream that stays below --min-speed`.

### Task 4: the `--min-speed` flag and help text

**Files:** `ia-cli/src/commands/download.rs`, `ia-cli/tests/cli.rs`

- [ ] **Step 1: Failing tests:**
  - Unit tests for `parse_rate` in `download.rs`: `"10K"` → 10240, `"1M"` → 1048576, `"1G"` → 1073741824, `"500"` → 500, `"0"` → 0, `"10k"` → 10240 (case-insensitive), `"10KB"`, `"1.5M"`, `"abc"`, `""`, `"-1"` → `Err` whose message names the accepted forms.
  - `download_help_describes_min_speed` in `tests/cli.rs`: `ia download --help` contains `--min-speed`, `10K`, `60`, `30`, and `0 disables`.
  - `download_rejects_bad_min_speed`: `ia download x --min-speed 10KB` exits 2 with the usage error.
- [ ] **Step 2: Run**; fail.
- [ ] **Step 3: Implement.**
  ```rust
  /// Abandon and resume a stream slower than this (10K, 1M, bytes; 0 disables)
  ///
  /// <full description: the rule, window, grace, what happens on a stall,
  /// the budget and its relation to --retries, the final error, units>
  #[arg(long, value_name = "RATE", default_value = "10K", value_parser = parse_rate)]
  min_speed: u64,
  ```
  A paragraph in `long_about` on stall detection and resume; an example `ia download nasa --min-speed 0` and `ia download nasa --min-speed 1M` in `after_long_help`. Pass `min_speed: args.min_speed` in `make_opts`.
- [ ] **Step 4: Run**; green. Commit: `feat(cli): add --min-speed and describe stall detection in download --help`.

### Task 5: docs

**Files:** `docs/usage.md`

- [ ] Flag table row: `` `--min-speed <RATE>` | Abandon and resume a stream averaging below RATE over the last 60 s (default: `10K`; `0` disables) ``.
- [ ] New subsection "Slow and stalled downloads" after "Partial files and the size check": the rule (floor, window, grace, timer), what a stall does (flush, `Range` re-request, no bytes lost, the md5 keeps rolling), the budget (`--retries`, separate from the three body-stream retries), the failure (`download of <name> stalled N times: ...`, `.part` kept, code `download_stalled` in `--json`), the units, `0` to disable, and the relation to the 60 s read timeout.
- [ ] Commit: `docs: describe --min-speed and stall detection`.

### Task 6: verification and review

- [ ] `just ci`.
- [ ] Code-reviewer pass on the branch; fix findings; re-run.
- [ ] PR against `main` with `Closes #11`; note the `DownloadOpts` field addition for the external GUI; squash-merge after checks pass; `scripts/ia-cleanup stall-detection` only after the merge is confirmed.
