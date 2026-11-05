#!/usr/bin/env bash
# Usage: cmp.sh ROUNDS [START]
# Comparison of tools per case, interleaved in each round (order rotated
# per round), 30 s between runs. Records: results/cmp.jsonl.
set -u
S=${BENCH:?set BENCH to a scratch directory}
NEW=${IA_NEW:?set IA_NEW to the ia-cli build under test}
OLD=${IA_OLD:-$S/rel024/ia-aarch64-apple-darwin}
PY=${IA_PY:?set IA_PY to the Python ia executable}
export MD5MAP=$S/bench/md5map.json
rounds=$1
start=${2:-0}
out=$S/bench/results/cmp.jsonl
printf 'ubuntu-18.04.5-desktop-amd64\nDebian-GNULinux-2.2-arch-i386-CD\ndebian_2.2r7_i386\n' > "$S/bench/items.txt"
U=ubuntu-18.04.5-desktop-amd64
URL="https://archive.org/download/$U/$U.iso?cnt=0"
GLOB='ubuntu-18.04.5-desktop-amd64.iso|Debian-GNULinux-2.2-arch-i386-CD-1of3.iso|binary-i386-1.iso'

run() {  # case tool round -- cmd...
  local c=$1 t=$2 r=$3; shift 4
  python3 -I "$S/bench/run_one.py" "$out" "cmp $c tool=$t r=$r" "$S/dl/run" -- "$@"
  sleep 30
}

cmd_for() {  # case tool -> sets CMD array
  case "$1:$2" in
  single:v0.24.0) CMD=("$OLD" download $U $U.iso --json) ;;
  single:new)     CMD=("$NEW" download $U $U.iso --json) ;;
  single:wget)    CMD=(wget -q -O $U.iso "$URL") ;;
  single:curl)    CMD=(curl -sSfL -o $U.iso "$URL") ;;
  single:python)  CMD=("$PY" download $U $U.iso -q) ;;
  multi:v0.24.0)  CMD=("$OLD" download Debian-GNULinux-2.2-arch-i386-CD --json) ;;
  multi:new)      CMD=("$NEW" download Debian-GNULinux-2.2-arch-i386-CD --json) ;;
  multi:python)   CMD=("$PY" download Debian-GNULinux-2.2-arch-i386-CD -q) ;;
  many:v0.24.0)   CMD=("$OLD" download gd-1995-03-17MVBtapes --glob '*.flac' --json) ;;
  many:new)       CMD=("$NEW" download gd-1995-03-17MVBtapes --glob '*.flac' --json) ;;
  many:python)    CMD=("$PY" download gd-1995-03-17MVBtapes -g '*.flac' -q) ;;
  batch:v0.24.0)  CMD=("$OLD" download --itemlist "$S/bench/items.txt" --glob "$GLOB" --json) ;;
  batch:new)      CMD=("$NEW" download --itemlist "$S/bench/items.txt" --glob "$GLOB" --json) ;;
  esac
}

for ((r = start; r < rounds; r++)); do
  for c in single multi many batch; do
    case $c in
    single) tools=(v0.24.0 new wget curl python) ;;
    multi|many) tools=(v0.24.0 new python) ;;
    batch) tools=(v0.24.0 new) ;;
    esac
    n=${#tools[@]}
    for ((k = 0; k < n; k++)); do
      t=${tools[$(((k + r) % n))]}
      cmd_for $c $t
      run $c $t $r -- "${CMD[@]}"
    done
  done
done
echo "CMP DONE $(date -u +%H:%M:%S)"
