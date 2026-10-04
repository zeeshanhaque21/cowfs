#!/bin/sh
# Start, stop or restart one private cowfs daemon: its own store, its own socket, its own mount.
#
# usage: cowfs-mount.sh start|stop|restart|status BASE_DIR
#
# BASE_DIR holds store/, mnt/, rt/ and daemon.pid. Every path is under BASE_DIR, so nothing here
# can reach another worker's daemon, store or mount.
#
# Nothing is signalled, unmounted or deleted until the pid's argv, its start time and the mount
# table have all been checked, and the log is flushed before and after. A pid whose argv is not
# this daemon binary is refused rather than killed: shared daemon 15263 is not ours.
set -eu

ACTION=${1:?usage: cowfs-mount.sh start|stop|restart|status BASE_DIR}
BASE=${2:?usage: cowfs-mount.sh start|stop|restart|status BASE_DIR}
BASE=$(CDPATH='' cd -- "$BASE" && pwd)
STORE="$BASE/store"
MNT="$BASE/mnt"
RT="$BASE/rt"
PIDF="$BASE/daemon.pid"
LOG="$BASE/daemon.log"
HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
TARGET=${COWFS_DAEMON_BIN:-$BASE/../target/release/cowfs-daemon}
# Canonical, so the argv match below compares like with like: the pid file records the path this
# script was given, and the kernel records the path the process was started with.
if [ -x "$TARGET" ]; then
    TARGET=$(CDPATH='' cd -- "$(dirname -- "$TARGET")" && pwd -P)/$(basename -- "$TARGET")
fi
SOCK="$RT/control.sock"

mkdir -p "$STORE" "$MNT" "$RT"
chmod 700 "$RT"

mounted() {
    awk -v m="$MNT" '$5 == m || $2 == m { found = 1 } END { exit !found }' /proc/mounts
}

owned_pid() {
    [ -f "$PIDF" ] || return 1
    pid=$(cat "$PIDF")
    [ -n "$pid" ] || return 1
    kill -0 "$pid" 2>/dev/null || return 1
    cmd=$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null || true)
    case "$cmd" in
        *"$TARGET"*) printf '%s\n' "$pid" ;;
        *) echo "pid $pid is not $TARGET: [$cmd]" >&2; return 1 ;;
    esac
}

wait_for() {
    # $1 is a shell test string; bounded, and it gives up on a gone process as well as on a
    # missing mount, so a daemon that died in startup cannot look like a slow one.
    i=0
    while [ "$i" -lt 120 ]; do
        if eval "$1"; then return 0; fi
        [ -f "$PIDF" ] || return 1
        pid=$(cat "$PIDF" 2>/dev/null || echo "")
        [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || return 1
        i=$((i + 1))
        sleep 1
    done
    return 1
}

start() {
    [ -x "$TARGET" ] || { echo "no daemon binary at $TARGET" >&2; exit 1; }
    if mounted; then echo "$MNT is already mounted" >&2; exit 1; fi
    rm -f "$SOCK"
    setsid "$TARGET" --store "$STORE" --mount "$MNT" --socket "$SOCK" --backend core \
        >> "$LOG" 2>&1 < /dev/null &
    echo $! > "$PIDF"
    if ! wait_for "test -S '$SOCK' && mounted"; then
        echo "the daemon did not mount within 120s" >&2
        tail -20 "$LOG" >&2
        exit 1
    fi
    # Record who is serving what, so a later stop can check it rather than assume it.
    pid=$(cat "$PIDF")
    {
        echo "--- started $(date -Is)"
        echo "pid $pid argv: $(tr '\0' ' ' < "/proc/$pid/cmdline")"
        echo "starttime $(awk '{print $22}' "/proc/$pid/stat")"
        awk -v m="$MNT" '$2 == m { print "mount: " $0 }' /proc/mounts
        echo "store $STORE socket $SOCK"
    } >> "$LOG"
    echo "started pid $pid mount $MNT store $STORE socket $SOCK"
}

stop() {
    pid=$(owned_pid) || { echo "no owned daemon pid" >&2; exit 1; }
    cmd=$(tr '\0' ' ' < "/proc/$pid/cmdline")
    starttime=$(awk '{print $22}' "/proc/$pid/stat")
    echo "--- stopping $(date -Is) pid $pid starttime $starttime argv: $cmd" >> "$LOG"
    awk -v m="$MNT" '$2 == m { print "mount before: " $0 }' /proc/mounts >> "$LOG"
    kill -TERM "$pid"
    i=0
    while [ "$i" -lt 60 ]; do
        kill -0 "$pid" 2>/dev/null || break
        i=$((i + 1))
        sleep 1
    done
    if kill -0 "$pid" 2>/dev/null; then
        echo "the daemon ignored SIGTERM after 60s" >&2
        exit 1
    fi
    i=0
    while [ "$i" -lt 30 ]; do
        mounted || break
        i=$((i + 1))
        sleep 1
    done
    mounted && { echo "still mounted after the daemon exited" >&2; exit 1; }
    echo "unmounted $MNT" >> "$LOG"
    rm -f "$PIDF"
    sync
    echo "stopped pid $pid"
}

case "$ACTION" in
    start) start ;;
    stop) stop ;;
    restart)
        stop
        start
        ;;
    status)
        if pid=$(owned_pid); then
            echo "owned daemon pid $pid"
            tr '\0' ' ' < "/proc/$pid/cmdline"; echo
        else
            echo "no owned daemon"
        fi
        mounted && awk -v m="$MNT" '$2 == m { print "mounted: " $0 }' /proc/mounts || echo "not mounted"
        ;;
    *)
        echo "usage: cowfs-mount.sh start|stop|restart|status BASE_DIR" >&2
        exit 2
        ;;
esac
