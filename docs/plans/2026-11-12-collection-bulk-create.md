# `ia collection create --spreadsheet`: create many collections from one file (#147)

**The gap.** `ia collection create` creates one collection per invocation. Creating many at once needs an external loop that sends one S3 request per line, with no validation before the first request and no record of what was created. This plan adds bulk input to the existing subcommand, in the shape the repository's other row-based bulk commands already have.

No request to archive.org is made while building or testing this. Every test that sends a request runs against a local wiremock server.

## Flag shape: `--spreadsheet <FILE>`

```
ia collection create IDENTIFIER --collection PARENT [OPTIONS]     # unchanged
ia collection create --spreadsheet <FILE> [OPTIONS]
```

The TODO that asked for this suggested `--from-csv` or `--from-jsonl`. The repository already has one flag for "one row per operation, read from a file": `--spreadsheet <PATH>` on `ia upload`, `ia metadata` and `ia tasks submit`, all backed by `ia_core::spreadsheet::read_spreadsheet`, which picks the format by extension (`.csv`, `.tsv`, `.xlsx`, `.ods`, `.jsonl`/`.ndjson`). Using it gives CSV and JSONL with one flag, TSV and spreadsheets at no cost, and the same header rules users already know from those commands (header names case-insensitive, a blank cell is no value, a leading byte-order mark ignored). Two format-named flags would be two names for one reader. `--itemlist` (as on `ia account`) is a list of plain values, one per line, not rows with columns, so it does not fit.

**Standard input.** Not read. The `--spreadsheet` commands take a path and choose the parser by its extension; standard input has no extension, and sniffing the format would be a rule no other `--spreadsheet` command has. The input this replaces is a file on disk.

**Clap.** `IDENTIFIER` is required unless `--spreadsheet` is given and conflicts with it. `--collection` is required unless `--spreadsheet` is given.

## Columns

| Column | Meaning |
|---|---|
| `identifier` | required; the new collection's identifier |
| `collection` | the parent collection; required on every row unless `--collection` is given |
| `image` | optional path to a cover image, relative to the current directory as the `file` column of `ia upload --spreadsheet` is |
| any other column (`title`, `description`, `subject`, …) | metadata, as `-m KEY:VALUE` would give it |

The same defaults a single `ia collection create` applies: `mediatype=collection` always (a `mediatype` column is dropped, as `-m mediatype:…` is), derive disabled, the image stored as `<identifier>_itemimage.<ext>`. `identifier` and `image` are never sent as metadata.

Multiple values use the indexed columns `ia upload --spreadsheet` reads (`subject[0]`, `subject[1]`, …), through the same function (`upload::batch::row_metadata`, made `pub(crate)`), so the same column means the same thing in both commands. A JSONL value that is not a string is sent as its JSON text, as the shared reader does for every command.

## Command-line values and row values

