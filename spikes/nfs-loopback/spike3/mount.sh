#!/bin/bash
# usage: mount.sh <backing> <mnt> [fusepass flags...]; prints server pid
B=$1; M=$2; shift 2
D=/home/zeeshanhaque/cowfs-spike3
mkdir -p "$B" "$M"
nohup setsid $D/target-fusepass/release/fusepass "$B" "$M" "$@" > $D/results/fusepass.log 2>&1 < /dev/null &
PID=$!
echo $PID > $D/fusepass.pid
for i in $(seq 1 100); do
  if ! kill -0 $PID 2>/dev/null; then echo "SERVER DIED"; cat $D/results/fusepass.log; exit 1; fi
  if grep -q " $M fuse.fusepass" /proc/mounts; then echo "mounted pid=$PID"; ps -o pid,args -p $PID | tail -1; exit 0; fi
  sleep 0.2
done
echo "MOUNT TIMEOUT"; exit 1
