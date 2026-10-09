#!/bin/bash
# Run AS ROOT (via sudo, by iso.sh). DIAGNOSTIC: one xfstests `check` per case on the cowfs arm, each on a fresh empty snapshot
# bind-mounted in its own private mount namespace, so a case that unmounts its TEST_DIR only fails itself.
W=/mnt/docs/Projects/cowfs-g5; OUT=$W/out/${OUTN:-iso-cowfs}; mkdir -p $OUT $W/tmp $W/wtmnt
C="${CB:-$W/target/debug}/cowfs --socket $W/run/c.sock"
for c in ${CASES:-$(cat $W/out/cls/sel.txt)}; do
  SN=iso-$c; $C snapshot create $SN >/dev/null 2>&1
  RB=$OUT/$c; mkdir -p $RB
  unshare -m --propagation private bash -c "mount --bind $W/mnt/$SN $W/wtmnt && umount -l $W/mnt && cd $W/ref/xfstests && env PATH=\$PATH:/usr/sbin TMPDIR=$W/tmp RESULT_BASE=$RB SCRATCH_DEV= SCRATCH_MNT= TEST_DIR=$W/wtmnt TEST_DEV=cowfs FSTYP=fuse timeout ${TMO:-300} ./check generic/$c </dev/null" > $OUT/$c.console 2>&1
  echo "$c rc=$?" >> $OUT/summary.txt
  $C snapshot rm $SN >/dev/null 2>&1
done
echo ALLDONE >> $OUT/summary.txt
