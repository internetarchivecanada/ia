# Download Size Check Implementation Plan

Closes #12: a truncated transfer is renamed into place as complete when `--checksum` is off.

**Goal:** `ia download` never renames a `.part` file into place when the number of bytes it received differs from the size in the item's metadata, and it refuses to write at all when the server's own idea of the file size (the `Content-Range` total on a 206) disagrees with the metadata.

**Architecture:** Two checks inside `download_file` in `ia-core/src/download/mod.rs`, each backed by a new `IaError` variant. The post-stream count check keeps `.part` and returns a retryable error, so the existing outer retry loop in `download_item_with_metadata` resumes with `Range`. The `Content-Range` check runs on every response `fetch_response` returns and fails permanently, because retrying cannot make the server and the metadata agree. Both skip `{identifier}_files.xml`, whose size in metadata is unknowable because the file records itself.

**Tech Stack:** Existing crates only. `wiremock` for every HTTP test.

**Decisions (Jake, 2026-09-28):**

- Byte count differs from metadata size after the stream ends: retryable error, `.part` kept. Applies whether or not the file has an md5.
- `Content-Range` total differs from metadata size on a 206: permanent error, `.part` kept, message names both sizes.
- The post-stream check is exact. A response up to 10% over the metadata size used to be accepted; it is now a mismatch like any other. The mid-stream too-large abort remains as an early exit for grossly oversized bodies.
- Exempt only `{identifier}_files.xml`. Files with no `size` in metadata are unaffected.
- No new flags. Help text and README unchanged. `docs/usage.md` gains a short paragraph.

**Out of scope:** checking the `Content-Range` start offset against the resume position; anything from #13 (md5 by default) or #14 (keeping the bad file on md5 mismatch).

---

## File Structure

| Action | Path | Responsibility |
|--------|------|----------------|
| Modify | `ia-core/src/error.rs` | Add `DownloadSizeMismatch` (retryable) and `ServerSizeMismatch` (permanent), their Display text, JSON codes, retryability |
| Modify | `ia-core/src/download/mod.rs` | `parse_content_range_total`, `is_size_unknowable`, `check_content_range`; the post-stream count check; tests |
| Modify | `docs/usage.md` | Paragraph in the download section on the size check and what a leftover `.part` means |

---

### Task 1: Error variants

**Files:** `ia-core/src/error.rs`

- [ ] **Step 1: Failing tests.** In the `tests` module of `error.rs`:
  - `download_size_mismatch_is_retryable`
  - `server_size_mismatch_is_not_retryable`
  - `json_download_size_mismatch` asserts code `download_size_mismatch` and `file`, `expected`, `received` in the JSON.
  - `json_server_size_mismatch` asserts code `server_size_mismatch` and `file`, `metadata_size`, `server_size`.
  - Display tests for both messages.
- [ ] **Step 2: Run** `cargo test -p ia-core size_mismatch` and confirm compile failure.
- [ ] **Step 3: Implement.**
  ```rust
  #[error("download size mismatch for {file}: expected {expected} bytes, received {received} bytes")]
  DownloadSizeMismatch { file: String, expected: u64, received: u64 },

  #[error("server reports {server_size} bytes for {file} but item metadata says {metadata_size} bytes")]
  ServerSizeMismatch { file: String, metadata_size: u64, server_size: u64 },
  ```
  `is_retryable`: `DownloadSizeMismatch => true`, `ServerSizeMismatch => false`. `to_json_error`: the two codes above with their fields in `extra`.
- [ ] **Step 4: Run** the tests; green. Commit: `feat(core): add size-mismatch error variants for downloads`.

### Task 2: Content-Range parsing and the exemption

**Files:** `ia-core/src/download/mod.rs`

- [ ] **Step 1: Failing unit tests** for `parse_content_range_total`:
  - `"bytes 13-21/22"` → `Some(22)`
  - `"bytes 0-0/1"` → `Some(1)`
  - `"bytes 13-21/*"` → `None`
  - `"bytes */22"` → `Some(22)`
  - `"BYTES 13-21/22"` → `Some(22)` (unit is case-insensitive)
  - `"items 1-2/3"`, `"bytes 13-21"`, `""`, `"bytes 1-2/abc"` → `None`
  
  And for `is_size_unknowable(identifier, name)`:
  - `("abc", "abc_files.xml")` → `true`
  - `("abc", "abc_meta.xml")`, `("abc", "other_files.xml")`, `("abc", "sub/abc_files.xml")` → `false`
- [ ] **Step 2: Run**; compile failure.
- [ ] **Step 3: Implement** both as small pure `fn`s next to `ensure_cnt_zero`. Doc comments explain the `_files.xml` case.
- [ ] **Step 4: Run**; green. Commit: `feat(core): parse Content-Range totals and recognise the self-describing _files.xml`.

### Task 3: The Content-Range check

**Files:** `ia-core/src/download/mod.rs`

