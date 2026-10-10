#!/bin/sh
# Runs one backend test, a hand-written module in this directory whose
# `; expect:` line says what should happen:
#   ; expect: exit N        compiles, links and exits with status N
#   ; expect: trap          compiles, links and dies of a signal (a failed check)
#   ; expect: object T      compiles to an object file for target T (x86_64 or
#                           aarch64), whatever machine this is
#   ; expect: error TEXT    jihoo-llc refuses it with a message containing TEXT
# Modules that run are freestanding: they need ld.lld, and nothing else.
#
#   run-test.sh <jihoo-llc> <ld.lld, or "" if there is none> <test.jir> <work dir>
set -u
llc=$1 ld=$2 test=$3 work=$4

expect=$(sed -n 's/^; expect: //p' "$test" | head -n 1)
if [ -z "$expect" ]; then
  echo "$test: no '; expect:' line"
  exit 1
fi
name=$(basename "$test" .jir)
mkdir -p "$work"
obj=$work/$name.o bin=$work/$name err=$work/$name.err

case $expect in
  error\ *)
    want=${expect#error }
    if "$llc" "$test" -o "$obj" 2>"$err"; then
      echo "$test: compiled, but should fail with: $want"
      exit 1
    fi
    if ! grep -qF -- "$want" "$err"; then
      echo "$test: expected an error containing: $want"
      cat "$err"
      exit 1
    fi
    exit 0
    ;;
esac

"$llc" "$test" -o "$obj" || exit 1
case $expect in
  object\ *)
    # The ELF header's e_machine, little-endian at byte 18.
    case ${expect#object } in
      x86_64) want=62 ;;
      aarch64) want=183 ;;
      *) echo "$test: unknown target in '$expect'"; exit 1 ;;
    esac
    machine=$(od -An -tu1 -j18 -N2 "$obj" | awk '{ print $1 + 256 * $2 }')
    if [ "$machine" != "$want" ]; then
      echo "$test: expected an object for ${expect#object } (e_machine $want), got e_machine $machine"
      exit 1
    fi
    exit 0
    ;;
esac
if [ -z "$ld" ]; then
  echo "$test: ld.lld not found: skipped"
  exit 77
fi
"$ld" -static --gc-sections -e _start -o "$bin" "$obj" || exit 1
"$bin"
status=$?
case $expect in
  "exit $status") exit 0 ;;
  trap) [ "$status" -gt 128 ] && exit 0 ;;
esac
echo "$test: expected '$expect', got exit status $status"
exit 1
