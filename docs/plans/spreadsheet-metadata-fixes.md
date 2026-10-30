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

1. **Per row, per field:** the indexed columns are placed with
   `fill_positions` (`ia-core/src/metadata/write.rs`), the slot rule that
   `apply_set` applies to repeated `-m` arguments of `ia metadata` since
   #65; one implementation, so the same syntax means the same thing in
   both places. A bare column is
   the field's list; an indexed column overwrites position N, or is
   appended when N is past the end, in index order. `subject=a` with
   `subject[1]=b` gives `[a, b]`; `subject=a` with `subject[0]=x` gives
   `[x]`. The readers do not keep column order, so columns are applied in
   column-name order, and of two columns for one position the later in
   that order wins (`subject[0]` over `subject[00]`). Every cell is a
   literal value: an upload creates the item, so there is nothing for a
   removal marker to remove. A blank value is no value. `ia upload`
   `-m` does not accept indexed keys. The Python client builds the indexed
   list without the bare value; this follows #65 instead.
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
  filled in, otherwise under its usual name. The remote name comes from the
  row only; `UploadOpts::remote_name` given by a caller is not used for a
  batch.
- A remote name on a row whose `file` is a directory is refused: the
  directory expands to several files. (The Python client treats it as a
  remote directory prefix; not adopted here.)
- The template writes `REMOTE_NAME` in every format, the name the Python
  client reads, so a CSV template works with both tools. Matching the
  column in any case is a deliberate divergence: the Python client reads
  only `REMOTE_NAME` and would send a lowercase column as metadata.

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

- `UploadOpts::remote_names: HashMap<PathBuf, String>` (new; `UploadOpts`
  is `#[non_exhaustive]`, so this is not a break), with a builder method:
  the remote name for a file argument, keyed by the path as passed to
  `upload_item`.
- `compute_keys` (`ia-core/src/upload/item.rs`), per file, in order:
  1. its argument has an entry in `remote_names`: that name. An entry for
     an argument that is a directory is refused (a directory expands to
     several files).
  2. `remote_name` with exactly one file: that name (unchanged).
  3. `keep_directories`: the kept path (unchanged).
  4. the basename (unchanged).

  Then `--remote-dir` is prefixed and `validate_key` applied, as for every
  key, so a remote name such as `../x` is refused before any request.
- Two different files given one key, where a remote name produced it, are
  refused (the second PUT would replace the first), extending the existing
  `--keep-directories` clash check.
- `ItemGroup::item_opts` fills `remote_names` from the rows and leaves
  `remote_name` unset. `validate_groups` drops the multi-row refusal and
  keeps the directory refusal. Because the names are keyed by file, one
  file on two rows of an item with different `REMOTE_NAME` values is
  refused there (only one name could be used).
- The clash check, when remote names are present, covers every key of
  the item, so a remote name equal to another row's file name is refused
  as well.
- That commit carries `Fixes #98`.

Tests (`ia-core/tests/upload_batch.rs`): an item of two rows with two
different `REMOTE_NAME`s gives two PUT paths; a row without one keeps its
name next to a row with one; `REMOTE_NAME` with `--remote-dir`; a
`REMOTE_NAME` of `../x` refused with no request; two rows with the same
`REMOTE_NAME` refused with no request. Unit tests for `compute_keys`.

### API note

`ItemGroup` is public with public fields; adding `remote_names` breaks
struct-literal construction by library callers. It is marked
`#[non_exhaustive]` in the same change, so only `group_records` builds one
outside the crate (which keeps `files` and `remote_names` the same length)
and later fields are not a further break. It ships in a minor version.

## Tests (`ia-core/tests/upload_batch.rs`, wiremock, reading the PUT)

1. `subject[0]=alpha`, `subject[1]=beta` → `x-archive-meta00-subject: alpha`,
   `x-archive-meta01-subject: beta`.
2. `-m subject:cli` plus the same row → only alpha, beta; a `-m` field the
   row does not name is still sent.
3. Columns `subject[2]`, `subject[10]`, `subject[0]` → values in index order
   0, 2, 10 (10 checks numeric, not string, order).
4. Bare `subject` plus `subject[0]` → only the indexed value; bare plus
   `subject[1]` → both; `subject[0]` plus `subject[00]` → the value of
   `subject[0]`.
5. A CSV written by `write_template_csv` with `REMOTE_NAME` filled in, read
   by `read_spreadsheet` → `PUT /item-1/renamed.txt`, no
   `remote--name` header.
6. A two-row item with a remote name → batch validation error.

Unit tests in `batch.rs` and `template.rs` cover the grouping order and the
JSONL header name.

## Review findings and resolution

Review of the full diff before the pull request:

- The `--spreadsheet --dry-run` listing merged metadata with its own copy
  of the old overwrite loop (`merge_metadata` in
  `ia-cli/src/commands/upload.rs`) and showed one indexed value. Fixed: the
  listing prints `ItemGroup::item_opts(..).metadata`; `merge_metadata` and
  its tests are removed; a CLI test covers the listing.
- A remote name on a directory row passed validation and was ignored or
  misapplied. Fixed: refused in `validate_groups`, with a test.
- The usage text said `--remote-dir` applies to a spreadsheet batch;
  `run_import` does not pass it. Fixed in the docs; wiring the flag in is
  a separate change.
- An empty value from a library-built record removed the `-m` value for
  its field. Fixed: blank values are dropped, with a test.
- `item_opts` kept a caller's `UploadOpts::remote_name` for every item
  without one. Fixed: the remote name comes from the row only.
- `ItemGroup` gained `#[non_exhaustive]`.
- Orchestrator review: the row rule was a copy that put the bare value
  first and kept both of two columns for one position, contrary to the #65
  rule it cited. Fixed: the slot logic of `apply_set` is extracted into
  `fill_positions`, which `apply_set` and `row_metadata` both call (the
  removal step stays in `apply_set`), with tests for
  the overwrite, the append and the repeated position.
- Orchestrator review: treating a `REMOVE_TAG` cell as a removal caused two
  defects (a removed field kept its `-m` values; a row of only removals
  was taken as the item's metadata, so a later row's metadata and the
  required-field check were skipped). Decision: an upload creates the
  item, so a cell is always a literal value, as `-m` is on the command
  line; a test pins it.
- Docs: header-name case rule narrowed (JSONL `identifier` is matched
  exactly), the first-row-with-metadata rule, the CSV-only compatibility
  with the Python client, and the existing rule that the required-field
  check looks at the row and not at `-m`.
- Not changed: the required-field check itself (`validate_groups` takes no
  options; validating the merged metadata needs a new function or a
  signature change, and predates this work).

## Out of scope

#96, #99, #100, #13, the single-file upload path, multipart. `ia verify
--spreadsheet` (`ia-core/src/verify.rs`) does not read `REMOTE_NAME`; that is
a separate change.
