#!/bin/sh
# usage: ab.sh <label> <n> <kind: incr|full> <variant>=<binary>:<mountopts> ...
# interleaves variants run by run; each variant gets a fresh server + mount, then one bench.py run (native then nfs)
cd "$(dirname "$0")"
label=$1; n=$2; kind=$3; shift 3
for i in $(seq $n); do
  for v in "$@"; do
    name=${v%%=*}; rest=${v#*=}; bin=${rest%%:*}; opts=${rest#*:}
    NFS_BIN=$bin python3 srv.py "$opts" >/dev/null || { echo "srv failed for $name"; exit 1; }
    if [ "$kind" = incr ]; then python3 bench.py "$label/$name" 1 native,nfs incr || exit 1
    else python3 bench.py "$label/$name" 1 || exit 1; fi
  done
done
