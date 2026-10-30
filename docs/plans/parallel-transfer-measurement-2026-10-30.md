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

## Results

Raw records: [`parallel-transfer-probe/results/upload.jsonl`](parallel-transfer-probe/results/upload.jsonl)
and [`parallel-transfer-probe/results/download.jsonl`](parallel-transfer-probe/results/download.jsonl).
MB/s is 10^6 bytes per second throughout.

### Environment

- Apple M1 Pro, macOS 27.2, on Wi-Fi (`en0`), one location, one session.
- `networkQuality -s` at 19:19 UTC: uplink capacity 28.376 Mbit/s
  (3.55 MB/s), downlink capacity 259.524 Mbit/s (32.4 MB/s), idle latency
  22.6 ms. This is a short test against Apple's servers; the fastest
  serial upload to IA (4.19 MB/s) exceeded it, so the practical uplink
  is about 4 MB/s.
- Upload runs: 2026-10-30 19:20:22 to 19:59:43 UTC. Download runs:
  20:00:02 to 20:46:51 UTC. The two never overlapped; runs were 30 s apart.
- Probe built with reqwest 0.13.5 (rustls), Rust 1.95.

### Upload (#22)

512 MiB file (536,870,912 bytes), IA-S3 over HTTP/1.1. Wall time runs from
the first part PUT to the last answer; it excludes initiate and abort.
Per-connection rate is the run's MB/s divided by its concurrency.

| Part size | Concurrency | Run | Wall (s) | MB/s | Per connection (MB/s) | Median part time (s) | Median answer wait (s) |
|---|---|---|---|---|---|---|---|
| 16 MiB | 1 | 0 | 263.2 | 2.04 | 2.04 | 7.7 | 2.47 |
| 16 MiB | 1 | 7 | 226.9 | 2.37 | 2.37 | 4.9 | 1.99 |
| 16 MiB | 2 | 1 | 186.8 | 2.87 | 1.44 | 11.1 | 1.79 |
| 16 MiB | 2 | 6 | 172.3 | 3.12 | 1.56 | 7.8 | 2.33 |
| 16 MiB | 4 | 2 | 170.2 | 3.16 | 0.79 | 18.7 | 2.14 |
| 16 MiB | 4 | 5 | 203.6 | 2.64 | 0.66 | 22.9 | 3.83 |
| 16 MiB | 8 | 3 | 233.8 | 2.30 | 0.29 | 48.4 | 4.10 |
| 16 MiB | 8 | 4 | 228.4 | 2.35 | 0.29 | 52.6 | 6.03 |
| 100 MiB | 1 | 8 | 128.0 | 4.19 | 4.19 | 22.3 | 1.35 |
| 100 MiB | 4 | 9 | 132.2 | 4.06 | 1.02 | 96.1 | 3.08 |

Mean of the two repeats, against serial at the same part size:

| Part size | Concurrency | Mean MB/s | Speed-up |
|---|---|---|---|
| 16 MiB | 1 | 2.21 | 1.00 |
| 16 MiB | 2 | 3.00 | 1.36 |
| 16 MiB | 4 | 2.90 | 1.32 |
| 16 MiB | 8 | 2.33 | 1.05 |
| 100 MiB | 1 | 4.19 | 1.00 |
| 100 MiB | 4 | 4.06 | 0.97 |

"Answer wait" is the time from the probe handing the last body slice to
the connection until the response arrived. It includes draining the
local socket buffer, so it overstates IA's own latency: run 7's median
"send" rate (6.19 MB/s) is above the uplink, which only a buffered send
can show. The split between send and wait is therefore indicative only;
the wall times are the measurement. Summed over a serial run, the waits
were 97 s of 263 s (run 0) and 122 s of 227 s (run 7) at 16 MiB parts,
and 19 s of 128 s at 100 MiB parts (run 8).

Totals: 268 part PUTs (8 runs of 32, 2 runs of 6), 5,368,709,120 bytes of
part bodies, every part accepted on its first attempt.

