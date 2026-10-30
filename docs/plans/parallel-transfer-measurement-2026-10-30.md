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
  (As built, the check runs once before each full-file run, not before
  each request; the datanode checks are not checked. See Results.)

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
  20:00:02 to 20:46:51 UTC, and one follow-up request 20:57:10 to
  21:00:48 UTC. The two never overlapped; runs were 30 s apart.
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

Mean of the two repeats (100 MiB rows: one run each), against serial at the same part size:

| Part size | Concurrency | Mean MB/s | Speed-up |
|---|---|---|---|
| 16 MiB | 1 | 2.20 | 1.00 |
| 16 MiB | 2 | 3.00 | 1.36 |
| 16 MiB | 4 | 2.90 | 1.31 |
| 16 MiB | 8 | 2.33 | 1.05 |
| 100 MiB | 1 | 4.19 | 1.00 |
| 100 MiB | 4 | 4.06 | 0.97 |

Medians in these tables are recomputed from the per-part records (the
mean of the two middle values for an even count); the probe's own logged
`median_*` fields take the upper middle value and differ for runs 4 and 9.

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

Totals for the planned runs: 34 requests through `/download/` in 9 runs,
4 direct datanode requests, 20,010,139,648 bytes received (18.64 GiB), no
stream error, no retry.

Follow-up run, added after review. The single-stream baseline above ran
over HTTP/2 and the parallel runs over HTTP/1.1, so one more request was
made to compare on the same transport: the first 1 GiB
(1,073,741,824 bytes) through `/download/` on one HTTP/1.1 connection
(`single-h1` in the probe), 20:57:10 to 21:00:48 UTC. It was redirected to
`ia800807`, answered 206, and ran at 4.93 MB/s with no error, in line with
the HTTP/2 single streams (5.67 and 4.74 MB/s). Session total received,
with this run: 21,083,881,472 bytes (19.64 GiB).

### Datanodes (#15)

All 34 `/download/` requests were redirected to `ia800807.us.archive.org`,
the item's `d2` (and, in the item metadata read before the runs, its
`server` field). Direct Range requests for the first 64 MiB, each on one
HTTP/1.1 connection, with the `LOW` authorization header, on this public
file:

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
  were answered 200 and the 10 aborts 204. An unauthenticated `check_limit`
  call for the item before the runs (terminal output, not in the
  records) reported `over_limit: 0`.
- Datanode: no error status, no `Retry-After`, no stream reset across
  9 runs with up to 8 concurrent connections from one client.
- The answer to a part PUT took longer as concurrency rose (median wait
  1.4 to 2.5 s serial, 2.1 to 3.8 s at 4 concurrent parts, 4.1 to 6.0 s at 8). With the
  local socket-buffer caveat above, that is consistent with the
  concurrent parts sharing one uplink; the probe cannot distinguish this
  from queuing on IA's side.

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
16 MiB parts (1.36× at two, 1.31× at four, 1.05× at eight), consistent
with overlapping the wait for IA's answer after each part, and even the best
16 MiB run (3.16 MB/s) stayed below the serial 100 MiB run, so the
existing large default part size already recovers more than parallelism
does. IA-S3 accepted all 268 part PUTs on the first attempt and sent no 503
`SlowDown`, `Retry-After` or other throttling response at up to eight
concurrent parts; the serial 100 MiB run reached about the uplink rate,
and more concurrency did not raise total throughput and at eight
lowered it. The probe cannot show the case the
issue was opened for, a client whose single stream falls well short of
its uplink (a fast link over a long path), so the issue can be closed
under its own "if it doesn't pay off" criterion and reopened only with a
measurement from such a client; if it is ever built, the default stays 1.

### #16, parallel Range requests within one file

