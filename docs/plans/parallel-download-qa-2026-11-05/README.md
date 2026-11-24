# Harness and records for the 2026-11-05 parallel download QA

Report: [`../2026-11-05-parallel-download-qa.md`](../2026-11-05-parallel-download-qa.md).
Every script reads its paths from the environment:

| Variable | Meaning |
|---|---|
| `BENCH` | A scratch directory; downloads go to `$BENCH/dl/run` and are deleted after each run |
| `IA_NEW` | The `ia-cli` build under test (`target/release/ia-cli`) |
| `IA_OLD` | The released v0.24.0 binary (default `$BENCH/rel024/ia-aarch64-apple-darwin`) |
| `IA_PY` | The Python `ia` executable |

The scripts expect themselves and `inputs/` to be copied into `$BENCH/bench/`, and write
records to `$BENCH/bench/results/`.

| File | What it does |
|---|---|
| `run_one.py` | Runs one command, samples its established TCP connections every 0.5 s with `lsof`, records wall time, exit, bytes, `--json` lines, the connection series, and (with `MD5MAP`) the md5 of every finished file after the timed window |
| `allproc.py` | Samples every `ia-cli` process's connections, for runs with several processes |
| `phase.sh` | Flag matrix and jobs cells: phases `single`, `multi`, `many` (build 3cd1cd5) and `new-single`, `new-multi`, `new-many`, `new-itemlist` (build `cddf717`) |
| `cmp.sh` | The comparison of v0.24.0, the new build, `wget`, `curl` and the Python `ia` |
| `contract.sh` | Contract checks 1, 2, 3 (kill one of two processes) and 5 |
| `par.sh` | GNU `parallel` cases a and b against one `--itemlist` run |
| `qa.sh`, `final-qa.sh` | Interrupt, resume, two processes, hidden override, progress display, exit codes |
| `summarize.py`, `contract_check.py` | The tables in the report |

`results/` holds one JSON record per run (the `series` field is the connection timeline:
`[seconds, {host: connections}]`); `timelines/` holds the `allproc.py` timelines
(`[seconds, {pid: {host: n}}, {host: total}]`) and the QA logs; `inputs/` holds the item
lists, globs and the md5 values taken from item metadata.

Two QA logs (`timelines/qa.log`, `timelines/contract-c3b.log`) show `exit=0` for
interrupted processes. The scripts read `$?` after a `date` command substitution in the
same `echo`, so those values are `date`'s status, not `ia`'s; see the report's "Exit
codes". `final-qa.log` records the status correctly.
