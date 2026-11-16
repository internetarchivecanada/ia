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
   (which then doubles, as after any poll), and polls again. The timeout check
   applies to it as to any other poll, so a timeout still bounds a wait that
   is throttled throughout. There is no separate attempt budget: the wait is a
   poll with its own timeout, not a retry of a failed request.
3. `ia tasks submit --wait` does the same in its poll loop and shows
   `rate limited, polling again in Ns` on the spinner. (Issue #159's pull
   request replaces that loop with the library function; whichever lands
   second drops the loop's copy.)

## Tests

- `get_tasks_429_is_rate_limited`, `wait_for_task_survives_429_on_poll` from
  the issue, and the same `429` check for `list_tasks`, `get_task_log` and
  `get_rate_limit`.
- A `wait_for_task` test with a timeout shorter than the `Retry-After`, ending
  in `TaskTimeout`.
- ia-cli: `ia tasks submit --wait` whose first poll gets `429` with
  `Retry-After: 1` completes and exits 0.