Build it, with the Range requests on separate connections, a default of
4 streams per file and a ceiling of 8. For a 2.04 GiB file on a
259.5 Mbit/s downlink, one stream averaged 5.21 MB/s over HTTP/2 and
4.93 MB/s over HTTP/1.1 (the follow-up run, first 1 GiB), while separate
HTTP/1.1 connections averaged 8.71 MB/s at 2 streams (1.67×), 12.55 MB/s
at 4 (2.41×) and 18.24 MB/s at 8 (3.50×), every md5 correct and no error
or throttling response from the datanode. Per-stream rate fell from 5.2
to 2.4 MB/s as streams were added, while the same datanode served
19.2 MB/s in total to eight connections, so the limit on one full-file
stream is neither the datanode's total capacity nor the local link;
short single-connection requests ran faster (8.4 to 18.2 MB/s for
64 MiB), so its cause (per-connection window, transfer length, or load
at the time) is not determined. Four ranges multiplexed on one HTTP/2
connection with reqwest's default settings gave 4.71 MB/s, no gain;
`ia-core`'s clients negotiate HTTP/2 with the datanodes, so the ranges
need separate connections (HTTP/1.1 was the transport tested; separate
HTTP/2 connections and tuned HTTP/2 windows were not). The default is 4
rather than 8 to keep one client's load on one datanode at four
connections until IA ops state what they accept; 8 measured faster and
drew no throttling here.

## What the probe could not determine

