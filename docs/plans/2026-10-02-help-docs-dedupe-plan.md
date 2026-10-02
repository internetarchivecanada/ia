# Help and docs: one statement per rule (Part 2, PR 2)

**Source:** `docs/reviews/2026-10-02-series-26-43-review.md` §3 and §4 (local file; `docs/reviews/` is gitignored), Jake's decisions 2026-10-02. Rule applied throughout (from #43): `--help` is for what a user can act on; a rule is stated once, on the flag it belongs to; internal recovery mechanics with no knob live in usage.md and in the error text.

## Help

- `-C, --checksum` (download.rs): short help "Verify md5 checksums (without it only the size is checked)"; the long help says the skip check is what reads every local file.
- `--multipart` (both upload structs): "(for large files or unreliable connections)" replaces "(recommended for files >5 GB)"; "(one read of the file)" dropped.
- Download `long_about`: the retry paragraph goes (it is `--retries`' help); the stall sentence becomes a pointer to `--min-speed`. The `--retries` example comment stops restating the wait rule, in download and upload alike.
- `--min-speed` long help: drops "No byte is lost or fetched twice", the "N counts every stall ... in --json" sentence and "Dropped connections have their own budget"; keeps what the floor means, that a stall spends a retry, what happens when they are gone, the RATE syntax and `0`.
- Upload `after_long_help`: the "Integrity & Skip Behavior" block goes; it restated `--clobber`, `--no-verify`, `--joblog` and `--multipart`. Its one fact not on a flag (`--clobber --no-verify` computes no md5) moves onto `--clobber`'s help. The example "Resume interrupted uploads (automatic with --joblog)" becomes "Skip the files a previous run finished"; `long_about` already says what resumes.
- Global `--joblog` (lib.rs): long help cut to one sentence; usage.md "Job logging" carries the per-command list.
- Search backends (three sites): "then fails as rate_limited" becomes "then the page fails with a rate-limit error"; the cap "from 1 s to 60 s" is stated.
- Not changed: `ia update`'s 30 s cap (a behavior, not a wording).

## Docs

- usage.md: `--checksum` row; `--multipart` row is one line pointing below; `--retries` rows (download and upload) name the flag's job and point to "Retries", which keeps the full rule; "Slow and stalled downloads" loses the mechanics sentences the help lost; "Stalled uploads" is the rule and the messages, not the rationale; "Resuming uploads" states the joblog skip once and loses the pre-#37 "Resumed status" paragraph; the global `--joblog` row points to "Job logging"; the "desktop GUI" sentence under Architecture goes.
- README: "re-running the same command resumes instead of starting over" becomes "re-running the same command skips what the log records as done".
- AGENTS.md: the "separate desktop GUI project" sentence goes; the public-API sentence stays.

## Tasks

- [ ] Red: `ia-cli/tests/cli.rs` tests for the `--checksum` short help, no ">5 GB" / "Integrity & Skip" / "rate_limited" in the upload, download and search help, the download `long_about` not carrying "up to a cap that doubles" twice.
- [ ] Green: the edits above; fix any help test that pinned removed wording.
- [ ] `just ci`; code-reviewer pass (help rendered, docs read against code); PR; merge after checks; `scripts/ia-cleanup help-docs-dedupe` after a confirmed merge.
