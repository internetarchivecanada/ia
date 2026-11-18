#!/usr/bin/env python3
"""Interleaved comparison of two ia-cli builds, with client cost.

Usage: bench.py RESULTS_JSONL ROUNDS [START_ROUND]

Environment:
  BENCH   scratch directory; downloads go to $BENCH/dl/<run> and are
          deleted after each run
  IA_NEW  the build under test
  IA_OLD  the released binary to compare against
  CASES   optional comma-separated subset of: single,multi,many,twoproc,
          tiny,small (default: the first four)
  GAP     seconds between runs (default 30)

Each run starts every process of the case under `/usr/bin/time -l`, which
gives its wall time, user and system CPU time and peak resident memory.
While the processes run, `lsof` is sampled every 0.5 s for the established
TCP connections of every ia-cli process of the run, counted per remote
host and summed per datanode pair. After the processes exit, every
finished file is hashed and compared with the md5 in the item metadata
(fetched once per item before the first round). Within a round the order
of the two builds alternates; the cases run in a fixed order.
"""
import hashlib
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
import datetime as dt

U = "ubuntu-18.04.5-desktop-amd64"
DEB = "Debian-GNULinux-2.2-arch-i386-CD"
GD = "gd-1995-03-17MVBtapes"
TINY = "playdrone-metadata-2014-10-14-09"
SMALL = "pastpages-stltoday-2016-02"

# case -> list of processes; each process is (identifier, extra args)
CASES = {
    "single": [(U, [f"{U}.iso"])],
    "multi": [(DEB, [])],
    "many": [(GD, ["--glob", "*.mp3|*.png"])],
    "twoproc": [
        (DEB, ["--glob", "*-1of3.iso"]),
        (DEB, ["--glob", "*-2of3.iso"]),
    ],
    # Added after the first run: per-file connection cost.
    "tiny": [(TINY, [])],
    "small": [(SMALL, ["--format", "JPEG"])],
}


def metadata(identifier):
    url = f"https://archive.org/metadata/{identifier}"
    req = urllib.request.Request(url, headers={"User-Agent": "bench/1"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def host_map(hosts):
    m = {}
    for h in hosts:
        try:
            for ip in socket.gethostbyname_ex(h)[2]:
                m[ip] = h.split(".")[0]
        except OSError:
            pass
    for ip in socket.gethostbyname_ex("archive.org")[2]:
        m[ip] = "archive.org"
    return m


def children(pid):
    # From `ps`, not `pgrep -P`: on the 2026-11-18 machine `pgrep -P`
    # listed every process, so the first run sampled the whole machine.
    out = subprocess.run(["ps", "-A", "-o", "pid=,ppid="], capture_output=True,
                         text=True).stdout
    return [int(a) for a, b in (l.split() for l in out.splitlines())
            if int(b) == pid]


def sample(pids):
    if not pids:
        return {}
    out = subprocess.run(
        ["lsof", "-nP", "-a", "-p", ",".join(map(str, pids)), "-iTCP",
         "-sTCP:ESTABLISHED", "-Fn"], capture_output=True, text=True).stdout
    counts = {}
    for line in out.splitlines():
        if line.startswith("n") and "->" in line:
            ip = line.split("->", 1)[1].rsplit(":", 1)[0]
            counts[ip] = counts.get(ip, 0) + 1
    return counts


def parse_time(path):
    txt = open(path).read()
    rec = {}
    m = re.search(r"([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys", txt)
    if m:
        rec["real_s"], rec["user_s"], rec["sys_s"] = map(float, m.groups())
    m = re.search(r"(\d+)\s+maximum resident set size", txt)
    if m:
        rec["max_rss_bytes"] = int(m.group(1))
    return rec


def md5_of(path):
    h = hashlib.md5()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 22), b""):
            h.update(chunk)
    return h.hexdigest()