- Why one full-file stream runs at about 5 MB/s when short requests to
  the same node ran faster (see the #16 recommendation).

- Results for other locations, networks or times of day. All runs came
  from one Wi-Fi client between 19:20 and 21:01 UTC; repeats of the same setting
  differed by up to 16% of the faster run (download single stream 5.67
  versus 4.74 MB/s; upload 16 MiB ×4 3.16 versus 2.64 MB/s).
- Parallel uploads from a client whose single stream does not fill its
  uplink. Here one 100 MiB-part stream already exceeded the `networkQuality`
  uplink figure.
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

## Follow-up: single-connection profile and datanode resampling (2026-10-30)

Two questions left open above: whether the rate of one connection (about
5 MB/s for a full-file stream) is a limit that sets in after some bytes or a
rate that holds from the first byte, and whether the item's two datanodes
differ in a way that persists. Raw records:
[`parallel-transfer-probe/results/single-connection-profile.jsonl`](parallel-transfer-probe/results/single-connection-profile.jsonl);
probe:
[`parallel-transfer-probe/src/bin/single-connection-profile.rs`](parallel-transfer-probe/src/bin/single-connection-profile.rs).
MB/s is 10^6 bytes per second; MiB and GiB are binary.

### Method

The probe is a separate binary of the probe project. It sends GET requests
only, sends no `Authorization` header, does not read the IA-S3 keys, and
discards the bytes it receives (the full-file run computes their md5). Each
request uses a fresh client, so it starts on new connections, and follows
redirects by hand within `*.archive.org`, recording each hop. `/download/`
requests carry `cnt=0`. Every file request is a `Range: bytes=0-N` request
from the first byte of `ubuntu-18.04.5-desktop-amd64.iso`, so every request
read the same leading bytes; caching on the datanode was not controlled.
The first-session checks sent the `LOW` authorization header; these
requests send none. One `GET
/metadata/ubuntu-18.04.5-desktop-amd64` at the start gave `d1`
`ia600807.us.archive.org`, `d2` `ia800807.us.archive.org`, `server`
`ia800807.us.archive.org`, and the size and md5 used above.

- Measurement A, rate profile of one connection. Through `/download/` on
  HTTP/1.1, in this order: 64 MiB, 256 MiB, 1 GiB, the full file
  (2,193,522,688 bytes), then 64 MiB, 256 MiB and 1 GiB again; then 1 GiB
  on HTTP/2. At the first chunk past each 16 MiB boundary, the probe
  records the time, the cumulative bytes and the rate of the slice since
  the previous record, using the actual bytes (a slice is 16 MiB plus part
  of one chunk). Slice 1 is timed
  from the arrival of the response headers, so it excludes the redirect and
  the time to the first byte; each later slice is timed from the end of the
  one before.
- Measurement B, datanode resampling. Seven samples, due every 10 minutes
  from the first (0 to 60 minutes). Each sample is three 64 MiB requests,
  each on a fresh HTTP/1.1 connection, always in this order: directly to
  `d1` (`https://{d1}{dir}/{file}`), directly to `d2`, and through
  `/download/`, recording the node the redirect chose. Recorded: status,
  time to the first byte, rate (and slices, as in A).
- A and B never overlapped; the probe runs one request at a time. A
  transfer of measurement A started only if it would finish before the
  next sample was due at an assumed 4 MB/s plus the 30 s pause, or
  straight after a sample. Each file request started at least 30 s after
  the previous file request ended. Every sample began on time.
- Derived figures, computed from the per-slice records: "Wall" runs from
  the first request (including the redirect) to the last byte; "MB/s" is
  bytes over wall; "Steady" is the rate over the second half of the
  transfer, from the first slice boundary at or past half the bytes to the
  end; "Median of slices 2+" is the median slice rate after slice 1,
  including a final partial slice; "Slice 1 above all later" marks a
  transfer whose slice 1 rate is above every later slice of the same
  transfer. In the run 3 profile, each row's rate is its bytes over its
  time span, the first row timed from the arrival of the headers.
- Budget: the probe refuses any file request that would take the bytes
  received from file requests over 8,000,000,000.

### Environment

- Same client as above: Apple M1 Pro, macOS 27.2, Wi-Fi (`en0`), same
  location. Probe built with reqwest 0.13.5 (rustls), Rust 1.95.
- `networkQuality -s` at 21:21:43 UTC: downlink capacity 258.804 Mbit/s
  (32.4 MB/s), uplink capacity 76.110 Mbit/s, idle latency 18.5 ms
  (Apple's servers, not IA). The `networkQuality`, macOS and Rust figures
  are from terminal output, not the records.
- Run: first request 21:22:31 UTC, last byte 22:24:17 UTC. Measurement A
  ran from 21:24:41 to 21:57:35 UTC.
- 8 transfers (A) and 21 requests in 7 samples (B), every one answered 206
  and complete, no stream error, no retry. Every `/download/` request (8 in A, 7 in B) was
  redirected to `ia800807` (`d2`). The full-file run's md5 matched the
  published md5. Received: 7,495,122,944 bytes (6,085,836,800 in A,
  1,409,286,144 in B), plus the one metadata response.

### Measurement A: rate profile of one connection

All transfers through `/download/`, all served by `ia800807`.

| Run | Start (UTC) | Size | Transport | Wall (s) | MB/s | Slice 1 MB/s | Steady MB/s | Median of slices 2+ | Slice 1 above all later |
|---|---|---|---|---|---|---|---|---|---|
| 0 | 21:24:41 | 64 MiB | HTTP/1.1 | 5.1 | 13.09 | 12.70 | 22.40 | 20.09 | no |
| 1 | 21:25:16 | 256 MiB | HTTP/1.1 | 46.0 | 5.84 | 5.90 | 6.58 | 6.82 | no |
| 2 | 21:26:32 | 1 GiB | HTTP/1.1 | 235.2 | 4.56 | 6.90 | 4.78 | 4.87 | no |
| 3 | 21:34:42 | full (2.04 GiB) | HTTP/1.1 | 375.5 | 5.84 | 9.05 | 5.60 | 5.97 | no |
| 4 | 21:41:28 | 64 MiB | HTTP/1.1 | 16.1 | 4.18 | 6.06 | 4.61 | 4.39 | yes |
| 5 | 21:44:34 | 256 MiB | HTTP/1.1 | 46.3 | 5.80 | 11.45 | 5.52 | 5.85 | yes |
| 6 | 21:45:51 | 1 GiB | HTTP/1.1 | 220.9 | 4.86 | 7.12 | 5.00 | 5.00 | no |
| 7 | 21:54:21 | 1 GiB | HTTP/2 | 193.5 | 5.55 | 8.16 | 5.69 | 5.74 | no |

Time to the first byte, including the redirect, was 158 ms to 2,805 ms.
Slice rates within one transfer ranged widely: in the 1 GiB runs from 1.67
to 7.15 MB/s (run 2), 2.63 to 9.18 (run 6) and 3.18 to 9.86 (run 7).

Full-file run (run 3), rate per 256 MiB of bytes (16 slices each; the last
row is the remaining 3 slices). Time is from the request start.

| Bytes (MiB) | Time (s) | MB/s | Slowest slice | Fastest slice |
|---|---|---|---|---|
| 0 to 256 | 1.9 to 42.9 | 6.54 | 4.00 | 9.05 |
| 256 to 512 | 42.9 to 85.0 | 6.37 | 4.00 | 9.93 |
| 512 to 768 | 85.0 to 130.6 | 5.88 | 3.96 | 10.19 |
| 768 to 1,024 | 130.6 to 175.0 | 6.05 | 4.75 | 9.04 |
| 1,024 to 1,280 | 175.0 to 218.7 | 6.15 | 4.48 | 8.03 |
| 1,280 to 1,536 | 218.7 to 260.1 | 6.49 | 4.62 | 10.07 |
| 1,536 to 1,792 | 260.1 to 309.2 | 5.47 | 3.23 | 6.93 |
| 1,792 to 2,048 | 309.2 to 366.5 | 4.68 | 3.47 | 6.66 |
| 2,048 to 2,091.9 | 366.5 to 375.5 | 5.09 | 4.82 | 5.34 |

The first four slices of run 3 ran at 9.05, 5.87, 7.23 and 6.02 MB/s; the
per-slice values for every transfer are in the JSONL (`rec.slices`).

### Measurement B: datanode resampling

64 MiB from the first byte, each request on a fresh HTTP/1.1 connection.
Every request answered 206.

| Sample | Start (UTC) | d1 `ia600807` MB/s | d2 `ia800807` MB/s | d1 / d2 | Redirect node | Redirect MB/s |
|---|---|---|---|---|---|---|
| 0 | 21:22:31 | 6.34 | 4.04 | 1.57 | `ia800807` (d2) | 5.30 |
| 1 | 21:32:31 | 4.94 | 4.45 | 1.11 | `ia800807` (d2) | 5.28 |
| 2 | 21:42:31 | 7.39 | 4.85 | 1.52 | `ia800807` (d2) | 6.28 |
| 3 | 21:52:31 | 16.99 | 5.85 | 2.90 | `ia800807` (d2) | 13.19 |
| 4 | 22:02:31 | 14.26 | 5.91 | 2.42 | `ia800807` (d2) | 4.22 |
| 5 | 22:12:31 | 4.52 | 7.90 | 0.57 | `ia800807` (d2) | 3.75 |
| 6 | 22:22:31 | 3.62 | 4.63 | 0.78 | `ia800807` (d2) | 4.99 |

Each sample's three requests ran over 80 to 107 s from the first request
to the last byte; the `d2` request started 33 to 48 s after the `d1`
request (start times in the JSONL), so the two figures in a row are not
simultaneous. Median over the seven samples: d1 6.34 MB/s (3.62 to
16.99), d2 4.85 MB/s (4.04 to 7.90), redirect 5.28 MB/s (3.75 to 13.19).
Time to the first byte on the direct requests was 81 ms to 217 ms, except
one of 1,164 ms (d1, sample 5); through the redirect it was 263 ms to
1,271 ms.

### Interpretation

Measurement A shows no drop that persists within the first 1 GiB of the six
transfers of 256 MiB or more (runs 1, 2, 3, 5, 6 and 7). Only the single
full-file run went past 1 GiB, and its rate dropped after 1,536 MiB
(260 s) and did not recover; one run cannot attribute that drop to a
limit rather than to a change in conditions, so the data can neither show
nor rule out a limit that sets in beyond about 1.5 GiB. Seven of the eight transfers had a steady rate of 4.61 to
6.58 MB/s, and from slice 2 on every transfer of 256 MiB or more had a
median slice rate of 4.87 to 6.82 MB/s, with slices from 1.67 to
10.19 MB/s. In six of the eight transfers slice 1 (the first 16 MiB,
1.5 to 2.8 s) ran faster than the transfer's steady rate (6.06 to
11.45 MB/s against 4.61 to 5.69), but in only two of them (runs 4 and 5)
was it faster than every later slice, and runs 0 and 1 had a slower slice
1 than their steady rate. The full-file run had 5.88 to 6.54 MB/s per
256 MiB for its first 1,536 MiB and 4.68 to 5.47 MB/s for the remainder
(after 260 s); with one run, this cannot be told apart from a change in
conditions at the time. The one 1 GiB run on HTTP/2 (5.55 MB/s) was 14%
and 22% faster than the two on HTTP/1.1 (4.86, 4.56 MB/s); it ran last
and only once, so it cannot show whether the transport matters. A single connection is not held to about 5 MB/s at all times: run 0
reached a steady 22.40 MB/s, above the 19.22 MB/s that eight connections
reached in total in the first session, and of the 23 requests of 64 MiB
in this session, 19 ran at 3.62 to 7.90 MB/s and 4 at 13.09 to
16.99 MB/s, the 4 on both nodes and at three different times (21:24,
21:52 to 21:53, 22:02). The faster 64 MiB checks of the first session (up
to 18.24 MB/s, sent with the `LOW` header) are therefore not explained by
transfer length alone. Explanations the data cannot separate: a limit per
connection whose level varies over time (for example with load); no
limit, with the rate set by the TCP window and round-trip time on this
path under changing conditions (the round-trip time to the datanodes was
not measured); load on the datanode, varying from minute to minute (disk
reads are uncertain, since every request read the same leading bytes and
caching was not controlled); and the local Wi-Fi link. The faster slice 1
may come from a burst allowance or from buffering on the path, including
bytes that arrived with the response headers; the data cannot tell which.

Measurement B shows the two datanodes' requests, about 40 s apart in a
fixed order, differing by a factor of 0.57 to 2.90. The `d1` request was
faster by more than 40% in four samples (0, 2, 3 and 4, from 21:22 to
22:02 UTC) and the `d2` request faster in two (5 and 6, at 22:12 and
22:22 UTC); in sample 1 the two were within 11%, inside the 16% spread
between repeats seen in the first session. The variation within one node
was as large as the difference between them: `d2` served 5.85 MB/s
directly and 13.19 MB/s through the redirect 41 s later (sample 3), and
`d1` went from 14.26 to 4.52 MB/s between samples 4 and 5. Because the
order was fixed (`d1` first, after the longest idle gap), a difference in
one sample cannot be separated from the time between the requests. The
`/download/` redirect chose `d2`, the item's `server`, for all 15
requests, including the samples in which `d1`'s request was faster; `d2`
also received every `/download/` request, so the two nodes did not see
the same request history from this client. Seven samples of one 64 MiB
request per node, from one client, cannot show whether a choice made at
the start of a transfer would remain the better one for its duration, or
why the nodes differ (load, caching, or network path).

### Questions for IA operations

1. Is the rate of one connection from a datanode shaped, or bounded by TCP
   send-buffer or congestion-control settings on `ia600807` and
   `ia800807`? Here one connection usually ran at 4.6 to 6.6 MB/s (seven
   of eight transfers), but whole 64 MiB requests sometimes ran at 13 to
   17 MB/s, one transfer had a steady rate of 22.4 MB/s (run 0), and
   individual 16 MiB slices reached 26.5 MB/s; eight
   connections to the same node reached 17.25 and 19.22 MB/s in total.
2. How does `/download/` choose between an item's `d1` and `d2`? In this
   run it sent all 15 requests to the item's `server` (`d2`), while direct
   requests to `d1` were at times much faster (up to 2.9×, in requests
   about 40 s apart). Is a client allowed to fetch from either node
   directly, and is there a load signal it should use to choose?
