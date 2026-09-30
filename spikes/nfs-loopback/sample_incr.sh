#!/bin/sh
# usage: sample_incr.sh <secs>  sample the server during one incremental rebuild on the mount
cd "$(dirname "$0")"; H=$(pwd)
P=$(cat out/server.pid)
ps -p $P -o command= | grep -q "nfs-loopback --root" || { echo "no server"; exit 1; }
cp out/mnt/build/src/main.rs out/main.rs.bak
echo "// s" >> out/mnt/build/src/main.rs
( sleep 0.3; sample $P $1 -file $H/out/sample_incr.txt >/dev/null 2>&1 ) &
( cd out/mnt/build && /usr/bin/time -p cargo build -q 2>&1 | grep real )
cp out/main.rs.bak out/mnt/build/src/main.rs
wait
