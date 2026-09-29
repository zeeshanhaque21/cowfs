#!/bin/sh
# usage: repro.sh <n>  clean debug builds on the mount, count failures
cd "$(dirname "$0")/out/mnt/build"
f=0
for i in $(seq $1); do
  rm -rf target
  if cargo build >/dev/null 2>/tmp/repro.err; then echo "run $i ok"; else f=$((f+1)); echo "run $i FAIL: $(grep -m1 '^error' /tmp/repro.err)"; fi
done
rm -f /tmp/repro.err
echo "failures $f/$1"
