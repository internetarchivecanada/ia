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
   adaptive by default": only `ia metadata` adapts its concurrency to the
   server (ia-core `AdaptiveLimiter`, used from ia-cli/src/commands/
   metadata.rs alone). Every other command uses a fixed pool of 8 when
   `--jobs` is omitted (ia-cli/src/lib.rs, `cli.jobs.unwrap_or(8)`). All
   four now say so.
3. `--delete-after-upload` help and row: "after verified upload". The CLI
   refuses the flag with `--no-verify` (ia-cli/src/commands/upload.rs, both
   paths), and since PR #44 the deletion follows IA's acceptance of the
   upload with its md5 (Content-MD5 on a single PUT and on every part;
   the completion manifest's md5s). The text now says what is checked and
   that `--no-verify` is refused.

## Missing

4. The joblog skip's own status. A file a rerun skips on the joblog's
   record prints "(resumed, already uploaded)", is `"status": "resumed"` in
   `--json`, has its own count in the summary line, and is not written to
   the joblog again. No doc said so. One sentence in "Resuming uploads".

## Stated more than once

5. The retry-wait rule (random wait up to a cap doubling from 1 s to 60 s,
   `Retry-After` taken as given, `Retry-After: 0` at once) was written out
   in full under `ia download`, `ia upload` and `ia ai`, and in short under
   `ia search` and `ia update`. It is one rule for the whole tool
   (`crate::retry`), so it is stated once, in a "Retry waits" section under
   Advanced features; each command's section keeps what is its own (what
   is retried, the budget, the `--json` key) and points there. `ia update`'s
   30 s cap stays where it is, as the one exception.
6. The `Range` resume mechanic opened "Slow and stalled downloads" in the
   same words as "After an interruption". The stalled-stream section now
   says a slow stream is treated like a dropped one and points up.

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

Filled in after the code-reviewer pass.
