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

## Not measured

- Back-off after a real 429 or 5xx. No datanode answered either during
  the run; the halving, the shedding of range streams before whole files
  and the regrowth are covered by the offline tests only.
- Connections to `archive.org` itself (the metadata request and the
  `/download/` redirect): the machine-wide sample cannot separate them
  from other programs' connections to the same addresses.
- A batch across several datanode pairs, and GNU `parallel`; both were
  measured on 2026-11-05 and not repeated.
