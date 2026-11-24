# `ia metadata export` resume and `export`/`audit` exit codes (#188, #189)

## #188: an interrupted CSV/TSV/XLSX export loses rows the joblog marks done

### Cause

`run_export` writes `{"op":"export","status":"ok"}` to the joblog as soon as
an item is fetched, but writes a CSV, TSV or XLSX output file only once, after
the last item. An interruption between the two leaves the joblog claiming rows
that no file holds. The rerun skips those items and has no rows to read back.

### Ordering rule

**A row reaches the output file before its item is marked `ok` in the joblog.**

- CSV/TSV/XLSX: fetched rows are held as pending. The whole file is rewritten
  (written to a hidden sibling, `.<name>.partial.<ext>`, then renamed over the
  output, so the file on disk is always a complete earlier version or a
  complete new one), and only then are the pending `ok` entries appended to
  the joblog. Error entries are written as they happen, since they claim no
  row.
- The file is rewritten (mid-run and on Ctrl+C only with `--joblog`; without
  one there is no resume, so the file is written once, at the end, and an
  interrupted run leaves an earlier file untouched):
  - at the end of the run;
  - on Ctrl+C (SIGINT): pending rows are written, then the command exits 130,
    as `ia download` does;
  - during the run, once rows are pending and at least
    `max(5 s, 10 × the duration of the last rewrite)` has passed since the
    last rewrite. The 5 s floor bounds the work a hard kill loses; the
    10 × term keeps rewrite time under about a tenth of the run as the file
    grows. Fixed rule, no flag. A mid-run rewrite that fails is a warning and
    is tried again at the next one; the final write failing is an error.
- JSONL file and stdout: the line is written (one `write_all` per line) before
  the joblog entry, which today comes first.

### What a crash between the two leaves

- Before the rename: the output file is the previous complete version and the
  joblog has no `ok` for the pending items, so the rerun fetches them again.
  Nothing is lost.
- After the rename, before the joblog entries: the file holds rows the joblog
  does not mark done. The rerun fetches those items again; **a fetched row
  replaces any row already in the file for the same identifier**, in place, so
  nothing is doubled. (If the joblog marks nothing done, the rerun does not
  read the file back and rewrites it from what it fetches, as today.)
- JSONL: a kill between a line's write and its joblog entry leaves the line in
  the file unmarked; the rerun appends that item's line a second time. One
  duplicate line per kill, never a lost one. Out of scope here: the JSONL file
  is appended to, not read back.

## #189: `export` and `audit` exit 0 when an item cannot be fetched

### Rule, matched to the other bulk commands

`ia metadata modify` ends with `bail!("{n} of {total} item(s) failed")`
(exit 1, `Error: …` on stderr) and, with `--json`, `process::exit(1)` after
the per-item lines on stdout. `ia download` and `ia upload` exit 1 when any
item or file failed. `docs/usage.md` gives `0`/`1` as the only exit codes.

- `export`: after writing every row it could and printing the summary, exits 1
  with `Error: <n> of <total> item(s) failed: <id>, <id>, …` (first 10
  identifiers, then `and <k> more`, pointing to the joblog when there is one).
  `<total>` counts the items fetched this run, not those skipped on resume.
- `audit`: after its normal output (terminal, `--json` lines, or `-o` file,
  each of which already names the item), exits 1. Without `--json`:
  `Error: <n> of <total> item(s) could not be fetched: <ids>`. With `--json`,
  as `modify --json` does: exit 1 with no extra stderr text; the item's stdout
  line names it. That line's `error` becomes `{"code", "message"}`, the form
  `ia collection create --json` uses for a per-item error, instead of a bare
  string.

## Tests (ia-cli/tests/metadata_export_resume.rs, wiremock, binary)

1. SIGINT after `item-a` is fetched while `item-b` stalls, then rerun: the CSV
   holds `item-a` and `item-b` once each. (The issue's test; fails on main.)
2. SIGKILL after the joblog marks `item-a` done (a checkpoint), `item-b`
   stalled: the file already holds `item-a`; the rerun does not fetch
   `item-a` and ends with both rows once.
3. State left by a crash after the rename and before the joblog: file has
   `item-a` and an old `item-b`, joblog marks only `item-a`. Rerun: `item-b`
   once, with the new value.
4. `export` with one missing item exits 1, names it, and writes the other row.
5. `audit` of a missing item exits 1 and names it; with `--json`, exit 1 and
   the line's `error` has `code` and `message`.

## Revision after review of PR #194

The invariant: **no state exists, after a run ends or is interrupted at any
point, in which the joblog marks an item `export` `ok` and the output file
holds no row for it.** These rules hold it:

1. **Rows the joblog marks done are always read back.** A resumed export
   reads the output file back whenever the joblog marks any item done, not
   only when an item of this run's list is skipped. Before, a list that did
   not include an item an earlier run had marked done rewrote the file
   without that item's row.
2. **`--no-resume` never rewrites the file before the end.** It reads
   nothing back, and the joblog (append-only) keeps the earlier `ok`
   entries, so a mid-run or Ctrl+C rewrite would replace a complete earlier
   file with the rows fetched so far. An interrupted `--no-resume` run
   therefore leaves the earlier file and marks nothing new: the rows it
   fetched are lost and fetched again by the next run. A `--no-resume` run
   that completes rewrites the file with the rows of its own list, which is
   what `--no-resume` asks for; an item that only an earlier run marked
   done and that is not in this list then has no row. That end state is as
   on `main` and is documented, not changed here.
3. **Nothing new, nothing written.** The file is not rewritten when no row
   was fetched since the last write (Ctrl+C before the first row, or a run
   in which every item failed), so an existing file is left as it is. A run
   with no rows and no existing file still creates one.
4. **Ctrl+C exits 130 even if the final write fails.** The write error is
   printed and the pending rows stay unmarked (the rerun fetches them).
5. **The output is replaced by rename only when that is safe.** Decided
   once, before any request:
   - a symlink is followed, and the file it points to is replaced (the
     link stays);
   - the replacement gets the mode of the file it replaces (owner and group
     cannot be kept by an unprivileged process);
   - when the target exists and is not a regular file (a FIFO,
     `/dev/fd/N`, `/dev/stdout`), or no file can be created in its
     directory, the file is written in place, once, at the end, as on
     `main`: no mid-run or Ctrl+C rewrite, since an in-place write is not
     atomic and a FIFO reader would see the file more than once. The joblog
     entries still follow the write.

### Second review

6. **An item is done when its latest `export` entry is `ok`.** A
   `--no-resume` run that fails an item an earlier run exported writes the
   file without its row; its `error` entry, being latest, makes it not
   done, so the next run fetches it.
7. **The format comes from the `-o` name**, also when a symlink there
   points to a file with another extension.
8. **A read-only output file is written in place**, which fails as on
   `main`, instead of being replaced by rename. Only a regular file is read
   back (a FIFO would block).
9. **In-place writes are not atomic.** A kill or a full disk during one can
   truncate the file; the invariant is stated for files replaced by rename,
   and the documentation says so.
10. Not changed: a symlink planted at the hidden sibling's name between the
    probe and the write (shared writable directories only); an output
    directory that does not exist still fails at the final write, as on
    `main`.
