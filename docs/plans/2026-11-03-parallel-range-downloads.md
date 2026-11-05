# Parallel Range downloads within one file (#16)

**Goal:** `ia download` fetches one large file over several HTTP Range requests at once, each on its own HTTP/1.1 connection, and an interrupted download resumes per part.

**Evidence:** [`parallel-transfer-measurement-2026-10-30.md`](parallel-transfer-measurement-2026-10-30.md). For a 2.04 GiB file, one connection averaged 5.21 MB/s; separate HTTP/1.1 connections averaged 2.41× that at 4 streams and 3.50× at 8; four ranges multiplexed on one HTTP/2 connection gained nothing (0.90×). No error or throttling response from the datanode at up to 8 connections.

## Decisions (maintainer, recorded in the issue and the task)

- One HTTP/1.1 connection per stream, up to 8 per file. Off by default (revised 2026-11-05, see "Per-server cap"): `--connections-per-file <N>`, 1 to 8, default 1, where `1` keeps the single-stream path.
- A cap on the download connections the process opens to one server, `--max-connections-per-server <N>`, default 6, counting every `--jobs` file and every connection of a file (added 2026-11-05, see "Per-server cap").
- Parallel ranges only when the server answers 206 with a `Content-Range`; a 200 answer falls back to the single-stream path. Files below a threshold use the single-stream path.
- A part journal beside the partial file, so an interrupted download resumes per part. It must survive a crash mid-write and is deleted on success.
- md5 verification stays on by default (#13), as a pass over the assembled file. The size check (#12) and the `_files.xml` exemption apply to the whole file.
- The stall rule (10 KiB/s over 60 s, 30 s grace), the retry budget (`--retries`) and `Retry-After` handling apply per stream.
- No change to upload code.

## Engineering calls (flagged for review)

### Part size and threshold

- **Part size: 64 MiB.** The file is cut into consecutive 64 MiB parts on a fixed grid (part *k* covers bytes `k × 64 MiB` up to the next boundary; the last part takes the remainder). Each stream takes the next unfetched part from a shared queue, fetches it with one `Range: bytes=a-b` request, and takes another when it is done.
- **Threshold: 256 MiB.** A file is fetched in parallel only when its metadata size is at least 256 MiB, which is four parts, one for each of the 4 connections the measurement recommended.
- **Why these numbers.** They are rclone's defaults for the same feature (`--multi-thread-chunk-size 64Mi`, `--multi-thread-cutoff 256Mi`, `--multi-thread-streams 4`), the established standard among downloaders that split one file over several connections; 4 connections, rclone's stream count, is also what the measurement recommended. The measurement supports them for archive.org in particular:
  - Every part costs one `/download/` redirect and a new time to the first byte, measured at 0.16 to 6.2 s. At the 3.3 MB/s per stream measured with 4 streams, a 64 MiB part takes about 20 s, so a typical first-byte wait of under a second costs a few percent. Parts of a few MiB would spend most of their time waiting for the first byte.
  - A queue of fixed parts limits the tail: when one stream is slow, the others take the remaining parts, and the last stream to finish is at most one part (about 20 s) behind. One range per stream, as the probe used, ties the whole file to the slowest stream (per-stream rates in one run differed by up to 25%).
  - Below 256 MiB a file has fewer than four parts, and a file of 64 to 128 MiB takes 13 to 27 s on one stream, of which the first-byte wait of each extra request would take back a large share. Several files are already fetched at once (`-j`, default 8), so small files gain little from splitting.
- Both numbers are constants, not flags: neither has a reason a user would set it, and the measurement does not vary them.

### Choosing the path

`download_file` keeps its skip, dry-run and path checks. The parallel path is taken when all of these hold: `DownloadOpts::connections_per_file` is 2 or more; the file's metadata has a size; the size is at least 256 MiB; and the file is not `{identifier}_files.xml`. Otherwise the single-stream path runs exactly as today.

The parallel path sends the request for the first unfetched part on its own, before any stream starts and before anything on disk changes:

- **206** whose `Content-Range` names exactly the requested range: the parallel download proceeds and the first stream consumes this response. A total in `Content-Range` that differs from the metadata size is `ServerSizeMismatch`, as on the single-stream path.
- **200**, or a 206 without a `Content-Range` naming exactly the requested range: the response is dropped and the file is fetched on the single-stream path. The server does not serve ranges reliably, so splitting would be unsafe.
- **An error** (a status, a connection failure): returned from `download_file` as the single-stream path returns a failed first request, so the per-file retry loop in `download_item_with_metadata` handles it with its backoff and `Retry-After`.

With `--count-views`, only this first request omits `cnt`; every later part request carries `cnt=0`, and so do the single-stream requests after a fallback, so one download counts one view, not one per part.

### Transport

`IaClient` gains a fourth reqwest transport, private to the crate: the download transport's settings (no redirects, followed by hand to keep `Authorization`; connect and read timeouts; the same User-Agent) with `http1_only()`. HTTP/1.1 carries one request at a time per connection, so the streams' concurrent requests each get their own connection from the pool, and a stream's next part reuses its idle connection. The existing transports are unchanged; the single-stream path keeps negotiating HTTP/2.

### Writing the parts

The `.part` is created once, after the first journal is written and before the streams start. Each stream opens it for writing (never creating or truncating it, so a `.part` that goes away mid-download fails the attempt instead of coming back with holes beside a journal), seeks to its part's next unfetched byte, and writes through a 256 KiB buffer, as the single-stream path does. Streams write disjoint ranges through separate file handles. The `.part` grows as streams write past its end; it is not pre-sized, so a file system without sparse files does no extra work. A body byte past the end of the requested range is never written.

The `.part` file is checked for a symlink before every open, as on the single-stream path; a link is removed and the file's attempt fails with the retryable `ResumeFailed`, so the next attempt finds no `.part` and starts over. Several streams may see the same link; the first removes it and the others report the same failure. A link can still be put in place between that check and the open, so on Unix the opened file is compared with what is at the path afterwards (device and inode, and not a link) before any byte is written; on a mismatch the handle is dropped unused and the attempt fails the same way.

### The part journal

**File:** `<name>.part.json`, beside `<name>.part`. An item file literally named `<name>.part.json` would share the path, as one named `<name>.part` already shares the partial file's. A file at that path that is not one of this code's journals (it does not parse as one, cannot be read, or is larger than any journal) is taken to be such an item file: it is never removed or replaced, and `<name>` is fetched on one stream, with any `.part` taken as a contiguous prefix. Refusing the parallel path was chosen over a second journal name, which could collide in the same way.

**Format:** one JSON object.

```json
{"version":1,"size":2193522688,"md5":"44343bc1373c6dc826ebb58839155c39","part_size":67108864,"done":[67108864,67108864,1048576,0]}
```

- `size` and `md5` are the item metadata's values when the journal was written (`md5` is `null` when the metadata has none). They identify the version of the file the bytes belong to.
- `part_size` fixes the grid; `done[k]` is how many bytes of part *k*, counted from its first byte, are on disk. A part is complete when `done[k]` equals its length.

**Crash safety:** the journal is replaced, never edited. A new version is written to `<name>.part.json.tmp` (created with `create_new` after any old temporary is removed, so it never writes through a link), synced to disk, and renamed over `<name>.part.json`. A crash leaves either the old or the new journal whole; a leftover temporary is never read. Before a stream records progress, it flushes its buffer and syncs the `.part` file's data, so the journal never claims bytes that are not on disk. Progress only grows, so an older journal is always safe to resume from.

**When it is written:**

- once, after the first request answered 206 and before any byte is written into the `.part` (so a `.part` without a journal is always a contiguous prefix of the file, which is what the single-stream path assumes);
- by a stream each time it has written another 16 MiB since its last record, and whenever its stream ends (the part complete, a body-stream error, a stall, or the download stopping).

A crash therefore loses at most 16 MiB per stream.

**When it is deleted:** after every part is complete, before the md5 pass. At that point every byte is on disk and synced, so the full-length `.part` is a contiguous prefix (all of it) and the single-stream rule holds again. An md5 mismatch then moves the `.part` to `.md5-mismatch` as today, and the next attempt starts from byte 0.

**Invariant:** a journal beside a `.part` means the `.part` may have holes and its length means nothing; no journal means the `.part` is a contiguous prefix.

### Resume

At the start of the parallel path:

1. A `.part` that is a symlink is removed, as on the single-stream path.
2. A file at the journal path that is not one of this code's journals is left alone and the file goes on one stream (see the file name above). A journal of this code is checked: it must not be a symlink or another non-regular file, must have a `.part` beside it, must have `version` 1, `size` and `md5` equal to the current metadata, the `part_size` in force (64 MiB; another one comes from another version of this code, and a tiny one from a damaged journal would mean millions of requests), one `done` entry per part, every entry no larger than its part, and the `.part` must be a regular file at least as long as the furthest byte the journal claims and no longer than the file. A journal that fails any check is deleted together with the `.part`, and the file starts from byte 0: holes cannot be told from data.
3. Without a journal, a regular `.part` of length L is a contiguous prefix. L above the metadata size: the `.part` is deleted and the file starts from 0. Otherwise the prefix is mapped onto the grid: parts ending at or before L are complete, the part containing L has `done = L − start`.
4. **Which parts are refetched:** every part with `done < length`, from byte `start + done` to its last byte. Nothing past `done` in a part is trusted. A complete part is not requested.
5. When every part is already complete, no request is sent; the md5 pass and the rename follow.

**Single-stream path with a journal present** (after `--connections-per-file 1`, after a 200 fell back, or on a file that is now below the threshold): before reading the `.part` length, the journal is settled. A valid journal yields its contiguous prefix: the complete parts from the start plus the `done` bytes of the first incomplete part. The journal is first rewritten to claim only that prefix, then the `.part` is truncated to it, then the journal is deleted; a crash at any step leaves a journal that claims no more than the `.part` holds, or no journal and a `.part` that is a prefix. An invalid journal is deleted with the `.part`. The single-stream path then resumes with `Range: bytes=<prefix>-` as today.

### Per-stream rules and failure reporting

A stream is one of the N workers. Its budgets last for the whole download attempt of the file, across the parts it takes:

- **Body-stream error** (connection drop, incomplete body, read timeout, a body that ends before the part does, or one that would run past it; of a chunk that runs past the end of the part only the bytes inside it are written, so a server whose body always runs a little long still completes the part): the stream records its progress, waits 0.5 s, 1.5 s, 4.5 s, and re-requests the rest of the part with `Range`, up to 3 times, as the single-stream path does (`MAX_STREAM_RETRIES`). A fourth error counts as one failed request (below) and the three are available again.
- **A part that arrived in full** is complete, whatever ended its stream at the same moment (a stall check or a body error racing the end of the body): nothing is spent on it.
- **Stall** (below `--min-speed`, with the same window and grace, judged on each stream's own clock): the stream records its progress and re-requests the rest of the part at once. Each stream has `--retries` stalls; one more fails the file with `DownloadStalled`, as today.
- **Failed request** (an error status that is retryable, a connection failure, a 206 for the wrong range, a 200 to a range request): the stream waits the standard backoff, or the failed response's `Retry-After` when it carried one, and sends the request again, up to `--retries` times. One more fails the file with the new `IaError::RangeFailed`: `download of <name> failed on bytes A-B after N attempts: <last error>`. It is not retryable: the per-file loop would only spend the same budget again against the same server.
- **Permanent failure** (a non-retryable status such as 403 or 404, `ServerSizeMismatch`): fails the file at once, with that error unchanged.
- **Local I/O error** (disk full, permission): fails the file at once, unchanged, so disk-full detection and the disk pool keep working.

When one stream fails the file, the others are told to stop: each records its progress and ends. The file's error is the first stream's; the `.part` and the journal are kept, so a later run resumes per part.

`Retry-After` on one stream's request delays that stream only; the other streams continue.

### Per-server cap

Added after review: IA operations asked that one client open at most 6 connections to one server at once. The Python `ia` downloads one file at a time; this port already opened up to 8 connections to one datanode with `--jobs 8` (the default), so the cap tightens existing behaviour as well as bounding parallel ranges. For the same reason parallel ranges are off by default: `--connections-per-file` defaults to 1.

- **What is counted.** Every request a download sends (`download_file`, on either path): each `--jobs` file and each range connection of a file. Metadata requests and other commands are not counted.
- **Which server.** The one the request lands on: the datanode after the `/download/` redirect, or the request's own host when there is no redirect. A slot is taken on a host before each request to it is sent; on a redirect that slot is given back before a slot on the next host is taken, so a request never holds two slots and never waits for one while holding another. The key is the URL's authority (host, and port when one is given).
- **How long.** From before the request is sent until the response, and the body read from it, are dropped. A stream gives up its slot before it sends its next request.
- **When the server is full.** The request waits for a slot; nothing is rejected or reduced. `--connections-per-file 8` with the default cap therefore runs at 6 connections for one file, without an error. A stream told to stop while it waits stops waiting.
- **Shape.** `ServerSlots`, a handle to a table of one semaphore per server, carried in `DownloadOpts::server_slots`. Clones share the table, so every file and item that clones one set of options shares the cap; the CLI makes one for the run. A new handle starts with every slot free.
- **Why requests stand for connections.** HTTP/1.1 carries one request at a time per connection and the HTTP/2 download transport multiplexes requests on one connection, so a client with at most N requests in flight to a server never needs more than N connections to it; an idle pooled connection is reused before a new one is opened.

### Completion

When every stream has finished and every part is complete: the `.part` length must equal the metadata size; the journal is deleted; when the file is verified (`DownloadOpts::verify`, an md5 in the metadata), the md5 of the `.part` is computed in one pass from disk (reported as `Verifying`, about 2 s per GB) and compared; the existing `finish_part` renames, keeps a mismatch as `.md5-mismatch`, removes an older one on success, and sets the mtime from the first response's `Last-Modified`.

### Progress

`Starting` with the bytes already on disk, then `Downloading` with the sum over all streams, every 256 KiB, then `Verifying` and `Complete` as today. No new status.

## Public API changes in `ia-core`

- `download::DownloadOpts::connections_per_file: usize` (new field; the struct is `#[non_exhaustive]`). Default 1. 0 and 1 mean the single-stream path; values above 8 are treated as 8.
- `download::DownloadOpts::server_slots: ServerSlots` (new field) and `download::ServerSlots` (new type: `new(max)`, `max()`, `Default` with 6), the per-server cap.
- `download::DEFAULT_CONNECTIONS_PER_FILE` (1), `download::MAX_CONNECTIONS_PER_FILE` (8) and `download::DEFAULT_MAX_CONNECTIONS_PER_SERVER` (6), new constants.
- `IaError::RangeFailed { file, start, end, attempts, source }`, new variant (the enum is `#[non_exhaustive]`); not retryable; JSON code `range_failed`.
- Behaviour: `download_file` (and the item functions that call it) holds a per-server slot for every request; with `connections_per_file` of 2 or more it fetches a file of 256 MiB or more over parallel ranges and may leave `<name>.part.json` beside `<name>.part` after an interrupted download; the single-stream path reads and settles such a journal.

## CLI

`--connections-per-file <N>` on `ia download`: 1 to 8, default 1; `1` is the single-stream path. `--max-connections-per-server <N>`: at least 1, default 6. The global `-j, --jobs` help names the cap. Help text, `long_about`, an example, the `docs/usage.md` flag table and a new section, and the README feature line.

## Tests (written before the implementation)

In-crate tests in `ia-core/src/download/parallel.rs`, with the part size and threshold shrunk through a `cfg(test)` override, as the stall tests shrink the window. wiremock serves ranges from a byte buffer; a raw loopback listener serves the stall, drop and concurrency cases.

- Grid and journal: part layout, prefix mapping, contiguous prefix, journal round trip, every validation rule, atomic replace (no temporary left; a link at the journal or the temporary path is replaced, not followed).
- A file at the threshold is fetched in N closed ranges, assembled, verified; no `.part` or journal left.
- The streams run at once on separate connections (the server holds every response until N connections are open).
- `streams = 1`, and a file below the threshold: one plain GET.
- 200 to the first request: single-stream fallback, correct file, no journal.
- `Content-Range` total differs from the metadata: `ServerSizeMismatch`.
- Resume: only parts not complete are requested, each from `start + done`; a stale journal (other size or md5), a corrupt journal, and a journal claiming more than the `.part` holds restart from 0; a `.part` without a journal is adopted as a prefix.
- Single-stream path with a journal: the `.part` is truncated to the contiguous prefix, the journal removed, the resume `Range` starts at the prefix.
- Per stream: a 503 with `Retry-After` on one range is retried by that stream and the file completes; a spent budget gives `RangeFailed` naming the range, keeps the `.part` and journal, and is not retried by the per-file loop; a later run fetches only the missing parts; a body-stream error re-requests the rest of the part; a stall re-requests the rest of the part; a spent stall budget gives `DownloadStalled`; a 404 on one range fails at once.
- md5 mismatch after assembly: `ChecksumMismatch`, `.md5-mismatch` kept, journal gone.
- `--count-views`: only the first request lacks `cnt`.
- Progress: the reported bytes only grow and end at the file size.
- `IaError::RangeFailed`: Display, not retryable, JSON.

- Per-server cap: 8 files with 8 jobs on one server never have more than 6 connections open; 8 connections for one file hold at 6; two servers with a cap of 6 reach 12 in all and 6 each; a cap of 1 serialises a parallel download, through a `/download/` redirect, and completes with the right bytes; `ServerSlots` keeps one count per server and shares it between clones.

CLI tests in `ia-cli/tests/`: both flags in help with their defaults, range validation (0 and 9 for `--connections-per-file`, 0 for `--max-connections-per-server` are usage errors), an end-to-end run on a 256 MiB body that issues one plain GET by default and four range requests with `--connections-per-file 4`, and `--jobs 8` on an 8-file item against a loopback server that never sees more than 6 connections at once.

One live read-only run against the public file used by the measurement, to confirm the 206 path end to end.

## Not covered

- The per-server cap does not cover `--zip-member`, `--zip-list` or the other commands' requests.
- No hermetic test redirects to another host (the redirect guard admits only the configured host and `*.archive.org`); keying by the host a request lands on is tested through a redirect to the same host and through two hosts reached directly.
- HTTP/1.1 is not negotiated in the tests (they run over plain HTTP, where it is the only protocol); that the datanode is reached over HTTP/1.1 is confirmed only by the live run.
- Sync cost on slow disks: on macOS the standard library's `sync_data` and `sync_all` issue `F_FULLFSYNC`, so each record costs two full flushes of the drive's cache. The live run below, on an internal SSD, shows no cost worth measuring; an external hard disk with eight streams was not tested.
- A journal is accepted for a different file of the same size when the metadata has no md5, since size and md5 are all that identify the version. The single-stream path has the same limit: it resumes any `.part` no longer than the file.
- A directory at the journal path is invalid and its removal fails, so every attempt of that file fails until it is removed by hand.
- A journal of this code damaged on disk so that it no longer parses is taken for an item file, and the `.part` beside it for a contiguous prefix; the md5 pass catches the holes, unless `--no-verify` is given. The atomic replace makes such damage a disk fault, not a crash.
- Power loss: the journal rename is not followed by a sync of the directory. After a power loss the older journal may come back, which claims less and is safe; the md5 pass is the backstop for data the file system lost.
- Other datanodes, locations and times of day than the measurement's.

## Out of scope

Upload parallelism (#22), datanode choice (#15), release work.

## Live check (2026-11-03)

One read-only run of the built CLI (debug build), with what were then the default flags (4 streams, no per-server cap), against the public file the measurement used, `ubuntu-18.04.5-desktop-amd64.iso` in item `ubuntu-18.04.5-desktop-amd64` (2,193,522,688 bytes), from the same client location: the first range request was answered 206, the file was fetched by 4 streams over 33 parts, the md5 matched, and the `.part` and `.part.json` were gone. 164.9 s from start to finish including the md5 pass, 13.3 MB/s, against the measurement's 12.55 MB/s mean for 4 connections.
