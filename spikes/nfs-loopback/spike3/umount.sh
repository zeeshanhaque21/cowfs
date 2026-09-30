#!/bin/bash
# usage: umount.sh <mnt>; unmounts and stops only the pid recorded in fusepass.pid after verifying its cmdline
D=/home/zeeshanhaque/cowfs-spike3
fusermount3 -u "$1" || { echo "umount failed"; exit 1; }
PID=$(cat $D/fusepass.pid 2>/dev/null)
sleep 0.5
if [ -n "$PID" ] && kill -0 $PID 2>/dev/null; then
  if tr '\0' ' ' < /proc/$PID/cmdline | grep -q "^$D/target-fusepass/release/fusepass"; then kill $PID; sleep 0.3; fi
fi
kill -0 $PID 2>/dev/null && echo "server still alive $PID" || echo "server gone"
