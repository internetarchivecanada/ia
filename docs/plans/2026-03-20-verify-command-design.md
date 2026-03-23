# Design: `ia verify`

**Date:** 2026-03-20
**Status:** Draft

## Summary

Add an `ia verify` command that asserts local files exist on archive.org with
matching checksums. Exits non-zero if any file cannot be verified. Designed for
automation pipelines where downstream steps must only run after upload integrity
is confirmed.

```bash
ia upload my-item ./files/ --quiet && ia verify my-item ./files/ --quiet && ./post-upload.sh
```

## Background

The existing upload verification mechanisms serve different purposes:

- **`--verify`** (default on): Sends `Content-MD5` header during upload for
  server-side validation. Catches corruption in transit but requires trusting
  that the server stored the file correctly after accepting it.
- **`--checksum`** (aka `--skip-existing`): Pre-upload skip — compares local MD5
  against remote metadata and skips files that already match. Not a verification
  tool; it's an optimization.

Neither provides a post-hoc assertion that specific files are present on
archive.org with correct checksums. `ia verify` fills this gap as a read-only
verification command that can be run independently of upload — immediately after,
hours later, or against files uploaded by other tools entirely.

## CLI Interface

```
ia verify <IDENTIFIER> [FILES...]
    [--checksum-file <PATH>]
    [--checksum-type <ALG>]
    [--match-names]
    [--glob <PATTERN>]
    [--format <FORMAT>]...
    [--source <SOURCE>]
    [--spreadsheet <PATH>]
    [--json]
    [-q | --quiet]
```

Note: `-j`/`--jobs` is a global option inherited from the top-level CLI.

**Command alias:** `ve` (consistent with `do`, `up`, `md`, `se`, `co`, `ls`, `ta`).

### Arguments

| Argument | Type | Required | Description |
|----------|------|----------|-------------|
| `IDENTIFIER` | positional | yes (unless `--spreadsheet`) | Item identifier to verify against |
| `FILES...` | positional | no | Local files or directories to verify. Shell globbing works for local filtering. |
| `--checksum-file` | flag | no | Pre-computed checksum file (GNU/BSD formats, auto-detects algorithm) |
| `--checksum-type` | flag | no | Hash algorithm: `md5` (default), `sha1`, `crc32`. Overrides auto-detection. |
| `--match-names` | flag | no | Require filename match in addition to hash match (default: hash-only) |
| `--glob` | flag | no | Filter which remote files to consider when matching (e.g. `'*.pdf'`) |
| `--format` | repeatable flag | no | Filter remote files by IA format field (repeatable, e.g. `--format JPEG --format PNG`) |
| `--source` | flag | no | Filter remote files by source: `original`, `derivative`, `metadata` |
| `--spreadsheet` | flag | no | Batch verify from CSV/TSV/XLSX/ODS/JSONL file |
| (global `-j`) | inherited | no | Concurrency for hashing + metadata fetches (uses global `--jobs`, default: 2) |
| `--json` | flag | no | JSONL output |
| `-q` / `--quiet` | flag | no | No output, just exit code |

### Input requirements

