#!/bin/sh
cd /mnt/docs/Projects/cowfs-8-21
for n in 1000 10000 100000 1000000; do
  while [ "$(cut -d. -f1 /proc/loadavg)" -ge 12 ]; do echo "load high, waiting"; sleep 30; done
  python3 m8.py $n 5 > logs/m8-$n.log 2>&1 || echo "FAILED $n"
done
echo ALLDONE
