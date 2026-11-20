#!/usr/bin/env bash
# QA on the contract build: two processes on one pair, SIGINT, resume,
# progress display, hidden override. Logs go to $Q.
set -u
S=${BENCH:?set BENCH to a scratch directory}
IA=${IA_NEW:?set IA_NEW to the ia-cli build under test}
Q=$S/qa/run
rm -rf "$Q"; mkdir -p "$Q/p1" "$Q/p2" "$Q/p3"
ID=ubuntu-18.04.5-desktop-amd64
F=$ID.iso
log() { echo "$(date -u +%H:%M:%S) $*" | tee -a "$Q/qa.log"; }

# 1. Two processes, same file, same pair; p1 under a pty for the display.
python3 -I "$S/bench/allproc.py" 120 "$Q/allproc.jsonl" &
SAMPLER=$!
log "start p1 (pty)"
( cd "$Q/p1" && script -q "$Q/p1.tty" "$IA" download "$ID" "$F" < /dev/null > /dev/null 2>&1; echo "p1 script exit=$?" >> "$Q/qa.log" ) &
sleep 15
log "start p2 (--json)"
( cd "$Q/p2" && "$IA" download "$ID" "$F" --json > "$Q/p2.out" 2> "$Q/p2.err"; echo "$(date -u +%H:%M:%S) p2 exit=$?" >> "$Q/qa.log" ) &
sleep 45
log "SIGINT both"
for p in $(pgrep -f "^$IA download"); do kill -INT "$p"; done
sleep 8
wait $SAMPLER 2>/dev/null
log "p1 dir:"; ls -la "$Q/p1/$ID" | tee -a "$Q/qa.log"
cp "$Q/p1/$ID/$F.part.json" "$Q/p1.part.json.before" 2>/dev/null
log "p2 dir:"; ls -la "$Q/p2/$ID" | tee -a "$Q/qa.log"

# 2. Resume p1 to completion, with debug logs showing the ranges asked for.
log "resume p1"
t0=$(date +%s)
( cd "$Q/p1" && RUST_LOG=ia_core=debug "$IA" download "$ID" "$F" --json > "$Q/p1-resume.out" 2> "$Q/p1-resume.err" ); e=$?
log "resume exit=$e wall=$(( $(date +%s) - t0 ))s"
cat "$Q/p1-resume.out" | tee -a "$Q/qa.log"
ls -la "$Q/p1/$ID" | tee -a "$Q/qa.log"
rm -rf "$Q/p1/$ID" "$Q/p2/$ID"

# 3. Hidden override above the budget: 8 connections per file.
log "override cpf 8"
python3 -I "$S/bench/allproc.py" 40 "$Q/override.jsonl" &
SAMPLER=$!
( cd "$Q/p3" && "$IA" download "$ID" "$F" --connections-per-file 8 --json > "$Q/p3.out" 2> "$Q/p3.err"; echo "$(date -u +%H:%M:%S) p3 exit=$?" >> "$Q/qa.log" ) &
sleep 30
for p in $(pgrep -f "^$IA download"); do kill -INT "$p"; done
sleep 8
wait $SAMPLER 2>/dev/null
rm -rf "$Q/p3/$ID"
log "QA DONE"
