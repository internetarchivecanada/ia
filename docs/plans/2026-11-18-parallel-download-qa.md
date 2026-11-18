# Parallel downloads: rebased PR #121 against v0.27.0 (2026-11-18)

A second measurement of the parallel download work in PR #121 (#16),
after the branch was brought up to date with main (0.27.0) and IA
operations agreed to the connection contract on 2026-11-18. It does not
replace the 2026-11-05 report
([`2026-11-05-parallel-download-qa.md`](2026-11-05-parallel-download-qa.md)),
whose numbers stand as recorded. Downloads only, from public items;
nothing was written to any archive.org item. The harness and the raw
records are in
[`parallel-download-qa-2026-11-18/`](parallel-download-qa-2026-11-18/).

## Summary

From one client (Apple M1 Pro, Wi-Fi), 2026-11-18 between 17:06 and
17:53 UTC, three rounds, the two builds interleaved in every case:

- **One 2.04 GiB file:** 24.94 MB/s against 9.22 MB/s for v0.27.0, a
  median of 2.7 times as fast (paired ratios 2.67, 2.72, 3.15), over 6
  connections against 1.
- **An item with a few large files:** 22.33 against 19.07 MB/s, 1.06 to
  1.32 times.
- **An item with many small files:** 24.67 against 24.83 MB/s, the same
  (0.99 to 1.14 times). v0.27.0 opened 8 connections to the datanode
  here; the rebased build held 6.
- **Two processes at once on one datanode:** 25.95 against 15.30 MB/s,
  1.44 to 1.83 times. The two processes together held at most 6
  connections to the datanode pair.
- **Client cost:** CPU time was lower than v0.27.0's in every case but
  many small files (equal): 20.4 against 25.4 s for the 2.04 GiB file.
  Peak resident memory was higher, 28 to 30 MiB against 19 to 30 MiB.
- **Many tiny files are slower (added later the same day, build
  `cb2d36e`):** an item of 1,267 files of a few kilobytes took a median
  of 26.0 s against 14.3 s for v0.27.0 (48.7 against 88.4 files per
  second, 0.55 times), and 497 JPEGs of about 190 KB took 22.6 against
  11.1 s (0.49 times), with 2 to 3 times the CPU time. The cause is a new
  TCP and TLS connection for every file, to archive.org and again to the
  datanode, where v0.27.0 reuses a few; see "Many small files" below.
- **Fixed by reusing connections (build `bb038f0`, 18:51 to 20:02
  UTC):** each slot now owns one connection that its next requests reuse
  (`docs/plans/2026-11-18-download-connection-reuse.md`). Measured again
  on all six cases, no case is slower than v0.27.0 on the median: tiny
  files 14.1 against 14.8 s, small files 15.3 against 15.8 s, one large
  file 2.9 times as fast. The tiny-file item opened 13 connections
  against 17 for v0.27.0. See "Connection reuse" below.
- **Contract:** no sample of any run of the rebased build saw more than
  6 established connections to a datanode pair. Every finished file
  matched the md5 in the item metadata (198 files per build), every
  process exited 0, and no run left a `.part` or `.part.json`.

## Builds

| Label | What |
|---|---|
| new | PR #121 at `63a8761`: main at `e96f39e` merged in, plus the range-stream retry counting. Release build, `cargo build --release -p ia-cli`. |
| v0.27.0 | Released `ia-aarch64-apple-darwin` 0.27.0 from the GitHub release, checked against `SHA256SUMS` and with `gh attestation verify`. |

The review fixes committed after the run (`cb2d36e`) change the wake-up
of a request waiting for a slot after a budget drop, two retry counters
and a test; none of them is on the path these runs took (no run saw a
429 or 5xx).

## Method

`bench.py` runs each case with both builds, in an order that alternates
by round and case, 30 s apart. Each process runs under
`/usr/bin/time -l`, which gives its user and system CPU time and peak
resident set size; for two processes, CPU is the sum and memory the
larger. MB/s is 10^6 bytes of finished files over the wall time of the
run, start to the last exit, including the metadata request and the md5
pass. After the run every finished file is hashed and compared with the
md5 in the item metadata, except `<identifier>_files.xml`, which records
its own md5 before it is final (ia-cli does not check it either). Every
0.5 s, `lsof` counts established TCP connections per remote host, summed
per datanode pair.

