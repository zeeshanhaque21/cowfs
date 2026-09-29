#!/bin/sh
# usage: attr.sh <tag> [debug|release]  one clean build on the mount with server op stats, CPU and a 10s sample
cd "$(dirname "$0")"; H=$(pwd)
tag=$1; prof=$2
P=$(cat out/server.pid)
ps -p $P -o command= | grep -q "nfs-loopback --root" || { echo "no server"; exit 1; }
rm -rf out/mnt/build/target
kill -USR1 $P; sleep 0.5
echo "server cpu before: $(ps -p $P -o time=)"
( sleep 3; sample $P 10 -file out/sample_$tag.txt >/dev/null 2>&1 ) &
if [ "$prof" = release ]; then a=--release; else a=; fi
( cd out/mnt/build && /usr/bin/time -p cargo build $a >/dev/null 2>$H/out/attr_cargo_$tag.err ); tail -3 out/attr_cargo_$tag.err
echo "server cpu after: $(ps -p $P -o time=)"
wait
kill -USR1 $P; sleep 0.5
awk '/^STATS/{buf=""} {buf=buf $0 "\n"} END{printf "%s", buf}' out/server.log
