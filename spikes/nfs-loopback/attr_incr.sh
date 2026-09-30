#!/bin/sh
# usage: attr_incr.sh  server op stats for a no-op build and a one-line-edit rebuild on the mount (target must exist)
cd "$(dirname "$0")"; H=$(pwd)
P=$(cat out/server.pid)
ps -p $P -o command= | grep -q "nfs-loopback --root" || { echo "no server"; exit 1; }
last() { awk '/^STATS/{buf=""} {buf=buf $0 "\n"} END{printf "%s", buf}' out/server.log; }
cd out/mnt/build
cargo build -q 2>/dev/null
kill -USR1 $P; sleep 0.3
/usr/bin/time -p cargo build -q 2>&1 | grep real | sed 's/^/noop /'
kill -USR1 $P; sleep 0.3; (cd $H; last) | tail -4
cp src/main.rs $H/out/main.rs.bak; echo "// e" >> src/main.rs
/usr/bin/time -p cargo build -q 2>&1 | grep real | sed 's/^/incr /'
cp $H/out/main.rs.bak src/main.rs
kill -USR1 $P; sleep 0.3; (cd $H; last)
