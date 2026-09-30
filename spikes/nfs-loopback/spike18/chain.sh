#!/bin/sh
# usage: chain.sh LOGNAME  then lines on stdin: TAG|CELLS|N|MOUNT_EXTRA|SRV_ARGS|NAME_STATS|WS
cd "$(dirname "$0")"
L=../out/spike18/$1.log
while IFS='|' read -r tag cells n mx sa ns ws; do
  [ -z "$tag" ] && continue
  echo "=== $tag $(date)" >> "$L"
  FRESH=1 MOUNT_EXTRA="$mx" SRV_ARGS="$sa" NAME_STATS="$ns" WS="${ws:-X}" python3 bench.py "$tag" "$cells" "$n" >> "$L" 2>&1 || echo "FAILED $tag rc=$?" >> "$L"
done
echo "CHAIN DONE $(date)" >> "$L"