**Sampling note.** In this run the sampler's `pgrep -P` listed every
process on the machine instead of the children of the benchmark, so the
connection counts are of every process on the client, not only the
ia-cli processes. No other program on the client used these datanodes,
so the per-datanode figures are what the datanodes saw from this
machine, which is the quantity the contract limits. The committed
`bench.py` now takes the children from `ps`. The records keep only the
datanode hosts.

## Cases

| Case | Command | Datanodes |
|---|---|---|
| single | `ia-cli download ubuntu-18.04.5-desktop-amd64 ubuntu-18.04.5-desktop-amd64.iso --json`: one file, 2,193,522,688 bytes | `ia600807`, `ia800807` |
| multi | `ia-cli download Debian-GNULinux-2.2-arch-i386-CD --json`: 16 files, three ISOs of 527 to 673 MB and 13 of 2.9 MB or less | `ia600502`, `ia800502` |
| many | `ia-cli download gd-1995-03-17MVBtapes --glob '*.mp3\|*.png' --json`: 48 files, 209,944,591 bytes in all, the largest 34 MB | `ia600107`, `ia800107` |
| twoproc | Two processes started together on `Debian-GNULinux-2.2-arch-i386-CD`, one with `--glob '*-1of3.iso'`, one with `--glob '*-2of3.iso'` | `ia600502`, `ia800502` |

All at the default `-j` (8).

## Results

| Case | Build | Runs | Median MB/s | Min to max MB/s | Median wall s | Median CPU s | Median peak RSS MiB | Peak connections per pair | md5 ok / bad | Exits |
|---|---|---|---|---|---|---|---|---|---|---|
| single | new | 3 | 24.94 | 24.88 to 25.07 | 88.0 | 20.40 | 29 | 6 | 3 / 0 | 0 |
| single | v0.27.0 | 3 | 9.22 | 7.91 to 9.35 | 238.0 | 25.35 | 19 | 1 | 3 / 0 | 0 |
| multi | new | 3 | 22.33 | 21.38 to 25.21 | 83.7 | 17.27 | 30 | 6 | 45 / 0 | 0 |
| multi | v0.27.0 | 3 | 19.07 | 18.60 to 21.12 | 98.0 | 19.07 | 25 | 5 | 45 / 0 | 0 |
| many | new | 3 | 24.67 | 24.66 to 26.34 | 8.4 | 2.12 | 29 | 6 | 144 / 0 | 0 |
| many | v0.27.0 | 3 | 24.83 | 23.08 to 24.98 | 8.3 | 2.09 | 30 | 8 | 144 / 0 | 0 |
| twoproc | new | 3 | 25.95 | 22.66 to 28.04 | 51.4 | 11.77 | 28 | 6 | 6 / 0 | 0, 0 |
| twoproc | v0.27.0 | 3 | 15.30 | 12.59 to 18.04 | 87.1 | 13.42 | 21 | 2 | 6 / 0 | 0, 0 |

Paired ratio, new over v0.27.0 (MB/s), per round:

| Case | Round 0 | Round 1 | Round 2 |
|---|---|---|---|
| single | 2.67 | 2.72 | 3.15 |
| multi | 1.06 | 1.32 | 1.15 |
| many | 0.99 | 1.14 | 0.99 |
| twoproc | 1.44 | 1.80 | 1.83 |

In every run, each pair's connections all went to one of its two hosts
(`ia800807`, `ia800502`, `ia800107`).

## Reading

- One connection to `ia800807` ran at 8 to 9 MB/s this afternoon, a
  third of the client's link, so six connections gained 2.7 times. On
  2026-11-05, when one connection reached 17 to 20 MB/s, the gain was
  1.4 to 1.6 times. The gain follows the speed of one connection, as the
  earlier report found.
- Many small files gain nothing on this client: v0.27.0 already opened
  8 TCP connections to the datanode, one per concurrent file (the
  2026-11-05 report saw v0.24.0 carry concurrent files over one HTTP/2
  connection; which protocol v0.27.0 used was not recorded), and both
  builds reach the client's link. The rebased build does it with 6.
- CPU time per byte is lower for the rebased build in the large-file
  cases. Peak memory is about 10 MiB higher for one large file (29
  against 19 MiB), where six streams run instead of one; it stays under
  32 MiB in every run.

## Many small files (added 18:11 to 18:21 UTC)

