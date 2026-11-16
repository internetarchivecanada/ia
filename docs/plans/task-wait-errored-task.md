# Waiting on a task that errors (issue #159)

## Problem

`tasks::wait_for_task` returns only when the task leaves the Tasks API
catalog. A task that fails stays in the catalog with color `red` until it is
rerun or cleared, so the wait polls until its timeout, or forever when the
timeout is `None`. Its doc comment says it returns for an errored task.

`ia tasks submit --wait` has its own copy of the poll loop, which does stop on
`red`, but with `std::process::exit(1)` and, under `-q`, no message at all.

## Change

1. `wait_for_task` returns `Ok(Some(entry))` when the polled catalog entry has
   color `red`. `Ok(None)` keeps its meaning: the task left the catalog. The
   return type already carries `Option<TaskEntry>`, and every poll before this
   change returned `None`, so no caller that worked before sees a different
   result for a task that finishes. No new error variant: `IaError` is matched
   exhaustively by consumers (see the example in `error.rs`), so a new variant
   would break them.
2. A new public function, `wait_for_task_with_progress`, takes a callback that
   receives each catalog entry seen while the task is still active.
   `wait_for_task` calls it with a callback that does nothing. This is
   additive.
3. `ia tasks submit --wait` drops its own loop and calls
   `wait_for_task_with_progress`, using the callback for the spinner text
   (queued, running, paused). On `Some(entry)` it prints the entry as JSON on
   stdout under `--json` (as today) and then fails with an error naming the
   task: `task <id> failed (error state); see ia tasks log <id>`. The error
   goes through the normal error path, so it is printed under `-q` too, and the
   exit code stays 1.

Paused (`brown`) tasks are not treated as finished: an administrator can
resume them.

## Tests

- ia-core: the failing test from the issue: a red catalog entry for task 42
  ends the wait with `Ok(Some(entry))` and color `red`, before the timeout.
- ia-cli: `ia tasks submit --wait` against a red catalog entry exits non-zero
  and stderr names the task.
