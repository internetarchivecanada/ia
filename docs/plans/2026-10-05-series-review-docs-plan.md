# Docs and help fixes from the series review

Date: 2026-10-05. Branch `docs/series-review-docs`. The independent review of
PRs #26–#49 (main at 179b503, version 0.21.0) checked every `--help` page
and docs/usage.md and README.md against the code. What it found, and what
changes here.

## Wrong against the code

1. docs/usage.md, "Partial files and the size check": the 416 arm that
   removes the `.part` is said to have `--json` code `resume_failed`. The
   per-file JSONL path (ia-cli/src/commands/download.rs, `json_file_result`)
   gives every per-file failure the code `download_failed` with the error
   text as the message, as the three neighbouring paragraphs say. The
   sentence now says the same. (Routing per-file failures through
   `IaError::to_json_error` would need `DownloadStatus::Failed` to carry
   the error, not its text; a separate change, not made here.)
2. `-j, --jobs` help ("omit for adaptive concurrency"), the `--jobs 0`
   error, the global options row in usage.md and README's "Concurrency is
   adaptive by default": only `ia metadata export` adapts its concurrency
   to the server (ia-core `AdaptiveLimiter::new(10, 2, 200)`, built in
   `run_export` alone; the reviewer caught my first wording, which said
   `ia metadata`). Metadata writes use a fixed 2, audit a fixed 10, and
   every other command a fixed 8 when `--jobs` is omitted (ia-cli/src/
   lib.rs, `cli.jobs.unwrap_or(8)`). All four now say so, and the export
   section describes the ramp (start at 10, halve on a 429 to no less
   than 2, grow by one per success to 200), which no doc did.
3. `--delete-after-upload` help and row: "after verified upload". The CLI
   refuses the flag with `--no-verify` (ia-cli/src/commands/upload.rs, both
   paths), and since PR #44 the deletion follows IA's acceptance of the
   upload with its md5 (Content-MD5 on a single PUT and on every part;
   the completion manifest's md5s). The text now says what is checked and
   that `--no-verify` is refused. The library leaves the gate to the
   caller: `UploadOpts.delete_after_upload` deletes on acceptance with
   `verify` off too; its doc comment now says so.

## Missing

4. The joblog skip's own status. A file a rerun skips on the joblog's
   record prints "(resumed, already uploaded)", is `"status": "resumed"` in
   `--json`, has its own count in the summary line, and is not written to
   the joblog again. No doc said so. One sentence in "Resuming uploads".

## Stated more than once

5. The retry-wait rule (random wait up to a cap doubling from 1 s to 60 s,
   `Retry-After` taken as given, `Retry-After: 0` at once) was written out
   in full under `ia download`, `ia upload` and `ia ai`, and in short under
   `ia search` and `ia update`. It is one rule for the retries that answer
   a failed request (`crate::retry`), so it is stated once, in a "Retry
   waits" section under Advanced features; each command's section keeps
   what is its own (what is retried, the budget, the `--json` key) and
   points there. The section also names what is not on the schedule (the
   reviewer caught "every retry in ia"): the in-stream re-request after a
   dropped download connection (0.5/1.5/4.5 s), the stall re-request and
   the dead-send re-send (at once), `ia tasks`' 2 s doubling, and `ia
   metadata`'s pause for the `Retry-After`.
6. The `Range` resume mechanic opened "Slow and stalled downloads" in the
   same words as "After an interruption". The stalled-stream section now
   says a slow stream is treated like a dropped one and points at the
   re-request rule at its own end, where the in-run re-request is
   described (the reviewer caught a pointer to the rerun's resume instead,
   and a repeat within the paragraph).

## Dated record

7. docs/plans/2026-03-05-upload-design.md's retry table lists `400
   BadDigest` as not retryable. PR #49 made it retryable. A dated note under
   the table, as the other superseded design rows have.

## Not changed, recorded

- `--joblog` / `--no-resume` render on every command's help page because
  they are clap global flags; the sentence is one line since PR #45.
  Making them per-command is a CLI structure change, not a docs one.
- "Stalled uploads" keeps its one explanatory clause about why the wait
  for IA's answer is not a stall; it tells the user what the rule covers.
- `ia update`'s 30 s cap (PR #45 recorded it as a behavior, not a
  wording).

## Tests

`every_command_renders_its_long_help` and the help-text tests in
ia-cli/tests/cli.rs run unchanged; the whole file is run, not a filter
(lesson from PR #45). usage.md and README.md have no tests.

## Review record

Code-reviewer pass 2026-10-05. Two Important, both in new text, both
fixed: the `--jobs` wording said `ia metadata` adapts (only `metadata
export` does; writes 2, audit 10) and nothing described the ramp; "every
retry in ia waits the same way" was contradicted by five retry sites, one
in the same file. Minor, fixed: the stalled-downloads paragraph repeated
itself and pointed at the rerun's resume; the ia-core field doc still said
"after verified upload" while the library deletes with `verify` off.
Verified by the reviewer, no change: the `download_failed` claim on every
per-file path; the md5 statement for both upload paths; the "resumed"
sentence against console, JSON (no `detail` key), summary and joblog; each
shortened command paragraph; the help pages' agreement with the docs.
Nit, left: download's `--retries` help omits "even above 60 s" where
upload's includes it; both behave the same.
