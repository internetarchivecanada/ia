# Upload Stall Detection Implementation Plan (#38)

**Issue:** #38. The upload transport has no read timeout, on purpose: the read-timeout clock is not reset by request-body writes, so a server that is legitimately silent while a large body uploads would abort the send. The consequence is that a part PUT, or the single-file PUT, to a server that stops reading hangs for as long as the kernel keeps the socket alive. No retry fires, because the request never fails. The upload counterpart of #11.

**Decision (Jake, 2026-10-02: "start #38").** Mirror download's `--min-speed`: while a body is being sent, its average rate over the last 60 s (or over the send's age while younger than that) is compared with the floor once a second, after a 30 s grace; below it the request is abandoned and retried under `--retries`; the same flag, defaults, wording and `0 disables` rule as download.

**What is judged, and what is not.** The detector judges the body send: bytes handed to the transport per second. Once the last chunk of the body has been handed over, judging stops; the wait for the server's response is not a send and is not judged (a part PUT's response legitimately arrives seconds after the body, while IA hashes it). A server that reads the whole body and never answers is therefore still unbounded. That is a response timeout, a different mechanism, and out of this issue's scope; it is recorded here so it is not mistaken for an oversight.

**Architecture.**

- **`crate::stall`** (moved from `download::stall`, tests included): `StallDetector`, `policy()`, `CHECK_INTERVAL`, the fixed `WINDOW`/`GRACE`, the `cfg(test)` `PolicyOverride`. The module doc speaks of streams, not downloads. `download::mod` imports it from the crate root.
- **`UploadOpts.min_speed: u64`**, default 10 KiB/s, 0 disables. Public API field, as `DownloadOpts.min_speed` is.
- **`upload::stall_watch`** (new module): `BodyWatch` holds an `Arc<Mutex<Option<StallDetector>>>` and a `done` flag; `BodyWatch::wrap(stream)` returns a `WatchedBody<S>` stream that records each chunk's length into the detector as it is pulled and sets `done` when the inner stream ends. `watch_send(watch, future)` runs the request future under `tokio::select!` against a `CHECK_INTERVAL` ticker; a tick asks the detector; a stall drops the future, which closes the connection, and returns `SendEnd::Stalled { observed, window_secs }`; the future's own result is `SendEnd::Done(result)`. A `min_speed` of 0 makes `watch_send` just await the future.
- **Part PUTs** (`upload_part_with_retry`): the `Bytes` body becomes a stream of 64 KiB slices (cheap: `Bytes::slice`) wrapped by the attempt's `BodyWatch`, sent with `Body::wrap_stream` and the explicit `Content-Length` already set. `send_with_retry`'s closure receives the attempt's `&BodyWatch` (`FnMut(&BodyWatch) -> Fut`); the five closures that send no body ignore it. `S3RetryCtx` gains `min_speed`. In `send_with_retry` a stall is retried like a transport failure while `attempt <= retries`, with no backoff before the re-send (download does the same for a stall: the problem is the peer, not load), and reported as `UploadProgressStatus::Retrying`; past the budget it returns `S3Failure { error: UploadStalled, code: None, attempts }`.
- **Single-file PUT** (`single.rs`): the body is always a `ProgressBody` stream now (with a no-op progress callback when the caller gave none), wrapped by a `BodyWatch`, sent through `watch_send`. A stall counts against the same `retries` budget as every other failure in that loop, with no wait before the re-send; past the budget the file fails with `UploadStalled`.
- **`IaError::UploadStalled { identifier, key, observed_bytes_per_sec, min_bytes_per_sec, window_secs, stalls }`**, displayed as `upload of {identifier}/{key} stalled {stalls} time(s): {observed} B/s over the last {window_secs} s is below the --min-speed floor of {min} B/s`, the download wording with "upload" and the key. Not retryable (the budget is spent). JSON code `upload_stalled`. Size: two `String`s, three `u64`, one `usize`, under the 104-byte `IaError` and the 128-byte `Result` line. `stalls` counts every stall in the attempt series, so it is one more than `--retries` when every attempt stalled (as for download).
- **A stalled part** goes through `KeptUpload::describe` like a spent budget: `part N of M stalled (X B/s over the last W s is below the --min-speed floor of Y B/s, after K attempts): multipart upload <id> is kept ... rerun ... or discard it with: ia upload cleanup <item> <key> --abort`. `describe` matches `UploadStalled` alongside `UploadFailed`; everything else still passes through.
- **CLI:** `--min-speed` on both upload argument structs with download's parser (`parse_rate` moves to a shared `commands::rate` module) and a help text mirroring download's, in the upload vocabulary; the `--retries` help says a stall spends one; `after_long_help` gains an example. `docs/usage.md`: a `--min-speed` row and a "Slow and stalled uploads" section under upload, next to Retries.

