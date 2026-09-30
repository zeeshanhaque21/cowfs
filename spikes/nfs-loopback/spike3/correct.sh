#!/bin/bash
# usage: correct.sh <label> [fusepass flags...]
L=$1; shift
D=/home/zeeshanhaque/cowfs-spike3
. $D/env.sh
export CARGO_TARGET_DIR=
unset CARGO_TARGET_DIR
B=$D/cb_$L; M=$D/cm_$L; OUT=$D/results/correct_$L.log
rm -rf $B; mkdir -p $B
bash $D/mount.sh $B $M "$@" > $OUT 2>&1 || { cat $OUT; exit 1; }
cd $D/tests
{
echo "== flags: $*"; uptime
timeout 900 python3 test_mount.py $M $B
timeout 300 python3 test_hardlink_readdir.py $M 4000
for m in nosync msync fsync; do timeout 300 python3 mmap_race.py $M/mm_$m 100 $m; done
timeout 300 python3 test_extra.py $M $B
} >> $OUT 2>&1
bash $D/umount.sh $M >> $OUT 2>&1
rm -rf $B
cat $OUT
