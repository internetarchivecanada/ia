# `ia upload --multipart[=auto|on|off|SIZE]` (#181)

## Background

- #21 (2026-10-05): IA ops advised against choosing multipart by file size, so that runs which die do not leave many unfinished multipart uploads on IA. ia-cli therefore sent every file as a single PUT and switched to multipart only after a single PUT of a file over 2 MiB died, for that file and for every later file over 2 MiB in the run. A rerun listed each item's unfinished multipart uploads and resumed those files.
- Since then, multipart has been asked for on by default where possible, with the suggestion to limit it to files over a certain size. This design does that.
- Multipart uploads create more catalog tasks per item than a single PUT, so small files must stay single PUTs. The TV pipeline (#179, `--no-multipart`) sends many 3 to 20 MB files per item and must never get multipart.

## Behaviour

One flag, `--multipart[=<auto|on|off|SIZE>]`, on the bare `ia upload`, `--spreadsheet` and `import`:

| Value | Rule |
|---|---|
| `auto` (default, no flag) | A file larger than 100 MiB (the part size, 104857600 bytes) is a multipart upload from its first attempt. A file at or under 100 MiB is always a single PUT. |
| `on`, bare `--multipart` | Every file is a multipart upload, as today. |
| `off`, `--no-multipart` | Every file is a single PUT, exactly as merged in #180. |
| `SIZE` | `auto` with SIZE as the cutoff: multipart for a file larger than SIZE, single PUT at or under it. |

A single PUT that dies is retried as a single PUT, whatever the setting: there is no switch on failure, and no run-wide switch for later files. The path for a file depends only on the flag and the file's size.

### SIZE syntax

The same syntax as `--min-speed` (`ia-cli/src/commands/rate.rs`): a whole number of bytes, or a whole number followed by `K`, `M` or `G` (either case) for powers of 1024. For example `--multipart=500M` or `--multipart=1G`. The parser is generalised to one size parser shared by both flags, and each flag keeps its own error message.

Nonsensical values are refused by clap with exit 2, and the message names the accepted forms. These are:
- anything that does not parse (`1.5G`, `10MB`, `-1`, `1T`, an empty value);
- a value that overflows u64;
- `0`. A cutoff of 0 would send every non-empty file as multipart, which is `on` stated less clearly, so the message points to `on`.

Values are matched exactly in lower case: `auto`, `on`, `off`. There is no lower or upper bound beyond that. A cutoff under 100 MiB makes a one-part upload for files between the cutoff and 100 MiB, as `--multipart` already does for any file of 100 MiB or less. A very large cutoff behaves as `off` for every real file. Both are what the user asked for.

### Parsing bare `--multipart`

The flag is `num_args = 0..=1`, `require_equals = true`, `default_missing_value = "on"`. A value must be attached with `=`, so `ia upload --multipart my-item big.iso` and `ia upload my-item --multipart big.iso` keep reading `my-item` and `big.iso` as positionals, as in existing scripts. `--multipart on` (with a space) is therefore `--multipart` followed by a positional `on`. This is the cost of keeping bare `--multipart` working, and the help and docs say to use `=`.

`--no-multipart` with `--multipart` in any form is refused (clap conflict, exit 2), as today.

## What happens to today's machinery

- **The run-wide switch and the fallback in `single.rs`** (`falls_back`, `continue_as_multipart`, the switch warning): removed. A dead single PUT is retried as a single PUT, and when its retries are spent it fails with the single-PUT error (`UploadFailed`, or `UploadStalled` for stalls), as `--no-multipart` does in #180.
- **The retry carry-over** (a fallen-back file carried its unspent attempts into each multipart request): removed together with the fallback. The `--retries` help and docs lose the sentence.
- **The start-of-run listing `GET /<item>?uploads` and the per-item record of unfinished uploads** (`record_unfinished_uploads`, `MultipartFallback`): removed.
  - A file over the cutoff takes the multipart path, which lists the unfinished uploads of that file's name itself before sending any part and resumes from the parts that check out, as `--multipart` does today. So a file over the cutoff still resumes on a rerun, and the item-level listing adds nothing for it.
  - A file at or under the cutoff is a single PUT, whatever IA holds: the simplest rule, predictable from the flag alone. An unfinished multipart upload of such a file, for example one left by an older ia-cli's fallback, is not resumed, listed or aborted. `ia upload cleanup` lists and aborts it. Resuming it would make the path depend on server state again, which is the behaviour being removed, and it would cost one listing request per item on every run to serve a leftover that is at most 100 MiB.
  - With this, `auto` sends no multipart request for an item whose files are all at or under the cutoff.
- **The time bound of the removed listing**: the start-of-run listing was given the response wait (120 s) over all its attempts, because `send_with_retry` bounds an answer's headers but not its body. The multipart path's own listings (and initiate) read their bodies with no bound, which mattered little while multipart was opt-in. Under `auto` every file over 100 MiB lists first, so initiate and both listings now read their bodies inside the retry budget (`send_read_with_retry`). A body that does not arrive within the response wait is re-sent at once, and a body whose read fails is retried after the usual wait. Each spends one of `--retries`, and past the budget the request fails, so a listing that could not be read is never taken for an empty one.
- **`--multipart` / `--no-multipart` before `import`**: both positions count, and the two flags together, or either flag twice, are refused across the positions as clap refuses them in one (exit 2).
- **`MULTIPART_FALLBACK_MIN_SIZE`** (2 MiB, public in `ia-core`): removed. It existed only for the fallback. The new cutoff is a new public constant, `MULTIPART_CUTOFF` = `DEFAULT_PART_SIZE` (100 MiB).

## ia-core API (all to be listed in the pull request)

- `Multipart` gains `Above(u64)`: a multipart upload for a file larger than this many bytes, and a single PUT otherwise.
- `Auto` changes meaning: it is now `Above(MULTIPART_CUTOFF)`. It was single PUT with the fallback and resume.
- `Always` and `Never` are unchanged.
- The enum stays `#[non_exhaustive]`, and the one `match` on it in the crate names every variant.
- Removed: `MultipartFallback` (type and re-export), `UploadOpts.multipart_fallback`, `UploadOptsBuilder::multipart_fallback` and `MULTIPART_FALLBACK_MIN_SIZE`. Added: `MULTIPART_CUTOFF`.
- `Multipart::is_multipart(self, file_size) -> bool` (public) states the rule in one place, for the library and for callers.

## CLI

- `UploadArgs.multipart` and `ImportArgs.multipart` become `Option<Multipart>`, using a value parser for `auto|on|off|SIZE`, and `multipart_setting` maps them.
- `--no-multipart` stays a separate bool flag that conflicts with `multipart`.
- `--multipart=off` and `--no-multipart` give the same `Multipart::Never`.

## Docs and help

Rewrite every multipart passage to state the new rule:
- `ia upload --help`: the long text, the flag help and the examples;
- `-h`;
- `docs/usage.md`: the flag table, "Stalled uploads", "Falling back to multipart" (replaced by "When a file is sent as multipart"), "Resuming a multipart upload", "Resuming uploads", the spreadsheet options and the Examples;
- the README row.

Add runnable examples for the default, `on`, `off`/`--no-multipart` and a custom SIZE.

## Tests (hermetic, written first)

Core:
- With a custom cutoff (`Above(1 MiB)`), a file of cutoff + 1 byte is multipart from the first attempt and a file of exactly the cutoff is a single PUT.
- Unit tests: `Auto` is `Above(100 MiB)`, so at 104857600 bytes a file is a single PUT and at 104857601 bytes it is multipart. No real 100 MiB body is sent; the integration tests use a small custom cutoff.
- A dead single PUT under the cutoff is retried as a single PUT, with no multipart request.
- A stalled single PUT with `Auto` is re-sent and fails as stalled.
- `on` sends every file as multipart.
- `off` is unchanged (the #180 tests stay).
- A rerun of a file over the cutoff resumes from its parts.
- A file under the cutoff with an unfinished upload on IA is a single PUT, and nothing is listed.

CLI:
- `--multipart=SIZE` parsing, including the refused values.
- Bare `--multipart` followed by positionals, in both positions.
- `--multipart=on|off|auto|SIZE` combined with `--no-multipart` is refused, both bare and with `--spreadsheet` and `import`.
- `--multipart=SIZE` on `--spreadsheet` and `import` against a mock: one file over and one at the cutoff.

The existing tests of the fallback and of the item-level listing are removed with the code, and existing tests of `--multipart` stay.

## Out of scope

Parallel parts, the 100 MiB part size, download code, options placed before a subcommand name being dropped, and the release.