**Tests.** The detector's unit tests move with it. The stall watch is unit-tested in `ia-core` with the `cfg(test)` `PolicyOverride` (window 2 s, grace 1 s) against a raw `tokio::net::TcpListener` that accepts, reads the first kilobyte and then stops reading without closing; the body is 16 MiB so it cannot fit in the loopback socket buffers and the send really stops: (a) `send_with_retry` with a watched 16 MiB part body and `retries: 1` returns `UploadStalled` with `attempts == 2` after two connections; (b) the same through `upload_file` (single PUT) returns `UploadStalled` with `stalls == 2`; (c) a part whose first attempt stalls and whose second attempt reaches a normal server (the listener hands off to a wiremock-style responder after one stall is not possible with one host, so: the listener answers `200` on its second connection after reading the full body) completes with `attempts == 2`; (d) `min_speed: 0` sends a watched body through wiremock unchanged (no detector built); (e) a stalled part through `upload_file_multipart` is kept on IA (`DELETE expect(0)` cannot be asserted on a raw listener; assert the error wording names the upload ID and the cleanup command). Every existing upload test runs with the watch on and its tiny bodies complete at once, so none can stall. CLI: `--help` wording, `parse_rate` shared.

---

### Task 1: the detector is shared

- [ ] `git mv ia-core/src/download/stall.rs ia-core/src/stall.rs`; `mod stall;` at the crate root; download imports `crate::stall`; module doc generalized. Tests move with it; `cargo test -p ia-core --lib stall` and the download stall tests stay green. Commit: `refactor(stall): the stall detector is a crate module`.

### Task 2: the error, the option, the shared parser

- [ ] **Step 1: Failing tests.** `error.rs`: display of `UploadStalled` (singular and plural), `is_retryable() == false`, JSON code `upload_stalled`, `size_of::<IaError>()` unchanged. `types.rs`: `UploadOpts::default().min_speed == 10 * 1024`. `ia-cli`: `parse_rate` tests move to the shared module; `upload --help` and `upload --spreadsheet --help` show `--min-speed`.
- [ ] **Step 2: Run**; fail.
- [ ] **Step 3: Implement.** Commit: `feat(upload): --min-speed option, UploadStalled error, shared rate parser`.

### Task 3: the watch, the part PUT, the single PUT

- [ ] **Step 1: Failing tests** (a) to (e) above, in `ia-core/src/upload/stall_watch.rs` and the `upload/retry.rs`/`single.rs` test modules.
- [ ] **Step 2: Run**; the stall tests hang today (that is the defect); run them with a timeout and record the hang as the red.
- [ ] **Step 3: Implement.** Commit: `fix(upload): abandon and retry a body send that stalls below --min-speed`.

### Task 4: a stalled part is a kept upload

- [ ] **Step 1: Failing test.** `KeptUpload::describe` on an `UploadStalled` failure produces the kept-upload message with the stall detail.
- [ ] **Step 2: Run**; fail (passes through today).
- [ ] **Step 3: Implement.** Commit: `fix(upload): a stalled part leaves the multipart upload on IA like any failed part`.

### Task 5: help and docs

- [ ] `--min-speed` help on both structs (mirroring download's), `--retries` help, an example, `docs/usage.md` row and "Slow and stalled uploads" section; CLI tests assert the wording. Commit: `docs(upload): --min-speed`.

### Task 6: verification and review

- [ ] `just ci`; code-reviewer pass; fix or record findings; PR; squash-merge after checks pass; `scripts/ia-cleanup upload-stall-detection` only after a confirmed merge.
