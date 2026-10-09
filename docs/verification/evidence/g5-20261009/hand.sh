#!/bin/bash
# usage: hand.sh ARM "CHECK ARGS"  (sudo password on stdin). DIAGNOSTIC hand run of xfstests check as root (not the gate).
# cowfs arm: a fresh empty snapshot is bind-mounted in a private mount namespace (host mounts untouched), main mount lazily detached
# there only so findmnt -S cowfs names exactly one mount, which common/rc _check_if_dev_already_mounted requires.
set -u
W=/mnt/docs/Projects/cowfs-g5; ARM=$1; ARGS=$2
read -r P
S() { printf '%s\n' "$P" | sudo -S -p "" "$@"; }
RB=$W/out/hand-$ARM-$(date +%H%M%S); S mkdir -p $RB $W/tmp
ENVV="PATH=\$PATH:/usr/sbin TMPDIR=$W/tmp RESULT_BASE=$RB SCRATCH_DEV= SCRATCH_MNT="
CK="cd $W/ref/xfstests && timeout ${TMO:-3000} ./check $ARGS </dev/null"
case $ARM in
native) S mountpoint -q $W/mnt-ext4 || S mount /dev/loop0 $W/mnt-ext4; S bash -c "$ENVV TEST_DIR=$W/mnt-ext4 TEST_DEV=/dev/loop0 FSTYP=ext4 bash -c '$CK'" > $RB.console 2>&1;;
cowfs)
  SN=wt-$(date +%H%M%S); S $W/target/debug/cowfs --socket $W/run/c.sock snapshot create $SN >/dev/null
  S mkdir -p $W/wtmnt
  S unshare -m --propagation private bash -c "mount --bind $W/mnt/$SN $W/wtmnt && umount -l $W/mnt && env $ENVV TEST_DIR=$W/wtmnt TEST_DEV=cowfs FSTYP=fuse bash -c '$CK'" > $RB.console 2>&1;;
esac
tail -n ${TAILN:-30} $RB.console; echo "RB=$RB"