### Download (#16)

`ubuntu-18.04.5-desktop-amd64.iso`, 2,193,522,688 bytes. Wall time runs
from the first request to the last byte; the md5 check comes after.

| Mode | Streams | Run | Wall (s) | MB/s | Per-stream MB/s | md5 | Node |
|---|---|---|---|---|---|---|---|
| Single GET (HTTP/2) | 1 | 0 | 386.6 | 5.67 | 5.67 | ok | ia800807 |
| Single GET (HTTP/2) | 1 | 8 | 462.8 | 4.74 | 4.74 | ok | ia800807 |
| HTTP/1.1, own connections | 2 | 1 | 241.5 | 9.08 | 4.54, 5.06 | ok | ia800807 |
| HTTP/1.1, own connections | 2 | 7 | 263.3 | 8.33 | 4.21, 4.17 | ok | ia800807 |
| HTTP/1.1, own connections | 4 | 2 | 173.2 | 12.66 | 3.17 to 3.86 | ok | ia800807 |
| HTTP/1.1, own connections | 4 | 6 | 176.3 | 12.44 | 3.11 to 3.42 | ok | ia800807 |
| HTTP/1.1, own connections | 8 | 3 | 127.2 | 17.25 | 2.16 to 2.57 | ok | ia800807 |
| HTTP/1.1, own connections | 8 | 5 | 114.1 | 19.22 | 2.40 to 2.68 | ok | ia800807 |
| HTTP/2, one connection | 4 | 4 | 465.8 | 4.71 | 1.18 each | ok | ia800807 |

Mean of the repeats, against the mean single stream (5.21 MB/s):

| Mode | Streams | Mean MB/s | Mean per stream (MB/s) | Speed-up |
|---|---|---|---|---|
| Single | 1 | 5.21 | 5.21 | 1.00 |
| HTTP/1.1 | 2 | 8.71 | 4.50 | 1.67 |
| HTTP/1.1 | 4 | 12.55 | 3.32 | 2.41 |
| HTTP/1.1 | 8 | 18.24 | 2.45 | 3.50 |
| HTTP/2 | 4 | 4.71 | 1.18 | 0.90 |

The response versions recorded confirm the transport of each mode: the
single GETs and the HTTP/2 run negotiated HTTP/2 with the datanode, the
other runs HTTP/1.1. Time to the first byte, including the `/download/`
redirect, ranged from 343 ms to 6,240 ms; the slowest were the two
requests of run 7 (6.0 and 6.2 s).

Totals: 34 requests through `/download/` in 9 runs, 4 direct datanode
requests, 20,010,139,648 bytes received (18.64 GiB), no stream error, no
retry.

### Datanodes (#15)

All 34 `/download/` requests were redirected to `ia800807.us.archive.org`,
the item's `d2` and its `server` field. Direct Range requests for the
first 64 MiB, with the `LOW` authorization header, on this public file:

| When | Node | Status | MB/s |
|---|---|---|---|
| Start (20:00 UTC) | `ia600807` (d1) | 206 | 18.24 |
| Start | `ia800807` (d2) | 206 | 8.35 |
| End (20:46 UTC) | `ia600807` (d1) | 206 | 6.63 |
| End | `ia800807` (d2) | 206 | 10.08 |

Both nodes serve the file directly. The faster node changed between the
two checks, one sample each.

### Observed server behavior

- IA-S3: no 503, no `SlowDown`, no `Retry-After` and no S3 error code on
  any of the 268 part PUTs, at up to 8 concurrent parts. The 10 initiates
  were answered 200 and the 10 aborts 204. `check_limit` before the run
  reported `over_limit: 0`.
- Datanode: no error status, no `Retry-After`, no stream reset across
  9 runs with up to 8 concurrent connections from one client.
