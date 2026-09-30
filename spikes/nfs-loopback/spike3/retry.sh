#!/bin/bash
# usage: retry.sh <batch.sh args...>; reruns batch.sh while it exits 6 (cpu.lock busy); resumable
while true; do
  bash /home/zeeshanhaque/cowfs-spike3/batch.sh "$@"; rc=$?
  [ $rc -eq 6 ] || { echo "WRAPPER EXIT rc=$rc"; exit $rc; }
  echo "lock busy, retrying"
done
