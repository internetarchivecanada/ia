#!/usr/bin/env bash
# Contract checks on the new build. Records: results/contract.jsonl, qa/contract/.
set -u
S=${BENCH:?set BENCH to a scratch directory}
NEW=${IA_NEW:?set IA_NEW to the ia-cli build under test}
export MD5MAP=$S/bench/md5map.json
out=$S/bench/results/contract.jsonl
C=$S/qa/contract; rm -rf "$C"; mkdir -p "$C/a" "$C/b"
U=ubuntu-18.04.5-desktop-amd64
printf '%s\nFreeBSD_3.2_Special_Edition_for_USENIX\n' $U > "$S/bench/items-samepair.txt"
printf '%s\nDebian-GNULinux-2.2-arch-i386-CD\n' $U > "$S/bench/items-twopairs.txt"
for r in 0 1; do
  python3 -I "$S/bench/run_one.py" "$out" "contract C1 same-pair r=$r" "$S/dl/run" -- "$NEW" download --itemlist "$S/bench/items-samepair.txt" \
    --glob "$U.iso|FreeBSD 3.2 Special Edition for USENIX (Disc 1)(1999).iso" --json
  sleep 30
  python3 -I "$S/bench/run_one.py" "$out" "contract C2 two-pairs r=$r" "$S/dl/run" -- "$NEW" download --itemlist "$S/bench/items-twopairs.txt" \
    --glob "$U.iso|Debian-GNULinux-2.2-arch-i386-CD-1of3.iso" --json
  sleep 30
done
# C3b: two processes on one pair; kill the second at 30 s; watch the first grow.
python3 -I "$S/bench/allproc.py" 75 "$C/c3b.jsonl" &
SAMP=$!
( cd "$C/a" && "$NEW" download $U $U.iso --json > a.out 2> a.err; echo "$(date -u +%T) A exit=$?" >> "$C/c3b.log" ) &
sleep 5
( cd "$C/b" && "$NEW" download $U $U.iso --json > b.out 2> b.err; echo "$(date -u +%T) B exit=$?" >> "$C/c3b.log" ) &
sleep 25
B=$(pgrep -f "^$NEW download $U $U.iso --json" | tail -1)
echo "$(date -u +%T) kill B pid=$B" >> "$C/c3b.log"
kill -INT "$B"
sleep 20
for p in $(pgrep -f "^$NEW download"); do kill -INT "$p"; done
echo "$(date -u +%T) kill A" >> "$C/c3b.log"
wait $SAMP
rm -rf "$C/a/$U" "$C/b/$U"
sleep 30
# C5: many small files with debug logs, to show none is split.
mkdir -p "$C/c5" && cd "$C/c5" && RUST_LOG=ia_core=debug "$NEW" download gd-1995-03-17MVBtapes --glob '*.flac' --json > c5.out 2> c5.err
echo "$(date -u +%T) C5 exit=$?" >> "$C/c3b.log"
rm -rf "$C/c5/gd-1995-03-17MVBtapes"
echo "CONTRACT DONE $(date -u +%T)"
