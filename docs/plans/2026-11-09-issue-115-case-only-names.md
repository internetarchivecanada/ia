# Files whose names differ only in case (#115)

**The gap.** An item can hold two files whose names differ only in case, such as `Photo.txt` and `photo.txt`. `download_item_with_metadata` in `ia-core/src/download/mod.rs` downloads every selected file to `dest_dir.join(name)`. On a case-insensitive filesystem (the default on macOS and Windows) both names are the same file: the second download replaces the first, and both are reported `Complete`. When the two have the same size and mtime, the second is instead reported `Skipped("size+mtime match")`, compared against the other file's bytes, and never downloaded. With `--jobs` above 1 the two transfers can write the same `.part` at once. In every case a file is reported as present while its bytes are missing or belong to another file.

## Options

1. **Refuse the later file with an error and download the rest.** The first file in the item's listing order downloads as usual; each later file whose name collides with it fails with an error naming both files, counted in `files_failed`. Nothing is overwritten, every reported result is true, and the local path of every file that downloads is still its name in the item, so reruns, `--checksum` skips and resumes behave as before.
2. **Disambiguate the name** (`photo (1).txt`, or a suffix with the original case encoded). Both files reach the disk, but one of them under a name the item does not have. Every later step that maps an item name to a local path (the size and mtime skip, `--checksum`, resume from `.part`, the job log, scripts that read the destination directory) would need the same mapping, and the chosen name depends on which file happened to be listed first. A rerun after the item gains or loses a file could map a name to a different local file.
3. **Refuse the whole item.** Safe, but fails every other file of the item for a conflict that involves two.
4. **Probe the destination filesystem for case sensitivity** and act only where names collide on it. Behaviour would differ between machines and between directories on one machine (a case-sensitive APFS volume, a mounted network share), and a probe needs a write to the destination before any download starts.

## Decision

Option 1. A refused file is a failure, not a skip: the user asked for it and it is not on disk, so the run exits non-zero and the job log records it like any other failure.

**Detection is by name, not by filesystem.** Before any download starts, the selected files are compared by their names folded to lowercase (`str::to_lowercase`, full Unicode lowercase of the whole path). Two names that fold to the same text collide. The comparison runs on every platform, so the same item gives the same result on Linux, macOS and Windows; on a case-sensitive filesystem this refuses a file that could have been stored, which is the cost of identical behaviour. The comparison covers the files selected for this run, not every file in the item: downloading `photo.txt` alone writes one file and replaces nothing from the same run.

**Which file wins.** The first in the order the item's metadata lists the selected files. That order is stable for a given item, so a rerun refuses the same file.

**Error.** The refused file's status is `Failed` with the message `photo.txt not downloaded: its name differs from Photo.txt only in letter case, so both would be written to the same path on a case-insensitive filesystem`, and a `Failed` progress event is emitted, as for any other failed file. A per-file failure reaches callers only as this message (in `--json` output the code is `download_failed`, as for every failed file), so no new `IaError` variant is added. No request is sent for the refused file.

**Out of scope.** Names that differ only in Unicode normalization (`é` as one code point or as `e` plus a combining accent) are also the same file on APFS; detecting them needs normalization, which the standard library does not provide, and no such item has been reported. Collisions across items written into one directory with `--no-directories` are a separate case.

## Tasks
- [x] Plan committed first.
- [ ] Red: an item with `Photo.txt` and `photo.txt` of different bytes: the first `Complete` with its own bytes on disk, the second `Failed` with an error naming both, `files_failed == 1`, `files_downloaded == 1`, and no request for the second file; the same with `Dir/a.txt` and `dir/A.txt`; distinct names (`Photo.txt`, `photo2.txt`) both download; three names in one fold group fail the second and third; a CLI test that the summary counts the refused file as failed and the exit code is 1.
- [ ] Green: the check in `download_item_with_metadata`, docs/usage.md.
- [ ] `cargo test -p ia-core -p ia-cli`, `cargo clippy -p ia-core -p ia-cli -- -D warnings`.
- [ ] Code-review pass; PR.