- [ ] **Step 1: Failing wiremock tests** in the existing `tests` module, modelled on `part_file_symlink_skips_resume`:
  - `content_range_total_mismatch_fails_permanently`: `.part` of 5 bytes on disk; Range request answered 206 with `Content-Range: bytes 5-29/30` while metadata says 40. Expect `Err(IaError::ServerSizeMismatch { metadata_size: 40, server_size: 30, .. })`, `.part` still 5 bytes, no final file.
  - `content_range_star_total_is_ignored`: same but `bytes 5-29/*`; download completes.
  - `content_range_mismatch_on_files_xml_is_ignored`: file named `test-item_files.xml`; completes.
- [ ] **Step 2: Run**; fail.
- [ ] **Step 3: Implement** `check_content_range(response: &reqwest::Response, identifier: &str, file: &FileMetadata) -> Result<()>`: only acts when status is 206, `file.size` is `Some`, the name is not exempt, and the header parses to `Some(total)`. Call it right after the initial `fetch_response` (before the 200-vs-206 branch) and right after the mid-stream re-request at the `'stream_retry` loop. It runs before any byte is written.
- [ ] **Step 4: Run**; green. Commit: `fix(download): fail before writing when Content-Range disagrees with item metadata`.

### Task 4: The post-stream count check

**Files:** `ia-core/src/download/mod.rs`

- [ ] **Step 1: Failing wiremock tests:**
  - `short_body_keeps_part_and_returns_retryable_error`: metadata size 32, server sends 20 bytes with a matching Content-Length, no `-C`. Expect `Err(IaError::DownloadSizeMismatch { expected: 32, received: 20, .. })`, `.part` exists with 20 bytes, final file absent.
  - `short_body_then_rerun_resumes_and_completes`: after the failure above, a second `download_file` call; Range mock (`header_exists("Range")`) answers 206 with the last 12 bytes and `Content-Range: bytes 20-31/32`. Completes, final content equals the full 32 bytes.
  - `short_body_recovers_through_outer_retry_loop`: `download_item_with_metadata` with `retries: 1`; first response short (`up_to_n_times(1)`), Range response completes it. Result has `files_downloaded == 1`.
  - `short_body_with_checksum_keeps_part`: same as the first, with `checksum: true` and a wrong md5 irrelevant; the size error wins and `.part` survives (this is the ordering guarantee).
  - `file_without_size_is_unaffected`: `size: None`, any body; completes.
  - `files_xml_size_mismatch_is_ignored`: `test-item_files.xml`, size 100, body 20 bytes; completes.
  - Flip `download_allows_slightly_oversized_response` into `slightly_oversized_response_is_a_size_mismatch`: 105 bytes for size 100 → `Err(DownloadSizeMismatch)`, `.part` kept.
- [ ] **Step 2: Run**; fail.
- [ ] **Step 3: Implement.** After `output.flush().await?; drop(output);` and before the md5 block:
  ```rust
  if let Some(expected) = file.size {
      if !is_size_unknowable(identifier, &file.name) && bytes_downloaded != expected {
          return Err(IaError::DownloadSizeMismatch { file: file.name.clone(), expected, received: bytes_downloaded });
      }
  }
  ```
  `.part` is deliberately left on disk. Update the comment on the too-large abort to say it is now only an early exit.
- [ ] **Step 4: Run** `cargo test -p ia-core -p ia-cli`; green. Commit: `fix(download): keep .part and fail when the byte count differs from item metadata`.

### Task 5: Docs

**Files:** `docs/usage.md`

- [ ] **Step 1:** In the download section, add a short paragraph: a file is renamed into place only when the byte count matches the size in the item's metadata; on a mismatch the `.part` file is kept and the next attempt resumes it with a `Range` request; if the server reports a different total size than the metadata, the download stops with `server_size_mismatch`; `{identifier}_files.xml` is exempt because it records its own size.
- [ ] **Step 2:** Commit: `docs: describe the download size check and leftover .part files`.

### Task 6: Verification and review

- [ ] `cargo fmt --all -- --check`, `cargo clippy -p ia-core -p ia-cli -- -D warnings`, `cargo test -p ia-core -p ia-cli`, `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p ia-core -p ia-cli`.
- [ ] Code-reviewer pass on the branch; fix findings; re-run.
- [ ] PR against `main` with `Closes #12`.

---

## Follow-up: two size dead ends (2026-10-01)

Part of #12. PR #23 left two cases where a `.part` file can never be resumed yet nothing removes it, so every attempt fails the same way until someone deletes the file by hand.

**Decisions (Jake, 2026-10-01):**

