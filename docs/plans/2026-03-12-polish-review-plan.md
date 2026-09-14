# Polish & Review Plan

**Goal:** Thoroughly review and polish the existing codebase — focusing on core functionality (upload, metadata), recent PRs (#255 tasks, #258 collection create), documentation completeness, error quality, test coverage, and public API surface.

**Architecture:** Nine tasks in two phases. Phase 1 (Tasks 1-5) is cross-cutting polish: docs, deprecation migration, error review, test gaps, and API surface. Phase 2 (Tasks 6-9) is deep code reviews of the four major modules, ordered by importance. Each task produces concrete fixes committed on a branch.

**Tech Stack:** Rust, cargo clippy, cargo test, assert_cmd, wiremock

---

## Chunk 1: Cross-Cutting Polish (Tasks 1-5)

### Task 1: Documentation Audit — `docs/usage.md`

The `ia collection create` command shipped with zero documentation. Verify all other commands are accurate.

**Files:**
- Modify: `docs/usage.md`
- Reference: `ia-cli/src/commands/collection.rs` (for collection create docs)
- Reference: `ia-cli/src/commands/tasks.rs` (verify tasks docs are accurate)
- Reference: Each `ia-cli/src/commands/*.rs` file (spot-check against docs)

- [ ] **Step 1: Add `ia collection create` section to usage.md**
  - Follow the existing format (see `ia upload` section as template)
  - Include: synopsis, description, all flags, examples
  - Get flag names and defaults from the clap definitions in `commands/collection.rs`

- [ ] **Step 2: Verify `ia tasks` documentation accuracy**
  - Compare each subcommand's documented flags against actual clap definitions
  - Check examples actually work (flags exist, output format matches)

- [ ] **Step 3: Spot-check 3-4 other command sections**
  - Pick: download, upload, metadata, search
  - Verify flags match current clap definitions (look for drift)
  - Note any discrepancies as fixes or issues

- [ ] **Step 4: Run `just ci`, commit documentation updates**
  ```
  just ci
  git add docs/usage.md
  git commit -m "docs: add collection create to usage.md, fix doc drift"
  ```

---

### Task 2: Migrate Deprecated `assert_cmd` API

27 calls to deprecated `Command::cargo_bin()` across 5 test files. Migrate to `cargo_bin_cmd!()` macro for consistency with newer test files.

**Files:**
- Modify: `ia-cli/tests/cli_schema.rs` (14 calls)
- Modify: `ia-cli/tests/config_print.rs` (8 calls)
- Modify: `ia-cli/tests/config_show.rs` (3 calls)
- Modify: `ia-cli/tests/config_login.rs` (1 call)
- Modify: `ia-cli/tests/tasks.rs` (1 call)
- Reference: `ia-cli/tests/cli.rs` (uses the new pattern)

The migration is mechanical:
```rust
// Old (deprecated):
let mut cmd = Command::cargo_bin("ia").unwrap();

// New:
let mut cmd = assert_cmd::cargo_bin_cmd!("ia");
```

- [ ] **Step 1: Migrate `cli_schema.rs`** (14 occurrences)
  - Replace all `Command::cargo_bin("ia").unwrap()` with `assert_cmd::cargo_bin_cmd!("ia")`
  - Keep `use assert_cmd::Command;` — the `cargo_bin_cmd!` macro depends on it

- [ ] **Step 2: Migrate `config_print.rs`** (8 occurrences)

- [ ] **Step 3: Migrate `config_show.rs`** (3 occurrences)

- [ ] **Step 4: Migrate `config_login.rs`** (1 occurrence)

- [ ] **Step 5: Migrate `tasks.rs`** (1 occurrence)

- [ ] **Step 6: Run `just ci` to verify**
  ```
  just ci
  ```
  Expected: all checks pass, no deprecation warnings for migrated files

- [ ] **Step 7: Commit**
  ```
  git add ia-cli/tests/
  git commit -m "chore: migrate deprecated Command::cargo_bin to cargo_bin_cmd! macro"
  ```

---

### Task 3: Systematic Error Message Review

Walk through each command's error paths and verify messages are clear, actionable, and consistent.

**Files:**
- Review: `ia-core/src/error.rs` (IaError enum — all variants)
- Review: `ia-cli/src/commands/*.rs` (each command's error handling)
- Review: `ia-core/src/tasks.rs` (new error paths)
- Review: `ia-core/src/collection.rs` (new error paths)

- [ ] **Step 1: Catalog all IaError variants**
  - Read `ia-core/src/error.rs`
  - For each variant, note: what triggers it, what message the user sees, whether it's actionable

- [ ] **Step 2: Review error display quality**
  - Check: Do error messages tell the user what went wrong AND what to do about it?
  - Check: Are HTTP errors surfaced with status code + URL + context?
  - Check: Are file I/O errors surfaced with the file path?
  - Check: Are auth errors distinguishable from other 403s?

- [ ] **Step 3: Review CLI error formatting**
  - Check: Is `anyhow` context added at the CLI layer for user-facing errors?
  - Check: Are errors styled consistently? (color, prefix, formatting)
  - Check: Does `--json` mode emit structured error JSON?

- [ ] **Step 4: Run `just ci`, fix any issues found, commit**

---

### Task 4: Integration Test Gap Analysis

Review what IS and ISN'T tested at the CLI integration level. Focus on common user error scenarios.

**Files:**
- Review: All `ia-cli/tests/*.rs` files
- Reference: `ia-cli/src/commands/*.rs` (to understand what code paths exist)

- [ ] **Step 1: Map test coverage by command**
  - For each CLI command, list what integration tests exist
  - Identify: which subcommands/flags have zero integration tests

- [ ] **Step 2: Identify missing error-path tests**
  - Common gaps: invalid args, missing required args, auth failures, network errors
  - Check: does each command test `--json` error output?

- [ ] **Step 3: Identify missing edge-case tests**
  - Empty results (0 items found, 0 tasks, etc.)
  - Very long identifiers, special characters in metadata values
  - Conflicting flags

- [ ] **Step 4: Write high-priority missing tests**
  - Pick the 5-10 most impactful gaps
  - Write tests using existing patterns (wiremock for API, assert_cmd for CLI)

- [ ] **Step 5: Run `just ci`**
  ```
  just ci
  ```

- [ ] **Step 6: Commit new tests**
  ```
  git add ia-cli/tests/ ia-core/src/
  git commit -m "test: add integration tests for error paths and edge cases"
  ```

---

### Task 5: ia-core Public API Review

Since ia-gui consumes ia-core as a library, the public API surface matters. Review for consistency, documentation, and ergonomics.

**Files:**
- Review: `ia-core/src/lib.rs` (re-exports)
- Review: `ia-core/src/tasks.rs` (40+ undocumented struct fields)
- Review: `ia-core/src/collection.rs` (well-documented — use as reference)
- Review: `ia-core/src/upload/types.rs` (another reference for good field docs)
- Review: `ia-core/src/types.rs` (core types used everywhere)

- [ ] **Step 1: Review re-exports in lib.rs**
  - Check: Are the right types re-exported for external consumers?
  - Check: Can ia-gui access everything it needs without reaching into submodules?
  - Check: Are any internal-only types accidentally public?

- [ ] **Step 2: Add field-level documentation to tasks.rs types**
  `TasksQuery`, `TasksResponse`, `TasksValue`, `TasksSummary`, `TaskEntry`, `TaskSubmission`, `TaskSubmitResponse`, `RateLimitInfo` — all have struct-level `///` but ~40 fields lack individual doc comments.
  - Use `collection.rs` field docs as the quality bar
  - Focus on fields where the name alone isn't self-explanatory

- [ ] **Step 3: Review type naming consistency**
  - Check: Do new types follow existing naming patterns?
    - `*Opts` for option bags (e.g., `DownloadOpts`, `UploadOpts`)
    - `*Result` for operation results
    - `*Query` for query parameters
  - Check: Are builder patterns used where appropriate?

- [ ] **Step 4: Review method signatures**
  - Check: Do functions take `&str` instead of `String` where possible?
  - Check: Are `Option<T>` parameters used correctly vs builder pattern?
  - Check: Is `IaClient` passed consistently (`&self` methods vs free functions)?

- [ ] **Step 5: Run `just ci`, commit documentation and API improvements**
  ```
  just ci
  git add ia-core/src/
  git commit -m "docs: add field-level documentation to tasks.rs public types"
  ```

---

## Chunk 2: Deep Code Reviews (Tasks 6-9)

### Task 6: Deep Code Review — Upload

The upload module is the largest in the codebase (4,188 lines core + 1,573 lines CLI + 560 lines tests). This is the most critical write path and must be thoroughly reviewed.

**Files:**
- Review: `ia-core/src/upload/types.rs` (485 lines) — upload option types, builders
- Review: `ia-core/src/upload/headers.rs` (194 lines) — S3 header construction
- Review: `ia-core/src/upload/validate.rs` (256 lines) — input validation
- Review: `ia-core/src/upload/single.rs` (575 lines) — single-file upload
- Review: `ia-core/src/upload/multipart.rs` (889 lines) — multipart upload + resume
- Review: `ia-core/src/upload/item.rs` (359 lines) — item-level upload orchestration
- Review: `ia-core/src/upload/batch.rs` (315 lines) — batch upload from spreadsheet
- Review: `ia-core/src/upload/template.rs` (442 lines) — template directory upload
- Review: `ia-core/src/upload/checksum.rs` (171 lines) — file checksumming
- Review: `ia-core/src/upload/check_limit.rs` (78 lines) — S3 rate limit polling
- Review: `ia-core/src/upload/s3_error.rs` (159 lines) — S3 XML error parsing
- Review: `ia-core/src/upload/progress_body.rs` (206 lines) — async progress streaming
- Review: `ia-core/src/upload/mod.rs` (59 lines) — module re-exports, shared helpers
- Review: `ia-cli/src/commands/upload.rs` (1,573 lines) — CLI layer
- Review: `ia-cli/tests/upload.rs` (560 lines) — CLI integration tests

**Review checklist:**

- [ ] **Step 1: Review types and validation** (`types.rs`, `validate.rs`, `headers.rs`)
  - Check: Are `UploadOpts` fields complete and correctly typed?
  - Check: Does validation catch all invalid states before hitting the network?
  - Check: S3 header construction — metadata encoding, `x-archive-meta` format, special chars
  - Check: Are header values properly URI-encoded for non-ASCII?

- [ ] **Step 2: Review single-file upload path** (`single.rs`, `check_limit.rs`, `s3_error.rs`)
  - Check: Content-Length always set? (IA S3 rejects chunked transfer)
  - Check: Retry logic — which errors are retried, which are terminal?
  - Check: Rate limit handling — does it poll correctly? Can it hang?
  - Check: S3 error XML parsing — does it handle all known error formats?
  - Check: Dry-run path — does it short-circuit before any network calls?

- [ ] **Step 3: Review multipart upload** (`multipart.rs`)
  - Check: Initiate → upload parts → complete flow — any gaps?
  - Check: Resume logic — list uploads → list parts → skip completed
  - Check: Abort/cleanup — does it clean up on failure?
  - Check: Per-part retry — is transient error detection (status >= 500) correct?
  - Check: Part ordering — are parts numbered correctly?

- [ ] **Step 4: Review orchestration** (`item.rs`, `batch.rs`, `template.rs`)
  - Check: Item-level — does it handle skip-existing, delete-after-upload correctly?
  - Check: Batch — spreadsheet parsing, per-item error handling, partial failure
  - Check: Template — directory traversal, key generation, metadata inheritance
  - Check: Concurrency — are jobs dispatched correctly? Race conditions?

- [ ] **Step 5: Review CLI layer** (`ia-cli/src/commands/upload.rs`)
  - Check: All flags documented and functional
  - Check: Progress display — does it handle edge cases (0 files, 1 file, many files)?
  - Check: `--json` output consistency
  - Check: Error messages from upload failures — clear and actionable?

- [ ] **Step 6: Review tests** (`ia-cli/tests/upload.rs` + inline tests in core)
  - Check: Are wiremock mocks realistic? (correct S3 response format, headers)
  - Check: Error paths tested? (auth failure, rate limit, S3 errors, network timeout)
  - Check: Multipart-specific tests — resume, abort, part failure

- [ ] **Step 7: File issues / fix trivial problems**
  - Trivial: typos, formatting, missing derives
  - Non-trivial: file as GitHub issues

- [ ] **Step 8: Run `just ci`, commit**
  ```
  just ci
  git add ia-core/src/upload/ ia-cli/src/commands/upload.rs ia-cli/tests/upload.rs
  git commit -m "review: upload module — fix issues from deep code review"
  ```

---

### Task 7: Deep Code Review — Metadata

The metadata module handles the core read/write/modify operations (1,800 lines core + 2,142 lines CLI + ~30 tests in cli.rs). This is the other critical write path.

**Files:**
- Review: `ia-core/src/metadata/read.rs` (133 lines) — GET metadata
- Review: `ia-core/src/metadata/write.rs` (1,430 lines) — modify POST, compute_patch
- Review: `ia-core/src/metadata/schema.rs` (226 lines) — schema lookup
- Review: `ia-core/src/metadata/mod.rs` (11 lines) — module re-exports
- Review: `ia-cli/src/commands/metadata.rs` (2,142 lines) — CLI layer (export, modify, append, append-list, insert, remove, import, schema)
- Review: `ia-cli/tests/cli.rs` (865 lines — contains ~30 metadata test functions)

**Review checklist:**

- [ ] **Step 1: Review metadata read path** (`read.rs`)
  - Check: Does it handle missing/empty metadata gracefully?
  - Check: Response parsing — ItemMetadata type correctness
  - Check: Auth headers — are they sent when needed?

- [ ] **Step 2: Review metadata write path** (`write.rs`)
  - Check: `compute_patch()` — does it correctly diff old vs new metadata?
  - Check: Patch operations — append, append-list, insert, remove all correct?
  - Check: Form encoding — `-target`, `-patch` format matches IA API expectations?
  - Check: Priority/access/secret headers — correctly passed?
  - Check: Are concurrent modifications handled? (optimistic locking?)
  - Check: Edge cases — empty values, removing last value, Unicode in keys/values

- [ ] **Step 3: Review schema lookup** (`schema.rs`)
  - Check: Does it handle unknown fields gracefully?
  - Check: Response parsing robustness

- [ ] **Step 4: Review CLI layer** (`ia-cli/src/commands/metadata.rs`)
  - Check: 8 subcommands — are they all consistent in style?
  - Check: `--json` output for each subcommand
  - Check: `key:value` parsing — edge cases (colons in values, empty values, whitespace)
  - Check: Import from spreadsheet — does it handle all formats?
  - Check: Help text quality across all subcommands

- [ ] **Step 5: Review tests** (`ia-cli/tests/cli.rs` metadata functions)
  - Check: Coverage of all 8 subcommands
  - Check: Error paths (invalid metadata, write failures, auth errors)
  - Check: Patch computation edge cases

- [ ] **Step 6: File issues / fix trivial problems**

- [ ] **Step 7: Run `just ci`, commit**
  ```
  just ci
  git add ia-core/src/metadata/ ia-cli/src/commands/metadata.rs ia-cli/tests/cli.rs
  git commit -m "review: metadata module — fix issues from deep code review"
  ```

---

### Task 8: Deep Code Review — `ia tasks` (PR #255)

The tasks command is the largest recent addition (1,669 lines core + 1,131 lines CLI + 1,186 lines tests). Review for correctness, consistency with existing patterns, and edge cases.

**Files:**
- Review: `ia-core/src/tasks.rs` (1,669 lines)
- Review: `ia-cli/src/commands/tasks.rs` (1,131 lines)
- Review: `ia-cli/tests/tasks.rs` (1,186 lines)
- Reference: `ia-core/src/search.rs` (pattern reference for API client functions)
- Reference: `ia-cli/src/commands/search.rs` (pattern reference for CLI subcommands)

**Review checklist:**

- [ ] **Step 1: Review core API types** (`ia-core/src/tasks.rs:1-175`)
  - Check: Are struct field types correct? (e.g., should any `String` be `Option<String>`?)
  - Check: Are `Serialize`/`Deserialize` derives correct for API roundtripping?
  - Check: Do `Default` impls make sense?
  - Check: Are there any public types that should be private?

- [ ] **Step 2: Review core API functions** (`ia-core/src/tasks.rs:176-580`)
  - Check: Error handling — are all HTTP errors mapped to meaningful `IaError` variants?
  - Check: Query parameter construction — are optional params correctly omitted when `None`?
  - Check: Response parsing — do we handle unexpected/malformed responses gracefully?
  - Check: Does `wait_for_task()` have a timeout or could it hang forever?

- [ ] **Step 3: Review core tests** (`ia-core/src/tasks.rs:580+`)
  - Check: Are wiremock matchers specific enough? (e.g., do they check query params?)
  - Check: Are error cases tested? (e.g., 404, 500, malformed JSON, empty responses)
  - Check: Are edge cases covered? (empty task lists, very large responses, missing fields)

- [ ] **Step 4: Review CLI command structure** (`ia-cli/src/commands/tasks.rs`)
  - Check: clap arg definitions — are help strings clear? Do short flags conflict with globals?
  - Check: `-h` (terse) vs `--help` (long + examples) — both present and correct?
  - Check: `--json` output — is it consistent with other commands' JSON format?
  - Check: Table output — column widths, truncation, alignment
  - Check: Error display — do CLI errors show useful context?

- [ ] **Step 5: Review CLI integration tests** (`ia-cli/tests/tasks.rs`)
  - Check: Do tests cover all subcommands (list, submit, log, rerun, rate-limit)?
  - Check: Are --json outputs validated structurally?
  - Check: Are error cases tested from the CLI level?

- [ ] **Step 6: File any issues found, fix trivial ones inline**
  - Trivial: typos, formatting, missing derives
  - Non-trivial: file as GitHub issues for separate work

- [ ] **Step 7: Run `just ci`, then commit fixes**
  ```
  just ci
  git add ia-core/src/tasks.rs ia-cli/src/commands/tasks.rs ia-cli/tests/tasks.rs
  git commit -m "review: ia tasks — fix issues from code review"
  ```

---

### Task 9: Deep Code Review — `ia collection create` (PR #258)

Smaller feature (567 lines core + 169 lines CLI + 263 lines tests). Review with same rigor.

**Files:**
- Review: `ia-core/src/collection.rs` (567 lines)
- Review: `ia-cli/src/commands/collection.rs` (169 lines)
- Review: `ia-cli/tests/collection.rs` (263 lines)

**Review checklist:**

- [ ] **Step 1: Review core implementation** (`ia-core/src/collection.rs`)
  - Check: `create_collection()` — does it handle all metadata correctly?
  - Check: Image upload path — content type inference, error handling
  - Check: Are S3 headers constructed correctly? (compare with upload module patterns)
  - Check: Error variants — are they specific enough for callers to act on?

- [ ] **Step 2: Review CLI command** (`ia-cli/src/commands/collection.rs`)
  - Check: Arg parsing — does `--metadata key:value` parsing match other commands?
  - Check: Help text quality
  - Check: Output format — success/error messages

- [ ] **Step 3: Review tests** (`ia-cli/tests/collection.rs`)
  - Check: Wiremock mock fidelity — does it match real IA S3 behavior?
  - Check: Edge cases — invalid identifiers, duplicate metadata, missing required fields
  - Check: Error path tests

- [ ] **Step 4: Run `just ci`, file issues / fix trivial problems, commit**
  ```
  just ci
  git add ia-core/src/collection.rs ia-cli/src/commands/collection.rs ia-cli/tests/collection.rs
  git commit -m "review: ia collection create — fix issues from code review"
  ```

---

## Execution Notes

**Resource budget:** Nine tasks sized for about half a week of agent time. Phase 1 (Tasks 1-5) is cheaper and mechanical. Phase 2 (Tasks 6-9) is token-intensive — deep reads of large files. Prioritization if budget runs tight:

**Phase 1 — do all of these first (cheap, high ROI):**
1. **Task 1** (usage.md) — missing docs are user-facing
2. **Task 2** (assert_cmd migration) — mechanical, low risk
3. **Task 3** (error review) — improves user experience
4. **Task 4** (test gaps) — improves confidence
5. **Task 5** (API docs) — important for ia-gui consumers

**Phase 2 — deep reviews, ordered by criticality:**
6. **Task 6** (upload review) — largest module, critical write path
7. **Task 7** (metadata review) — second-largest, other critical write path
8. **Task 8** (tasks review) — largest recent PR
9. **Task 9** (collection review) — smallest, already well-documented

**Branch strategy:** All fixes go on a single `polish/review-and-cleanup` branch, one commit per task. This keeps the review work grouped and easy to merge.

**What NOT to do:**
- Don't refactor working code that isn't broken
- Don't add features or change behavior
- Don't reorganize file structure
- If a review surfaces a significant issue, file a GitHub issue rather than fixing inline
