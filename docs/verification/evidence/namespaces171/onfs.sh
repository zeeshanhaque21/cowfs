#!/bin/bash
# usage: onfs.sh LABEL BASE_DIR    -> isolation tests (TMPDIR on that fs) + cargo matrix N=6 for INCR=0 and 1
set -u
. /mnt/docs/Projects/cowfs-171/env.sh
L=$1; B=$2; mkdir -p "$B/tmp" "$B/run"
export TMPDIR=$B/tmp
echo "== $L: $(findmnt -no FSTYPE,SOURCE,OPTIONS -T "$B")"
python3 -m unittest discover -s $W/src/bench -p test_namespaces.py -v > $W/logs/iso-$L.log 2>&1; echo "isolation rc=$?"
grep -E "^(Ran|OK|FAILED)|namespace was|FAIL|ERROR" $W/logs/iso-$L.log
echo "isolation tests ok: $(grep -c 'Isolation.*ok$' $W/logs/iso-$L.log) of 9 ; refusals ok: $(grep -c 'Refusals.*ok$' $W/logs/iso-$L.log) of 7 (+1 skipped off-Linux control)"
for I in 0 1; do $W/matrix171.sh $B/run 6 $I 2>&1 | tee $W/logs/cargo-$L-inc$I.log | grep -v "^  \./"; done
