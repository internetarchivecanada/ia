# Parallel Range downloads within one file (#16)

**Goal:** `ia download` fetches one large file over several HTTP Range requests at once, each on its own HTTP/1.1 connection, and an interrupted download resumes per part.

**Evidence:** [`parallel-transfer-measurement-2026-10-30.md`](parallel-transfer-measurement-2026-10-30.md). For a 2.04 GiB file, one connection averaged 5.21 MB/s; separate HTTP/1.1 connections averaged 2.41× that at 4 streams and 3.50× at 8; four ranges multiplexed on one HTTP/2 connection gained nothing (0.90×). No error or throttling response from the datanode at up to 8 connections.

## Decisions (maintainer, recorded in the issue and the task)

- One HTTP/1.1 connection per stream, up to 8 per file. The connection contract agreed with IA operations on 2026-11-05 (see "Contract") replaces the earlier flags: the cap of 6 per datanode is a constant, every connection counts, and the split is automatic.
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

### Contract

Agreed with IA operations on 2026-11-05. The user-facing documentation carries these four points verbatim:

- At most 6 connections to any one datanode from one machine, across all ia-cli processes (running several copies, with GNU parallel or otherwise, shares the same 6), counting every connection, whole files and byte ranges of one file alike.
- The 6 is per datanode, not a total: a run that pulls from three datanodes may hold up to 6 on each.
- Multi-connection per file is on by default but invisible to the user, and only fills connections the budget already allows. One 2 GiB file is fetched as 6 byte ranges; an item with 50 files downloads 6 files at a time, none split; a batch across many items spreads across their datanodes, none split. A node never sees more than 6 either way.
- If a datanode answers 429 or any 5xx, ia-cli honours Retry-After and then halves its connection budget to that node for the rest of the run (6 → 3 → 2 → 1), dropping byte-range splits before whole files. The budget grows back by one for every minute without a 429 or 5xx from that node, never above 6.

The one change users may notice: a single item with 8 or more files used to download 8 files at once (`-j 8`), all from the same datanode; it now runs 6 at a time to that node.

### Choosing the path

`download_file` keeps its skip, dry-run and path checks. The parallel path is taken when all of these hold: the file's metadata has a size; the size is at least 256 MiB; the file is not `{identifier}_files.xml`; and `DownloadOpts::connections_per_file` is not `Some(1)` (the hidden `--connections-per-file 1`, which never splits). Otherwise the single-stream path runs as before.

### Automatic split, files before ranges

The number of streams a large file runs is fixed when it starts, after its first range request has been answered with a 206 (the first request holds one slot; see below for the server):

- **Automatic** (the default, `connections_per_file` `None`): `min(8, parts still to fetch, 1 + room)`, where `room` is the slots free on the file's datanode after every other file about to start has one. Free means a permit of this process's live budget for that node is available and one of the slot files is unlocked (counted at that moment, without taking them). "About to start" means every file in this process that has started (its `-j` place taken, its checks done) and does not hold a slot yet; their datanode is not known until their first request is answered, so the count covers files on any datanode, which can only make a split smaller. A file with no room runs one stream: it still fetches its parts one after another through the journal.
- **Pinned** (the hidden `--connections-per-file N`, 2 to 8): `min(N, 8, parts still to fetch)` streams, each waiting for its slot.

There is no rebalancing while a file runs, other than shedding under a back-off (below). Each stream takes a slot for every part request and gives it back when the part ends, so a file waiting for its first slot gets the next one free, before a stream's next part: files go before ranges. With the three examples of the contract:

- **One 2 GiB file** (33 parts), nothing else running: its first request takes one slot, nothing else is about to start, five more are free: `min(8, 33, 6)` = 6 streams.
- **An item with 50 files** under `-j 8`: eight files have their places, six hold slots and two wait. A file of 256 MiB or more among them finds `room` = 0 (two files waiting) and runs one stream; the item downloads 6 files at a time, none split.
- **A batch across many items** under `-j 8`: files keep starting, so `room` stays at 0 and no file splits; the files spread over their items' datanodes, at most 6 on each.

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

