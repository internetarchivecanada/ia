# `ia tasks submit`: joblog, resume key and `--wait` (issues #186, #187, #190)

## Problems

All three are in `ia-cli/src/commands/tasks.rs`.

- #186: the single-item path never opens the joblog, and both batch paths
  (identifier and spreadsheet) collect every result before writing any entry.
  A rerun after an interruption, or a rerun whose resume leaves one item,
  submits again tasks that IA already accepted.
- #187: resume skips a row when its identifier has any `ok` `task-submit`
  entry, whatever the command. A failed `make_dark` row for an item whose
  `derive` row succeeded is never retried, and a joblog from a `--cmd derive`
  run makes a later `--cmd make_dark` run skip those items.
- #190: `--spreadsheet` returns to its own function before the `--wait`
  check, and that function has no wait logic, so `--wait` is ignored and the
  command exits 0 without polling. The same function also drops `-p`
  values and silently ignores a malformed `--args` value.

## What identifies a row for resume

A task is identified by the item, the command and the task arguments:

- `item`: the identifier, as today.
- `file`: the task key, the command with `.php` appended when missing (as
  `ia_core::tasks::submit_task` sends it), followed, when the task has
  arguments, by a space and the arguments as a JSON object with sorted keys.
  Examples: `derive.php`, `derive.php {"remove_derived":"*.jpg"}`.

The comment, priority, `--reduced-priority` and `-p` values are not part of
the key: they change how a task is queued, not which task it is, and a rerun
with a reworded comment must not submit the task again.

`file` is used rather than a new field so that `ia-core` is not changed: the
`JoblogEntry` struct, its format and every other command's joblog stay as
they are. `ia status --joblog` deduplicates by `(item, file)`, so it now counts
and lists each task separately (`item/make_dark.php` under "Failed files")
instead of one line per item.

A row is skipped when the joblog holds an `ok` `task-submit` entry with the
same `item` and `file`. An `ok` entry with an empty `file` was written by an
earlier version, which recorded no command; it matches every task for that
item, as it did before, so a joblog written by an older version never causes a
task to be submitted twice. Only entries written by this version can tell a
`derive` from a `make_dark`.

Any `ok` entry counts, not only the latest: a task IA accepted is never sent
again from the same joblog, even if a later `--no-resume` run recorded an
error for it.

## When a record is written

One entry per task, written as soon as that task's submission finishes (the
POST is answered, or its 429 retries run out), before the next result is
handled. This holds in all three modes: single identifier, identifier batch
and spreadsheet. The writer appends each line with one unbuffered write, so a
process killed after an entry is written leaves it in the file.

`--wait` does not change the entry: it records the submission, which is what a
rerun must not repeat. A task that is accepted and then fails while waited on
is recorded `ok` and is not submitted again by a rerun; `ia tasks rerun`
reruns it.

## `--wait` with `--spreadsheet`

The rule of the identifier modes applies: `--wait` is accepted when one task
remains to submit after resume, and refused before any request when more than
one remains. With one task, the spreadsheet mode submits it, then polls it as
the single-item mode does; a task that ends in error exits 1 with the same
message.

`-p` values apply to every spreadsheet row, and a malformed `--args` value is
an error, as in the identifier modes.

## Nothing left to submit

When resume skips every task the input names, the command prints the
`resuming:` line and exits 0, as `ia account` and `ia collection create` do. With `--wait` it exits 1
instead: no task of this run is there to poll, and exit 0 would tell a script
that the task completed.
Before this change it failed with `no identifiers provided` / `no tasks to
submit`. Empty input still fails.

## Structure

Both modes build a list of `TaskSubmission`s, then share one path: resume
filter, `--wait` check, dry run, and submission. Batch results are handled as
they arrive (`buffer_unordered` consumed with `next()`), printing and writing
the joblog per result. The single-identifier output (`Task submitted: <id>`)
and the batch output (`ok <id> (task <n>)` and the summary line) are unchanged.

## Tests (wiremock, `ia-cli/tests/tasks_submit_resume.rs`)

1. #186 single item: one identifier in an itemlist, run twice with the same
   joblog; one POST, second run exits 0.
2. #186 interrupted batch: `item-fast` answered at once, `item-slow` after
   60 s; kill the process once `item-fast` is answered; the joblog has
   `item-fast` `ok`.
3. #187: spreadsheet rows `item-a,derive` and `item-a,make_dark`, `make_dark`
   answered 500 on the first run; the rerun sends `make_dark` again and not
   `derive`.
4. #187 identifier mode: a `--cmd derive` joblog does not skip a later
   `--cmd make_dark` run.
5. An old-format entry (`file` empty) still skips the item.
6. #190: a one-row spreadsheet with `--wait` polls the task; a red task exits
   1; two rows with `--wait` are refused with no POST.
7. `-p` values reach the spreadsheet POST body.
