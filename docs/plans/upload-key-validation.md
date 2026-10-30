# Upload key validation (#96, #100)

## Problem

**#96.** With `--keep-directories`, `compute_keys` in `ia-core/src/upload/item.rs` uses each local path exactly as given as the remote key. `build_s3_url` in `ia-core/src/upload/mod.rs` percent-encodes each `/`-separated segment, which leaves `.` and `..` unchanged, and URL parsing then resolves them against the item path:

- `ia upload my-item ../scans --keep-directories` gives the key `../scans/a.pdf` and the request `PUT /scans/a.pdf`. The file goes to the item `scans`, and with `x-archive-auto-make-bucket` on the first file the request can create that item.
- `ia upload my-item /data/scans --keep-directories` gives the key `/data/scans/a.pdf` and the request `PUT /my-item//data/scans/a.pdf`: an empty segment, and the local directory layout in the remote name.

This is a security fix: a request must never leave the item named on the command line.

**#100.** `--remote-dir` is joined with `format!("{dir}/{key}")`, so `--remote-dir scans/` gives `scans//a.bin`. The file lands under a different name than asked for, and a later run without the slash looks up another name.

## Rule

### Key validation (every request)

`upload::validate_key` in `ia-core/src/upload/mod.rs` is the one check. A key is refused when it:

- is empty;
- starts with `/`;
- has a `..` segment;
- has a `.` segment;
- has an empty segment (`a//b`, or a trailing `/`).

A refused key is an error, never rewritten. `build_s3_url` calls the check and returns `Result<String>`, so every request that names a file inside an item is covered: single PUTs, every multipart request, and spreadsheet uploads (`upload_batch` passes the spreadsheet's keys to `upload_file`). `compute_keys` also checks every key it builds, so `upload_item` refuses a bad key before any request is sent, including on a dry run.

Error (new variant `IaError::InvalidKey { key, reason }`, JSON code `invalid_key`):

```
invalid remote file name '../scans/a.pdf': it has a '..' segment; a remote name is a path inside the item, with no leading '/' and no '.', '..' or empty segments
```

The reasons are `it is empty`, `it starts with '/'`, `it has a '..' segment`, `it has a '.' segment`, and `it has an empty segment`.

### `--keep-directories` keys

Each command-line path argument is normalized lexically: `.` components are dropped and `name/..` pairs cancel. Then:

1. A relative argument that does not start with `..` keeps its path as given, without `./`: `scans/a.pdf` → `scans/a.pdf`, `./scans/sub/a.pdf` → `scans/sub/a.pdf`, and `.` gives the paths below the current directory. This is the behavior today for those paths.
2. An absolute argument, or one that starts with `..`, keeps only its own last name and what is below it: `../scans` → `scans/a.pdf`, `/data/scans` → `scans/a.pdf`, `/data/a.pdf` → `a.pdf`.
3. An argument with no last name (`..`, `../..`, `/`) keeps only what is below it: `ia upload my-item .. --keep-directories` names `../x/a.pdf` as `x/a.pdf`, as `.` does for the current directory.

To know which argument a file came from, `upload_item` expands each argument on its own and keeps the pair; the combined list is sorted by file path as before. Keys are built from path components joined with `/`.

The key validation still runs on the result, so a path that cannot be named inside the item is refused before any request.

### `--remote-dir`

Leading and trailing `/` are trimmed before joining: `scans`, `scans/` and `/scans/` all give `scans/a.bin`. A value that is empty after trimming adds no prefix. A value with `..`, `.` or an empty inner segment is refused by the key validation.

## Documentation

- `--keep-directories` help in `ia-cli/src/commands/upload.rs` and the option table in `docs/usage.md` state the rule above.
- `--remote-dir` help and `docs/usage.md` state that surrounding `/` are ignored.

## Tests

`ia-core/tests/upload_item.rs` (wiremock). Every test asserts that every PUT path starts with `/<identifier>/` and has no empty segment.

- An argument with a leading `..` (a relative path from the test's working directory to a temporary directory): the PUT is `/test-item/<dir name>/a.bin`.
- An absolute argument: the PUT is `/test-item/<dir name>/sub/a.bin`.
- A plain relative directory, given with `./`: the PUT keeps the path below the working directory.
- `validate_key` directly: each refused form, the error text naming the key and the rule, and valid keys accepted.
- `--remote-dir` given as `scans/`, `/scans/` and `scans`: each PUT is `/test-item/scans/a.bin`.

Unit tests in `ia-core/src/upload/item.rs` for `compute_keys` (normalization, the three argument cases, `--remote-dir` with `..` refused) and in `ia-core/src/upload/mod.rs` for `build_s3_url` refusing a bad key.

## Tasks

- [x] Plan committed first.
- [ ] #96: tests (red), `validate_key`, `IaError::InvalidKey`, `build_s3_url` returns `Result`, per-argument expansion and the key rule in `compute_keys`, help and usage text.
- [ ] #100: tests (red), trimming in `compute_keys`, help and usage text.
- [ ] `cargo test`, `cargo clippy --all-targets -- -D warnings`.
- [ ] Code review; PR.
