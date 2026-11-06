# Full-text search: request key `q` and server errors (#124)

## Problem

`ia search fts` returns no results. The first request body sent by `ia_core::search::fts` is

```json
{"query": "!L computer", "size": 1000, "scroll": true}
```

Since IA's full-text search API (`https://be-api.us.archive.org/ia-pub-fts-api`) started validating its parameters, it answers that body with HTTP 200 and an error envelope and no hits:

```json
{"user_message": {"msg_type": "ERROR", "text": "the parameter q is required"}, "ia-pub-fts-api-wrapper": true}
```

The client checks only the status code, finds no hits, and ends the stream with no error, so the CLI prints `search  0 results` and `no results found for query: computer`. The server's message is lost.

## API behaviour (checked live on 2026-11-05, unauthenticated)

| Request body | Answer |
|---|---|
| `{"q": "!L computer", "size": 3, "scroll": true}` | 200; `_scroll_id`, `hits.total` 6524253 (a number), 3 hits |
| `... "size": 2, "from": 3, "scroll": true` | 502 `{"error": "UPSTREAM_ERROR", "message": "The search service is temporarily unavailable, please try again later."}` |
| `... "size": 2, "scroll": true` (same, no `from`) | 200 |
| `... "size": 2, "from": 1, "scroll": false` | 200; hits 2 and 3 of the first request; no `_scroll_id` |
| `... "scope": "all"` or `"scope": "nasa_index"` | 200; same total; response reports `fts-api.auth.context_scope: "standard"` |
| `{"q": "{\"match\": {\"text\": \"moon landing\"}}", ...}` and the same wrapped in `{"query": ...}` | 200; `hits.total` 0 |
| `{"q": "{\"match\": {", ...}` (invalid JSON) or `{"q": "", ...}` | 200; no hits, no `user_message` |
| `GET ?q=!L computer` | 200; `hits.total` 6524059, 10 hits |

The same GET sent with an S3 `Authorization` header for a staff account reported `hits.total` 10497800: credentials widen the scope. The Python library sends the same header, so the two clients agree; nothing changes here.

Scroll continuation pages carry `fields` values as plain strings (`"filename": "x_hocr_searchtext.txt.gz"`) where the first page sends one-element lists.

Each hit looks like

```json
{"_index": "fts_main_v2", "_id": "comingofageinlan0000your|1319f8442aca0dbe3c25322fea16539a99ce3f94",
 "_score": ..., "highlight": {"text": ["... {{{computer}}} ..."]},
 "fields": {"identifier": ["comingofageinlan0000your"], "filename": [...], "page_num": [...], "meta_title": [...], ...}}
```

`_id` is the identifier, a `|`, and the SHA-1 of the matching file. One item can produce several hits (one per matching file or page).

## Changes

### Request

The first request body is built from:

- `q`: the query, with the `!L ` prefix unless `opts.dsl` (unchanged);
- `size`: 1000 (unchanged default), a JSON number;
- `scroll`: `true`, a JSON boolean;
- then each `opts.params` pair. `size` and `from` that parse as unsigned integers are sent as JSON numbers and `scroll` of `true`/`false` as a JSON boolean; every other value stays a string, as before. The CLI's `--scope`, `--size` and `--from` reach `fts` through these pairs, so `--size 5` now sends `"size": 5` instead of `"size": "5"`.
- When the body has `from`, `scroll` is set to `false` and the stream ends after that one page. The API rejects `from` with scroll (the 502 above) and honours it without.

The continuation request (`POST {base}?scroll=true` with `{"scroll_id": ...}`) is unchanged; it works live.

`ia search fts --num-found` also sends `--scope` (it was dropped before); `fts_num_found` already sends `q` as a GET parameter.

### Response

One helper reads every FTS response (`fts` pages and `fts_num_found`):

1. A non-2xx status is `IaError::Http` with the body text as the message, as now.
2. A 2xx body that is not JSON is the new `IaError::SearchFailed { status, message }`, the message naming the problem and the start of the body.
3. A 2xx body with `user_message` is `SearchFailed` with the message's `text`. A 2xx body with a string or object `error` (the shape of the 502 above) is `SearchFailed` with its `message`, else the `error` value.
4. Otherwise the body is a page. No hits, or an empty or missing `_scroll_id`, ends the stream cleanly, as now.

`hits.total` is read as a number or as `{"value": N}` (Elasticsearch 7 and later return the object unless told otherwise; the API returns a number today).

`SearchFailed` displays as `search failed: {message}`, has `--json` error code `search_failed` with an extra `status`, and is not retryable: a validation error does not change on retry.

### Result shape

- `SearchResult::identifier` is the hit's `fields.identifier` (first element when it is a list), else the part of `_id` before `|`, else `_id`. The old value was `_id` itself, which is not an identifier, so `ia search fts --itemlist | ia download` could not work. `identifier` is removed from `fields`, as the scrape and advanced backends do.
- The hit's `_id` (unique per hit; the identifier was its only carrier before) and `highlight` (the matching text) are kept in `fields`. The Python library prints the whole hit in JSON mode; `ia-cli` dropped it.

### CLI

`run_fts` needs no change for the error path: `run_output` returns on the first error before it prints the count line. Help text and `docs/usage.md` change to:

- `--scope`: sent as the API's `scope`; the documented `nasa_index` example is removed (the API accepts any value and reports the standard scope for an unauthenticated request).
- `--size`: results per request (default 1000).
- `--from`: start at this offset and fetch one page of `--size` results, without scrolling.
- `--dsl`: the query is sent as is, without the `!L` prefix. The JSON example is removed: it returns no hits on the live API (filed separately).

## Not changed here (filed)

- `--dsl`: the DSL format the API expects is unknown; the documented example returns `hits.total` 0.
- `--timeout`: stored in `SearchOpts.timeout` and never applied to a request, on every search backend.
