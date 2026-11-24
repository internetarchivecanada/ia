# `ia upload --no-multipart` (#179)

## Problem

Without `--multipart`, every file is sent as a single PUT, but multipart still happens on its own:

1. A single PUT of a file larger than 2 MiB that dies on the way (dead send, no answer, transport error) with a retry left continues as a multipart upload (`falls_back` and `continue_as_multipart` in `ia-core/src/upload/single.rs`).
2. That turns on the run's `MultipartFallback` switch, and every later file larger than 2 MiB in the run is sent as multipart from the start.
3. At the start of each item, `upload_item` lists the item's unfinished multipart uploads (`GET /<item>?uploads`, `record_unfinished_uploads` in `ia-core/src/upload/item.rs`), and every listed file is sent as multipart and resumed.

Nothing turns this off. A pipeline that sends many 3 to 20 MB files per item needs every file to be one PUT, so that no unfinished multipart upload is left in an item and no extra catalog tasks are made.

## Design

### ia-core

`UploadOpts.multipart: bool` becomes `UploadOpts.multipart: Multipart`, a `#[non_exhaustive]` enum:

- `Multipart::Auto` (default): today's behaviour without `--multipart`: single PUT, fallback, run-wide switch, resume of listed files.
- `Multipart::Always`: today's `multipart = true`.
- `Multipart::Never`: every file is a single PUT, retried as a single PUT.

A three-way setting rather than a second bool, so there is no fourth state (both on) to refuse, and the planned later redesign (a size cutoff) can add a variant without another field. `#[non_exhaustive]` lets it add one without a breaking change.

`UploadOptsBuilder::multipart` takes `impl Into<Multipart>`, with `From<bool>` (`true` → `Always`, `false` → `Auto`), so builder callers passing a bool keep compiling. Direct field assignment `opts.multipart = true` does not compile any more; that is the public API change, noted in the pull request.

With `Never`:

- `upload_file` decides single PUT whatever `MultipartFallback` holds.
- `falls_back` returns false, so a dead single PUT is retried as a single PUT and, once out of retries, fails with the existing single-PUT messages (`upload of <item>/<key> stalled N times: ...`, `... failed after N attempts ...`). The run's switch is never turned on.
- `upload_item` skips the start-of-run listing of unfinished multipart uploads entirely. One request fewer per item, and no multipart request of any kind is sent in the run.

### What the user is told about an unfinished multipart upload

Nothing at run time: without the listing the run does not know of one, and the point of the flag is to send no multipart request. The file is sent as a single PUT; the unfinished upload stays on IA (IA may clean it up after 30 days or more). `docs/usage.md` and `--help` say so and point at `ia upload cleanup <item>` to list it and `--abort` to discard it.

### ia-cli

`--no-multipart` on `UploadArgs` (bare command and `--spreadsheet`) and `ImportArgs`, `conflicts_with = "multipart"` (exit 2, as `--size-hint` with `--no-size-hint`). Both are mapped to `Multipart::{Always, Never, Auto}` in the two places the options are built.

### Docs

`docs/usage.md`: flag table row, "Falling back to multipart" (the sentence "Nothing turns the fallback off" goes), "Resuming a multipart upload", the spreadsheet option list. `ia upload --help` long text and the `--multipart` help. README row.

## Tests (hermetic, written first)

Core (`ia-core/tests/upload_item.rs`):

- A file over 2 MiB whose first single PUT dies is retried as a single PUT and uploaded; a later file over 2 MiB in the same run is a single PUT; no multipart request (`?uploads`, `partNumber`, `uploadId`) and no listing is sent; the run's switch stays off.
- A single PUT over 2 MiB that dies on every attempt fails with the single-PUT error, no multipart request sent.
- A rerun on an item with an unfinished multipart upload of the file: a single PUT, no listing.

CLI (`ia-cli/tests/upload.rs`):

- `--no-multipart --multipart` is a usage error (exit 2), bare and with `--spreadsheet`.
- `--no-multipart` on `--spreadsheet` and on `import` against a mock: single PUT, no listing.

## Out of scope

The size cutoff, the 2 MiB threshold, parallel parts, `--multipart=<value>` syntax, download code.
