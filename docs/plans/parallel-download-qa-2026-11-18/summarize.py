#!/usr/bin/env python3
"""Markdown tables from bench.py records.

Usage: summarize.py RESULTS_JSONL
"""
import json
import statistics
import sys

CASES = ["single", "multi", "many", "twoproc", "tiny", "small"]
BUILDS = ["new", "v0.27.0"]


def med(xs):
    return statistics.median(xs) if xs else float("nan")


def main():
    recs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
    # <identifier>_files.xml records its own md5 before it is final, so its
    # metadata md5 never matches the file; ia-cli does not check it either.
    for r in recs:
        xml = [n for n in r["md5_bad"] if n.endswith("_files.xml")]
        r["md5_bad"] = [n for n in r["md5_bad"] if n not in xml]
        r["md5_absent"] = r["md5_absent"] + xml
    print("| Case | Build | Runs | Median MB/s | Min to max MB/s | Median wall s "
          "| Median files/s | Median CPU s | Median peak RSS MiB | Peak connections per pair | md5 ok / bad | Exits |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for c in CASES:
        for b in BUILDS:
            rs = [r for r in recs if r["case"] == c and r["build"] == b]
            if not rs:
                continue
            mbs = [r["mb_s"] for r in rs]
            pair = {}
            for r in rs:
                for k, v in r["peak_conns_per_pair"].items():
                    pair[k] = max(pair.get(k, 0), v)
            peak = ", ".join(f"`{k}` {v}" for k, v in pair.items() if v)
            ok = sum(r["md5_ok"] for r in rs)
            bad = sum(len(r["md5_bad"]) for r in rs)
            exits = sorted({e for r in rs for e in r["exits"]})
            print(f"| {c} | {b} | {len(rs)} | {med(mbs):.2f} | {min(mbs):.2f} to {max(mbs):.2f} "
                  f"| {med([r['wall_s'] for r in rs]):.1f} "
                  f"| {med([r.get('files_done', 0) / r['wall_s'] for r in rs]):.1f} "
                  f"| {med([r['cpu_s'] for r in rs]):.2f} "
                  f"| {med([r['max_rss_bytes'] for r in rs]) / 2**20:.0f} | {peak} | {ok} / {bad} "
                  f"| {','.join(map(str, exits))} |")
    print()
    print("Paired ratio new / v0.27.0 (MB/s) per round:")
    print()
    rounds = sorted({r["round"] for r in recs})
    print("| Case | " + " | ".join(f"Round {i}" for i in rounds) + " |")
    print("|---|" + "---|" * len(rounds))
    for c in CASES:
        cells = []
        for i in rounds:
            n = [r["mb_s"] for r in recs if r["case"] == c and r["round"] == i and r["build"] == "new"]
            o = [r["mb_s"] for r in recs if r["case"] == c and r["round"] == i and r["build"] == "v0.27.0"]
            cells.append(f"{n[0] / o[0]:.2f}" if n and o and o[0] else "-")
        print(f"| {c} | " + " | ".join(cells) + " |")
    absent = sorted({n for r in recs for n in r["md5_absent"]})
    partial = [r["partials"] for r in recs if r["partials"]]
    print()
    print("Files without an md5 in the metadata:", absent or "none")
    print("Runs leaving partial files:", partial or "none")
    print("Window:", min(r["start_utc"] for r in recs), "to", max(r["end_utc"] for r in recs))


if __name__ == "__main__":
    main()
