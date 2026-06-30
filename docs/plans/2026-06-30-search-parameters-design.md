# Search parameters on `--search` commands + clearer empty-result errors

Date: 2026-06-30
Status: Approved, implementing

## Motivation

Two problems, surfaced by a real session:

```
$ ia metadata export --search 'collection:radio4all'
Error: No input provided. Pass files, --search, or pipe identifiers via stdin.
```

1. **Misleading error.** `--search` *was* given; the scrape query simply matched
   zero items. Reporting "No input provided" sent the user hunting for a CLI bug
   when the real issue was the query. The message must distinguish "a source was
   given but matched nothing" from "no source at all."

2. **No way to pass search parameters.** When `--search` is used as an *input
   source* (not the `ia search` command), there is no way to pass scrape
   parameters such as sort order or page size. `download` and `ai qa` already
   support `-p/--parameters`; the other `--search` commands do not.

## Part A — Search parameters

Universal flag: `--search-parameter KEY:VALUE` (repeatable, `KEY=VALUE` also
accepted, long-only), parsed by the existing
`crate::commands::search::parse_extra_params` and fed into `SearchOpts.params`.
Sorting rides the passthrough, e.g. `--search-parameter sorts='addeddate desc'`.

**Design rule: `-p` is reserved for a command's *primary* request params, never
for the `--search` input selector.** So `-p` stays on `ia search` (the search
*is* the request) and on `tasks submit` (`-p` = raw task param). On every command
where `--search` is merely an input selector — `metadata`, `download`, `ai` — the
search params get the explicit long-only `--search-parameter`, and `-p` is left
free for that command's own request params.

| Command | Today | Change |
|---|---|---|
| `download` | had `-p/--parameters` for search | rename to `--search-parameter` (drop `-p`); reuse `parse_extra_params` |
| `ai qa` | had `-p/--search-parameters` | rename to `--search-parameter` (drop `-p`) |
| `ai analyze` (feature `ai-analyze`) | `--search` only | add `--search-parameter` |
| `metadata export` | `--search` only | add `--search-parameter` |
| `metadata` batch (`BatchInput` → audit/modify/append/append-list/insert/remove) | `--search` only | add `--search-parameter` to `BatchInput` (covers all at once) |
| bare `metadata` (`MetadataArgs`) | `--search` only | add `--search-parameter` |
| `tasks submit` | `-p` = **task** param | add long-only `--search-parameter` |
| `ia search` | `-p/--parameters` (primary) | unchanged |

Dropping `-p` from `download`/`ai qa` is a breaking change to those pre-existing
flags; acceptable for this pre-1.0, deliberately-not-drop-in-compatible CLI.

**Cleanup:** standardize on `parse_extra_params`; `download`'s private
`search_opts_from_params` becomes a thin wrapper that delegates to it, removing
the duplicate (and the `:`-vs-`=` precedence inconsistency).

## Part B — Clearer empty-result error

New shared helper in `ia-cli/src/identifier.rs`:

```rust
pub fn empty_input_message(
    search: Option<&str>,
    itemlist: Option<&Path>,
    no_source_help: &str,
) -> String
```

- `search` active, 0 results → `search returned 0 results for '<query>'.` plus a
  hint to test the query with `ia search scrape '<query>'`.
- `itemlist` active, empty → names the file.
- no source → the command-specific `no_source_help` (existing examples).

Applied at every empty-input bail: `collect_identifiers_from_export`, the audit
caller, and `run_write` (modify/append/etc.). The bare-`metadata` path already
names the source; it is left as-is (it warns + returns Ok rather than erroring).

## Testing (hermetic)

- CLI parse tests: `--search-parameter` accepted on each updated command;
  `--search-parameter` accepted on `tasks submit`; malformed value (`foo`)
  rejected with the expected error.
- Behavior test: wiremock scrape returning `{"items":[]}` → command errors with
  "0 results for '<query>'", not "No input provided".
- Plumbing test: a `sorts=...` param reaches the scrape request query string.

## Scope notes

- `ia search`'s own `-p` is unrelated and unchanged.
- Not converging metadata's bespoke collectors onto the shared
  `identifier::collect_identifiers` — out of scope; only messaging + params here.