### Slots

- **What is counted.** Every request a download sends: the single-stream request of each file under `-j`, and every range request of a split file, the first one included. Metadata requests and other commands are not counted.
- **Which server.** The one the request lands on: the datanode after the `/download/` redirect, or the request's own host when there is no redirect. A slot is taken on a host before each request to it is sent, because whether an answer is a redirect is known only once it arrives; on a redirect that slot is given back before a slot on the next host is taken, so a request never holds two slots and never waits for one while holding another. The redirect hop therefore counts against archive.org's own 6 while it runs. The key is the URL's authority (host, and port when one is given), with one exception below.
- **An item's two datanodes are one server.** Added after review (2026-11-05): an item is stored on a pair of datanodes, `d1` and `d2` in its metadata, and `/download/` may redirect to either, so a file's streams can land on both. The pair is one server for the cap: when the item's metadata names both, a request that lands on either host takes its slot under one key, the two host names lowercased, sorted and joined with `+` (`ia600807.us.archive.org+ia800807.us.archive.org`), so the two together never see more than 6. When the metadata lacks either name, or a request lands on a host that is neither, the key is that host's authority as above. Downloads of single files from outside an item's metadata (`download_file` with default options) key by host.
- **How long.** From before the request is sent until its response and body are dropped or the body has ended. A download gives its slot back before any wait (the backoff after a dropped body, `Retry-After`, the standard backoff), before the `.part` is hashed after a 416, and before its next request.
- **When the server is full.** The request waits for a slot; nothing is rejected. A range stream told to stop while it waits stops waiting.
- **Shape.** `ServerSlots`, a handle to a table of one entry per server (a semaphore holding the live budget), carried in `DownloadOpts::server_slots`. Clones share the table; the CLI makes one for the run. The cap, `MAX_CONNECTIONS_PER_SERVER` = 6, is a constant; a different cap exists only inside the crate, for tests.
- **What is bounded.** Open sockets, not only transfers in flight. Every download request asks the server to close the connection after its answer (`Connection: close`), and the transport that carries them keeps no idle connections (`pool_max_idle_per_host(0)`), so a socket lives only as long as the slot that sent its request, and active plus idle sockets to a server never exceed its 6. Both download paths, one stream and split, use that transport. Added after review (2026-11-05). The cost is a TCP and TLS handshake per request: per file for whole files, and per 64 MiB part for split ones, which is how the Python `ia` behaves. A pool sized to the cap was the other choice and does not hold the bound: hyper returns a connection to the pool after the body ends, asynchronously, so a new request can open a seventh socket while a sixth is on its way back, and its pool is per host, so an item's two datanodes would keep idle sockets on each.

### Back-off

On a 429 or any 5xx from a datanode, that node's live budget in this process halves for the rest of the run, rounded up and never below 1, which gives the contract's 6 → 3 → 2 → 1 (integer division would go from 3 straight to 1); the request is retried as before, after `Retry-After` or the standard backoff. Both kinds of answer count: a 429 asks the client to slow down, and a 5xx is the server failing under its load; either is a signal of distress, and fewer connections is the response to both.

- **When.** The budget halves when the answer arrives, not after the wait: during the wait the node sees fewer connections either way, and this way none start under the old budget.
- **Once per round.** Each slot remembers the budget it was taken under. Only an answer to a request sent under the current budget halves it again, so six connections answered 429 together halve 6 to 3 once, not to 1.
- **Regrowth.** Every 60 s with no 429 or 5xx from the node, the budget grows by one, up to 6. A 429 or 5xx at the floor of 1 restarts the 60 s.
- **The semaphore.** The budget is the permit count of the node's semaphore: a drop forgets the permits that are free and records the rest as owed, forgetting them as connections return them; growth pays back an owed permit first, then adds one. The slot files are not touched: each process keeps its own live budget.
- **Shedding.** When the budget falls below the connections in use, range streams of split files stop first: such a stream records its progress, puts its part back on its file's queue for the surviving streams, and ends, giving its slot back (to be forgotten). A file keeps at least one stream. Whole-file connections are never cut; their slots are forgotten as they end. A file starting under a reduced budget splits only into the room that budget leaves.
- **Notice.** One line per node per drop on stderr, `server ia800807 asked us to slow down; using 3 connections` (the first label of an archive.org host name, else the whole authority), and with `--json` one line on stdout instead, `{"event":"slow_down","server":…,"status":429,"connections_before":6,"connections_after":3}`. Library callers get the same through `ServerSlots::with_notices`.

