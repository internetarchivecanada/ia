# Spreadsheet upload metadata fixes (#97, #98)

Two defects in `ia upload --spreadsheet`, both in how a row's columns become
item metadata and file names.

## #97: indexed columns reach IA as one value

### Problem

`group_records` (`ia-core/src/upload/batch.rs`) turns `subject[0]` and
`subject[1]` into two `subject` pairs in `HashMap` order. The merge with the
command-line metadata then finds the first existing `subject` entry and
overwrites it for every pair, so one value survives, chosen by `HashMap`
order. The dashboard path (`ia-cli/src/tui/upload_app.rs`) copies the same
loop.

### Rules

1. **Per row, per field:** a bare column (`subject`) comes first, then the
   indexed columns (`subject[N]`) in ascending numeric index. Two columns
   that parse to the same index (`subject[0]`, `subject[00]`) are both kept,
   ordered by column name. This is the rule `prepare_metadata` applies to
   repeated `-m` arguments since #65 (`collection:a` then `collection[1]:b`
   gives `[a, b]`). The Python client drops the bare value in this case;
   the divergence is deliberate, since a value the user gave should not
   vanish.
2. **Fields** are emitted in name order, so the header list is the same on
   every run.
3. **Row against `-m`:** if the row names a field (bare or indexed), the
   row's values replace every `-m` value for that field. `-m` fields the row
   does not name are kept. This matches the Python client
   (`dict.update` in `ia_upload.py`, then `prepare_metadata` replacing the
   bare value with the indexed list) and lets a row override a `-m` default
   for a single-value field such as `mediatype`.
4. The header encoder numbers repeated keys in list order, so
   `subject[0]=alpha`, `subject[1]=beta` become `x-archive-meta00-subject:
   alpha` and `x-archive-meta01-subject: beta`.

### Change

- `group_records` builds each row's metadata with rules 1 and 2.
- New `ItemGroup::item_opts(&self, base: &UploadOpts) -> UploadOpts` applies
  rule 3 (and the remote name, below). `upload_batch` and the dashboard
  both call it; the copied loop in `upload_app.rs` is removed.

## #98: `REMOTE_NAME` is sent as metadata

### Problem

The template writes a `REMOTE_NAME` column. Every spreadsheet reader
lowercases column names (deliberately: column names are case-insensitive),
so `fields.remove("REMOTE_NAME")` never matches, the value is sent as item
metadata `remote_name`, and the file keeps its local name. The template
column list is also written out three times (CSV in `ia-core`, TSV and XLSX
in `ia-cli`), and the JSONL template writes `remote_name` through serde.

### Rules

- The column is matched as `remote_name` after lowercasing, so any case
  works. It is never item metadata.
- Per row: the file is stored under the remote name when the column is
  filled in, otherwise under its usual name (`--remote-dir` still applies).
- The template writes `REMOTE_NAME` in every format, the name the Python
  client reads, so one template works with both tools.

### Change on this branch

- `group_records` removes `remote_name` and records it per file in a new
  `ItemGroup::remote_names` field, parallel to `files`.
- `ItemGroup::item_opts` sets `UploadOpts::remote_name` when the group has
  one file and that file has a remote name; `compute_keys` already applies
  it to a single file.
- `validate_groups` refuses a group of two or more files in which any row
  has a remote name, naming the identifier. `compute_keys`
  (`ia-core/src/upload/item.rs`) takes one remote name for a whole item and
  ignores it for more than one file; it is being rewritten for #96/#100, so
  per-file names land on a second branch after that merges. Until then the
  case is an error rather than a silently ignored column.
- `ia-core/src/upload/template.rs` gains `TEMPLATE_COLUMNS`, used by the CSV,
  TSV and XLSX writers; `TemplateRow::remote_name` serializes as
  `REMOTE_NAME`.
- `docs/usage.md`: column names are case-insensitive; `REMOTE_NAME`; the
  multi-value and `-m` rules.
- The `--spreadsheet --dry-run` listing shows a file's remote name when the
  row has one.

### Change on the second branch (after #96/#100)

`compute_keys` takes a remote name per file (for example a
`&[Option<String>]` parallel to `files`, or per-file keys on `UploadOpts`):
a file with a remote name uses it instead of the basename or the
`--keep-directories` path, then `--remote-dir` is prefixed as for any key.
`validate_groups` drops the multi-file refusal. That commit carries
`Fixes #98`.

### API note

`ItemGroup` is public with public fields; adding `remote_names` breaks
struct-literal construction by library callers. It ships in a minor
version.

## Tests (`ia-core/tests/upload_batch.rs`, wiremock, reading the PUT)

1. `subject[0]=alpha`, `subject[1]=beta` → `x-archive-meta00-subject: alpha`,
   `x-archive-meta01-subject: beta`.
2. `-m subject:cli` plus the same row → only alpha, beta; a `-m` field the
   row does not name is still sent.
3. Columns `subject[2]`, `subject[10]`, `subject[0]` → values in index order
   0, 2, 10 (10 checks numeric, not string, order).
4. Bare `subject` plus `subject[0]` → bare first.
5. A CSV written by `write_template_csv` with `REMOTE_NAME` filled in, read
   by `read_spreadsheet` → `PUT /item-1/renamed.txt`, no
   `remote--name` header.
6. A two-row item with a remote name → batch validation error.

Unit tests in `batch.rs` and `template.rs` cover the grouping order and the
JSONL header name.

## Out of scope

#96, #99, #100, #13, the single-file upload path, multipart. `ia verify
--spreadsheet` (`ia-core/src/verify.rs`) does not read `REMOTE_NAME`; that is
a separate change.
