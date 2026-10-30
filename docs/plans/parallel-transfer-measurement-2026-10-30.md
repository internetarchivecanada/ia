# Parallel transfer measurement (2026-10-30)

Input for #22 (upload multipart parts in parallel) and #16 (parallel Range
requests within one large download), with datanode observations for #15.
Nothing in `ia-core` or `ia-cli` changes. The probe lives in
[`parallel-transfer-probe/`](parallel-transfer-probe/), a standalone cargo
project outside the workspace.

## Plan

This section was written and committed before any measurement ran.

### Questions

1. Upload (#22): does uploading the parts of one multipart upload
   concurrently raise throughput to IA-S3, and by how much at 2, 4 and 8
   concurrent parts? Does IA-S3 throttle (503 `SlowDown`, `Retry-After`)
   as concurrency rises? Does each connection's throughput fall as more run?
   How much of a part's time is the wait for IA's answer after the body is
   sent (measured at a median of about 3.9 s for 1 KiB parts in the #40
   probe), and does concurrency hide it?
2. Download (#16): do 2, 4 and 8 concurrent Range requests over disjoint
   ranges of one large file beat one stream, and by how much? Does it matter
   whether the requests run on separate HTTP/1.1 connections or as HTTP/2
   streams on one connection?
3. Datanodes (#15): which node does the `/download/` redirect choose each
   time, and does a direct request to the item's other node (`d1`/`d2`)
   work?

### Protocol facts the probe reproduces

From `ia-core/src/upload/multipart.rs` and `ia-core/src/download/mod.rs`:

- IA-S3 host `https://s3.us.archive.org`; `Authorization: LOW {access}:{secret}`.
- Initiate: `POST /{id}/{key}?uploads`, `Content-Length: 0`; the upload ID is
  `<UploadId>` in the XML answer.
- Part: `PUT /{id}/{key}?partNumber={N}&uploadId={ID}` with an explicit
  `Content-Length` (IA-S3 refuses chunked transfer encoding) and
  `Content-MD5` (base64 of the part's md5), as `ia-core` sends by default.
- Abort: `DELETE /{id}/{key}?uploadId={ID}`. Listing: `GET /{id}?uploads`.
- Download: `GET https://archive.org/download/{id}/{file}?cnt=0` (the
  `cnt=0` parameter keeps the request off the public view counter), redirects
  followed by hand to `*.archive.org` with the `Authorization` header kept,
  as `fetch_response` does.
- User-Agent carries the S3 access key in the platform comment, as `ia-core`
  sends, so archive.org attributes the traffic to the account.
- `ia-core`'s reqwest clients enable HTTP/2. A pre-check on 2026-10-30 found
  that IA-S3 answers over HTTP/1.1 only and that the datanode
  (`ia800807.us.archive.org`) and `archive.org` negotiate HTTP/2. Concurrent
  uploads therefore always use separate connections; concurrent downloads
  through one `ia-core` client would share one TCP connection to the
  datanode unless the client is told otherwise.

### Upload method

- Test item `ia-cli-parallel-probe-20261030` in `test_collection`
  (`mediatype:data`), created by the probe with one small single PUT
  (`README.txt` describing the probe, `x-archive-auto-make-bucket:1`,
  `x-archive-queue-derive:0`). The probe waits until the metadata API lists
  the item before the first multipart initiate, because an abort seconds
  after an item-creating initiate has been observed to answer 404
  `NoSuchBucket`.
- Test file: 512 MiB of random bytes from `/dev/urandom`, generated once,
  outside the repository.
- Part size 16 MiB (32 parts). The repository default,
  `DEFAULT_PART_SIZE`, is 100 MiB, which gives 6 parts for this file: too
  few to keep 8 workers busy. Two extra runs at 100 MiB (concurrency 1 and
  4, 6 parts) show whether the result holds at the default size.
- Concurrency 1, 2, 4, 8, each a fresh multipart upload of the same file.
  Each level runs twice, in the order 1, 2, 4, 8, then 8, 4, 2, 1, so a
  trend over time does not line up with concurrency.
- Workers take the next part number from a shared queue. Each part is read
  from disk before its request starts; disk time is not in the request
  timing.
- Recorded per part: start, the instant the last body byte was handed to
  the connection, the instant the answer arrived, status, `Retry-After`,
  S3 error code. Per run: wall time from the first part PUT to the last
  answer, MB/s (10^6 bytes per second), the per-part send rate, and the
  wait for the answer.
- A 503 or other non-2xx part answer is recorded, waited out
  (`Retry-After` when given, otherwise 5 s) and retried, up to 5 attempts.
  The retry counts toward the byte budget.
- No upload is completed. Every upload ID the probe initiates is aborted
  at the end of its run, and again at the end of the program if any abort
  failed. The probe ends by listing the item's unfinished uploads and
  reports the count; `ia-cli upload cleanup ia-cli-parallel-probe-20261030`
  confirms it independently.
- Budget: 10 runs, about 5 GiB of part bodies. The probe refuses to start
  a run that would take the session total over 6 GiB.

### Download method

- File: `ubuntu-18.04.5-desktop-amd64.iso` in item
  `ubuntu-18.04.5-desktop-amd64`, 2,193,522,688 bytes (2.04 GiB), md5
  `44343bc1373c6dc826ebb58839155c39`. Chosen because it is public, in the
  1 to 4 GiB range, a single original file, stable content with a published
  md5, and its item has two distinct datanodes (`d1`
  `ia600807.us.archive.org`, `d2` `ia800807.us.archive.org`).
- Single stream: one `GET` without `Range`, as `ia download` sends.
- Parallel: the file is split into N contiguous ranges of equal size (the
  last takes the remainder), one per worker, each fetched with
  `Range: bytes=a-b` through its own `/download/` redirect (so every request
  records the node it landed on), and written at its offset into one
  pre-sized file.
- Two transport modes. HTTP/1.1: each worker has its own connection
  (the client is restricted to HTTP/1.1). HTTP/2: all workers share one
  client, which multiplexes them on one connection to the datanode, as an
  unmodified `ia-core` client would.
- Runs: single stream twice; HTTP/1.1 at N = 2, 4, 8, twice each (order
  2, 4, 8, then 8, 4, 2); HTTP/2 at N = 4 once. Nine full downloads,
  about 18.4 GiB.
- Datanode check (#15): at the start and at the end, a 64 MiB Range
  request directly to `d1` and to `d2` (`https://{node}{dir}/{file}`),
  recording status and throughput. About 256 MiB.
- md5 of the assembled file checked after every run.
- Recorded per request: node, HTTP version, time to the first byte,
  bytes, duration, MB/s. Per run: wall time, MB/s, md5 match.
- Budget: the probe refuses to start a request that would take the
  session total over 20 GiB.

### Environment and pacing

- One machine, one network, one session; nothing else transfers to or from
  IA during the runs. Uploads and downloads never overlap; 30 s pause
  between runs.
- Local link capacity is measured once with macOS `networkQuality`
  (Apple's servers, not IA), to tell an IA-side limit from a local one. The
  single-stream runs are the uplink and downlink baselines to IA.

### What the plan cannot answer

- One client location and one time window. Results from elsewhere, or at
  another hour, may differ; the #16 report of 0.1 to 1 MB/s per stream came
  from another location.
- One datanode pair for the download and one IA-S3 front end for the upload.
- Whether IA ops consider N connections per client acceptable. The probe
  can show whether IA throttles; it cannot show what IA prefers.
- The cost on IA's side of a completed multipart upload (assembly), since
  no upload is completed.
