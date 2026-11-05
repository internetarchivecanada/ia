#!/usr/bin/env python3
"""Per-pair connection peaks for every new-build run.

A datanode ia6NNNNN / ia8NNNNN / ia9NNNNN belongs to pair NNNN (its last
four digits). Prints, per run: peak per pair at any single sample, samples
over 6 for any pair, peak to archive.org, and the number of pairs busy at
the same sample. Inputs: run_one JSONL records (series per run) and
allproc JSONL timelines (all ia-cli processes; other sessions' loopback
connections are ignored).
"""
import json
import socket
import sys

_rd = {}


def host(k):
    if k[0].isdigit():
        if k == "127.0.0.1":
            return None
        if k not in _rd:
            try:
                _rd[k] = socket.gethostbyaddr(k)[0].split(".")[0]
            except OSError:
                _rd[k] = k
        return _rd[k]
    return k


def pairs_of(c):
    p, ao = {}, 0
    for k, n in c.items():
        h = host(k)
        if h is None:
            continue
        if h == "archive.org":
            ao += n
        elif h.startswith("ia"):
            p[h[4:]] = p.get(h[4:], 0) + n
    return p, ao


def report(label, samples):
    peak, over, ao_peak, busy = {}, 0, 0, 0
    for c in samples:
        p, ao = pairs_of(c)
        for k, n in p.items():
            peak[k] = max(peak.get(k, 0), n)
        over += any(n > 6 for n in p.values())
        ao_peak = max(ao_peak, ao)
        busy = max(busy, sum(1 for n in p.values() if n))
    pk = ", ".join(f"{k} {v}" for k, v in sorted(peak.items()))
    print(f"| {label} | {pk} | {over} | {ao_peak} | {busy} | {len(samples)} |")


print("| Run | Peak per pair | Samples with a pair over 6 | Peak archive.org | Most pairs busy at once | Samples |")
print("|---|---|---|---|---|---|")
for path in sys.argv[1:]:
    lines = [json.loads(l) for l in open(path) if l.strip()]
    if lines and isinstance(lines[0], dict):
        for r in lines:
            lab = r["label"]
            if not (lab.startswith(("new-", "contract", "par")) or "tool=new" in lab):
                continue
            report(lab, [c for _, c in r["series"]])
    else:
        # allproc: [t, per_pid, totals]
        report(path.rsplit("/", 1)[-1], [tot for _, _, tot in lines])
