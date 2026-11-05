#!/usr/bin/env python3
"""Sample established TCP connections of every ia-cli process every 0.5 s.

Usage: allproc.py SECONDS OUT_JSONL
Writes one line per sample: [t, {pid: {host: n}}, {host: total}].
Stops after SECONDS, or 5 s after the last ia-cli process is gone.
"""
import json
import socket
import subprocess
import sys
import time

HOSTS = ["ia600807", "ia800807", "ia600502", "ia800502", "ia601500",
         "ia801500", "ia800107", "ia600107", "ia600906", "ia800906",
         "ia903104", "ia803104", "ia600102", "ia800102", "ia600403",
         "ia800403", "ia601805", "ia801805"]
hm = {}
for h in HOSTS:
    for ip in socket.gethostbyname_ex(f"{h}.us.archive.org")[2]:
        hm[ip] = h
for ip in socket.gethostbyname_ex("archive.org")[2]:
    hm[ip] = "archive.org"

secs, out = float(sys.argv[1]), sys.argv[2]
t0 = time.monotonic()
gone = None
with open(out, "w") as f:
    while time.monotonic() - t0 < secs:
        o = subprocess.run(["lsof", "-nP", "-a", "-c", "ia-cli", "-iTCP",
                            "-sTCP:ESTABLISHED", "-Fpn"],
                           capture_output=True, text=True).stdout
        per, tot, pid = {}, {}, None
        for line in o.splitlines():
            if line.startswith("p"):
                pid = line[1:]
                per.setdefault(pid, {})
            elif line.startswith("n") and "->" in line:
                ip = line.split("->", 1)[1].rsplit(":", 1)[0]
                h = hm.get(ip, ip)
                per[pid][h] = per[pid].get(h, 0) + 1
                tot[h] = tot.get(h, 0) + 1
        alive = subprocess.run(["pgrep", "-x", "ia-cli"],
                               capture_output=True).returncode == 0
        if not alive and time.monotonic() - t0 > 5:
            gone = gone or time.monotonic()
            if time.monotonic() - gone > 5:
                break
        f.write(json.dumps([round(time.monotonic() - t0, 2), per, tot]) + "\n")
        f.flush()
        time.sleep(0.5)