1. A body that runs past the metadata size but stays within the 10% mid-stream tolerance is a permanent failure. The post-stream count check returns `ServerSizeMismatch` (not the retryable `DownloadSizeMismatch`) when `bytes_downloaded > expected`, and the `.part` file is removed. A `.part` longer than the file is not a prefix of anything; resuming from it can only produce another 416. The mid-stream too-large abort for grossly oversized bodies is unchanged.
2. A 416 on a resume `Range` request maps to a size error using the 416's `Content-Range: bytes */total`. The server is saying the `.part` offset is at or past the end of its copy of the file.
   - `total` differs from the metadata size: `ServerSizeMismatch` naming both sizes. `.part` is kept, as the existing 206 check does: nothing from that response is written and retrying cannot make the two agree.
   - `total` equals the metadata size, or the metadata size is unknown or exempt (`_files.xml`): the `.part` already holds at least the whole file. It is removed and `DownloadSizeMismatch { expected: total, received: offset }` is returned, which is retryable, so the next attempt starts from byte 0.
   - A 416 without a parseable `bytes */total` keeps today's behavior: `IaError::Http { status: 416 }`.

**Engineering call, not a Jake decision:** the `total == metadata size` branch of (2). The literal decision would produce "server reports 32 bytes but item metadata says 32 bytes", which names no disagreement. Restarting from zero is the one outcome that is correct for every way a `.part` can reach the file's full length (a crash between the final flush and the rename, a `.part` written by an older version that accepted oversize bodies, or a file dropped there by something else).

**Where the 416 is handled:** `fetch_response` turns every non-2xx into `IaError::Http` after consuming the body, which discards `Content-Range`. It now returns a 416 response unchanged when the caller sent a `Range` header (`resume_from.is_some()`); callers that pass `None` (`scandata`, `zip`) see no change. `download_file` checks each fetched response for a 416 before `check_content_range` and returns `range_not_satisfiable_error` for it, on the initial request and on the mid-stream re-request alike. The non-success tail of `fetch_response` moves into `http_error_from` so the 416 fallback can reuse it.

### Task A: Oversize body within tolerance

- [ ] **Step 1: Failing tests** in `ia-core/src/download/mod.rs`:
  - Replace `slightly_oversized_response_is_a_size_mismatch` with `oversized_body_within_tolerance_is_permanent_and_deletes_part`: 105 bytes for size 100 → `Err(ServerSizeMismatch { metadata_size: 100, server_size: 105 })`, no `.part`, no final file.
  - `oversized_resume_body_deletes_part`: 5-byte `.part`, 206 with `Content-Range: bytes 5-29/30` (total matches metadata 30) but a 27-byte body → `ServerSizeMismatch { 30, 32 }`, `.part` gone.
  - `oversized_body_is_not_retried`: `download_item_with_metadata` with `retries: 2`, mock `.expect(1)` → `files_failed == 1`.
- [ ] **Step 2: Run**; fail (today they return `DownloadSizeMismatch` and keep `.part`).
- [ ] **Step 3: Implement** in the post-stream check: branch on `bytes_downloaded > expected`. Update the `ServerSizeMismatch` doc comment ("nothing is written" no longer holds for this path).
- [ ] **Step 4: `docs/usage.md`**: the "When the body ran long" sentence now describes the permanent failure and the deleted `.part`.
- [ ] **Step 5: Run**; green. Commit: `fix(download): fail permanently and delete .part when the body outruns the metadata size`.

### Task B: 416 on a resume Range request

- [ ] **Step 1: Failing tests:**
  - `range_not_satisfiable_with_different_total_fails_permanently`: 40-byte `.part`, metadata 32, Range mock answers 416 with `Content-Range: bytes */30` → `ServerSizeMismatch { 32, 30 }`, `.part` still 40 bytes.
  - `range_not_satisfiable_at_metadata_total_deletes_part_and_restarts`: same `.part`, 416 with `bytes */32` → `DownloadSizeMismatch { expected: 32, received: 40 }`, retryable, `.part` gone; a second call (no `Range` header, 200 with 32 bytes) completes.
  - `range_not_satisfiable_on_files_xml_deletes_part`: `test-item_files.xml`, 416 with `bytes */30`, metadata 100 → `DownloadSizeMismatch { 30, 40 }`, `.part` gone.
  - `range_not_satisfiable_without_content_range_is_an_http_error`: 416, no header → `Err(Http { status: 416, .. })`, `.part` kept.
  - `fetch_response_416_without_range_is_an_http_error`: `resume_from: None` → `Err(Http { status: 416, .. })` (the `scandata`/`zip` contract).
  - Raw-TCP `stream_retry_response_416_fails_permanently`: 32-byte promise chopped at 12, the Range re-request answered `416` with `content-range: bytes */12` → `ServerSizeMismatch { 32, 12 }`, `.part` holds the 12 bytes.
- [ ] **Step 2: Run**; fail (today every case is `Http { status: 416 }`).
- [ ] **Step 3: Implement** `range_not_satisfiable_error`, `http_error_from`, and the `fetch_response` exception described above.
- [ ] **Step 4: `docs/usage.md`**: add the 416 paragraph after the `Content-Range` one.
- [ ] **Step 5: Run**; green. Commit: `fix(download): map a 416 on a resume request to a size error instead of a dead end`.

### Task C: Verification and review

- [ ] `just ci`.
- [ ] Code-reviewer pass; fix findings; re-run.
- [ ] PR against `main` with `Part of #12`; squash-merge after checks pass; `scripts/ia-cleanup size-dead-ends` only after the merge is confirmed.
