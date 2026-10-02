# Docs Audit Implementation Plan (#17)

**Issue:** #17. An agent that read the docs and `--help` concluded that `ia download` "resumes at file granularity, so an interrupted 7 GB download restarts from zero". The docs made that a fair reading: every mention of "resume" described the joblog, and byte-level resume (`.part` + `Range`) appeared nowhere. Agents learn the tool from `--help` and the docs, so both must describe what download and upload actually do, including partial-file behavior. Done last, after the download and upload fixes landed (this run: #31 to #35 and PR #32), so the docs describe the final behavior.

**What this run already fixed on the way** (each PR documented its own behavior in `--help` and `docs/usage.md`): download Retries, Partial files, Slow and stalled downloads, Checksum mismatches (#11, #12, #14, #32); upload Retries, Resuming a multipart upload, Verifying a multipart upload, Multipart part failures, cleanup (#31, #18, #19, #20, #24); `ia search` and `ia ai` retry sentences (#32). What remains is the cross-cutting audit below.

**Findings (2026-10-02, at main after PR #35 and the #24 branch):**

1. **Hidden long help on eleven commands.** A doc comment on a `Commands::X` variant in `ia-cli/src/lib.rs` becomes that command's `about` and clears the `long_about` set on its argument struct, so `ia <cmd> --help` shows one line where paragraphs were written. Fixed for `download` (#11), `ai` and `update` (#32). Still hidden: `collection`, `list`, `metadata`, `search`, `status`, `tasks`, `upload`, `verify`, `completions`, `man`, `config`. For `upload` that means none of the retry, resume and verification text written in this run renders in `ia upload --help` beyond the flag docs and the examples.
2. **README command table** (`README.md:50-51`): `download` says "resume" without saying what resumes (part of a file, not just whole files); `upload` says "multipart for large files, automatic resume", which reads as automatic multipart and as resume of parts, while multipart is opt-in and "automatic resume" there means the joblog.
3. **Global help** (`ia-cli/src/lib.rs:78-83`): `--joblog` says "(enables auto-resume)" as if the joblog were the only resume mechanism; `--no-resume` says "upload all files fresh" on every command, including download.
4. **`docs/usage.md` "Resuming Uploads"** (line 664) covers only the joblog, with no pointer to multipart part resume two sections above; the download section has no single place that says what a rerun does after an interruption (the facts are spread over Retries, Partial files, Stalled, Checksum mismatches).
5. **`docs/design-philosophy.md`** "Byte-range resume" bullet is accurate; nothing to change there. `docs/plans/README.md` is an index; nothing to change.

**Architecture.**

- **Help rendering:** for each of the eleven variants, move the one-line doc comment onto the struct as `about = "..."` (keeping the exact text, so `ia --help` is unchanged) and replace the variant's doc comment with the same two-line comment `Download` carries. A CLI test per command asserts that a phrase from its `long_about` renders in `ia <cmd> --help`, so the defect cannot return silently. Mechanical: a cheaper model does the moves; the tests are written first.
- **Global help:** `--joblog`: "Write operation results to a JSONL log file; a rerun with the same --joblog skips the files it lists as done (download resumes partial files from their .part regardless, see `ia download --help`)". `--no-resume`: "Ignore the joblog's record of finished files and process every file again".
- **README:** `download`: "Concurrent downloads that resume partial files from where they stopped, md5 verification, glob/format filters, multi-disk pool, ZIP-member extraction, TUI dashboard"; `upload`: "Single file or batch from a spreadsheet; opt-in multipart for large files that resumes from the parts already on IA and verifies the assembled file; a joblog skips files already done".
- **usage.md:** download gains a short "After an interruption" paragraph at the top of its behavior sections that says, in order: a killed or dropped download leaves `<name>.part`, the rerun continues it from the bytes on disk with a `Range` request, the finished file is checked by size (and md5 with `--checksum`), and `--joblog` on top of that skips files already finished. "Resuming Uploads" becomes "Resuming uploads" with a first paragraph that distinguishes the two mechanisms: a single PUT restarts from zero, `--multipart` resumes from the parts IA holds after checking them (link to the section above), and the joblog skips files already finished on either path.
- **The fresh-agent test** from the issue: give a model only `ia --help`, `ia download --help`, `ia upload --help` and the README, and ask "If a 7 GB download or upload is interrupted, what happens on rerun?" Run once before the changes (expected wrong) and once after (expected right) through a `haiku` subagent with the rendered text pasted in, and record both answers in this plan. Not a CI test; a one-time check.

---

### Task 1: long help renders for every command

- [x] **Step 1: Failing tests.** `ia-cli/tests/cli.rs`: one test, a table of `(command, phrase)` with a phrase unique to each struct's `long_about`, asserting `ia <command> --help` contains it; also that `ia --help` still shows each one-liner.
- [x] **Step 2: Run**; failed on the first command checked (collection).
- [x] **Step 3: Implement** (mechanical, a `sonnet` model moved the eleven one-liners onto the structs as `about` with the exact text; `ia --help` unchanged, verified). Committed together with Task 2's code as `fix(cli): every command's long help renders; the resume flags say what resumes`.

### Task 2: global help, README, usage.md

- [x] **Step 1: Failing tests.** `ia --help`, `ia download --help` and `ia upload --help` contain the new `--joblog`/`--no-resume` wording and not "upload all files fresh".
- [x] **Step 2: Run**; fail.
- [x] **Step 3: Implement**; the code in the Task 1 commit; README and usage.md (the download "After an interruption" paragraph, the "Resuming uploads" opening, the global options table, the Job logging section) in `docs: say what resumes, where, for download and upload`.

### The fresh-agent check

**Before** (2026-10-02, a `haiku` model given only `ia --help`, `ia download --help`, `ia upload --help` and the README as rendered at main after PR #36; question: "If a 7 GB download or a 7 GB upload is interrupted, what happens on rerun? Does it start over or continue? Does it differ for download versus upload, for `--multipart`, for `--joblog`?"):

> For download: when rerun, a dropped connection resumes from bytes on disk using a Range request, and the partial file is kept as `.part` for later runs. For upload without `--multipart`: the file does not explicitly state what happens on interruption; it only describes the resume behavior with `--multipart`, which resumes from parts already on IA if every part matches by size and md5, otherwise starts fresh and leaves the stale upload for cleanup. Both download and upload support `--joblog`, which "enables auto-resume"; the file does not explain what auto-resume means without `--joblog`. The file does not distinguish between how download and upload handle interruption except where noted above.

Download is right already (the help written in #11 to #14 carries it; the issue's original wrong answer predates them). The two gaps left are the ones this audit closes: a plain upload's behavior after an interruption, and what the joblog's "auto-resume" means. The `--multipart` text the agent quotes correctly was hidden in `ia upload --help`'s `long_about` and reached it only through the flag's own doc comment.

**After, first pass** (same model, same question, the help and README as of the Task 1 and Task 2 commits):

> Download: when interrupted and rerun, `ia download` resumes a partial `.part` file from where it stopped, regardless of whether `--joblog` is used or what files the joblog lists as done. Upload without `--multipart`: the file does not explicitly state what happens when a single-file upload is interrupted and the command is rerun. Upload with `--multipart`: when rerun, it resumes from parts already on IA, but only if every part on IA matches the local file by size and md5; otherwise a fresh upload starts and the stale upload is left for cleanup. `--joblog` for both: a rerun skips files the log lists as done, the same way for download and upload (unless `--no-resume` overrides it), independently of the resumption mechanisms above.

Three of four right; the remaining gap was real: usage.md said what a plain upload does after an interruption, but `ia upload --help` did not, and the check gives the agent only the help and the README. `ia upload --help`'s `long_about` gained a paragraph: one PUT per file unless `--multipart`; an interrupted single-PUT file is sent again from byte 0 on the rerun, except a file the item already lists with the same md5, which is skipped; a `--multipart` file resumes from the parts on IA after checking them; `--joblog` skips finished files on top of either. Test: `upload_help_says_what_an_interrupted_single_put_does`.

**After, second pass** (same model, same question, with the upload long_about paragraph):

> For download: "Each file streams to <name>.part and is renamed into place once its byte count matches the item metadata. A dropped connection is resumed with a Range request from the bytes on disk." With `--joblog`, a rerun "skips the files it lists as done (download resumes a partial file from its .part regardless)." For upload without `--multipart`: "a single-PUT file is sent again from byte 0 on the rerun, except that a file the item already lists with the same md5 is skipped." For upload with `--multipart`: "A rerun resumes only when every part IA holds matches the local file by size and md5; otherwise a fresh upload starts and the stale one is left for cleanup." Summary: download always resumes from .part; upload without --multipart restarts from zero unless the file exists remotely with matching md5; upload with --multipart resumes if all parts match, otherwise restarts. With `--joblog`, files logged as done are skipped independently of the above mechanics.

Every part right except one sentence the code review then caught: the joblog rule it echoed for download ("files logged as done are skipped") was upload's rule; download's joblog skips fully finished items. The check had validated the docs against themselves.

**After, third pass** (same model, with the corrected joblog wording, and the question extended to ask what exactly the joblog skips for each):

> Download without `--joblog` resumes from the bytes on disk via Range request to the `.part` file; with `--joblog`, fully finished items are skipped, though a partial file still resumes from `.part` regardless. Upload with a single PUT (without `--multipart`) starts from byte 0 on rerun, except when the item already lists the file with matching md5 (which is skipped); with `--joblog`, files the previous run finished are additionally skipped. Upload with `--multipart` resumes from parts already on IA, but only if every part matches the local file by size and md5; otherwise a fresh upload starts. The file does not state the combined behavior of `--joblog` with `--multipart` upload explicitly.

Every part right, including the per-item rule for download. (The combined case it calls unstated is covered by "--joblog adds a layer on top of either" in the upload long help; a reader could miss it, and it is a note, not a gap.) The issue's acceptance criterion ("the fresh-agent test gives correct answers for both download and upload") is met.

### Review findings (2026-10-02), closed before the PR

Important, fixed: (1) the new `--joblog` wording said a rerun "skips the files it lists as done", which is upload's rule; download's joblog resume is per item (`joblog::items_fully_downloaded`: an item with any failed file is entered again in full, and its finished files are then skipped by the local size and mtime check). The help, the four usage.md places and the test say "finished files for upload, fully finished items for download". The recorded "after" answer repeated the wrong sentence, so the check had validated the docs against themselves; it is rerun below. (2) The `config` row of the long-help test used "Log in to archive.org", which also appears in the `login` subcommand's one-liner that `ia config --help` lists even when the long help is hidden; the phrase is now "view configuration, validate credentials", which only the long help has.

Suggestions, taken: `--no-resume`'s help names the commands that read it (download, upload, ai); the `--joblog` doc comment has a blank line after its first sentence so `-h` shows a short form; usage.md's "Resuming uploads" no longer says uploads "resume from where they left off" right after the paragraph that says a single PUT does not. From the whole-run audit: the symlink `.part` rule (#25) is now in `ia download --help` and in usage.md; the dropped-connection re-request's fixed waits are in usage.md; usage.md's `--delete-after-upload` row carries the multipart caveat the help has; the two `--checksums` rows name the real flag `--checksum-file`; design-philosophy's byte-range bullet says the size is checked always and the md5 with `--checksum`.

For Jake (not blocking): `metadata export` and `tasks submit` auto-resume from the joblog but do not read `--no-resume`; a code gap for a follow-up issue. Noted, no change: the check_limit poll and which S3 codes retry are in usage.md but not in the `--retries` help; the control calls' fixed three retries are stated only in design-philosophy.

### Task 3: the fresh-agent check, verification, review

- [x] Before/after answers recorded here. [ ] `just ci`; code-reviewer pass; fix or record; PR; squash-merge after checks pass; `scripts/ia-cleanup docs-audit` only after a confirmed merge.
