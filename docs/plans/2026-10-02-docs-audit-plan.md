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

- [ ] **Step 1: Failing tests.** `ia-cli/tests/cli.rs`: one test, a table of `(command, phrase)` with a phrase unique to each struct's `long_about`, asserting `ia <command> --help` contains it; also that `ia --help` still shows each one-liner.
- [ ] **Step 2: Run**; fail for the eleven.
- [ ] **Step 3: Implement** (mechanical, cheaper model). Commit: `fix(cli): every command's long help renders`.

### Task 2: global help, README, usage.md

- [ ] **Step 1: Failing tests.** `ia --help` contains the new `--joblog`/`--no-resume` wording and not "upload all files fresh"; `ia download --help` likewise (global options render there).
- [ ] **Step 2: Run**; fail.
- [ ] **Step 3: Implement**; README and usage.md edits with it. Commit: `docs: say what resumes, where, for download and upload`.

### Task 3: the fresh-agent check, verification, review

- [ ] Before/after answers recorded here; `just ci`; code-reviewer pass; fix or record; PR; squash-merge after checks pass; `scripts/ia-cleanup docs-audit` only after a confirmed merge.
