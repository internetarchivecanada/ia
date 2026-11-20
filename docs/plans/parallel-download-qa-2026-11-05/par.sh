#!/usr/bin/env bash
# GNU parallel driving 8 ia-cli processes vs one --itemlist run; cases a, b.
set -u
S=${BENCH:?set BENCH to a scratch directory}
NEW=${IA_NEW:?set IA_NEW to the ia-cli build under test}
export MD5MAP=$S/bench/md5map.json
out=$S/bench/results/par.jsonl
mkdir -p "$S/qa/par"
one() {  # case tool round
  local c=$1 t=$2 r=$3
  python3 -I "$S/bench/allproc.py" 900 "$S/qa/par/$c-$t-r$r.jsonl" &
  local samp=$!
  sleep 1
  if [ "$t" = parallel ]; then
    python3 -I "$S/bench/run_one.py" "$out" "par $c tool=parallel r=$r" "$S/dl/run" -- \
      parallel -j 8 --colsep '\t' "$NEW" download {1} {2} --json :::: "$S/bench/par-$c.tsv"
  else
    python3 -I "$S/bench/run_one.py" "$out" "par $c tool=itemlist r=$r" "$S/dl/run" -- \
      "$NEW" download --itemlist "$S/bench/par-$c-items.txt" --glob "$(cat "$S/bench/par-$c-glob.txt")" --json
  fi
  sleep 3; kill $samp 2>/dev/null; wait $samp 2>/dev/null
  sleep 30
}
for r in 0 1; do
  for c in a b; do
    if [ $r = 0 ]; then one $c parallel $r; one $c itemlist $r
    else one $c itemlist $r; one $c parallel $r; fi
  done
done
echo "PAR DONE $(date -u +%T)"
