#!/usr/bin/env bash
set -u
S=${BENCH:?set BENCH to a scratch directory}
NEW=${IA_NEW:?set IA_NEW to the ia-cli build under test}
OLD=${IA_OLD:-$S/rel024/ia-aarch64-apple-darwin}
F=$S/qa/final; rm -rf "$F"; mkdir -p "$F/tty" "$F/jnew" "$F/jold" "$F/pnew" "$F/pold"
U=ubuntu-18.04.5-desktop-amd64
log() { echo "$(date -u +%T) $*" >> "$F/final.log"; }
# 1. progress display under a sized pty, interrupted at 20 s
( cd "$F/tty" && script -q "$F/tty.out" bash -c "stty cols 120 rows 40; exec $NEW download $U $U.iso" < /dev/null > /dev/null 2>&1; log "tty exit=$?" ) &
sleep 20; for p in $(pgrep -f "^$NEW download $U"); do kill -INT $p; done; sleep 5
# 2. SIGINT exit codes: new and v0.24.0, with --json and without (plain, no tty)
for v in new old; do
  B=$NEW; [ $v = old ] && B=$OLD
  for mode in json plain; do
    d="$F/${mode:0:1}$v"; extra=(); [ $mode = json ] && extra=(--json)
    ( cd "$d" && "$B" download $U $U.iso "${extra[@]}" > out.txt 2> err.txt; log "$v $mode SIGINT exit=$?" ) &
    sleep 10; for p in $(pgrep -f "^$B download $U"); do kill -INT $p; done; sleep 5
  done
done
rm -rf "$F"/*/$U
log "FINAL QA DONE"
echo "FINAL QA DONE"
