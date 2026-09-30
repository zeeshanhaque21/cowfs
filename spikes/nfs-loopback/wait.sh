#!/bin/sh
# usage: wait.sh <logfile> <max_idle_s>  blocks until <logfile>.done, exits 1 if log stalls
log=$1; idle=${2:-300}; last=""; n=0
while [ ! -f "$log.done" ]; do
  cur=$(wc -c < "$log" 2>/dev/null)
  if [ "$cur" = "$last" ]; then n=$((n+5)); else n=0; last=$cur; fi
  [ $n -ge "$idle" ] && { echo "STALL"; tail -5 "$log"; exit 1; }
  sleep 5
done
cat "$log"; echo "rc=$(cat "$log.done")"
