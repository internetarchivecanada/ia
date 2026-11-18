# Harness and records for the 2026-11-18 measurement

Report: [`../2026-11-18-parallel-download-qa.md`](../2026-11-18-parallel-download-qa.md).

| File | What it does |
|---|---|
| `bench.py` | Runs the four cases with both builds, interleaved, under `/usr/bin/time -l`; samples established connections per datanode every 0.5 s; checks every finished file against the md5 in the item metadata |
| `summarize.py` | The tables in the report |
| `results/2026-11-18.jsonl` | One record per run; `series` is the connection timeline, `[seconds, {host: connections}]`, datanode hosts only |

Environment: `BENCH` (a scratch directory; downloads go to `$BENCH/dl/` and are
deleted after each run), `IA_NEW` (the build under test), `IA_OLD` (the released
binary), optionally `CASES` and `GAP`.

```bash
python3 -I bench.py "$BENCH/results.jsonl" 3
python3 -I summarize.py "$BENCH/results.jsonl"
```
