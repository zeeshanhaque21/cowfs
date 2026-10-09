#!/bin/bash
# usage: dstart.sh up|down   (sudo password on stdin, one line). Root daemon: xfstests check needs root.
set -eu
W=/mnt/docs/Projects/cowfs-g5; BIN=${BIN:-$W/target/debug}
read -r P
S() { printf '%s\n' "$P" | sudo -S -p "" "$@"; }
case $1 in
up)
  S mkdir -p $W/store $W/mnt $W/run; S chmod 700 $W/run
  S bash -c "setsid $BIN/cowfs-daemon --backend core --store $W/store --mount $W/mnt --socket $W/run/c.sock </dev/null >$W/logs/daemon.log 2>&1 & echo \$! > $W/run/daemon.pid"
  for i in $(seq 30); do grep -q " $W/mnt " /proc/self/mountinfo && break; sleep 1; done
  grep " $W/mnt " /proc/self/mountinfo || { tail -n 20 $W/logs/daemon.log; exit 1; }
  cat $W/run/daemon.pid
  ;;
down)
  S $BIN/cowfs --socket $W/run/c.sock shutdown || true
  for i in $(seq 30); do grep -q " $W/mnt " /proc/self/mountinfo || break; sleep 1; done
  grep " $W/mnt " /proc/self/mountinfo && echo "STILL MOUNTED" || echo "unmounted"
  ;;
esac