def run(results, case, build, binary, rnd, meta, hm, pairs):
    base = os.environ["BENCH"]
    work = os.path.join(base, "dl", f"{case}-{build}-r{rnd}")
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    procs = []
    start_utc = dt.datetime.now(dt.timezone.utc)
    t0 = time.monotonic()
    for i, (ident, extra) in enumerate(CASES[case]):
        tfile = os.path.join(work, f".time{i}")
        so = open(os.path.join(work, f".stdout{i}"), "w")
        se = open(os.path.join(work, f".stderr{i}"), "w")
        cmd = ["/usr/bin/time", "-l", "-o", tfile, binary, "download", ident,
               *extra, "--json"]
        procs.append((subprocess.Popen(cmd, cwd=work, stdout=so, stderr=se),
                      tfile, cmd[4:]))
    series = []
    while any(p.poll() is None for p, _, _ in procs):
        pids = [c for p, _, _ in procs if p.poll() is None
                for c in children(p.pid)]
        c = sample(pids)
        series.append([round(time.monotonic() - t0, 2),
                       {hm.get(ip, ip): n for ip, n in c.items()}])
        time.sleep(0.5)
    wall = time.monotonic() - t0
    end_utc = dt.datetime.now(dt.timezone.utc)

    peak_host, peak_pair = {}, {}
    for _, c in series:
        for h, n in c.items():
            peak_host[h] = max(peak_host.get(h, 0), n)
        for key, (a, b) in pairs.items():
            n = c.get(a, 0) + c.get(b, 0)
            peak_pair[key] = max(peak_pair.get(key, 0), n)

    per_proc = []
    for i, (p, tfile, cmd) in enumerate(procs):
        r = parse_time(tfile)
        r["exit"] = p.returncode
        r["cmd"] = " ".join(["ia-cli", *cmd[1:]])
        per_proc.append(r)

    want = {}
    for ident, _ in CASES[case]:
        for f in meta[ident]["files"]:
            if "md5" in f:
                want[f["name"]] = f["md5"]
    nbytes, md5_ok, md5_bad, md5_none, partials = 0, 0, [], [], []
    for root, _, names in os.walk(work):
        for n in names:
            if n.startswith("."):
                continue
            pth = os.path.join(root, n)
            rel = os.path.relpath(pth, work)
            name = rel.split(os.sep, 1)[1] if os.sep in rel else rel
            if n.endswith((".part", ".part.json", ".tmp", ".md5-mismatch")):
                partials.append(rel)
                continue
            nbytes += os.path.getsize(pth)
            # <identifier>_files.xml records its own md5 before it is
            # final, so the metadata md5 cannot match; not checked.
            if name in want and not name.endswith("_files.xml"):
                if md5_of(pth) == want[name]:
                    md5_ok += 1
                else:
                    md5_bad.append(name)
            else:
                md5_none.append(name)
    rec = {
        "case": case, "build": build, "round": rnd,
        "start_utc": start_utc.isoformat(timespec="seconds"),
        "end_utc": end_utc.isoformat(timespec="seconds"),
        "wall_s": round(wall, 2), "bytes": nbytes,
        "mb_s": round(nbytes / wall / 1e6, 2),
        "cpu_s": round(sum(r.get("user_s", 0) + r.get("sys_s", 0)
                           for r in per_proc), 2),
        "max_rss_bytes": max(r.get("max_rss_bytes", 0) for r in per_proc),
        "exits": [r["exit"] for r in per_proc],
        "procs": per_proc,
        "files_done": md5_ok + len(md5_bad) + len(md5_none),
        "md5_ok": md5_ok, "md5_bad": md5_bad, "md5_absent": md5_none,
        "partials": partials,
        "peak_conns_per_host": peak_host, "peak_conns_per_pair": peak_pair,
        "series": series,
    }
    with open(results, "a") as f:
        f.write(json.dumps(rec) + "\n")
    print(json.dumps({k: rec[k] for k in (
        "case", "build", "round", "start_utc", "wall_s", "mb_s", "cpu_s",
        "max_rss_bytes", "exits", "md5_ok", "md5_bad", "peak_conns_per_pair")}),
        flush=True)
    shutil.rmtree(work, ignore_errors=True)


def main():
    results = sys.argv[1]
    rounds = int(sys.argv[2])
    start = int(sys.argv[3]) if len(sys.argv) > 3 else 0
    builds = [("new", os.environ["IA_NEW"]), ("v0.27.0", os.environ["IA_OLD"])]
    cases = os.environ.get("CASES", "single,multi,many,twoproc").split(",")
    gap = float(os.environ.get("GAP", "30"))
    meta = {i: metadata(i) for i in {i for c in cases for i, _ in CASES[c]}}
    pairs, hosts = {}, []
    for m in meta.values():
        a, b = m["d1"], m["d2"]
        hosts += [a, b]
        sa, sb = a.split(".")[0], b.split(".")[0]
        pairs[f"{sa}+{sb}"] = (sa, sb)
    hm = host_map(hosts)
    for rnd in range(start, rounds):
        for case in cases:
            order = builds if (rnd + cases.index(case)) % 2 == 0 else builds[::-1]
            for build, binary in order:
                run(results, case, build, binary, rnd, meta, hm, pairs)
                time.sleep(gap)
    print("DONE", dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"))


if __name__ == "__main__":
    main()