- The answer to a part PUT took longer as concurrency rose (median wait
  1.4 to 2.5 s serial, 2.1 to 3.8 s at 4 concurrent parts, 4.1 to 6.0 s at 8). With the
  local socket-buffer caveat above, that is consistent with the
  concurrent parts sharing one uplink rather than with IA-side throttling.

### Test item end state

`ia-cli-parallel-probe-20261030` (`test_collection`, `mediatype:data`)
holds one file, `README.txt` (161 bytes), plus IA's generated
`_files.xml`, `_meta.xml` and `_meta.sqlite`. The probe's closing listing
found 0 unfinished multipart uploads, and `ia-cli upload cleanup
ia-cli-parallel-probe-20261030` reports "No incomplete multipart uploads".
No multipart upload was completed.

## Recommendations

### #22, parallel multipart parts

Do not build it. On this client, with an uplink near 4 MB/s, one stream
at the default 100 MiB part size reached 4.19 MB/s and four concurrent
100 MiB parts reached 4.06 MB/s: no gain. Concurrency helped only at
16 MiB parts (1.36× at two, 1.32× at four, 1.05× at eight), by
overlapping the wait for IA's answer after each part, and even the best
16 MiB run (3.16 MB/s) stayed below the serial 100 MiB run, so the
existing large default part size already recovers more than parallelism
does. IA-S3 accepted all 268 part PUTs on the first attempt with no 503
`SlowDown` or `Retry-After` at up to eight concurrent parts, so throttling
is not the obstacle; the uplink is. The probe cannot show the case the
issue was opened for, a client whose single stream falls well short of
its uplink (a fast link over a long path), so the issue can be closed
under its own "if it doesn't pay off" criterion and reopened only with a
measurement from such a client; if it is ever built, the default stays 1.

### #16, parallel Range requests within one file

Build it, with each Range request on its own HTTP/1.1 connection, a
default of 4 streams per file and a ceiling of 8. For a 2.04 GiB file on
a 259.5 Mbit/s downlink, one stream averaged 5.21 MB/s, while separate
HTTP/1.1 connections averaged 8.71 MB/s at 2 streams (1.67×), 12.55 MB/s
at 4 (2.41×) and 18.24 MB/s at 8 (3.50×), every md5 correct and no error
or throttling response from the datanode. Per-stream rate fell only from
5.2 to 2.4 MB/s as streams were added, and the same datanode served
19.2 MB/s to eight connections, so the single-stream rate is a
per-connection limit, not the datanode's or the local link's. Four
streams multiplexed over one HTTP/2 connection gave 4.71 MB/s, no better
than one stream; `ia-core`'s clients negotiate HTTP/2 with the
datanodes, so an implementation that sends concurrent ranges through the
existing client would gain nothing, and the range requests need an
HTTP/1.1-only client. The default is 4 rather than 8 to keep one
client's load on one datanode at four connections until IA ops state
what they accept; 8 measured faster and drew no throttling here.

## What the probe could not determine

- Results for other locations, networks or times of day. All runs came
  from one Wi-Fi client in one 87-minute window; repeats of the same setting
  differed by up to 16% of the faster run (download single stream 5.67
  versus 4.74 MB/s; upload 16 MiB ×4 3.16 versus 2.64 MB/s).
- Parallel uploads from a client whose single stream does not fill its
  uplink. Here one 100 MiB-part stream already reached the uplink.
- IA's own latency per part. The answer wait includes the local socket
  buffer draining, and the probe has no view of the wire.
- Behavior at more than 8 streams or parts, or with `-j` files multiplied
  by per-file streams; whether other datanodes or the IA-S3 front end
  throttle under heavier load; and what per-client connection count IA
  ops consider acceptable.
- The cost of completing a multipart upload of many parts, since every
  upload was aborted.
- Whether direct datanode URLs work for access-restricted or dark items
  (#15); only a public file was tested.
- Whether the faster datanode stays faster over a transfer (#15): the two
  direct checks per node, 46 minutes apart, disagreed.
