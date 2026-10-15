#!/bin/sh
# The benchmarks of docs/internals/memory.md: builds each with the backend
# JIHOO_LLC names and prints the best of three runs, and its peak memory.
#   bench/memory/run.sh [label]        (inside `nix develop`, from the checkout)
set -u
dir=$(dirname "$0") label=${1:-run} out=${TMPDIR:-/tmp}/jihoo-bench-memory
mkdir -p "$out"
for p in inplace pushpop words trees snapshot shared_tags tree_edit; do
  cargo run -q --release -- build "$dir/$p.jh" -o "$out/$p" || { echo "$p: build failed"; continue; }
  best=999 mem=0
  for k in 1 2 3; do
    t=$( { /usr/bin/env time -f "%e %M" "$out/$p" >/dev/null; } 2>&1 | tail -n 1)
    s=${t% *} mem=${t#* }
    best=$(echo "$s $best" | awk '{ print ($1 < $2) ? $1 : $2 }')
  done
  echo "$p $label ${best}s ${mem}KB"
done
