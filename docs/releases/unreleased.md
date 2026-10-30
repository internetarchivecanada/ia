# ia (unreleased)

Changes merged after `v0.23.1` and not yet in a release. At release time
this file becomes `docs/releases/<version>.md`.

## Changed

- **`ia download` verifies the md5 of every download by default** (#13).
  Before, the md5 was compared only with `-C`/`--checksum`, so a corrupt
  transfer of the right length was renamed into place and reported as a
  success. The md5 is computed from the stream as it is written and
  compared with the item metadata before the rename; a fresh download is
  not read back from disk. A resumed download first reads the bytes
  already in its `.part` file (about 2 s per GB at the 400 to 600 MB/s
  measured on an Apple M1 Pro). A file whose metadata has no md5, or an
  empty one, is not checked, nor is `<identifier>_files.xml`, which
  records its own md5 before it is final. A mismatch is handled as with
  `--checksum` before: the bytes are kept as `<name>.md5-mismatch`, the
  file is downloaded again from byte 0, and the same wrong md5 twice in a
  row fails the file for good. A file whose md5 in the metadata is wrong
  used to download without complaint and now fails; `--no-verify` turns
  the check off and accepts a download on its size alone.
- **`-C`/`--checksum` on `ia download` is only the skip check.** It still
  skips a local file when its md5 matches the item metadata (reading
  every local file); it no longer controls verification of downloads.
  `-C --no-verify` gives the md5 skip check without verification. Two
  failures `-C` used to produce are gone: a file whose metadata md5 is
  empty no longer fails with a checksum mismatch, and
  `<identifier>_files.xml` no longer fails twice and ends with "the
  source file or its metadata is likely wrong"; both complete on the
  size check.
- `ia-core`: `DownloadOpts` gains `verify: bool` (default `true`).
  `DownloadOpts.checksum` now means only the md5 skip check. A consumer
  that set `checksum = true` keeps both behaviors; one that left it
  `false` now gets verification, and sets `verify = false` to keep the
  old behavior. `DownloadOpts` is `#[non_exhaustive]`, so the new field
  breaks no code that builds it through `DownloadOpts::default()`.

## Fixed

- **`ia upload --spreadsheet` sends every value of indexed columns**
  (#97). `subject[0]` and `subject[1]` used to reach IA as one header,
  chosen by hash order. Each value is now its own header
  (`x-archive-meta00-subject`, `x-archive-meta01-subject`, ...) in
  numeric index order. A row follows the rule `ia metadata` applies to
  `-m`: a bare column is the field's list and `subject[N]` overwrites
  position N or is appended past the end. A cell of `REMOVE_TAG`
  removes that value (new: it used to be sent as the text
  `REMOVE_TAG`), and the field still counts as named by the row. A
  field the row fills in
  replaces every `-m` value for that field, as in the Python client;
  `-m` fields the row leaves empty are still sent. The `--dry-run`
  listing and the `--dashboard` path use the same merge.
- **The template's `REMOTE_NAME` column names the file and is never
  item metadata** (#98). The readers lowercase column names, so the
  column used to be sent as metadata `remote_name` while the file kept
  its local name. It is now matched in any case; a single-row item with
  it filled in is stored under that name. A filled-in `REMOTE_NAME` in
  an item of several rows, or on a row whose `file` is a directory,
  stops the batch before any request. Every template format writes the
  same columns; the JSONL template now writes `REMOTE_NAME`, not
  `remote_name`.
- `ia-core`: `ItemGroup` gains `remote_names` and `#[non_exhaustive]`,
  which breaks code that builds it with a struct literal;
  `group_records` builds it. New `ItemGroup::item_opts`, and
  `upload::template::{TEMPLATE_COLUMNS, REMOTE_NAME_COLUMN}`.
