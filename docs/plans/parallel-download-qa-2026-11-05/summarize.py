#!/usr/bin/env python3
"""Print raw and per-cell Markdown tables from results/*.jsonl.

Node peaks are recomputed from the lsof series: every remote other than
archive.org is a datanode; unknown IPs are named by reverse DNS. Runs with
a nonzero exit are listed but left out of the medians.
"""
import json
import re
import socket
import statistics
import sys
from collections import OrderedDict

_rdns = {}


def name(k):
    if not k[0].isdigit():
        return k
    if k not in _rdns:
        try:
            _rdns[k] = socket.gethostbyaddr(k)[0].split(".")[0]
        except OSError:
            _rdns[k] = k
    return _rdns[k]


def peaks(series):
    node, total, ao = {}, 0, 0
    for _, c in series:
        c = {name(k): n for k, n in c.items()}
        dn = {k: n for k, n in c.items() if k != "archive.org"}
        for k, n in dn.items():
            node[k] = max(node.get(k, 0), n)
        total = max(total, sum(dn.values()))
        ao = max(ao, c.get("archive.org", 0))
    return node, total, ao


for path in sys.argv[1:]:
    recs = [json.loads(l) for l in open(path) if l.strip()]
    print(f"\n### {path.rsplit('/', 1)[-1]}\n")
    print("| Cell | Round | Start (UTC) | Wall (s) | MB/s | Peak TCP per datanode | Peak total | Peak archive.org | Exit | File results |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    cells = OrderedDict()
    for r in recs:
        m = re.match(r"(.*) r=(\d+)$", r["label"])
        cell, rnd = m.group(1), m.group(2)
        cells.setdefault(cell, [])
        if r["exit"] == 0:
            cells[cell].append(r)
        st = {}
        for s in r["statuses"]:
            k = s.get("status") or s.get("event") or "?"
            st[k] = st.get(k, 0) + 1
        node, total, ao = peaks(r["series"])
        nodes = ", ".join(f"{h} {n}" for h, n in sorted(node.items())) or "none"
        sts = ", ".join(f"{k} {v}" for k, v in st.items()) or "none"
        print(f"| {cell} | {rnd} | {r['start_utc'][11:19]} | {r['wall_s']:.1f} | "
              f"{r['mb_s']:.2f} | {nodes} | {total} | {ao} | {r['exit']} | {sts} |")
    print()
    print("| Cell | Runs | Median MB/s | Min | Max |")
    print("|---|---|---|---|---|")
    for cell, rs in cells.items():
        if not rs:
            continue
        v = [r["mb_s"] for r in rs]
        print(f"| {cell} | {len(v)} | {statistics.median(v):.2f} | {min(v):.2f} | {max(v):.2f} |")