Every per-collection flag of the single form is kept and becomes a default for every row: `--collection`, `--title`, `--description`, `--subject`, `--image`, `-m`. A field a row fills in replaces every command-line value for that field, for that row only; fields the row leaves blank or does not have keep the command-line values. This is the rule `ia upload --spreadsheet` applies between `-m` and a row (`ItemGroup::item_opts`, #97), so `--collection parent` gives a shared parent, and a row's own `collection` cell overrides it. `--image` with a row's `image` cell: the cell wins.

## Validation before the first request

Every row is checked before any request, and every problem is reported together, as `upload::batch::validate_groups` does:

- the identifier is valid (`validate_identifier`);
- no identifier is on two rows (creating one collection twice from one file is a mistake in the file);
- the row has a parent collection, from its own cell or `--collection`;
- the image, from the cell or `--image`, exists and has an extension (the checks `create_collection` already makes, moved into one function both use);
- every metadata key can be sent as an S3 header (`encode_metadata_headers`).

Failure is `IaError::Config("batch validation failed:\n  <identifier>: <problem>\n  …")`, the message shape the upload batch uses, exit 1, nothing sent; with `--json`, `{"error": {…}}` on stderr through `write_json_error`. An empty file is `spreadsheet contains no records`, as `ia tasks submit --spreadsheet` says. A malformed `-m` is the error the single form gives. Missing S3 keys are checked once, after validation and before the first request, and stop the command rather than failing each row; a dry run needs no keys, as the single form's does.

Not reported: a row whose `identifier` cell is blank. The shared reader drops such rows for every `--spreadsheet` command; changing that changes `ia upload`, `ia metadata` and `ia tasks submit`, so it is left to its own issue.

## Library API (`ia_core::collection`)

```rust
/// One collection to create, from a spreadsheet row merged with defaults.
#[non_exhaustive]
pub struct CollectionRow { pub identifier: String, pub metadata: Vec<(String, String)>, pub image: Option<PathBuf> }

/// Turn spreadsheet records into rows: merge `defaults` and `default_image`
/// as described above, validate every row, and report every problem.
pub fn collection_rows(records: Vec<SpreadsheetRecord>, defaults: &[(String, String)], default_image: Option<&Path>) -> Result<Vec<CollectionRow>>
```

Rows keep file order. Each row is then created with the existing `create_collection`, which is unchanged in behavior.

## Running

Rows run concurrently up to `--jobs`, default 8: creating a collection is an IA-S3 PUT, as an upload is, and `ia upload --spreadsheet` uses the global default. One row's failure does not stop the others. Results print in completion order.

## Output

- **Human:** `created: <url>` per row on stdout, the line the single form prints, dropped at `-q`; `error: <identifier>: <message>` on stderr, dropped at `-qq`. With more than one row and not `-qq`: `<n> created, <m> failed` on stderr.
- **`--json`:** one object per row on stdout. Success is the single form's object, `{"identifier","status","url"}` (`status` the HTTP status). Failure is `{"identifier", "error": {"code","message",…}}`, the body of `IaError::to_json_error`, as `ia account` embeds it. Both go to stdout so a consumer reads one stream with one line per row.
- **Exit status:** 1 through `std::process::exit(1)` when any row failed, else 0.

## `--dry-run`

Validates everything as above, sends nothing, needs no keys. Prints each row as the single form's dry run does (the human block, or the JSON object with `mediatype`, `metadata` and `image`), then `dry-run: would create <n> collection(s), nothing sent` on stderr unless `--json` or `-qq`. Not written to the job log.

## Job log and the retry path

`--joblog <PATH>` records one entry per row: op `collection-create`, item the identifier, file empty, `ok` with the elapsed time or `error` with the message, written before the row's result is printed. A rerun with the same job log and file skips identifiers whose latest `collection-create` entry is `ok`, and prints `resuming: skipping <n> collection(s) already created in joblog` unless `-q`; failed rows run again. Validation still covers every row, so a rerun of a corrected file is checked the same way. `--no-resume` runs every row. `ia status --joblog <PATH> --failed-items` lists the identifiers that failed. The job log applies to `--spreadsheet` only; the single form ignores it, as today.

A zero-byte create PUT (no image) is sent once; a throttled or failed row is retried by rerunning with the job log. Retrying that PUT within the run, as image uploads already are, is a separate change (filed as its own issue).

## Tests

ia-core unit tests (`collection.rs`):
- a CSV-shaped record set gives rows in file order with `identifier` and `image` removed from metadata;
- `--collection` default used when the row has none; a row's `collection` replaces it; `-m subject:x` replaced by a row's `subject`, and kept when the row has none;
- `image` cell replaces `--image`; `--image` used when the cell is absent;
- indexed `subject[0]`, `subject[1]` give two values;
- every problem reported in one error: invalid identifier, duplicate identifier, missing parent, missing image, image without extension, unencodable metadata key; nothing else reported for valid rows.

CLI tests (`ia-cli/tests/collection_bulk.rs`), each with a config whose host is the wiremock server and with test S3 keys:
- CSV and JSONL files: one PUT per row to `/<identifier>` with the merged `x-archive-meta*` headers (shared `--collection`, a row override), `created:` lines and the summary;
- an `image` column: the image PUT to `/<identifier>/<identifier>_itemimage.<ext>`;
- one row answered 403: the others still created, its error on stderr, exit 1;
- validation: a file with several bad rows sends no request (`expect(0)`), names every bad row, exit 1; with `--json` the error is `{"error":{…}}` on stderr;
- `--json`: one object per row, success and error;
- `--dry-run`: no request, needs no keys, prints each row and the summary;
- `--joblog`: entries written; a rerun skips the created rows and retries the failed one; `--no-resume` runs all;
- clap: `IDENTIFIER` with `--spreadsheet` is a usage error; neither is a usage error; `--spreadsheet` without `--collection` is accepted;
- missing keys: exit 1, no request.

## Help and docs

`long_about` and `after_long_help` of `ia collection create`, the `--spreadsheet` flag's help; docs/usage.md `ia collection` section (columns, defaults rule, validation, output, job log); README.md command list.

## Tasks
- [ ] Plan committed first.
- [ ] Red: tests above.
- [ ] Green: `collection_rows`, `--spreadsheet`.
- [ ] Help, docs/usage.md, README.md.
- [ ] `cargo test -p ia-core -p ia-cli`, `cargo clippy -p ia-core -p ia-cli -- -D warnings`.
- [ ] Code review; PR.