### Across processes

Added after review (decided 2026-11-05): the cap of 6 holds per datanode across every `ia` process of one user on one machine, because IA operations' concern is several copies of `ia` running at once. It is a cap per server, not one total for the machine.

- **Slot files.** For each server there are N slot files, `<root>/connections/<server>/slot-<i>` for `i` from 0 to N−1, where N is the cap. `<root>` is `$IA_CACHE_DIR` when it is set (so tests, and users whose home directory is on NFS without locks, can choose a local directory), and otherwise `ia` under the per-user cache directory: `$XDG_CACHE_HOME`, else `~/.cache`, on Linux and other Unix systems; `~/Library/Caches` on macOS; `%LOCALAPPDATA%` on Windows. This follows the `dirs` crate's `cache_dir()` for these variables, written out rather than adding that crate (the crate had no cache directory before); unlike `dirs` it does not fall back to the password database when `HOME` is unset, or to the known-folder API on Windows, as the crate's config lookup does not either. An empty variable counts as unset; a relative `IA_CACHE_DIR` is made absolute from the current directory once, so processes started in different directories name the same files. `<server>` is the slot table's key, the URL authority or an item's datanode pair, with every character other than a lowercase letter, a digit, `.` and `-` written as `%XX`, and dots too when the whole name is dots, so `127.0.0.1:8080` becomes `127.0.0.1%3A8080`, a pair `ia600807.us.archive.org%2Bia800807.us.archive.org`, and no name can step outside the directory. The files are created as they are first needed, so a server has as many as were ever held at once (at most the largest cap used), and never deleted. A slot file another user created and this one may not write is opened read-only, which is enough to lock it.
- **Holding a slot.** A connection holds a slot by taking an exclusive, non-blocking advisory lock on one of the files (`flock` on Unix, `LockFileEx` on Windows, through the `fs4` crate's exclusive `try_lock`) for exactly as long as it holds the in-process slot. Dropping the slot unlocks the file, closes it, and only then gives the in-process permit back: the next stream of the process, woken by the permit, finds the file free, and on Windows the lock goes at once rather than whenever the system gets to a closed handle. The kernel also releases it when the process exits or crashes, so there is no stale lock to clean up and nothing is read from a file's existence or content.
- **Waiting.** The in-process semaphore comes first, so a process only looks at the files for a slot it could use. With its permit, it tries its files in order; when every one is locked by another process it waits, 50 ms at first, doubling to at most 500 ms, and tries again. Each look runs on tokio's blocking pool, since on NFS a lock is a network round trip and a hung mount must not stall the async workers. After 10 s of waiting, one warning per server says the slots are held by other `ia` processes, since a wait is otherwise silent at the default log level. The wait is part of the request, so a range stream told to stop stops waiting. It is not fair between processes: a freed slot goes first to the same process's next waiting stream, so a second process may wait until the first has nothing more queued for that server.
- **Fallback.** If the directory cannot be created, a slot file cannot be opened, or locking fails with an error other than "would block" (a read-only file system, NFS without lock support, running out of file handles: each held slot keeps one file open), one warning naming the path is logged and the cap applies within the process only for the rest of the run. A download never fails because of the slot files.
- **Different caps.** Every `ia` process uses the same 6 files per server. A table with another cap (only possible inside the crate, for tests) shares the files and tries only its own first N. A process's live budget, lowered by a back-off, limits how many slots it asks for, not which files it tries.
- **Shape.** `ServerSlots::default()` keeps the cap within the process, so library callers and tests do not write to the user's cache unless they ask to. `ServerSlots::across_processes(dir)` adds the slot files under `dir`, and `download::default_slot_dir()` returns `<root>/connections`, or `None` when no cache directory can be named. The CLI uses both; with no cache directory it warns once and keeps the cap per process.
- **Limits.** Only `ia` processes that use the slot files count: another program, or an older `ia`, is not seen. Processes of different users have different cache directories, so they do not share a cap; a shared `IA_CACHE_DIR` should stay private, since anyone who can write to it can hold the slots. Home directories on NFS without lock support fall back to the per-process cap; on NFS whose locks work, every machine using that home shares one cap, and `IA_CACHE_DIR` on a local disk keeps it per machine.

### Completion

When every stream has finished and every part is complete: the `.part` length must equal the metadata size; the journal is deleted; when the file is verified (`DownloadOpts::verify`, an md5 in the metadata), the md5 of the `.part` is computed in one pass from disk (reported as `Verifying`, about 2 s per GB) and compared; the existing `finish_part` renames, keeps a mismatch as `.md5-mismatch`, removes an older one on success, and sets the mtime from the first response's `Last-Modified`.

### Progress

`Starting` with the bytes already on disk, then `Downloading` with the sum over all streams, every 256 KiB, then `Verifying` and `Complete` as today. No new status.

## Public API changes in `ia-core`

- `download::DownloadOpts::connections_per_file: Option<usize>` (new field; the struct is `#[non_exhaustive]`). Default `None`, the automatic split; `Some(1)` never splits; `Some(n)` pins `n` streams (values above 8 are treated as 8).
- `download::DownloadOpts::server_slots: ServerSlots` (new field) and `download::ServerSlots` (new type): `Default` (within the process), `across_processes(dir)` (with the slot files), `with_notices(f)` (a callback for each budget drop), `max()`.
- `download::BudgetDrop` (new, `#[non_exhaustive]`): `server`, `status`, `before`, `after`, the argument of that callback.
- `download::default_slot_dir()` (new function), the slot files' default directory.
- `download::MAX_CONNECTIONS_PER_SERVER` (6) and `download::MAX_CONNECTIONS_PER_FILE` (8), new constants.
- `IaError::RangeFailed { file, start, end, attempts, source }`, new variant (the enum is `#[non_exhaustive]`); not retryable; JSON code `range_failed`.
- Behaviour: `download_file` (and the item functions that call it) holds a slot on its datanode for every request, splits a file of 256 MiB or more into the room its datanode has, may leave `<name>.part.json` beside `<name>.part` after an interrupted download, and the single-stream path reads and settles such a journal.

## CLI

No new visible flag. `--connections-per-file <N>` (1 to 8) is hidden: it pins the number of streams for a large file, `1` meaning never split, for debugging and support; it is not in `docs/usage.md`. `-j, --jobs` keeps its name and default (8); its help for download says "Files downloaded at once across all servers (default 8). Any one server gets at most 6 connections, so files on the same server may wait." `-j` is a global flag, so this is a paragraph of its long help, and the help for the other commands is unchanged. The `docs/usage.md` section "Connections" carries the contract.

## Tests (written before the implementation)

In-crate tests in `ia-core/src/download/parallel.rs`, with the part size and threshold shrunk through a `cfg(test)` override, as the stall tests shrink the window. wiremock serves ranges from a byte buffer; a raw loopback listener serves the stall, drop and concurrency cases.

- Grid and journal: part layout, prefix mapping, contiguous prefix, journal round trip, every validation rule, atomic replace (no temporary left; a link at the journal or the temporary path is replaced, not followed).
- A file at the threshold is fetched in N closed ranges, assembled, verified; no `.part` or journal left.
- The streams run at once on separate connections (the server holds every response until N connections are open).
- The hidden pin of 1, and a file below the threshold: one plain GET.
- 200 to the first request: single-stream fallback, correct file, no journal.
- `Content-Range` total differs from the metadata: `ServerSizeMismatch`.
- Resume: only parts not complete are requested, each from `start + done`; a stale journal (other size or md5), a corrupt journal, and a journal claiming more than the `.part` holds restart from 0; a `.part` without a journal is adopted as a prefix.
- Single-stream path with a journal: the `.part` is truncated to the contiguous prefix, the journal removed, the resume `Range` starts at the prefix.
- Per stream: a 503 with `Retry-After` on one range is retried by that stream and the file completes; a spent budget gives `RangeFailed` naming the range, keeps the `.part` and journal, and is not retried by the per-file loop; a later run fetches only the missing parts; a body-stream error re-requests the rest of the part; a stall re-requests the rest of the part; a spent stall budget gives `DownloadStalled`; a 404 on one range fails at once.
- md5 mismatch after assembly: `ChecksumMismatch`, `.md5-mismatch` kept, journal gone.
- `--count-views`: only the first request lacks `cnt`.
- Progress: the reported bytes only grow and end at the file size.
- `IaError::RangeFailed`: Display, not retryable, JSON.

- Contract, counted on loopback servers: one large file alone splits into exactly 6 with the right bytes; 50 small files under `-j 8` never exceed 6 and send no `Range`; 3 large files under `-j 8` reach 6 in all, and every file's first request comes before any file's second part (files before ranges); three servers reach 6 each, 18 in all with enough `-j`, and no more than `-j` in all; jobs and ranges share one server's 6.
- Back-off: the budget arithmetic (6 → 3 → 2 → 1, floor 1, one drop per round, growth by one per interval up to 6, owed permits); one 429 holds later requests to 3 and a 503 does the same; a split file under a drop sheds range streams first and completes with the right bytes; the notice callback runs once per drop.
- An item's datanode pair: the key is the two names sorted and joined, whatever the order of `d1` and `d2`, and either host maps to it; with either name missing there is no pair. One item, one large file, eight streams pinned, a front server whose redirect alternates between two nodes: never more than 6 open on the two together. Every download connection, for whole files and split ones, carries one request and asks to close.
- Across processes: slot file names per server (a port, an IPv6 address, an all-dots name); two tables sharing one directory, as two processes would, never hold more than the cap together; a table with a larger cap uses files beyond a smaller one's; a slot file this user may not write is locked read-only; a slot directory that is a file falls back to the per-process cap without failing.

CLI tests in `ia-cli/tests/`: the `-j` help and no visible connection flags; the hidden `--connections-per-file` range check; a 256 MiB body splits into its four parts by default and is one plain GET with the hidden `--connections-per-file 1`; 50 small files never exceed 6 connections and send no `Range`. Across processes: two `ia download` processes never exceed 6 together; a process killed while holding all 6 slots frees them and a second process then completes; a cache path that cannot hold slot files warns once and keeps the process cap. The back-off notice, once per drop, in text and in `--json`.

One live read-only run against the public file used by the measurement, to confirm the 206 path end to end.

## Not covered

- The cap does not cover `--zip-member`, `--zip-list` or the other commands' requests.
- The request to archive.org itself, before the `/download/` redirect, goes to a front-end service, not a datanode, and is left outside the contract. A run of the earlier code was seen holding 7 keep-alive connections to archive.org. It now takes a slot on archive.org's own 6 while it runs and closes its connection with its answer, as every download request does, but nothing was measured against that service.
- The handshake cost of a new connection per request (see "What is bounded") was not measured on a live datanode.
- The 10 s wait warning has no automated test; it was read in code only.
- The redirect guard admits only the configured host and `*.archive.org`; in this crate's own tests it also admits `127.0.0.1`, so a front server can redirect between two loopback servers on different ports. The CLI tests cannot, so the pair key is tested in `ia-core` only. Three items on three pairs through `--itemlist` are not tested as such; three servers reached directly are, in `ia-core`.
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