A review of `cb2d36e` asked whether opening a connection per file makes
an item of very many small files slower than v0.27.0. The cases above
used files of 4 MB or more, which do not show it. Two cases were added
to `bench.py` and run the same way: the rebased build at `cb2d36e` (the
#121 head, same download code as `63a8761` plus the review fixes)
against v0.27.0, three interleaved rounds, default `-j`, 30 s apart.
Records: `parallel-download-qa-2026-11-18/results/2026-11-18-small-files.jsonl`.
From these runs on the sampler takes the children of each run from `ps`,
so connection counts are of the ia-cli processes only, archive.org
included.

| Case | Command |
|---|---|
| tiny | `ia-cli download playdrone-metadata-2014-10-14-09 --json`: 1,267 files downloaded, 7,035,224 bytes, median file 5 KB, largest 235 KB (`ia601906`, `ia801906`) |
| small | `ia-cli download pastpages-stltoday-2016-02 --format JPEG --json`: 497 JPEGs, 261,889,784 bytes, median 189 KB, 18 KB to 1.3 MB |

| Case | Build | Median wall s | Min to max wall s | Median files/s | Median CPU s | Median peak RSS MiB | Peak connections | md5 ok / bad | Exits |
|---|---|---|---|---|---|---|---|---|---|
| tiny | new | 26.0 | 25.8 to 29.5 | 48.7 | 5.63 | 34 | `ia801906` 6, archive.org 7 | 3,798 / 0 | 0 |
| tiny | v0.27.0 | 14.3 | 12.5 to 14.9 | 88.4 | 1.97 | 32 | `ia801906` 8, archive.org 9 | 3,798 / 0 | 0 |
| small | new | 22.6 | 21.5 to 22.7 | 22.0 | 4.46 | 33 | archive.org 7 | 1,491 / 0 | 0 |
| small | v0.27.0 | 11.1 | 10.6 to 13.0 | 44.8 | 2.91 | 34 | archive.org 9 | 1,491 / 0 | 0 |

Paired ratio of throughput, new over v0.27.0: tiny 0.48, 0.51, 0.55;
small 0.57, 0.49, 0.49. `<identifier>_files.xml` is not counted in the
md5 column (see Method).

`pastpages-stltoday-2016-02` is answered by archive.org itself: its
`/download/` request returns 200 with no redirect to a datanode, so
every connection of both builds went to archive.org, which the new build
holds to 6 (7 with the metadata connection) and v0.27.0 does not (9).

### Cause

One run of each build on the tiny item with
`RUST_LOG=hyper_util=debug`, which logs every new TCP connection:

| Build | Wall s | CPU s (user + sys) | New TCP connections | To archive.org | To `ia801906` | Connections reused |
|---|---|---|---|---|---|---|
| new | 27.5 | 7.5 | 2,541 | 1,272 | 1,269 | 0 (1 log line) |
| v0.27.0 | 13.0 | 2.6 | 17 | 9 | 8 | 5,064 log lines |

- **A connection per request.** Every download request sends
  `Connection: close` on a transport with no idle pool (addition B of
  #121, which makes the cap count open sockets, not only transfers). A
  file therefore costs two TCP connects and two TLS handshakes: the
  `/download/` request to archive.org and the redirected request to the
  datanode. The median TCP connect took 16 ms in these logs; each TLS
  handshake adds at least one more round trip. v0.27.0 makes 17
  connections for the whole item and reuses them.
- **CPU.** The extra CPU time (2.9 times) is the 2,541 TLS handshakes.
- **Six connections instead of eight** account for part of the wall
  time. v0.27.0 with `-j 6` took 16.8, 20.0 and 16.7 s (median 16.8 s)
  on the tiny item at 18:22 to 18:24 UTC, against 14.3 s at its default
  `-j 8`: about 1.17 times. The new build's 26.0 s is a further 1.55
  times, which is the per-file connection cost.

For files of a few megabytes or more the handshakes are a small part of
each transfer, which is why the earlier cases did not show this. The
code was not changed.

## Connection reuse (added 18:51 to 20:02 UTC)

After the small-file runs above, the download transport was changed so
that each of a server's 6 slots owns one HTTP/1.1 connection, kept for
2 s after its last request and reused by the slot's next request, with
the slot file locked for as long as the socket may live
(`docs/plans/2026-11-18-download-connection-reuse.md`). All six cases
were measured again, the #121 head at `bb038f0` against v0.27.0, three
interleaved rounds, default `-j`, 30 s apart, with the sampler counting
the ia-cli processes only. Records:
`parallel-download-qa-2026-11-18/results/2026-11-18-reuse.jsonl`. The
link was slower this evening than in the afternoon (one connection to
`ia800807` ran at 4.9 to 7.4 MB/s), so compare within this run, not with
the tables above.

| Case | Build | Median wall s | Min to max wall s | Median MB/s | Median files/s | Median CPU s | Median peak RSS MiB | Peak connections per pair | md5 ok / bad |
|---|---|---|---|---|---|---|---|---|---|
| single | new | 113.5 | 100.9 to 155.4 | 19.32 | | 24.41 | 30 | 6 | 3 / 0 |
| single | v0.27.0 | 332.5 | 298.1 to 447.0 | 6.60 | | 29.15 | 19 | 1 | 3 / 0 |
| multi | new | 123.5 | 95.8 to 142.3 | 15.13 | | 22.80 | 31 | 6 | 45 / 0 |
| multi | v0.27.0 | 158.8 | 120.1 to 166.5 | 11.77 | | 23.07 | 25 | 6 | 45 / 0 |
| many | new | 9.5 | 8.4 to 10.7 | 21.72 | 5.0 | 2.15 | 31 | 6 | 144 / 0 |
| many | v0.27.0 | 9.5 | 9.5 to 13.6 | 21.72 | 5.0 | 2.21 | 31 | 8 | 144 / 0 |
| twoproc | new | 63.2 | 53.6 to 141.1 | 21.08 | | 14.84 | 28 | 6 | 6 / 0 |
| twoproc | v0.27.0 | 116.1 | 103.8 to 151.0 | 11.48 | | 16.03 | 20 | 2 | 6 / 0 |
| tiny | new | 14.1 | 13.0 to 16.0 | 0.50 | 89.7 | 2.10 | 35 | 6 | 3,798 / 0 |
| tiny | v0.27.0 | 14.8 | 14.2 to 18.3 | 0.48 | 85.7 | 2.06 | 32 | 8 | 3,798 / 0 |
| small | new | 15.3 | 15.2 to 23.2 | 17.13 | 32.5 | 3.30 | 34 | archive.org 7 | 1,491 / 0 |
| small | v0.27.0 | 15.8 | 12.9 to 17.0 | 16.54 | 31.4 | 3.41 | 34 | archive.org 9 | 1,491 / 0 |

Paired ratio of throughput, new over v0.27.0, rounds 0, 1, 2: single
3.29, 2.62, 2.88; multi 1.12, 1.25, 1.35; many 1.13, 1.00, 1.27;
twoproc 1.64, 2.16, 1.07; tiny 0.92, 1.00, 1.42; small 0.85, 0.68,
1.11. Every process exited 0 and no run left a `.part` or `.part.json`.

- **Against the acceptance bar.** No case is slower than v0.27.0 on the
  median. The two paired ratios below 0.9 are both on the small-file
  item, whose round-1 run of the new build took 23.2 s against 15.2 and
  15.3 s in the other two rounds; its median is 15.3 against 15.8 s.
- **Six connections instead of eight.** On the tiny item, v0.27.0 at
  `-j 6` took a median of 16.8 s against 14.3 s at `-j 8` (18:22 to
  18:24 UTC), so six connections could have cost up to 1.17 times there.
  The new build at six is not slower than v0.27.0 at eight; on this
  client the per-file round trips, not the connection count, set the
  pace once connections are reused.
- **Connections opened.** One run of the new build on the tiny item with
  `RUST_LOG=hyper_util=debug` (18:49 UTC, 15.6 s): 13 new TCP
  connections in all, 7 to archive.org and 6 to `ia801906`, against
  2,541 before the change and 17 for v0.27.0. CPU time per case is now
  equal to v0.27.0's or lower.
- **Contract.** The highest number of established connections to a
  datanode pair in any sample of the new build was 6, idle ones
  included. In two of the three one-file runs the item's two hosts both
  served it at different times (`ia600807` and `ia800807`, 6 each, never
  more than 6 together). archive.org itself saw at most 7 from one
  process and 8 from two (6 slots plus one metadata connection per
  process).
- **Review fixes after the run.** `9924913` changes four edge paths found
  in review (a request on the pair's other host waiting on another
  process while its own idle slots could serve it, a connection kept
  after an abandoned body, the close grace on a cancelled request, and a
  host given as an IP address). None is on the path these runs took.

## Not measured

- Back-off after a real 429 or 5xx. No datanode answered either during
  the run; the halving, the shedding of range streams before whole files
  and the regrowth are covered by the offline tests only.
- Connections to `archive.org` itself (the metadata request and the
  `/download/` redirect): the machine-wide sample cannot separate them
  from other programs' connections to the same addresses.
- A batch across several datanode pairs, and GNU `parallel`; both were
  measured on 2026-11-05 and not repeated.
