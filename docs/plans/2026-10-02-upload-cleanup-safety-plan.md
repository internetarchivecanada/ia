# Upload Cleanup Safety Implementation Plan (#24)

**Issue:** #24. `ia upload cleanup ITEM FILE` aborts that file's incomplete multipart upload immediately, with no prompt, while the help reads like it lists: `[FILE]` is "Specific file to clean up", `--abort-all` says "without confirmation" as if the other path confirmed, and there is no `--dry-run`. A user who wanted to look at a 55 GB upload's state before deciding lost 17 GB of parts.

**Decision (Jake, 2026-10-01, and his comment on the issue):** listing is the default, with or without FILE. Aborting requires an explicit `--abort` (one file) or `--abort-all`. No interactive prompt (scripts and agents run this). Fix the help to say which flags abort. Add `--dry-run`. Sequenced after #18 and #19: cleanup follows the rules those two settled (a failed part leaves the upload for cleanup; a stale upload is left for cleanup), and the help fix feeds #17.

**Architecture.**

- `CleanupArgs`: `file: Option<String>` ("Only this file's uploads"), `--abort` ("Abort the listed upload(s) of FILE; requires FILE"), `--abort-all` ("Abort every incomplete upload of the item"), `--dry-run` ("Show what --abort or --abort-all would abort, abort nothing"), `--json`. `--abort` without FILE and `--abort` with `--abort-all` are clap errors (`requires`, `conflicts_with`). `--dry-run` without either abort flag is the listing (clap allows it; the output is the same listing).
- Listing shows, per upload: upload ID, key, initiated time, and the parts IA holds (count and bytes), from `list_parts` per upload. That is what the reporter wanted to see before deciding; one GET per upload. In `--json`, each entry is a CLI-side object with the core fields (`key`, `upload_id`, `initiated`) plus `parts` and `bytes` (the core `MultipartUploadInfo` type is unchanged; today the CLI serializes it directly). The listing's trailer says how to abort: `Use --abort with FILE, or --abort-all, to abort.`
- Abort (`--abort FILE` or `--abort-all`): for each target, abort and print `aborted <item>/<key> (<id>, N parts, X bytes)`; `--json` emits `{"action":"aborted", ..., "parts": N, "bytes": X}`. With `--dry-run`: `would abort ...` and `"action":"would_abort"`, no DELETE sent.
- #18's message, the usage.md "Multipart part failures" section and the two `--help` examples that name `ia upload cleanup <item> <key>` as the way to discard a kept upload change to `ia upload cleanup <item> <key> --abort`. The `--multipart` help's `'ia upload cleanup ITEM FILE'` sentence likewise. `KeptUpload::describe`'s unit tests and the usage.md quotes follow.
- `run_cleanup` keeps its shape; the branch "no file and no --abort-all → list" becomes "no abort flag → list".

**Engineering calls (flagged in the PR):** parts and bytes in the listing (one `list_parts` per upload; the issue's optional suggestion 5, taken because it is the information the reporter was after); `--dry-run` without an abort flag is accepted and lists.

---

### Task 1: listing is the default; aborting needs a flag

**Files:** `ia-cli/src/commands/upload.rs`, `ia-cli/tests/cli.rs`, `ia-core/tests/*` (none), `docs/usage.md`

- [x] **Step 1: Failing tests.** CLI tests in `ia-cli/tests/cli.rs` drive the binary against a wiremock server with `--insecure --host <addr>` and `IA_ACCESS_KEY_ID`/`IA_SECRET_ACCESS_KEY` from the environment, mounting mocks through a `tokio` runtime's `block_on` (the pattern the metadata joblog tests use); there are no cleanup tests today. The S3 mocks are `GET /<item>?uploads` (the listing), `GET /<item>/<key>?uploadId=` (parts), `DELETE /<item>/<key>?uploadId=` (abort, `expect(0)` where no abort may happen): `cleanup ITEM FILE` lists that file's uploads and sends no `DELETE`; `cleanup ITEM FILE --abort` sends the `DELETE` for that upload only; `cleanup ITEM --abort-all` deletes every upload; `cleanup ITEM --abort` exits 2 ("requires FILE"); `cleanup ITEM FILE --abort --abort-all` exits 2; `--dry-run --abort` prints `would abort` and sends nothing; `--json` shapes; the listing shows parts and bytes; `upload cleanup --help` says which flags abort and no longer says "without confirmation".
- [x] **Step 2: Run**; seven of ten failed (`--abort` and `--dry-run` unknown; FILE aborted; no parts in the listing or JSON; old help). The two clap-error tests passed for the wrong reason (an unknown flag is also exit 2) and `--abort-all` already worked; all three pin the final behavior.
- [x] **Step 3: Implement.** `CleanupArgs` gains `--abort` (`requires = "file"`, `conflicts_with = "abort_all"`) and `--dry-run`; `run_cleanup` lists parts and bytes per upload through `list_parts`, lists unless an abort flag is set, prints `aborted`/`would abort` lines and the matching JSON objects. Commit: `fix(upload): cleanup lists by default; aborting needs --abort or --abort-all; --dry-run`.

### Task 2: the kept-upload message and docs name the new invocation

**Files:** `ia-core/src/upload/multipart.rs` (+ unit tests), `ia-cli/src/commands/upload.rs` help, `docs/usage.md`, `docs/plans/2026-10-02-upload-part-failure-plan.md` (dated note)

- [x] **Step 1: Failing tests.** `KeptUpload::describe` tests end with `ia upload cleanup item f.bin --abort`; the CLI help test for the kept-upload example asserts `--abort` (the not-resuming warning has no log capture; its text changed alongside).
- [x] **Step 2: Run**; two `describe` tests and the help test failed on the missing `--abort`.
- [x] **Step 3: Implement.** The message, the warning, both `--multipart` doc comments, the help example, the three usage.md quotes, the cleanup section's table and examples, and a dated note in the #18 plan. Commit: `docs(upload): the way to discard a kept multipart upload is cleanup --abort`.

### Task 3: verification and review

- [ ] `just ci`; code-reviewer pass; fix or record findings; PR; squash-merge after checks pass; `scripts/ia-cleanup upload-cleanup-safety` only after a confirmed merge.
