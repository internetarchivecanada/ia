# Parallel downloads: benchmark and QA of PR #121 (2026-11-05)

Measurements and checks of the parallel download work in PR #121 (#16)
against archive.org, from one client, on 2026-11-05 between 17:48 and
23:12 UTC. Downloads only; nothing was written to any archive.org item.
Nothing in `ia-core` or `ia-cli` changes. The harness, the raw records
and the commands are in
[`parallel-download-qa-2026-11-05/`](parallel-download-qa-2026-11-05/).

## Summary

On this client, a 243.5 Mbit/s (30.4 MB/s) Wi-Fi downlink, the build that
implements the connection contract agreed with IA operations
(`cddf717`, called "new" below) downloaded a 2.04 GiB file at a median of
26.6 MB/s over 6 connections, against 19.3 MB/s for the released
v0.24.0 and 16.8 to 18.3 MB/s for `wget`, `curl` and the Python `ia`, all
over one connection, in interleaved runs between 20:59 and 22:28 UTC
(1.38 times v0.24.0). The gain depends on how fast one connection is at
the time, not on the client: in a loaded period earlier the same evening
(17:48 to 19:08 UTC) one connection ran at 2.69 MB/s and six at
10.9 MB/s (4.1 times), while later, when one connection reached 17 to
20 MB/s, six gained 1.4 to 1.6 times and the client's downlink was the
limit. Against v0.24.0 the new build's median was 1.38 to 1.63 times
faster in the three single-item cases (one large file, a few large
files, many smaller files) and 1.20 times for a batch across three
datanode pairs, and it never exceeded 6 connections to a datanode pair
in any sample of any run (about 9,500 samples taken every 0.5 s).
The cap of 6 instead of 8 cost 7 to 8% of throughput for one file. No check
of the connection contract failed; one reporting bug that predates the
PR was filed as [#122](https://github.com/internetarchivecanada/ia/issues/122).

## Builds and tools

| Label | What | Connections |
|---|---|---|
| v0.24.0 | Released `ia-cli` 0.24.0, `ia-aarch64-apple-darwin` from the GitHub release, checksum verified against `SHA256SUMS` and with `gh attestation verify` | One HTTP/2 connection per datanode; concurrent files are streams on it |
| 3cd1cd5 | PR #121 with `--connections-per-file` (default 1) and `--max-connections-per-server` (default 6) | Ranges on own HTTP/1.1 connections; whole files on the HTTP/2 client |
| new (`cddf717`) | PR #121 head implementing the contract: 6 per datanode pair across all `ia` processes, splitting automatic, `Connection: close` | Every file and every 64 MiB part on its own HTTP/1.1 connection |
| wget | GNU Wget 1.25.0, `wget -q -O <file> <url>` | One HTTP/1.1 connection |
| curl | curl 8.7.1, `curl -sSfL -o <file> <url>` | One connection |
| Python ia | `internetarchive` 5.12.1 at `3e2a57d`, `ia download ... -q` | One connection, files one at a time |

`wget` and `curl` fetched `https://archive.org/download/<id>/<file>?cnt=0`,
following the redirect to the datanode as `ia-cli` does.

## Results per case

MB/s is 10^6 bytes per second, from the bytes of the finished files over
the wall time of the command (start to exit, including the metadata
request and, for `ia-cli`, its md5 pass over the file). Every finished
file was also checked by the harness against the md5 in the item
metadata after the command exited, for every tool; all 39 comparison runs
exited 0 and every checked file matched. "Connections" is the highest
number of established TCP connections to one datanode at any 0.5 s
sample. All runs of a case ran in the same rounds, the tool order rotated
each round, 30 s apart. Raw rows are in Appendix B.

### One large file

`ubuntu-18.04.5-desktop-amd64.iso`, 2,193,522,688 bytes, item
`ubuntu-18.04.5-desktop-amd64` (d1 `ia600807`, d2 `ia800807`). Every
request was answered by `ia800807`.

| Tool | Runs | Median MB/s | Min to max | Connections to `ia800807` |
|---|---|---|---|---|
| new | 3 | 26.59 | 22.69 to 28.14 | 6 |
| v0.24.0 | 3 | 19.25 | 13.64 to 22.57 | 1 |
| wget | 3 | 18.29 | 7.43 to 20.47 | 1 |
| curl | 3 | 16.77 | 13.55 to 20.51 | 1 |
| Python ia | 3 | 18.09 | 17.49 to 18.69 | 1 |

### One item with a few large files

`Debian-GNULinux-2.2-arch-i386-CD`, 16 files, 1,868,329,288 bytes: three
ISOs of 659,271,680, 673,302,528 and 527,175,680 bytes and 13 files of
2.9 MB or less (d1 `ia600502`, d2 `ia800502`; every request answered by
`ia800502`).

| Tool | Runs | Median MB/s | Min to max | Connections to `ia800502` |
|---|---|---|---|---|
| new | 3 | 29.50 | 26.48 to 30.22 | 6 |
| v0.24.0 | 3 | 18.15 | 16.22 to 18.54 | 1 |
| Python ia | 3 | 19.70 | 16.94 to 19.74 | 1 |

### One item with many smaller files

The 16 FLAC files of `gd-1995-03-17MVBtapes` (`--glob '*.flac'`), 44 to
185 MB each, 1,070,770,959 bytes; none reaches 256 MiB, so none is split
(d1 `ia600107`, d2 `ia800107`; every request answered by `ia800107`).

| Tool | Runs | Median MB/s | Min to max | Connections to `ia800107` |
|---|---|---|---|---|
| new | 3 | 26.79 | 25.41 to 29.26 | 6 |
| v0.24.0 | 3 | 18.02 | 12.62 to 27.16 | 1 |
| Python ia | 3 | 19.09 | 13.06 to 23.80 | 1 |

### A batch across datanodes

`--itemlist` of three items on three pairs, one ISO each through
`--glob`: the Ubuntu ISO above (`ia800807`), `Debian-GNULinux-2.2-arch-i386-CD-1of3.iso`
(`ia800502`) and `binary-i386-1.iso` of `debian_2.2r7_i386` (669,122,560
bytes, `ia801500`), 3,521,916,928 bytes in all, default `-j 8`.

| Tool | Runs | Median MB/s | Min to max | Connections per datanode (peak) | Total at once (peak) |
|---|---|---|---|---|---|
| new | 3 | 29.27 | 25.68 to 29.29 | `ia800807` 6, `ia800502` 5, `ia801500` 6 | 15 to 17 |
| v0.24.0 | 3 | 24.40 | 18.43 to 27.44 | 1 each | 3 |

### Paired ratios, new over v0.24.0

Because conditions changed within minutes, the ratio of two runs in the
same round is a better guide than the ratio of the medians.

| Case | Round 0 | Round 1 | Round 2 |
|---|---|---|---|
| One large file | 1.18 | 1.18 | 2.06 |
| A few large files | 1.63 | 1.82 | 1.46 |
| Many smaller files | 0.99 | 1.62 | 2.01 |
| Batch across pairs | 0.94 | 1.20 | 1.59 |

### Connections per file on one file (build 3cd1cd5)

The flag matrix of the brief, on the Ubuntu ISO: `--connections-per-file`
1, 4 and 8, each with `--max-connections-per-server` 6 and 8. An
archive.org outage from about 18:59 to 19:27 UTC split the runs into two
periods with very different conditions, so they are reported apart.
Medians across the two periods would mean nothing.

Loaded period, 17:48 to 19:08 UTC, every request answered by `ia800807`
except as noted:

| Connections in use | Runs (MB/s) | Median MB/s | Against one connection |
|---|---|---|---|
| 1 (`cpf 1`, cap 6 and 8) | 2.69, 2.70, 2.61 | 2.69 | 1.00 |
| 4 (`cpf 4`, cap 6 and 8) | 7.37, 7.68, 7.96 | 7.68 | 2.86 |
| 6 (`cpf 8`, cap 6) | 10.68, 11.14 | 10.91 | 4.06 |
| 8 (`cpf 8`, cap 8) | 12.11, 11.64 | 11.88 | 4.42 |

One further `cpf 4` run (6.37 MB/s, 18:54) met the start of the outage
and is left out. In the second `cpf 8`, cap 6 run the redirect sent parts
to both of the item's datanodes, with 6 connections open to `ia600807`
and 6 to `ia800807` at once; the contract build counts the pair as one
server, which removes this (see "Options working together").

Light period, 19:30 to 19:46 UTC, after the outage:

| Connections in use | Runs (MB/s) | Median MB/s | Against one connection |
|---|---|---|---|
| 1 | 12.70, 17.53, 19.49 | 17.53 | 1.00 |
| 4 | 26.63, 22.64 | 24.64 | 1.41 |
| 6 | 25.42 | 25.42 | 1.45 |
| 8 | 27.24 | 27.24 | 1.55 |

The same pair on the new build, 20:21 to 20:32 UTC, default (6
connections) against the hidden `--connections-per-file 1`: 27.10,
28.90, 27.69 (median 27.69) against 17.78, 17.37, 21.41 (median 17.78),
1.56 times.

### Jobs on one item (builds 3cd1cd5 and new)

Build 3cd1cd5 on the few-large-files item, two rounds, 19:46 to 20:09
UTC: `-j 1`, 4 and 8 with one connection per file gave 19.39, 17.19 and
19.48 MB/s, and every one of these runs held a single TCP connection to
`ia800502`; with 4 connections per file, 25.84, 28.14 and 29.23 MB/s.
Whole files went through the HTTP/2 client, so concurrent files were
streams on one connection: an HTTP/2 debug log of
`ia download Debian-GNULinux-2.2-arch-i386-CD --glob '*.png' -j 3` shows
the three requests sent within 20 microseconds as stream ids 1, 3 and 5
of one connection.

Many smaller files, 3cd1cd5 at 20:09 to 20:19 UTC: `-j 8` with a cap of
8, 16.15 and 15.88 MB/s on one connection; `-j 6`, 17.26 and 28.30
(one to two connections); `-j 1`, 14.61, 15.93, 15.73. The same on the
new build at 20:34 to 20:44 UTC: `-j 8` 25.37, 27.80, 27.59 (median
27.59); `-j 6` 27.83, 31.92, 25.18 (median 27.83); `-j 1` 20.06, 20.10,
22.76 (median 20.10). Under the contract `-j 8` and `-j 6` open the same
6 connections, and their medians differ by 1%: the cost of turning "8
files at once" into 6 is nil here because the client link was already
the limit at 6.

## What the numbers say

- **Where the gain comes from.** One connection to a datanode ran at
  anything from 2.6 to 27.2 MB/s during the evening, with no change on
  the client; several connections in parallel add up. When one
  connection is slow (17:48 to 19:08 UTC), 6 connections gave 4.1 times
  one; when one connection is already close to the client's link,
  6 connections gave 1.4 to 1.6 times, because the link is full. The new
  build's own throughput stayed between 21.9 and 32.2 MB/s in every run
  after 20:21 UTC, while one-connection tools ranged from 7.4 to
  27.2 MB/s: multiple connections mostly remove the dependence on how
  fast one connection happens to be.
- **The client link is the limit at 6.** This client's downlink measured
  30.4 MB/s (`networkQuality`, 17:20 UTC). The new build reached 25 to
  32 MB/s on one pair, and the three-pair batch reached the same 25.4 to
  29.3 MB/s with 15 to 18 connections: the second and third pair added
  no throughput here, only connections. On this client, beyond 6
  connections in total, nothing was gained.
- **8 against 6.** For one file, 8 connections were 8% faster than 6 in
  the loaded period (11.88 against 10.91 MB/s) and 7% in the light period
  (27.24 against 25.42). For many files, `-j 8` and `-j 6` on the new
  build differ by 1%.
- **The old `--jobs` did nothing for one datanode.** In v0.24.0 and
  3cd1cd5, concurrent whole files from one datanode shared one HTTP/2
  connection, so `-j 8` ran at the speed of one connection (single
  TCP connection observed in every such run). The new build's move to one
  HTTP/1.1 connection per file is what makes the many-files case faster
  (1.49 times v0.24.0).
- **The per-pair cap holds.** In about 9,500 samples of the
  new-build runs no datanode pair ever had more than 6 established
  connections, including two `ia` processes on one pair and eight
  processes under GNU `parallel` (see "Contract adherence").
- **The cost of `Connection: close`.** Every file and every 64 MiB part
  opens a new connection to `archive.org` for the redirect and a new one
  to the datanode. TCP setup to `ia800807` took 33 to 61 ms and TLS was
  complete at 68 to 130 ms (round trip 42 ms average, five `curl`
  samples at 22:29 UTC), so a part costs about 0.3 to 0.45 s of setup,
  about 2.2 s of an 80 s download of the 2 GiB file (33 parts over
  6 streams), about 3%. The runs show no measurable loss from it. The
  servers close these connections, so the TCP `TIME_WAIT` state stays on
  the datanode and on `archive.org`: while the new build fetched parts
  the client held no `TIME_WAIT` socket to `207.241.*` (four samples,
  22:30:01 to 22:30:31 UTC).
- **A whole file does not grow into freed slots.** In a multi-file run
  (`new-multi`, round 0) the last 13.5 s of 68.5 s ran on one connection
  to `ia800502` while five slots were free; the split is decided when a
  file starts, so a large file that started whole while the other files
  held the slots stays on one connection after they finish.

## Contract adherence

Every check below is on the new build (`cddf717`) and uses the
established-connection counts sampled every 0.5 s, per datanode and for
`archive.org` separately, as evidence. A pair is an item's `d1` and `d2`
(`ia600807` and `ia800807` are pair 0807). Across about 9,500 samples of
the new build (55 records, some runs sampled twice), no pair ever had more
than 6 established connections.

| # | Check | Runs | Observed | Result |
|---|---|---|---|---|
| 1 | Two items on one pair at once: `--itemlist` of `ubuntu-18.04.5-desktop-amd64` and `FreeBSD_3.2_Special_Edition_for_USENIX` (both on `ia600807`/`ia800807`), `-j 8`, one ISO each (2,193,522,688 and 659,798,016 bytes) | 2, 22:29 and 22:33 UTC | Pair peak 6 in 183 and 176 samples, none over; 29.14 and 30.26 MB/s; both files md5 verified | Pass |
| 2 | Two items on different pairs at once: the Ubuntu ISO and `Debian-GNULinux-2.2-arch-i386-CD-1of3.iso` (`ia800502`), `-j 8` | 2, 22:31 and 22:36 UTC | Peaks 6 and 6 (12 at one sample), then 6 and 5 (11); both pairs busy in the same samples throughout, not one after the other; 28.16 and 30.40 MB/s | Pass |
| 3 | Two `ia` processes on one pair (the Ubuntu ISO in two directories, the second started 5 to 15 s later) | 2, 20:56 and 22:38 UTC | Pair peak 6 across both processes in 122 and 102 samples; the second process logged `waiting for a connection slot on ia600807.us.archive.org+ia800807.us.archive.org: all 6 are held by other ia processes (a suspended one keeps its slots)`; when it was interrupted at 30.0 s, the first went from 5 to 6 connections by the next sample (30.49 s) | Pass |
| 4 | One large file alone (the Ubuntu ISO, default flags) | 6, 20:21 to 22:09 UTC | Exactly 6 to `ia800807` at the peak of every run, none to `ia600807` or any other datanode; md5 verified by `ia` and by the harness | Pass |
| 5 | Many small files alone (16 FLAC files of `gd-1995-03-17MVBtapes`, `-j 8`) | 12, 20:34 to 22:21 UTC, and one with debug logging at 22:39 UTC | Peak 6 to `ia800107` (1 at `-j 1`); the debug run made 16 `/download/` requests for 16 files, logged no `Range` request and no "parallel ranges" line, and waited in-process for a slot 12 times | Pass |
| 6 | Idle sockets | all | On 3cd1cd5 idle keep-alive connections held `ia800502` at 7 for 61 s under a cap of 6 (`-j 4` and `-j 8` with 4 connections per file); on `cddf717` the same item at `-j 8` peaked at exactly 6 in all six runs, and the count fell as files finished (from 6 to 1 between 46 and 54 s in `new-multi` round 0); the sample after each process exit showed 0; the client held no `TIME_WAIT` socket while parts completed, so the servers close the connections | Pass |
| 7 | Back-off after 429 or 5xx | none live | Cannot be triggered against the real service, and no 429 or 5xx was seen from a datanode during the new-build runs (no `slow_down` event in any `--json` output). Covered offline by `ia-core/src/download/slots.rs` (`a_429_or_a_5xx_halves_the_budget_down_to_one`, `other_statuses_leave_the_budget_alone`, `one_round_of_answers_drops_the_budget_once`, `slots_in_use_over_the_new_budget_are_forgotten_as_they_return`, `range_streams_over_the_budget_are_asked_to_stop`, `whole_file_connections_are_asked_to_stop_only_after_the_grace`, `the_budget_grows_by_one_per_calm_interval_up_to_the_cap`, `a_429_or_5xx_restarts_the_calm_interval`, `a_request_waiting_for_a_slot_gets_one_when_the_budget_grows`), `ia-core/src/download/parallel.rs` (`after_a_429_the_server_sees_at_most_three`, `an_items_two_datanodes_share_one_budget_of_six`) and `ia-cli/tests/download_parallel.rs` (`a_429_prints_one_slow_down_line`, `a_429_with_json_gives_one_slow_down_event`). At `cddf717`, `cargo test -p ia-core --lib -- download::slots:: …` ran 31 tests and the CLI filter 2, all passing (23:12 UTC) | Offline only |
| 8a | GNU `parallel -j 8` of eight `ia download <item> <file>` on one pair: six files of the Ubuntu item (the ISO and five of 68 bytes to 203 KB) and both FreeBSD 3.2 ISOs (659,798,016 and 681,523,200 bytes), 3,535,206,278 bytes | 2, 22:40 and 22:55 UTC | Pair peak 6 across the eight processes in 240 and 233 samples, none over; in each run two processes logged the waiting line above; three or four processes held connections (the others' files finished at once), with 274, 116 and 99 connection-seconds (round 0) and 303, 138 and 113 (round 1), the most for the process with the 2 GiB file; no process waited without progress for long. Wall 123.0 and 118.9 s, against 117.5 and 117.6 s for one `--itemlist` run of the same files | Pass |
| 8b | GNU `parallel -j 8` of eight files of 275 to 643 MiB on eight pairs (`ia800807`, `ia800502`, `ia801500`, `ia800906`, `ia803104`/`ia903104`, `ia800102`, `ia800403`, `ia801805`), 4,252,058,292 bytes | 2, 22:46 and 23:00 UTC | Every pair at 6 or below in all samples, all eight pairs busy at once, 46 datanode connections at the peak; wall 146.3 and 132.1 s against 147.6 and 135.0 s for one `--itemlist` run (22 and 25 connections at the peak), 28.8 to 32.2 MB/s in every run | Pass |

All runs in this section exited 0 and every file matched its md5.

Two observations outside the contract's per-datanode terms:

- **`archive.org`.** Each `ia` process keeps one connection to `archive.org`
  open for its whole run, the metadata client's keep-alive connection,
  which is not a download request. One process therefore shows 1 to 3
  (the redirect requests come and go beside it), at most 7 for about a
  second at the start of a multi-file run. Eight processes under
  `parallel` held 8 for their whole run and up to 14 in the first 11 s
  (8 kept plus up to 6 redirects in flight). The redirects stayed within
  `archive.org`'s own 6; the kept metadata connections are not counted
  by design.
- **More processes, more connections, same throughput.** Under
  `parallel`, eight processes on eight pairs held 46 connections for the
  throughput one `--itemlist` run reached with 22 to 25, and that the new
  build reached on a single pair with 6.

## Options working together

Checked on 3cd1cd5 (the three flags) and again on `cddf717` (the
contract).

On 3cd1cd5, with `--connections-per-file` and `--max-connections-per-server`:

- `--connections-per-file 8` under the default cap of 6 ran at 6 to
  `ia800807` and printed nothing, as documented, except in one run where
  the `/download/` redirect sent parts to both of the item's datanodes:
  6 connections were open to `ia600807` and 6 to `ia800807` from 38 s to
  185 s, 12 for one file asked to use 8. The cap counted each host
  separately. Resolved in `cddf717`, which counts the pair as one server.
- The cap counted requests in flight, not connections: idle keep-alive
  connections stayed open, so `ia800502` had 7 established connections
  for 61 s of a run under a cap of 6 (`-j 4 --connections-per-file 4`,
  both rounds; also `-j 8`), and `archive.org` 7 to 8. Resolved in
  `cddf717` by `Connection: close`.
- `-j` with one connection per file added no connections and no speed on
  one datanode, because whole files shared one HTTP/2 connection.
  Resolved in `cddf717`.
- Out-of-range values were refused with exit 2 and a clear message
  (`--connections-per-file 0` and `9`, `--max-connections-per-server 0`,
  `-j 0`); `--max-connections-per-server` had no upper limit (100 was
  accepted). Exit 2 for an argument error is clap's code, as for every
  flag of the CLI, against the 0 or 1 that `AGENTS.md` describes.
- During the archive.org outage, a 4-connection download survived a
  stall and a `502 Bad Gateway` and finished; a one-connection download
  failed after its 5 retries were spent within about 55 s (waits of 0.03,
  2.8, 5.0 and 8.5 s), its `.part` kept. That retry schedule predates
  PR #121. With `--json`, a run whose metadata request got the 503 page
  printed no JSON line, and stderr received the whole HTML page; JSON
  errors for every command are recorded as open work in `AGENTS.md`.

On `cddf717`:

- **Interrupt and resume per part.** A default download of the Ubuntu
  ISO, interrupted with SIGINT after 45 s while a second process shared
  the pair, left `.part` and `.part.json`; the journal recorded
  1,510,008,786 bytes (21 whole parts and 3 partial). The rerun logged
  `resuming from the part journal ... on_disk=1510008786` and
  `downloading over parallel ranges ... streams=6 parts=12`, finished in
  29 s with status `ok` and the md5 verified. The 683,513,902 remaining
  bytes in 29 s is 23.6 MB/s; the whole file in that time would have
  needed 75 MB/s, more than the link.
- **Exit codes.** SIGINT gave exit 130 in each of the 12 interrupts
  whose status was recorded correctly (both builds, with and without
  `--json`, alone and beside another process, 5 to 60 s after the
  start); four more were logged by the QA script as 0 because it read
  `$?` after a `date` command substitution, and the code
  (`ia-cli/src/commands/download.rs`, `std::process::exit(130)` right
  after `interrupted:`) has no path that exits 0 there. A completed run
  exits 0, a missing item 1, an argument error 2.
- **`--json`.** One line per finished file with `bytes`, `elapsed_ms`,
  `file`, `item` and `status`; no line for a file cut off by an
  interrupt. No `slow_down` event occurred, as no datanode answered 429
  or 5xx.
- **Progress display.** One bar per command with the total bytes, size,
  rate and file count (`━━━╸─── 283.74 MiB/2.04 GiB 22.93 MiB/s (0/1
  files)`); it does not show how many connections are in use. With
  `TERM` unset no bar is drawn.
- **Hidden override.** `--connections-per-file 8` ran at 6 connections
  (68 samples, peak 6); `0` and `9` are refused with exit 2;
  `--max-connections-per-server` is now refused as an unknown argument.
- **Summary line.** `HTTP: 1 requests · p50 249 ms · p95 249 ms` after a
  plain download of three files: the line counts only the metadata
  request, not the `/download/`, redirect and range requests. It
  predates PR #121 (v0.24.0 prints the same) and is filed as
  [#122](https://github.com/internetarchivecanada/ia/issues/122).

No check of the connection contract failed, so no contract bug was filed.

## The automatic rule (question 3)

Question 3 asked whether one switch should hide the flags. The contract
agreed with IA operations on 2026-11-05 answers it as built: splitting
is automatic and on by default, `--max-connections-per-server` is gone,
and `--connections-per-file` remains as a hidden override. Against the
three cases the contract names:

- **One large file: 6 byte ranges. Right.** It is the largest count the
  budget allows, it measured 1.38 times v0.24.0 (median) and 4.1 times
  one connection when one connection was slow, and 8 would have added
  only 7 to 8%.
- **An item of many files: 6 files at a time, none split. Right.** Each
  file on its own connection is what makes concurrency real (1.49 and
  1.63 times v0.24.0 for the two item cases). Splitting the files as
  well could not add connections, since the budget is already full. The
  one gap is the tail: a large file that started whole keeps one
  connection after the other files finish (13.5 s of a 68.5 s run on
  one connection with five slots free). Letting a whole-file download
  continue as ranges when slots free up, as the part journal already
  allows for a `.part` left by one stream, would close it.
- **A batch across many items: spread across datanodes, none split.
  Right for the server, not needed for this client.** The batch held 6 on
  each of three pairs, 15 to 18 connections, for the same throughput
  that 6 connections reached on one pair. Each pair saw no more than the
  contract allows, so this costs no single datanode more than agreed,
  but the client took 15 to 18 shares of the shared path for what 6
  delivered, and 46 when eight `ia` processes ran under GNU `parallel`
  (see "Costs the client cannot measure").

Is there a case for the hidden override? Yes, one: a user who wants one
connection per file, to share a slow or metered link with others or to
take a download through a proxy that handles `Range` badly. For that
`--connections-per-file 1` should stay; it does not limit how many whole
files run at once, so such a user also needs `-j 1`. There is no case
for raising the count from the client side: `--connections-per-file 8`
runs at 6 (checked), which is what the contract requires.

Recommendation: keep the rule as built, with two changes. First, let a
whole-file download take free slots as ranges once other files finish
(the tail above). Second, consider a client-wide ceiling on the sum
across pairs, because on a link like this one the second and third pair
added connections without throughput; 6 to 12 in total would have lost
nothing measurable here. A client on a much faster link than this one
would need the measurement before such a ceiling is set.

## Costs the client cannot measure

The gains above are what one user sees. They are not free, and two of
the costs fall where a client-side benchmark has no view:

- **Storage.** Item data is moving to pools built as raidz3 with a 1 MiB
  record size. A read touches every disk in the group, so concurrent
  reads contend whether they are different files or different ranges of
  one file. On a fast link, parallel reads lower the pool's total
  throughput. A range costs the server the same as a whole file under
  `--jobs`; the per-pair cap bounds both.
- **Network share.** TCP sessions on a congested link converge on a fair
  share each. Several sessions from one client, as these are, take a
  larger share at the expense of other users of that link. Part of every
  gain measured here, and possibly most of it in the loaded period, is
  share taken from other traffic, and nothing measured on this client can
  separate the two.
- **Connection setup and `TIME_WAIT`.** With `Connection: close`, each
  64 MiB part and each file costs a TCP and TLS handshake at the datanode
  and at `archive.org`, and the closed socket waits in `TIME_WAIT` on the
  server, which closes first: about 35 sockets per 2 GiB file at each.
- **Connections that bought nothing.** In the batch case the client held
  15 to 18 connections for the throughput of 6, and eight processes under
  GNU `parallel` across eight pairs held 46 for the same 29 to 32 MB/s.
  Each pair was within its limit, but in total these are three to eight
  times what the client could use.

IA operations' position is that parallel transfer helps in some cases
and hurts in others and that the degree matters; default-off was their
first request, and the contract is the agreed alternative.

## Not measured

- Back-off: a 429 or 5xx halving the budget cannot be triggered against
  the live service and is covered by offline tests only (see check 7).
- Other clients, links, locations and times of day. One Wi-Fi client,
  one evening; one connection's rate ranged over a factor of eight
  during it.
- A client whose link is faster than 6 connections can fill, where the
  second and third pair of a batch could add throughput.
- Server-side effects: datanode disk load, `TIME_WAIT` counts, and the
  share taken from other users.
- The macOS full-sync cost of the part journal on a slow disk (the
  scratch disk is the internal SSD).
- Upload; the Python `ia` batch case; `wget` and `curl` for anything but
  the single file (they fetch one URL).

## Appendix A: method

Every number comes from one harness (`run_one.py`) that starts the
command in an empty directory, samples `lsof -nP -a -p <pid> -iTCP
-sTCP:ESTABLISHED` every 0.5 s, maps each remote IP to a datanode name
(resolving the item's `d1` and `d2` and `archive.org` beforehand), and
records the wall time, exit code, bytes of finished files, the `--json`
lines and the connection series. After the command exits it computes the
md5 of each finished file and compares it with the item metadata (outside
the timed window), then deletes the downloads. For runs with more than
one `ia` process (two shells, GNU `parallel`), a second sampler
(`allproc.py`) records every `ia-cli` process's connections the same
way. Commands are in `phase.sh` (flag matrix and jobs, builds 3cd1cd5 and
new), `cmp.sh` (the comparison), `contract.sh` and `par.sh` (contract
checks) and `qa.sh` and `final-qa.sh` (interrupts, resume, display),
with the summaries from `summarize.py` and `contract_check.py`. To rerun,
set `BENCH` to a scratch directory and `IA_NEW`, `IA_OLD` and `IA_PY` to
the three binaries, as the scripts' headers describe.

Environment: Apple M1 Pro, macOS 27.2, Wi-Fi (`en0`), the same client and
location as the 2026-10-30 measurement. `networkQuality -s` at 17:20 UTC:
downlink 243.542 Mbit/s, uplink 43.434 Mbit/s, idle latency 15.1 ms
(Apple's servers). Builds with Rust 1.95.0, `cargo build --release`.
Datanode addresses: `ia600807` 207.241.227.157, `ia800807`
207.241.230.87, `ia600502` 207.241.227.182, `ia800502` 207.241.230.52,
`ia601500` 207.241.227.110, `ia801500` 207.241.228.150, `archive.org`
207.241.224.2.

Events during the session: archive.org was unavailable from about 18:59
to 19:27 UTC (search, metadata and `/download/` answered 503 "Temporarily
Offline" or did not connect); measurement paused and resumed at 19:30
after three consecutive good checks a minute apart. One 8.5 MB check
download overlapped the first second of the 20:02:22 run, and five 1 KiB
`curl` requests and a few `netstat` samples ran during the contract
checks at 22:29 to 22:31; neither is material. Between about 21:00 and 21:04
UTC, during the round-0 single-file runs of v0.24.0 and the new build,
about 255 small metadata requests (`/metadata/<id>/d1`) were sent one at
a time to find items sharing a datanode pair.

## Appendix B: raw timings

Every run, in the order run. "Peak TCP per datanode" counts only datanodes this
harness had named in advance; `contract_check.py` resolves the rest. Runs
under GNU `parallel` show "none" because the harness watches the `parallel`
process itself; their timelines are in `timelines/par-*.jsonl`. File names:
`single`, `multi`, `many` are build 3cd1cd5; `new-*`, `contract` and `par` are
`cddf717`; `cmp` is the comparison. Times UTC.

### single.jsonl

| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |
|---|---|---|---|---|---|---|---|---|---|
| single cpf=1 cap=6 | 0 | 17:48:56 | 814.6 | 2.69 | ia800807 1 | 1 | 2 | 0 | ok 1 |
| single cpf=1 cap=8 | 0 | 18:03:01 | 812.1 | 2.70 | ia800807 1 | 1 | 2 | 0 | ok 1 |
| single cpf=4 cap=6 | 0 | 18:17:04 | 297.4 | 7.37 | ia800807 4 | 4 | 4 | 0 | ok 1 |
| single cpf=4 cap=8 | 0 | 18:22:32 | 285.6 | 7.68 | ia800807 4 | 4 | 4 | 0 | ok 1 |
| single cpf=8 cap=6 | 0 | 18:27:48 | 205.3 | 10.68 | ia800807 6 | 6 | 8 | 0 | ok 1 |
| single cpf=8 cap=8 | 0 | 18:31:43 | 181.1 | 12.11 | ia800807 8 | 8 | 8 | 0 | ok 1 |
| single cpf=1 cap=8 | 1 | 18:35:15 | 839.2 | 2.61 | ia800807 1 | 1 | 2 | 0 | ok 1 |
| single cpf=4 cap=6 | 1 | 18:49:44 | 275.6 | 7.96 | ia800807 4 | 4 | 4 | 0 | ok 1 |
| single cpf=4 cap=8 | 1 | 18:54:50 | 344.5 | 6.37 | ia800807 4 | 4 | 4 | 0 | ok 1 |
| single cpf=8 cap=6 | 1 | 19:01:05 | 196.8 | 11.14 | ia600807 6, ia800807 6 | 12 | 7 | 0 | ok 1 |
| single cpf=8 cap=8 | 1 | 19:04:52 | 188.4 | 11.64 | ia800807 8 | 8 | 8 | 0 | ok 1 |
| single cpf=1 cap=6 | 1 | 19:08:31 | 335.1 | 0.00 | ia800807 1 | 1 | 2 | 1 | error 1 |
| single cpf=4 cap=6 | 2 | 19:14:36 | 49.8 | 0.00 | none | 0 | 1 | 1 | none |
| single cpf=1 cap=6 | 1 | 19:30:23 | 172.7 | 12.70 | ia800807 1 | 1 | 2 | 0 | ok 1 |
| single cpf=4 cap=6 | 2 | 19:33:46 | 82.4 | 26.63 | ia800807 4 | 4 | 4 | 0 | ok 1 |
| single cpf=4 cap=8 | 2 | 19:35:38 | 96.9 | 22.64 | ia800807 4 | 4 | 4 | 0 | ok 1 |
| single cpf=8 cap=6 | 2 | 19:37:45 | 86.3 | 25.42 | ia800807 6 | 6 | 7 | 0 | ok 1 |
| single cpf=8 cap=8 | 2 | 19:39:42 | 80.5 | 27.24 | ia800807 8 | 8 | 8 | 0 | ok 1 |
| single cpf=1 cap=6 | 2 | 19:41:33 | 125.1 | 17.53 | ia800807 1 | 1 | 2 | 0 | ok 1 |
| single cpf=1 cap=8 | 2 | 19:44:08 | 112.5 | 19.49 | ia800807 1 | 1 | 2 | 0 | ok 1 |

| Cell | Runs | Median MB/s | Min | Max |
|---|---|---|---|---|
| single cpf=1 cap=6 | 3 | 12.70 | 2.69 | 17.53 |
| single cpf=1 cap=8 | 3 | 2.70 | 2.61 | 19.49 |
| single cpf=4 cap=6 | 3 | 7.96 | 7.37 | 26.63 |
| single cpf=4 cap=8 | 3 | 7.68 | 6.37 | 22.64 |
| single cpf=8 cap=6 | 3 | 11.14 | 10.68 | 25.42 |
| single cpf=8 cap=8 | 3 | 12.11 | 11.64 | 27.24 |

### multi.jsonl

| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |
|---|---|---|---|---|---|---|---|---|---|
| multi jobs=1 cpf=1 | 0 | 19:46:31 | 92.5 | 20.20 | ia800502 1 | 1 | 2 | 0 | ok 16 |
| multi jobs=4 cpf=1 | 0 | 19:48:34 | 107.8 | 17.33 | ia800502 1 | 1 | 2 | 0 | ok 16 |
| multi jobs=8 cpf=1 | 0 | 19:50:52 | 99.9 | 18.71 | ia800502 1 | 1 | 2 | 0 | ok 16 |
| multi jobs=1 cpf=4 | 0 | 19:53:02 | 72.4 | 25.81 | ia800502 5 | 5 | 5 | 0 | ok 16 |
| multi jobs=4 cpf=4 | 0 | 19:54:44 | 68.4 | 27.32 | ia800502 7 | 7 | 8 | 0 | ok 16 |
| multi jobs=8 cpf=4 | 0 | 19:56:23 | 66.1 | 28.25 | ia800502 7 | 7 | 7 | 0 | ok 16 |
| multi jobs=4 cpf=1 | 1 | 19:57:59 | 109.6 | 17.05 | ia800502 1 | 1 | 2 | 0 | ok 16 |
| multi jobs=8 cpf=1 | 1 | 20:00:19 | 92.3 | 20.24 | ia800502 1 | 1 | 3 | 0 | ok 16 |
| multi jobs=1 cpf=4 | 1 | 20:02:22 | 72.2 | 25.87 | ia800502 5 | 5 | 5 | 0 | ok 16 |
| multi jobs=4 cpf=4 | 1 | 20:04:04 | 64.5 | 28.97 | ia800502 7 | 7 | 7 | 0 | ok 16 |
| multi jobs=8 cpf=4 | 1 | 20:05:39 | 61.9 | 30.21 | ia800502 7 | 7 | 7 | 0 | ok 16 |
| multi jobs=1 cpf=1 | 1 | 20:07:11 | 100.6 | 18.58 | ia800502 1 | 1 | 2 | 0 | ok 16 |

| Cell | Runs | Median MB/s | Min | Max |
|---|---|---|---|---|
| multi jobs=1 cpf=1 | 2 | 19.39 | 18.58 | 20.20 |
| multi jobs=4 cpf=1 | 2 | 17.19 | 17.05 | 17.33 |
| multi jobs=8 cpf=1 | 2 | 19.48 | 18.71 | 20.24 |
| multi jobs=1 cpf=4 | 2 | 25.84 | 25.81 | 25.87 |
| multi jobs=4 cpf=4 | 2 | 28.14 | 27.32 | 28.97 |
| multi jobs=8 cpf=4 | 2 | 29.23 | 28.25 | 30.21 |

### many.jsonl

| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |
|---|---|---|---|---|---|---|---|---|---|
| many jobs=8 cap=8 | 0 | 20:09:22 | 66.3 | 16.15 | ia800107 1 | 1 | 2 | 0 | ok 16 |
| many jobs=6 cap=6 | 0 | 20:10:58 | 62.0 | 17.26 | ia800107 2 | 2 | 2 | 0 | ok 16 |
| many jobs=1 cap=6 | 0 | 20:12:31 | 73.3 | 14.61 | ia800107 1 | 1 | 2 | 0 | ok 16 |
| many jobs=6 cap=6 | 1 | 20:14:14 | 37.8 | 28.30 | ia800107 1 | 1 | 2 | 0 | ok 16 |
| many jobs=1 cap=6 | 1 | 20:15:23 | 67.2 | 15.93 | ia800107 1 | 1 | 2 | 0 | ok 16 |
| many jobs=8 cap=8 | 1 | 20:17:00 | 67.4 | 15.88 | ia800107 1 | 1 | 2 | 0 | ok 16 |
| many jobs=1 cap=6 | 2 | 20:18:38 | 68.1 | 15.73 | ia800107 1 | 1 | 2 | 0 | ok 16 |

| Cell | Runs | Median MB/s | Min | Max |
|---|---|---|---|---|
| many jobs=8 cap=8 | 2 | 16.02 | 15.88 | 16.15 |
| many jobs=6 cap=6 | 2 | 22.78 | 17.26 | 28.30 |
| many jobs=1 cap=6 | 3 | 15.73 | 14.61 | 15.93 |

### new-single.jsonl

| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |
|---|---|---|---|---|---|---|---|---|---|
| new-single default | 0 | 20:21:13 | 80.9 | 27.10 | ia800807 6 | 6 | 6 | 0 | ok 1 |
| new-single cpf1 | 0 | 20:23:04 | 123.3 | 17.78 | ia800807 1 | 1 | 1 | 0 | ok 1 |
| new-single cpf1 | 1 | 20:25:38 | 126.3 | 17.37 | ia800807 1 | 1 | 1 | 0 | ok 1 |
| new-single default | 1 | 20:28:14 | 75.9 | 28.90 | ia800807 6 | 6 | 2 | 0 | ok 1 |
| new-single default | 2 | 20:30:00 | 79.2 | 27.69 | ia800807 6 | 6 | 2 | 0 | ok 1 |
| new-single cpf1 | 2 | 20:31:50 | 102.5 | 21.41 | ia800807 1 | 1 | 1 | 0 | ok 1 |

| Cell | Runs | Median MB/s | Min | Max |
|---|---|---|---|---|
| new-single default | 3 | 27.69 | 27.10 | 28.90 |
| new-single cpf1 | 3 | 17.78 | 17.37 | 21.41 |

### new-many.jsonl

| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |
|---|---|---|---|---|---|---|---|---|---|
| new-many jobs=8 | 0 | 20:34:03 | 42.2 | 25.37 | ia800107 6 | 6 | 4 | 0 | ok 16 |
| new-many jobs=6 | 0 | 20:35:15 | 38.5 | 27.83 | ia800107 6 | 6 | 2 | 0 | ok 16 |
| new-many jobs=1 | 0 | 20:36:24 | 53.4 | 20.06 | ia800107 1 | 1 | 2 | 0 | ok 16 |
| new-many jobs=6 | 1 | 20:37:47 | 33.5 | 31.92 | ia800107 6 | 6 | 2 | 0 | ok 16 |
| new-many jobs=1 | 1 | 20:38:51 | 53.3 | 20.10 | ia800107 1 | 1 | 2 | 0 | ok 16 |
| new-many jobs=8 | 1 | 20:40:14 | 38.5 | 27.80 | ia800107 6 | 6 | 2 | 0 | ok 16 |
| new-many jobs=1 | 2 | 20:41:23 | 47.0 | 22.76 | ia800107 1 | 1 | 1 | 0 | ok 16 |
| new-many jobs=8 | 2 | 20:42:40 | 38.8 | 27.59 | ia800107 6 | 6 | 1 | 0 | ok 16 |
| new-many jobs=6 | 2 | 20:43:50 | 42.5 | 25.18 | ia800107 6 | 6 | 2 | 0 | ok 16 |

| Cell | Runs | Median MB/s | Min | Max |
|---|---|---|---|---|
| new-many jobs=8 | 3 | 27.59 | 25.37 | 27.80 |
| new-many jobs=6 | 3 | 27.83 | 25.18 | 31.92 |
| new-many jobs=1 | 3 | 20.10 | 20.06 | 22.76 |

### new-multi.jsonl

| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |
|---|---|---|---|---|---|---|---|---|---|
| new-multi jobs=8 | 0 | 20:45:02 | 68.5 | 27.26 | ia800502 6 | 6 | 7 | 0 | ok 16 |
| new-multi jobs=8 | 1 | 20:46:41 | 85.1 | 21.95 | ia800502 6 | 6 | 5 | 0 | ok 16 |
| new-multi jobs=8 | 2 | 20:48:36 | 73.4 | 25.45 | ia800502 6 | 6 | 5 | 0 | ok 16 |

| Cell | Runs | Median MB/s | Min | Max |
|---|---|---|---|---|
| new-multi jobs=8 | 3 | 25.45 | 21.95 | 27.26 |

### new-itemlist.jsonl

| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |
|---|---|---|---|---|---|---|---|---|---|
| new-itemlist jobs=8 | 0 | 20:50:20 | 138.9 | 25.35 | ia800502 6, ia800807 6, ia801500 6 | 18 | 3 | 0 | ok 3 |
| new-itemlist jobs=8 | 1 | 20:53:09 | 135.7 | 25.95 | ia800502 6, ia800807 4, ia801500 5 | 15 | 6 | 0 | ok 3 |

| Cell | Runs | Median MB/s | Min | Max |
|---|---|---|---|---|
| new-itemlist jobs=8 | 2 | 25.65 | 25.35 | 25.95 |

### cmp.jsonl

| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |
|---|---|---|---|---|---|---|---|---|---|
| cmp single tool=v0.24.0 | 0 | 20:59:19 | 114.0 | 19.25 | ia800807 1 | 1 | 2 | 0 | ok 1 |
| cmp single tool=new | 0 | 21:01:47 | 96.7 | 22.69 | ia800807 6 | 6 | 2 | 0 | ok 1 |
| cmp single tool=wget | 0 | 21:03:57 | 107.2 | 20.47 | ia800807 1 | 1 | 0 | 0 | none |
| cmp single tool=curl | 0 | 21:06:18 | 130.8 | 16.77 | ia800807 1 | 1 | 1 | 0 | none |
| cmp single tool=python | 0 | 21:09:03 | 125.4 | 17.49 | ia800807 1 | 1 | 0 | 0 | none |
| cmp multi tool=v0.24.0 | 0 | 21:11:42 | 100.8 | 18.54 | ia800502 1 | 1 | 2 | 0 | ok 16 |
| cmp multi tool=new | 0 | 21:13:56 | 61.8 | 30.22 | ia800502 6 | 6 | 4 | 0 | ok 16 |
| cmp multi tool=python | 0 | 21:15:31 | 94.8 | 19.70 | ia800502 1 | 1 | 1 | 0 | none |
| cmp many tool=v0.24.0 | 0 | 21:17:39 | 39.4 | 27.16 | ia800107 1 | 1 | 2 | 0 | ok 16 |
| cmp many tool=new | 0 | 21:18:50 | 40.0 | 26.79 | ia800107 6 | 6 | 2 | 0 | ok 16 |
| cmp many tool=python | 0 | 21:20:02 | 45.0 | 23.80 | ia800107 1 | 1 | 0 | 0 | none |
| cmp batch tool=v0.24.0 | 0 | 21:21:19 | 128.4 | 27.44 | ia800502 1, ia800807 1, ia801500 1 | 3 | 2 | 0 | ok 3 |
| cmp batch tool=new | 0 | 21:24:04 | 137.1 | 25.68 | ia800502 5, ia800807 4, ia801500 6 | 15 | 5 | 0 | ok 3 |
| cmp single tool=new | 1 | 21:26:57 | 82.5 | 26.59 | ia800807 6 | 6 | 3 | 0 | ok 1 |
| cmp single tool=wget | 1 | 21:28:53 | 295.3 | 7.43 | ia800807 1 | 1 | 0 | 0 | none |
| cmp single tool=curl | 1 | 21:34:22 | 107.0 | 20.51 | ia800807 1 | 1 | 1 | 0 | none |
| cmp single tool=python | 1 | 21:36:43 | 121.3 | 18.09 | ia800807 1 | 1 | 1 | 0 | none |
| cmp single tool=v0.24.0 | 1 | 21:39:19 | 97.2 | 22.57 | ia800807 1 | 1 | 2 | 0 | ok 1 |
| cmp multi tool=new | 1 | 21:41:30 | 63.3 | 29.50 | ia800502 6 | 6 | 3 | 0 | ok 16 |
| cmp multi tool=python | 1 | 21:43:06 | 94.7 | 19.74 | ia800502 1 | 1 | 1 | 0 | none |
| cmp multi tool=v0.24.0 | 1 | 21:45:14 | 115.2 | 16.22 | ia800502 1 | 1 | 2 | 0 | ok 16 |
| cmp many tool=new | 1 | 21:47:42 | 36.6 | 29.26 | ia800107 6 | 6 | 3 | 0 | ok 16 |
| cmp many tool=python | 1 | 21:48:51 | 56.1 | 19.09 | ia800107 1 | 1 | 1 | 0 | none |
| cmp many tool=v0.24.0 | 1 | 21:50:19 | 59.4 | 18.02 | ia800107 1 | 1 | 2 | 0 | ok 16 |
| cmp batch tool=new | 1 | 21:51:50 | 120.2 | 29.29 | ia800502 4, ia800807 5, ia801500 6 | 15 | 6 | 0 | ok 3 |
| cmp batch tool=v0.24.0 | 1 | 21:54:27 | 144.3 | 24.40 | ia800502 1, ia800807 1, ia801500 1 | 3 | 2 | 0 | ok 3 |
| cmp single tool=wget | 2 | 21:57:27 | 120.0 | 18.29 | ia800807 1 | 1 | 0 | 0 | none |
| cmp single tool=curl | 2 | 22:00:01 | 161.8 | 13.55 | ia800807 1 | 1 | 1 | 0 | none |
| cmp single tool=python | 2 | 22:03:17 | 117.4 | 18.69 | ia800807 1 | 1 | 0 | 0 | none |
| cmp single tool=v0.24.0 | 2 | 22:05:48 | 160.8 | 13.64 | ia800807 1 | 1 | 2 | 0 | ok 1 |
| cmp single tool=new | 2 | 22:09:03 | 78.0 | 28.14 | ia800807 6 | 6 | 2 | 0 | ok 1 |
| cmp multi tool=python | 2 | 22:10:54 | 110.3 | 16.94 | ia800502 1 | 1 | 1 | 0 | none |
| cmp multi tool=v0.24.0 | 2 | 22:13:18 | 103.0 | 18.15 | ia800502 1 | 1 | 4 | 0 | ok 16 |
| cmp multi tool=new | 2 | 22:15:34 | 70.5 | 26.48 | ia800502 6 | 6 | 6 | 0 | ok 16 |
| cmp many tool=python | 2 | 22:17:17 | 82.0 | 13.06 | ia800107 1 | 1 | 1 | 0 | none |
| cmp many tool=v0.24.0 | 2 | 22:19:11 | 84.9 | 12.62 | ia800107 1 | 1 | 2 | 0 | ok 16 |
| cmp many tool=new | 2 | 22:21:08 | 42.1 | 25.41 | ia800107 6 | 6 | 2 | 0 | ok 16 |
| cmp batch tool=v0.24.0 | 2 | 22:22:22 | 191.1 | 18.43 | ia800502 1, ia800807 1, ia801500 1 | 3 | 2 | 0 | ok 3 |
| cmp batch tool=new | 2 | 22:26:10 | 120.3 | 29.27 | ia800502 5, ia800807 6, ia801500 6 | 17 | 7 | 0 | ok 3 |

| Cell | Runs | Median MB/s | Min | Max |
|---|---|---|---|---|
| cmp single tool=v0.24.0 | 3 | 19.25 | 13.64 | 22.57 |
| cmp single tool=new | 3 | 26.59 | 22.69 | 28.14 |
| cmp single tool=wget | 3 | 18.29 | 7.43 | 20.47 |
| cmp single tool=curl | 3 | 16.77 | 13.55 | 20.51 |
| cmp single tool=python | 3 | 18.09 | 17.49 | 18.69 |
| cmp multi tool=v0.24.0 | 3 | 18.15 | 16.22 | 18.54 |
| cmp multi tool=new | 3 | 29.50 | 26.48 | 30.22 |
| cmp multi tool=python | 3 | 19.70 | 16.94 | 19.74 |
| cmp many tool=v0.24.0 | 3 | 18.02 | 12.62 | 27.16 |
| cmp many tool=new | 3 | 26.79 | 25.41 | 29.26 |
| cmp many tool=python | 3 | 19.09 | 13.06 | 23.80 |
| cmp batch tool=v0.24.0 | 3 | 24.40 | 18.43 | 27.44 |
| cmp batch tool=new | 3 | 29.27 | 25.68 | 29.29 |

### contract.jsonl

| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |
|---|---|---|---|---|---|---|---|---|---|
| contract C1 same-pair | 0 | 22:29:24 | 97.9 | 29.14 | ia800807 6 | 6 | 3 | 0 | ok 2 |
| contract C2 two-pairs | 0 | 22:31:37 | 101.3 | 28.16 | ia800502 6, ia800807 6 | 12 | 7 | 0 | ok 2 |
| contract C1 same-pair | 1 | 22:33:53 | 94.3 | 30.26 | ia800807 6 | 6 | 3 | 0 | ok 2 |
| contract C2 two-pairs | 1 | 22:36:02 | 93.8 | 30.40 | ia800502 5, ia800807 6 | 11 | 4 | 0 | ok 2 |

| Cell | Runs | Median MB/s | Min | Max |
|---|---|---|---|---|
| contract C1 same-pair | 2 | 29.70 | 29.14 | 30.26 |
| contract C2 two-pairs | 2 | 29.28 | 28.16 | 30.40 |

### par.jsonl

| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |
|---|---|---|---|---|---|---|---|---|---|
| par a tool=parallel | 0 | 22:40:55 | 123.0 | 28.74 | none | 0 | 0 | 0 | ok 8 |
| par a tool=itemlist | 0 | 22:43:38 | 117.5 | 30.10 | ia800807 6 | 6 | 3 | 0 | ok 2 |
| par b tool=parallel | 0 | 22:46:16 | 146.3 | 29.07 | none | 0 | 0 | 0 | ok 8 |
| par b tool=itemlist | 0 | 22:49:24 | 147.6 | 28.82 | ia800102 1, ia800403 5, ia800502 2, ia800807 1, ia800906 3, ia801500 1, ia801805 4, ia803104 2, ia903104 3 | 22 | 5 | 0 | ok 8 |
| par a tool=itemlist | 1 | 22:52:33 | 117.6 | 30.06 | ia800807 6 | 6 | 3 | 0 | ok 2 |
| par a tool=parallel | 1 | 22:55:10 | 118.9 | 29.73 | none | 0 | 0 | 0 | ok 8 |
| par b tool=itemlist | 1 | 22:57:49 | 135.0 | 31.51 | ia600807 1, ia800102 4, ia800403 1, ia800502 3, ia800807 1, ia800906 6, ia801500 2, ia801805 5, ia803104 2, ia903104 2 | 25 | 3 | 0 | ok 8 |
| par b tool=parallel | 1 | 23:00:45 | 132.1 | 32.19 | none | 0 | 0 | 0 | ok 8 |

| Cell | Runs | Median MB/s | Min | Max |
|---|---|---|---|---|
| par a tool=parallel | 2 | 29.23 | 28.74 | 29.73 |
| par a tool=itemlist | 2 | 30.08 | 30.06 | 30.10 |
| par b tool=parallel | 2 | 30.63 | 29.07 | 32.19 |
| par b tool=itemlist | 2 | 30.16 | 28.82 | 31.51 |
