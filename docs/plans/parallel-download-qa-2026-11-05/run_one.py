#!/usr/bin/env python3
"""Run one `ia-cli download` and record what it did on the wire.

Usage: run_one.py RESULTS_JSONL LABEL WORKDIR -- ia-cli download ...

Samples `lsof` every 0.5 s for the process's established TCP connections,
counted per remote IP and mapped to datanode host names. Records wall
time, exit code, bytes of finished files, the JSON lines printed, and the
connection-count time series. Deletes WORKDIR's downloads afterwards
unless KEEP=1 is set in the environment.
"""
import datetime as dt
import json
import os
import shutil
import socket
import subprocess
import sys
import time

HOSTS = ["ia600807", "ia800807", "ia600502", "ia800502", "ia601500",
         "ia801500", "ia800107", "ia600107"]


def host_map():
    m = {}
    for h in HOSTS:
        fq = f"{h}.us.archive.org"
        try:
            for ip in socket.gethostbyname_ex(fq)[2]:
                m[ip] = h
        except OSError:
            pass
    for ip in socket.gethostbyname_ex("archive.org")[2]:
        m[ip] = "archive.org"
    return m


def sample(pid):
    out = subprocess.run(
        ["lsof", "-nP", "-a", "-p", str(pid), "-iTCP", "-sTCP:ESTABLISHED",
         "-Fn"], capture_output=True, text=True).stdout
    counts = {}
    for line in out.splitlines():
        if line.startswith("n") and "->" in line:
            remote = line.split("->", 1)[1]
            ip = remote.rsplit(":", 1)[0]
            counts[ip] = counts.get(ip, 0) + 1
    return counts


def main():
    results, label, workdir = sys.argv[1:4]
    assert sys.argv[4] == "--"
    cmd = sys.argv[5:]
    hm = host_map()
    os.makedirs(workdir, exist_ok=True)
    start_utc = dt.datetime.now(dt.timezone.utc)
    t0 = time.monotonic()
    out_path = os.path.join(workdir, "stdout.jsonl")
    err_path = os.path.join(workdir, "stderr.txt")
    with open(out_path, "w") as so, open(err_path, "w") as se:
        p = subprocess.Popen(cmd, cwd=workdir, stdout=so, stderr=se)
        series = []
        while p.poll() is None:
            c = sample(p.pid)
            series.append([round(time.monotonic() - t0, 2),
                           {hm.get(ip, ip): n for ip, n in c.items()}])
            time.sleep(0.5)
        wall = time.monotonic() - t0
    end_utc = dt.datetime.now(dt.timezone.utc)
    # Datanode connections only (not archive.org metadata/redirect).
    nodes = {}
    first = last = None
    for t, c in series:
        dn = {h: n for h, n in c.items() if h.startswith("ia")}
        for h, n in dn.items():
            nodes[h] = max(nodes.get(h, 0), n)
        tot = sum(dn.values())
        if tot:
            first = t if first is None else first
            last = t
    peak_total = max((sum(n for h, n in c.items() if h.startswith("ia"))
                      for _, c in series), default=0)
    lines = []
    with open(out_path) as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    lines.append(json.loads(line))
                except ValueError:
                    lines.append({"raw": line})
    with open(err_path) as f:
        stderr = f.read()
    nbytes = 0
    files = []
    for root, _, names in os.walk(workdir):
        for n in names:
            pth = os.path.join(root, n)
            if pth in (out_path, err_path):
                continue
            sz = os.path.getsize(pth)
            files.append([os.path.relpath(pth, workdir), sz])
            if not n.endswith((".part", ".part.json", ".tmp")):
                nbytes += sz
    # md5 of finished files against item metadata, after the timed window.
    md5_ok, md5_bad = 0, []
    mp = os.environ.get("MD5MAP")
    if mp:
        import hashlib
        want = json.load(open(mp))
        for rel, _ in files:
            base = os.path.basename(rel)
            if base in want:
                h = hashlib.md5()
                with open(os.path.join(workdir, rel), "rb") as fh:
                    for chunk in iter(lambda: fh.read(1 << 22), b""):
                        h.update(chunk)
                if h.hexdigest() == want[base]:
                    md5_ok += 1
                else:
                    md5_bad.append(base)
    rec = {
        "md5_ok": md5_ok,
        "md5_bad": md5_bad,
        "label": label,
        "cmd": " ".join(cmd),
        "start_utc": start_utc.isoformat(timespec="seconds"),
        "end_utc": end_utc.isoformat(timespec="seconds"),
        "exit": p.returncode,
        "wall_s": round(wall, 2),
        "bytes": nbytes,
        "mb_s": round(nbytes / wall / 1e6, 2) if wall else None,
        "net_span_s": (round(last - first, 2) if first is not None else None),
        "peak_conns_per_node": nodes,
        "peak_conns_total": peak_total,
        "statuses": [{k: v for k, v in l.items()
                      if k in ("file", "name", "status", "error", "identifier",
                               "code", "message", "raw", "event", "server",
                               "connections_before", "connections_after")} for l in lines],
        "stderr_tail": stderr[-2000:],
        "files": files,
        "series": series,
    }
    with open(results, "a") as f:
        f.write(json.dumps(rec) + "\n")
    print(json.dumps({k: rec[k] for k in (
        "label", "start_utc", "exit", "wall_s", "bytes", "mb_s",
        "net_span_s", "peak_conns_per_node", "peak_conns_total",
        "md5_ok", "md5_bad")}))
    if os.environ.get("KEEP") != "1":
        shutil.rmtree(workdir, ignore_errors=True)


if __name__ == "__main__":
    main()
