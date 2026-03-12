# Design: `ia collection create`

**Date:** 2026-03-11
**Status:** Implemented

## Summary

Add an `ia collection create` command that creates Internet Archive collections
via S3 PUT. Collections are items with `mediatype=collection`. The command
requires key metadata fields as a best-practice guardrail, supports arbitrary
additional metadata via `-m`, and optionally uploads a collection image in the
same request.

## Background

Creating a collection on Internet Archive is done through the S3-like API.
It's a PUT request to `s3.us.archive.org/{identifier}` with metadata headers
and either a zero-byte body (no image) or an image body at
`s3.us.archive.org/{identifier}/{identifier}_itemimage.{ext}`.

Collections are just items with `mediatype=collection`. "Adding an item to a
collection" is a metadata operation on the child item (`collection=foo`), not
an operation on the collection itself.

## CLI Interface

```
ia collection create <IDENTIFIER> \
    --title <TITLE> \
    --description <DESC> \
    --subject <SUBJ> \
    --collection <PARENT> \
    [--image <PATH>] \
    [-m key:value]... \
    [--derive] \
    [--dry-run] \
    [--json]
```

### Arguments

| Argument | Type | Required | Description |
|----------|------|----------|-------------|
| `IDENTIFIER` | positional | yes | New collection identifier |
| `--title` / `-t` | flag | yes | Collection title |
| `--description` | flag | yes | Collection description |
| `--subject` / `-s` | flag | yes | Subject/topic |
| `--collection` / `-C` | flag | yes | Parent collection identifier |
| `--image` / `-I` | flag | no | Path to image file |
| `-m` / `--metadata` | repeatable | no | Additional `key:value` metadata |
| `--derive` | flag | no | Enable derive (default: derive is off for collections) |
| `--dry-run` | flag | no | Show what would be done without sending request |
| `--json` | flag | no | Machine-readable JSON output |

### Behavior

- `mediatype: collection` is always injected and cannot be overridden
- `x-amz-auto-make-bucket: 1` is always set
- `x-archive-queue-derive: 0` is set by default; use `--derive` to enable derivation
- User-provided `-m` values can set any additional metadata
- Without `--image`: PUT to `s3.us.archive.org/{id}` with `Content-Length: 0`
- With `--image`: PUT to `s3.us.archive.org/{id}/{id}_itemimage.{ext}` with
  image as body and appropriate Content-Length. The file extension is preserved
  from the source file. If the file has no extension, error before sending.
  Content-Type is inferred from the extension (e.g., `image/jpeg`, `image/png`)
  using a simple hardcoded mapping; unrecognized extensions use
  `application/octet-stream`.

### Output

**Normal:** `created: https://archive.org/details/{identifier}`

**JSON:**
```json
{
  "identifier": "my-collection",
  "status": 200,
  "url": "https://archive.org/details/my-collection"
}
```

**JSON error:** (consistent with other commands)
```json
{
  "error": true,
  "identifier": "my-collection",
  "status": 403,
  "message": "Access denied"
}
```

**Non-JSON error:** Standard error output via `anyhow`, e.g.,
`error: S3 returned 403: Access denied`

**`--quiet`:** Suppresses the "created:" output line (global flag, handled the
same way as other commands).

### Command Registration

- Top-level command: `Collection` in the `Commands` enum
- Alias: `col`
- Subcommand enum `CollectionCommand` with `Create` variant (extensible for
  future operations like `list`)

## Core Module

### File: `ia-core/src/collection.rs`

Single public function, no module directory needed:

```rust
pub async fn create_collection(
    client: &IaClient,
    identifier: &str,
    metadata: &[(String, String)],
    image: Option<&Path>,
    queue_derive: bool,
    dry_run: bool,
) -> Result<CreateCollectionResult>
```

The `metadata` parameter uses `&[(String, String)]` (ordered key-value pairs)
to match the existing `encode_metadata_headers` API and preserve insertion
order for deterministic header numbering. The CLI layer converts its required
flags + `-m` pairs into this flat list.

### `CreateCollectionResult`

```rust
pub struct CreateCollectionResult {
    pub identifier: String,
    pub status: u16,
    pub url: String,
}
```

### Flow

1. Build S3 URL — base or with image filename
2. Build metadata headers via existing `upload::headers` module
3. Always inject `mediatype: collection` and `x-amz-auto-make-bucket: 1`
4. Set `x-archive-queue-derive: 0` unless `queue_derive` is true
5. If image: read file, set body + Content-Length; otherwise Content-Length: 0
6. S3-authenticated PUT via `IaClient`
7. Parse response, return `CreateCollectionResult` or error

### Reused Infrastructure

- `upload::headers` — S3 metadata header encoding (`x-archive-meta-` format)
- `upload/mod.rs` — `build_s3_url()` / `build_s3_item_url()` for URL
  construction. These are currently `pub(crate)` which is sufficient since
  `collection.rs` is in the same crate. If collection logic grows into its own
  module directory in the future, promote these to `pub` or move to a shared
  `s3` utility module.
- `upload::validate` — `validate_identifier()` for identifier validation
- `upload::s3_error` — S3 error response parsing
- `client.rs` — S3 auth (access/secret keys)

## CLI Layer

### File: `ia-cli/src/commands/collection.rs`

Follows the upload pattern (enum-based routing):

```rust
#[derive(Debug, Args)]
pub struct CollectionArgs {
    #[command(subcommand)]
    pub command: CollectionCommand,
}

#[derive(Debug, Subcommand)]
pub enum CollectionCommand {
    Create(CreateArgs),
}
```

Dispatches to `run_create()`. Extensible for future subcommands.

## Error Handling

| Condition | Handling |
|-----------|----------|
| Invalid identifier | `validate_identifier` error (existing) |
| Image file not found | Early `bail!` before network call |
| Image has no extension | Early `bail!` — extension required for S3 key |
| S3 error response | Parse with `upload::s3_error` |
| Auth missing | `IaError::AuthRequired` (existing) |

## Testing

### ia-core (`collection.rs`)

Wiremock-based integration tests:
- Create without image (zero-body PUT, correct headers)
- Create with image (image body, correct URL path, Content-Length)
- Metadata header encoding (title, description, subject, collection)
- `mediatype: collection` always present
- Additional `-m` metadata included in headers
- Dry-run returns early without network call
- S3 error response handling
- Auth required error

### ia-cli (integration tests)

- Missing required flags produce errors
- Successful create with all required flags
- `--json` output format
- `--image` with valid file
- `--image` with nonexistent file errors
- `-m` additional metadata passed through

## Future Extensions

The `ia collection` namespace leaves room for:
- `ia collection list` — list items in a collection (wrapper around search with `collection:foo`)
- Other collection-specific operations as needed

These are explicitly **not** in scope for this work.