At least one of `FILES...`, `--checksum-file`, or `--spreadsheet` is required.
Clap enforces this via `required_unless_present_any`. Bare `ia verify my-item`
with no input produces a clap error pointing users to the required options.
This leaves room for future features (e.g. verifying an item's internal
consistency using IA's own checksums) — when that's designed, the requirement
can be relaxed.

## Verification Algorithm

### Default Mode (hash-only)

For each local file (or checksum entry):

1. Compute local hash (or read from `--checksum-file` or spreadsheet column).
2. Fetch remote item metadata via `GET /metadata/{identifier}`.
3. Search remote files for any file whose hash matches the local hash.
4. If found: **verified** (report matched remote filename if different).
5. If not found: **missing** (no remote file has this hash).

### `--match-names` Mode

Same as above, but step 3 requires both hash AND filename to match:

1. Find the remote file with the same name.
2. If found and hash matches: **verified**.
3. If found but hash differs: **mismatch** (report both hashes).
4. If not found at all: **missing**.

The `mismatch` status only exists in `--match-names` mode because in hash-only
mode, a name mismatch is irrelevant — only the hash matters.

## Hash Algorithm Support

### Auto-detection from `--checksum-file`

The parser auto-detects the algorithm by hash length:

| Length | Algorithm |
|--------|-----------|
| 32 hex chars | MD5 |
| 40 hex chars | SHA-1 |
| 8 hex chars | CRC32 (ambiguous — see note) |

`--checksum-type` overrides auto-detection when needed.

**CRC32 ambiguity:** 8 hex characters is short enough to collide with truncated
hashes or other content. In GNU format (bare `hash  filename`), 8-char hashes
are only interpreted as CRC32 if `--checksum-type crc32` is explicitly set.
In BSD format (`CRC32 (file) = hash`), the prefix disambiguates. Without
`--checksum-type`, an unrecognized 8-char hash in GNU format is skipped with
a warning.

### Supported algorithms

These match the fields available in IA's `FileMetadata`:

| Algorithm | `FileMetadata` field | Checksum file prefix (BSD format) |
|-----------|---------------------|-----------------------------------|
| `md5` | `.md5` | `MD5 (file) = ...` |
| `sha1` | `.sha1` | `SHA1 (file) = ...` |
| `crc32` | `.crc32` | `CRC32 (file) = ...` |

### Checksum file format support

Reuse and extend `upload::checksum::parse_checksums()`:

- **GNU format**: `<hash>  <filename>` or `<hash> <filename>`
- **BSD format**: `<ALG> (<filename>) = <hash>` (extend to support SHA1/CRC32 prefixes)
- Mixed formats in a single file: supported
- Unrecognized lines: skipped with warning

### Forward compatibility

If IA adds new hash fields to `FileMetadata` in the future (e.g. `sha256`),
the remote matching side is forward-compatible — the algorithm name maps
directly to the JSON field name. However, local hash computation for new
algorithms requires a code change and new dependency. A future `--checksum-type
sha256` would need a `sha256` crate added. Pre-computed hashes via
`--checksum-file` would work immediately for any algorithm, since only the
remote lookup needs the field name.

## Spreadsheet Support

`--spreadsheet` accepts the same formats as `ia upload import`:
CSV, TSV, XLSX, ODS, JSONL.

### Required columns

| Column | Required | Description |
|--------|----------|-------------|
| `identifier` | yes | Item identifier |
| `file` | yes | Local file path |

### Optional hash columns

| Column | Description |
|--------|-------------|
| `md5` | Pre-computed MD5 hash (skips local computation) |
| `sha1` | Pre-computed SHA-1 hash |
| `crc32` | Pre-computed CRC32 hash |

Which hash column to use is determined by `--checksum-type` (default: `md5`).
If a hash column is present, use it instead of computing locally. If the hash
column is missing or empty for a row, compute from local file.

All other columns (metadata fields) are ignored.

### Behavior

1. Parse spreadsheet, extract `identifier`, `file`, and optional hash column.
2. Group records by identifier.
3. For each identifier, fetch remote metadata once.
4. Verify each file against the remote item.
5. Report per-file results, grouped by identifier.

### File path resolution

The `file` column is resolved the same way as `ia upload import`: relative to
the current working directory. If the column contains a remote-style key (e.g.
`subdir/file.pdf`), it's treated as a local path. If the file doesn't exist
locally and a hash column is present, the hash is used directly (no local file
access needed). The filename from the `file` column is used for `--match-names`
comparisons.

### Missing local files

If a local file doesn't exist and no hash column is present:
- Error with clear message: `"file not found: path/to/file.txt — provide
  --checksum-file or add md5 column to spreadsheet"`
- Counts as a verification failure (contributes to non-zero exit).

## Output

### Console (default)

Single item:
```
▸ my-item  (3 files)
  ✓ file1.pdf       512 MB  d41d8cd9…
  ✓ report.pdf      2.1 MB  a1b2c3d4…  (remote: report_final.pdf)
  ✗ image.png       no matching hash on remote

my-item  1 error
  ✓ 2 verified · ✗ 1 missing
```

Batch (spreadsheet):
```
▸ item-one  (2 files)
  ✓ file1.pdf       512 MB  d41d8cd9…
  ✓ file2.pdf       1.0 GB  e5f6a7b8…

▸ item-two  (3 files)
  ✓ doc.pdf         100 KB  11223344…
  ✗ data.csv        md5 mismatch (local: abc123… remote: def456…)
  ✓ notes.txt       2 KB    55667788…  (remote: notes_v2.txt)

Verified 2 items, 5 files
  ✓ 4 verified · ✗ 1 mismatch
```

The `(remote: other_name.pdf)` annotation appears when hash-only matching finds
the content under a different filename. In `--match-names` mode, this annotation
never appears (names must match exactly).

### JSON (`--json`)

Per-file JSONL to stdout:

```jsonl
{"identifier":"my-item","local_file":"file1.pdf","status":"verified","remote_key":"file1.pdf","local_hash":"d41d8cd9...","remote_hash":"d41d8cd9...","algorithm":"md5","bytes":536870912}
{"identifier":"my-item","local_file":"report.pdf","status":"verified","remote_key":"report_final.pdf","local_hash":"a1b2c3d4...","remote_hash":"a1b2c3d4...","algorithm":"md5","bytes":2202009}
{"identifier":"my-item","local_file":"image.png","status":"missing","local_hash":"fff000...","algorithm":"md5","bytes":1048576}
```

`--match-names` mode adds `mismatch` status:
```jsonl
{"identifier":"item-two","local_file":"data.csv","status":"mismatch","remote_key":"data.csv","local_hash":"abc123...","remote_hash":"def456...","algorithm":"md5","bytes":51200}
```

Per-file errors (network timeout, etc.) use the same `VerifyResult` schema with
`"status":"error"` and go to stderr. Fatal errors (bad config, auth failure)
use the standard `JsonError` format (`{"error":{"code":..., "message":...}}`).

```jsonl
{"identifier":"my-item","local_file":"file.pdf","status":"error","detail":"connection timeout","algorithm":"md5"}
```

### Quiet (`-q`)

No output. Exit code only.

- `-q`: no output
- `-qq`: no output (same behavior, consistent with other commands)

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | All files verified successfully |
| 1 | One or more files missing, mismatched, or errored |

This is the core design goal — a non-zero exit for automation gating:
```bash
ia verify my-item ./files/ --quiet && ./next-step.sh
```

## Architecture

### Core module: `ia-core/src/verify.rs`

```rust
/// Result of verifying a single file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyResult {
    pub identifier: String,
    pub local_file: String,
    pub status: VerifyStatus,
    pub remote_key: Option<String>,
    pub local_hash: Option<String>,
    pub remote_hash: Option<String>,
    pub algorithm: String,
    /// Local file size in bytes (None when verifying from pre-computed hash
    /// without local file access).
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyStatus {
    Verified,
    Missing,
    Mismatch,
    Error,
}

/// Options for verify operations.
#[derive(Debug, Clone)]
pub struct VerifyOpts {
    /// Hash algorithm to use.
    pub algorithm: HashAlgorithm,
    /// Pre-computed checksums keyed by filename.
    pub checksums: Option<HashMap<String, String>>,
    /// Require filename match (not just hash).
    pub match_names: bool,
    /// Filter remote files by glob pattern.
    pub glob: Option<String>,
    /// Filter remote files by format.
    pub format: Vec<String>,
    /// Filter remote files by source (original/derivative/metadata).
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub enum HashAlgorithm {
    #[default]
    Md5,
    Sha1,
    Crc32,
}

/// Verify files in a single item.
pub async fn verify_item(
    client: &IaClient,
    identifier: &str,
    files: &[VerifyInput],
    opts: &VerifyOpts,
    progress: impl Fn(VerifyResult),
) -> Result<Vec<VerifyResult>>;

/// Input for verification — either a local file path or a pre-computed hash.
pub enum VerifyInput {
    /// Local file to hash and verify.
    LocalFile(PathBuf),
    /// Pre-computed hash (from --checksum-file or spreadsheet column).
    Hash { filename: String, hash: String },
}
```

### Key design decisions

1. **Single metadata fetch per item.** `verify_item()` calls
   `client.get_item(identifier)` once, then checks all files against the
   returned `FileMetadata` list.

2. **Reuse existing infrastructure.** Checksum parsing, spreadsheet reading,
   file glob/format filtering, and MD5 computation all exist in `ia-core`.
   Extend `parse_checksums()` for SHA-1/CRC32 support.

3. **Progress callback pattern.** Same pattern as upload — caller passes a
   closure that receives each `VerifyResult` as it's produced. CLI layer
   decides how to display.

4. **Concurrency.** Local hash computation runs on blocking thread pool (same
   as upload). Batch mode fetches multiple items concurrently with `-j`.

### CLI module: `ia-cli/src/commands/verify.rs`

Standard command module following the project pattern:
- Clap derive struct with `-h` (terse) / `--help` (long + examples)
- `after_long_help` with `color-print` examples
- Calls `verify_item()` for single-item mode
- Groups spreadsheet records and runs concurrent verification for batch mode
- Handles output mode dispatch (console/JSON/quiet)

### Extending checksum parsing

The current `parse_checksums()` returns `HashMap<String, String>` and only
handles MD5. To avoid breaking existing upload callers, add a new function:

```rust
/// Parsed checksum entry with detected algorithm.
pub struct ChecksumEntry {
    pub hash: String,
    pub algorithm: HashAlgorithm,
}

/// Parse a checksums file, auto-detecting algorithm per entry.
/// Supports MD5 (32 hex), SHA-1 (40 hex), CRC32 (8 hex, BSD prefix only
/// unless --checksum-type crc32 is set).
pub fn parse_checksums_multi(
    content: &str,
    forced_algorithm: Option<HashAlgorithm>,
) -> HashMap<String, ChecksumEntry>;
```

The existing `parse_checksums()` remains unchanged for upload's MD5-only use.
A checksum file must use a single algorithm throughout (no mixing MD5 and SHA-1
lines). If `forced_algorithm` is set, all hashes are interpreted as that type.

### Hash computation

Currently only `compute_file_md5()` exists. Add:
- `compute_file_sha1()` using `sha1` crate
- `compute_file_crc32()` using `crc32fast` crate
- Generic `compute_file_hash(path, algorithm)` dispatcher
- All with async wrappers on blocking thread pool

## Testing Strategy

### Unit tests (ia-core)

- **Checksum parsing**: Extended formats (SHA-1, CRC32, BSD prefixes, mixed, auto-detection)
- **Hash computation**: Known test vectors for MD5, SHA-1, CRC32
- **Verify logic**: Hash-only matching, `--match-names` matching, missing files, mismatches
- **Algorithm selection**: Auto-detect from hash length, `--checksum-type` override
- **Spreadsheet column extraction**: `md5`, `sha1`, `crc32` columns, missing columns, empty values

### Integration tests (ia-core, wiremock)

- **Single item verification**: Mock metadata endpoint, verify files against it
- **Hash-only mode**: File with different name but matching hash → verified
- **Match-names mode**: Same hash, different name → missing; same name, different hash → mismatch
- **Missing file on remote**: No matching hash → missing
- **Multiple algorithms**: Verify with SHA-1, CRC32 against mocked metadata
- **Glob/format filtering**: Only consider matching remote files
- **Checksum-file input**: Verify using pre-computed hashes (no local files needed)
- **Mixed results**: Some verified, some missing → overall failure
- **Network error handling**: Metadata fetch fails → error status
- **Empty item**: Item exists but has no files → all local files report missing

### CLI integration tests (ia-cli)

- **Exit code 0**: All files verified
- **Exit code 1**: Any file missing or mismatched
- **Console output format**: Correct icons, summary line, remote name annotation
- **JSON output**: Valid JSONL, correct fields, stderr for errors
- **Quiet mode**: No stdout/stderr, correct exit code
- **`--checksum-file` mode**: Works without local files present
- **`--checksum-type` override**: Uses specified algorithm
- **`--match-names` mode**: Different behavior from default
- **`--glob` filtering**: Only matches against filtered remote files
- **`--spreadsheet` mode**: Reads identifier/file columns, ignores metadata columns
- **Spreadsheet with hash column**: Uses pre-computed hashes from spreadsheet
- **Spreadsheet missing local file**: Clear error message suggesting alternatives
- **`--source` filtering**: Only matches against original/derivative/metadata files
- **Bare `ia verify item`**: Clap error requiring files/checksum-file/spreadsheet
- **Help text**: `-h` terse, `--help` long with examples

### Edge cases

- Local file exists but is empty (valid — MD5 of empty = d41d8cd9...)
- Remote file has no hash field (skip file when building match candidates — it
  can't be matched. In `--match-names` mode where a name-matched remote file
  lacks the requested hash field, report as `error` with detail "remote file
  has no {algorithm} hash")
- Multiple remote files with the same hash (hash-only mode: first match wins)
- Very large files (verify hashing doesn't block async runtime)
- Identifier doesn't exist (metadata fetch returns 404 → all files report missing)
- Spreadsheet with duplicate identifier+file rows (deduplicate, verify once)

## Help Text

### Short help (`-h`)

```
Verify that local files exist on archive.org with matching checksums

Usage: ia verify <IDENTIFIER> [FILES]... [OPTIONS]

Arguments:
  <IDENTIFIER>  Item identifier to verify against
  [FILES]...    Local files or directories to verify

Options:
      --checksum-file <PATH>  Pre-computed checksum file (GNU/BSD formats)
      --checksum-type <ALG>   Hash algorithm [default: md5] [possible values: md5, sha1, crc32]
      --match-names           Require filename match, not just hash
      --glob <PATTERN>        Filter remote files to consider
      --format <FORMAT>       Filter remote files by format (repeatable)
      --source <SOURCE>       Filter remote files by source (original/derivative/metadata)
      --spreadsheet <PATH>    Batch verify from spreadsheet
      --json                  JSONL output
  -q, --quiet                 Suppress output (just exit code)
  -h, --help                  Print help (use --help for examples)
```

### Long help (`--help`)

Includes `after_long_help` with color-print examples:

```
Examples:
  # Verify specific files were uploaded
  ia verify my-item file1.pdf file2.pdf

  # Verify an entire directory
  ia verify my-item ./local-files/

  # Verify using pre-computed checksums (no local files needed)
  ia verify my-item --checksum-file md5sums.txt

  # Use SHA-1 instead of MD5
  ia verify my-item ./files/ --checksum-type sha1

  # Require exact filename match
  ia verify my-item ./files/ --match-names

  # Only verify PDFs on the remote item
  ia verify my-item ./files/ --glob '*.pdf'

  # Batch verify from upload spreadsheet
  ia verify --spreadsheet upload.csv

  # Gate a script on successful verification
  ia verify my-item ./files/ -q && ./post-upload.sh

  # Generate checksums, upload, then verify without re-hashing
  md5sum ./files/* > checksums.txt
  ia upload my-item ./files/
  ia verify my-item --checksum-file checksums.txt
```

## Non-goals

- **Polling/waiting for upload completion.** `ia verify` checks current state.
  If a file hasn't appeared yet, it reports `missing`. Users can retry or add
  their own polling loop.
- **Repairing mismatches.** `ia verify` only reports. Re-upload is a separate
  `ia upload` invocation.
- **Verifying item-level metadata.** This command verifies files only.
- **Dashboard mode.** Not needed for a read-only check. Console/JSON/quiet
  are sufficient.

## Dependencies

### New crates

- `sha1` — SHA-1 hash computation
- `crc32fast` — CRC32 computation

### Existing infrastructure reused

- `IaClient::get_item()` — fetch remote metadata
- `upload::checksum::parse_checksums()` — extend for multi-algorithm support
- `upload::checksum::compute_file_md5_async()` — pattern for blocking hash computation
- `spreadsheet::read_spreadsheet()` — multi-format spreadsheet reader
- `files::list()` with `FileFilter` — remote file filtering (glob, format, source)
- `output.rs` — shared progress styles and helpers
