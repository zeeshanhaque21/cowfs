#!/bin/sh
# Mount a PathVfs over the macOS NFS loopback, for the cowfs arm of bench/run-pair.sh.
#
# usage: mount.sh BACKING_DIR MOUNTPOINT start|stop|status
#
# `start` launches bench/mount-nfs in the background, waits for the mount to appear in the mount
# table, and writes its pid to MOUNTPOINT.pid. `stop` SIGTERMs that pid and verifies the mount is
# gone; it refuses to kill anything whose command line is not this binary.
set -eu

BACKING=${1:?usage: mount.sh BACKING_DIR MOUNTPOINT start|stop|status}
MOUNT=${2:?usage: mount.sh BACKING_DIR MOUNTPOINT start|stop|status}
ACTION=${3:-start}
# mount(8) prints the resolved path, so compare against that, not the /var/folders spelling.
mkdir -p "$MOUNT"
MOUNT=$(CDPATH='' cd -- "$MOUNT" && pwd -P)
mkdir -p "$BACKING"
BACKING=$(CDPATH='' cd -- "$BACKING" && pwd -P)
HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
BIN="$HERE/mount-nfs/target/release/mount-nfs"
PIDF="$MOUNT.pid"

mounted() { mount | grep -F " on $MOUNT " > /dev/null 2>&1; }

case "$ACTION" in
start)
    [ -x "$BIN" ] || { echo "build it first: cargo build --release --manifest-path $HERE/mount-nfs/Cargo.toml" >&2; exit 1; }
    mkdir -p "$BACKING" "$MOUNT"
    if mounted; then echo "already mounted at $MOUNT" >&2; exit 1; fi
    nohup "$BIN" "$BACKING" "$MOUNT" > "$MOUNT.log" 2>&1 < /dev/null &
    PID=$!
    i=0
    while [ "$i" -lt 150 ]; do
        if ! kill -0 "$PID" 2>/dev/null; then echo "SERVER DIED"; cat "$MOUNT.log"; exit 1; fi
        if mounted; then echo "$PID" > "$PIDF"; echo "mounted pid=$PID"; exit 0; fi
        i=$((i + 1)); sleep 1
    done
    echo "MOUNT TIMEOUT"; kill -TERM "$PID" 2>/dev/null || true; exit 1
    ;;
stop)
    if [ ! -f "$PIDF" ]; then
        mounted && { echo "mounted with no pid file, unmounting" >&2; /sbin/umount "$MOUNT"; }
        echo "no pid file"; exit 0
    fi
    PID=$(cat "$PIDF")
    CMD=$(ps -o command= -p "$PID" 2>/dev/null || true)
    case "$CMD" in
    *"$BIN"*) ;;
    *) echo "pid $PID is not $BIN: [$CMD]" >&2; exit 1 ;;
    esac
    kill -TERM "$PID"
    i=0
    while [ "$i" -lt 60 ]; do
        kill -0 "$PID" 2>/dev/null || break
        i=$((i + 1)); sleep 1
    done
    mounted && { echo "still mounted, forcing" >&2; /sbin/umount -f "$MOUNT"; }
    rm -f "$PIDF"
    if mounted; then echo "STILL MOUNTED" >&2; exit 1; fi
    echo "unmounted"
    ;;
status)
    if mounted; then echo "mounted"; mount | grep -F " on $MOUNT "; else echo "not mounted"; fi
    ;;
*) echo "usage: mount.sh BACKING MOUNT start|stop|status" >&2; exit 2 ;;
esac
