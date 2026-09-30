#!/bin/bash
# usage: batch.sh <workloads> <extra bench args or -> <label>=<flags with spaces as _> ...
# one cpu.lock held across all configs; e.g. batch.sh x_clean+x_noop - t_base=- t_thr4=--threads_4
D=/home/zeeshanhaque/cowfs-spike3
LOCK=/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/cpu.lock
MACPID=/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike3/macpid
WL=$1; EX=$2; shift 2
[ "$EX" = "-" ] && EX=""
t0=$(date +%s)
until mkdir $LOCK 2>/dev/null; do
  [ $(( $(date +%s) - t0 )) -gt 900 ] && { echo "CPU LOCK BUSY 15 MIN, STOPPING"; cat $LOCK/owner; exit 6; }
  echo "waiting for cpu.lock $(( $(date +%s) - t0 ))s owner: $(cat $LOCK/owner 2>/dev/null)"
  sleep 10
done
echo "spike3 $(cat $MACPID 2>/dev/null || echo 0) $(date '+%Y-%m-%d %H:%M:%S')" > $LOCK/owner
trap 'rm -rf $LOCK' EXIT
echo "lock acquired after $(( $(date +%s) - t0 ))s"
cd $D
for c in "$@"; do
  L=${c%%=*}; F=${c#*=}; F=${F//_/ }
  echo "=== config $L flags '$F'"
  python3 bench.py $L " $F" $WL --nolock $EX || { echo "config $L FAILED rc=$?"; exit 2; }
done
echo "ALL DONE"
