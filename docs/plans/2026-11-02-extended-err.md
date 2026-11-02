# Metadata reads: `extended_err=1` and missing items (#118)

## Problem

`GET /metadata/{identifier}` answers a missing item with HTTP 200 and `{}`, not a 404 (checked live on 2026-11-02, with and without `extended_err=1`). The crate maps only a 404 to `IaError::NotFound`, so `{}` deserializes into an `ItemMetadata` with every field defaulted, and every caller reports an empty success:

- `ia metadata <id>` prints a record of `null` fields and exits 0.
- `ia metadata --exists <id>` exits 0, because `exists()` returns `Ok(true)`.
- `ia download <id>` reports zero files downloaded and exits 0.
- `ia list <id>` prints "no files match filters".

The Metadata API's `extended_err=1` flag (https://archive.org/developers/md-read.html, "Extended errors") makes an item that cannot be located but shows signs of existing return an integer `errcode` beside the human-readable `error`, instead of `{}`. An item with no history still returns `{}`. A successful response is unchanged (checked live on `nasa`: the same top-level keys, no `errcode`).

## Requests

Every `GET /metadata/{identifier}` sends `extended_err=1`:

- `metadata::read::get_with_params` (and so `get`, `exists`, `IaClient::get_item`, `get_item_with_params`, `item_exists`, and every caller of those: download, list, verify, upload's checksum skip and completion check, collection checks, the AI pipeline);
- the pre-write fetch in `metadata::write::modify_compound`;
- the dry-run fetch in `ia-cli`'s `run_dry_run_compound`.

The schema read (`/metadata/ia-metadata/schema`) is a partial read of one element and is out of scope, as are the metadata write POST and `dark_ok`.

## Response rule

One public function, `ia_core::metadata::check_item_body(identifier, &serde_json::Value) -> Result<()>`, beside the public constant `ia_core::metadata::EXTENDED_ERR_PARAM` that the three requests send, applied to every 200 body above after it is parsed as JSON and before it is used:

1. A body with a `metadata` or a `files` key is an item. Any `errcode` beside them is ignored, so a code documented as informational (106, a secondary metadata copy was used) never fails a read that returned the item.
2. Otherwise a body with an `error` or an `errcode` key is `IaError::ItemUnavailable`. With no `error`, the message is `no message`.
3. Otherwise (`{}`, or any other body with none of those keys) it is `IaError::NotFound`.

The 404 branch stays.

## Error type

```rust
#[error("item unavailable: {identifier}: {message}{}", errcode_note(code))]
ItemUnavailable {
    identifier: String,
    /// The response's `errcode`; `None` when the API sent only `error`.
    code: Option<u16>,
    message: String,
}
```

Display: `item unavailable: some-item: item is pending creation (errcode 101)`; without a code, the parenthesis is left out. The API documents that not every error has a code yet, so `code` is optional; an `errcode` that is not an integer in `u16` range is treated as absent and the message is kept.

JSON error code `item_unavailable`, with extra fields `identifier` and, when present, `errcode`.

## Retry

`IaError::is_retryable` is true for `ItemUnavailable` with code 102 (data node offline or not responding) and false for every other code and for no code. 101 (creation pending) and 104 (deleted) do not change within a retry budget.

The read loop in `metadata::read::get_with_params` already retries body-decode failures three times (0.5 s, 1.5 s backoff). It also retries an `ItemUnavailable` that `is_retryable` accepts, under the same budget and backoff. The pre-write fetch and the CLI dry-run fetch have no loop of their own and keep none; their callers see the typed error.

`get_once` now parses the body as a `serde_json::Value`, applies the rule, then converts to `ItemMetadata`. A body that is not JSON still fails in `reqwest`'s decode step and is retried as before. A body that is JSON but cannot become an `ItemMetadata` now fails once as `IaError::Json` instead of being retried as a decode error; that failure does not change on retry.

## `exists()`

Unchanged code, new results through the rule: `Ok(true)` for an item, `Ok(false)` for `{}` (and for a 404), `Err(ItemUnavailable)` for an `errcode` response, so a caller can tell "gone" from "unreachable right now". `upload::validate::check_collections` already logs and skips an `Err`.

## CLI

No new exit paths; the existing ones now fire:

- `ia metadata <id>`: exits 1 with `failed to fetch metadata for <id>` and the cause `item not found: <id>`.
- `ia metadata --exists <id>`: exits 1 (with `--json`, prints `{"identifier": "<id>", "exists": false}` first). The multi-identifier form branches on `IaError::NotFound` the same way.
- `ia download <id>`: the item fails with `item not found: <id>` and the command exits 1.
- `ia list <id>`: exits 1 with `failed to fetch metadata for <id>` and the cause.
- `--dry-run` metadata writes: `item not found: <id>` from the shared rule instead of an empty-source patch.

Upload's checksum skip and multipart completion check already treat any metadata error as "not there", so a new item (`{}`) or one still being created (101) uploads as before.

## Tests

`ia-core` (wiremock, in `metadata/read.rs` and `tests/metadata_write.rs`):

- 200 `{}` → `NotFound`; `exists` → `Ok(false)`.
- 200 `{"error": "...", "errcode": 102}` then a normal body → the item, two requests.
- 200 `{"error": "...", "errcode": 102}` every time → `ItemUnavailable { code: Some(102) }` after three requests.
- 200 `{"error": "...", "errcode": 104}` → `ItemUnavailable { code: Some(104) }` after one request; `exists` → `Err`.
- 200 `{"error": "..."}` → `ItemUnavailable { code: None }`.
- A normal body, matched only when `extended_err=1` is on the query, parses as before; with `dark_ok=1` both are sent.
- `modify_compound`: the GET carries `extended_err=1`; `{}` → `NotFound` and no POST.
- `error.rs`: `Display`, `to_json_error`, `is_retryable` for 101, 102, 104 and no code.

`ia-cli` (`tests/cli.rs`, `tests/download_search_list.rs`): with `{}` mocked, `metadata <id>` exits 1 with "item not found"; `metadata --exists <id>` exits 1 (`--json` prints `"exists": false`); `download <id>` and `list <id>` exit 1 with "item not found"; the dry-run metadata write sends `extended_err=1`.

## Docs

The error code table in `docs/plans/2026-02-23-agent-friendly-output-design.md` gains `item_unavailable`. `docs/usage.md` notes how a missing or unavailable item is reported.
