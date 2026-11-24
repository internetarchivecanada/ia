#!/usr/bin/env bash
# Usage: phase.sh single|multi|itemlist ROUNDS
# Runs every cell of the phase ROUNDS times, rotating the cell order each
# round, 30 s between runs. Records go to results/<phase>.jsonl.
set -u
S=${BENCH:?set BENCH to a scratch directory}
IA=${IA_NEW:?set IA_NEW to the ia-cli build under test}
phase=$1
rounds=${2:-3}
start=${3:-0}
mkdir -p "$S/bench/results"
out="$S/bench/results/$phase.jsonl"

case $phase in
single)
  cells=("1 6" "1 8" "4 6" "4 8" "8 6" "8 8")
  ;;
multi)
  cells=("1 1" "4 1" "8 1" "1 4" "4 4" "8 4")
  ;;
many)
  cells=("8 8" "6 6" "1 6")
  ;;
itemlist)
  cells=("6 6" "8 6")
  printf 'ubuntu-18.04.5-desktop-amd64\nDebian-GNULinux-2.2-arch-i386-CD\ndebian_2.2r7_i386\n' > "$S/bench/items.txt"
  ;;
# Contract build (cddf717): no cap flag; jobs only.
new-single)
  cells=("default" "cpf1")
  ;;
new-multi)
  cells=("8")
  ;;
new-many)
  cells=("8" "6" "1")
  ;;
new-itemlist)
  cells=("8")
  printf 'ubuntu-18.04.5-desktop-amd64\nDebian-GNULinux-2.2-arch-i386-CD\ndebian_2.2r7_i386\n' > "$S/bench/items.txt"
  ;;
esac

n=${#cells[@]}
for ((r = start; r < rounds; r++)); do
  for ((k = 0; k < n; k++)); do
    cell=${cells[$(((k + r) % n))]}
    set -- $cell
    case $phase in
    single)
      label="single cpf=$1 cap=$2 r=$r"
      args=(download ubuntu-18.04.5-desktop-amd64 ubuntu-18.04.5-desktop-amd64.iso
            --connections-per-file "$1" --max-connections-per-server "$2" --json)
      ;;
    multi)
      label="multi jobs=$1 cpf=$2 r=$r"
      args=(download Debian-GNULinux-2.2-arch-i386-CD --jobs "$1"
            --connections-per-file "$2" --json)
      ;;
    many)
      label="many jobs=$1 cap=$2 r=$r"
      args=(download gd-1995-03-17MVBtapes --glob '*.flac' --jobs "$1"
            --max-connections-per-server "$2" --json)
      ;;
    itemlist)
      label="itemlist cpf=$1 cap=$2 jobs=3 r=$r"
      args=(download --itemlist "$S/bench/items.txt"
            --glob 'ubuntu-18.04.5-desktop-amd64.iso|Debian-GNULinux-2.2-arch-i386-CD-1of3.iso|binary-i386-1.iso'
            --jobs 3 --connections-per-file "$1" --max-connections-per-server "$2" --json)
      ;;
    new-single)
      label="new-single $1 r=$r"
      args=(download ubuntu-18.04.5-desktop-amd64 ubuntu-18.04.5-desktop-amd64.iso --json)
      [ "$1" = cpf1 ] && args+=(--connections-per-file 1)
      ;;
    new-multi)
      label="new-multi jobs=$1 r=$r"
      args=(download Debian-GNULinux-2.2-arch-i386-CD --jobs "$1" --json)
      ;;
    new-many)
      label="new-many jobs=$1 r=$r"
      args=(download gd-1995-03-17MVBtapes --glob '*.flac' --jobs "$1" --json)
      ;;
    new-itemlist)
      label="new-itemlist jobs=$1 r=$r"
      args=(download --itemlist "$S/bench/items.txt"
            --glob 'ubuntu-18.04.5-desktop-amd64.iso|Debian-GNULinux-2.2-arch-i386-CD-1of3.iso|binary-i386-1.iso'
            --jobs "$1" --json)
      ;;
    esac
    python3 -I "$S/bench/run_one.py" "$out" "$label" "$S/dl/run" -- "$IA" "${args[@]}"
    sleep 30
  done
done
echo "PHASE $phase DONE $(date -u +%H:%M:%S)"
