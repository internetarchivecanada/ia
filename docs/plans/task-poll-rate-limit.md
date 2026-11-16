# Rate limiting while polling a task (issue #160)

## Problem

The transport middleware does not retry a `429`; it leaves it to the code that
made the request. `tasks::get_tasks`, `tasks::list_tasks`,
`tasks::get_task_log` and `tasks::get_rate_limit` turn a `429` into
`IaError::Http { status: 429, .. }`, not `IaError::RateLimited`, unlike
`submit_task`, `rerun_task` and the metadata reads. `wait_for_task` and the
`ia tasks submit --wait` loop pass every poll error up, so one throttled poll
ends the wait after the task was already queued, and its `Retry-After` is
never honoured.

## Change

1. One helper in `ia-core/src/tasks.rs` turns a failed response into the
   error: `429` becomes `RateLimited { retry_after }` (0 when the header is
   absent, as elsewhere), anything else `Http`. All six Tasks API calls use it.
2. `wait_for_task`: a `RateLimited` poll is not an error. It sleeps the
   `Retry-After` when the server gave one, otherwise the current poll interval
   (which then doubles; a wait the server set leaves the interval as it
   was), and polls again. The timeout still bounds the wait: when the
   `Retry-After` reaches past it, the wait ends with `TaskTimeout` at once,
   since no poll is allowed before then. There is no separate attempt budget:
   the wait is a poll with its own timeout, not a retry of a failed request.
3. `ia tasks submit --wait` polls through `wait_for_task_with_progress`
   (since issue #159's change), so it gets the same handling. The progress
   callback takes a `TaskWaitProgress` event, `Active(entry)` or
   `RateLimited { wait }`, and the spinner shows
   `rate limited, polling again in Ns`. The function is not in a release yet,
   so its signature can still change.
4. Without a timeout, a server that keeps answering `429` keeps the wait
   going, as a task that stays queued does.

## Tests

- `get_tasks_429_is_rate_limited`, `wait_for_task_survives_429_on_poll` from
  the issue, and the same `429` check for `list_tasks`, `get_task_log` and
  `get_rate_limit`.
- A `wait_for_task` test with a timeout shorter than the `Retry-After`, ending
  in `TaskTimeout` without sleeping out the `Retry-After`.
- ia-cli: `ia tasks submit --wait` whose first poll gets `429` with
  `Retry-After: 1` completes and exits 0.
