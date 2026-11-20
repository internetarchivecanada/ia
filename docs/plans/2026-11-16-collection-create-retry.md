# Retry the collection create PUT (#152)

## Problem

`ia collection create` without `--image` sends one zero-byte PUT to IA-S3
(`create_without_image` in `ia-core/src/collection.rs`) over `raw_http`, a
client with no retry layer, and has no retry loop of its own. A `503
SlowDown`, a `429`, a connection reset or a timeout fails the create at
once. In a bulk run (`--spreadsheet`) that row fails and is retried only by
rerunning with `--joblog`. With `--image`, the same create goes through
`upload_file` and is retried under the upload policy.

## Change

Send the zero-byte PUT through `upload::retry::send_with_retry`, the policy
every IA-S3 control request already uses, over `upload_http` as those
requests do. No second policy is written. That gives the create:

- S3 `<Code>` classification (`should_retry_s3`): `SlowDown`, `429` and
  transient 5xx are retried; `AccessDenied` and other permanent codes are
  not.
- `Retry-After` honoured on every retried response.
- Transport failures (connect, timeout, reset) retried.
- IA's plain-text spam 503 treated as permanent.
- The per-attempt send and answer watch (#60).

The budget is the one the image path gets from `UploadOpts::default()`
(10 retries, 1 s to 60 s backoff with jitter), so both create paths retry
the same way. No new flag and no public API change: `create_collection`
keeps its signature.

The PUT is idempotent (same item, same headers), so replaying it after a
lost answer is safe; it is the same exposure the image path already takes.

A row that still fails returns `IaError::UploadFailed` as before (now with
"after N attempts" when more than one was made), so the CLI reports it and
writes it to the job log as today.

## Tests (written first, against wiremock)

- `503 SlowDown` then `200`: succeeds, two requests.
- `429` with `Retry-After: 1` then `200`: succeeds, and at least one second
  passed.
- `403 AccessDenied`: fails after one request (existing test, plus a request
  count).
- In the CLI bulk path: a throttled row followed by success is created, and
  the run goes on to the next row.

## Files

`ia-core/src/collection.rs` and, if needed, `ia-cli/src/commands/collection.rs`
and their tests.
