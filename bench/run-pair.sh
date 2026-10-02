#!/bin/sh
# Interleaved native/cowfs/native/cowfs run of bench/gates.py, under the shared CPU lock.
#
# usage: run-pair.sh NATIVE_ROOT COWFS_ROOT REPS [GATES]
#
# Interleaving is the point: two machines' worth of drift, thermal state and
# whatever else is running show up as arm-to-arm differences if the arms are
# adjacent, and cancel if they are not. The order is fixed native, cowfs, native,
# cowfs, so the two native runs bracket the pair and their ratio is the noise floor.
#
# The CPU lock is an atomic mkdir, retried every 10 s for 15 minutes, and removed by a trap
# so a kill still frees it. Set COWFS_BENCH_CPU_LOCK to move it.
set -eu

NATIVE_ROOT=${1:?usage: run-pair.sh NATIVE_ROOT COWFS_ROOT REPS [GATES]}
COWFS_ROOT=${2:?usage: run-pair.sh NATIVE_ROOT COWFS_ROOT REPS [GATES]}
REPS=${3:?usage: run-pair.sh NATIVE_ROOT COWFS_ROOT REPS [GATES]}
GATES=${4:-g1,g2,g3,g4,g5,g6}

HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
LOCK=${COWFS_BENCH_CPU_LOCK:-/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/cpu.lock}
DEADLINE=$(( $(date +%s) + 900 ))

while ! mkdir "$LOCK" 2>/dev/null; do
    if [ "$(date +%s)" -ge "$DEADLINE" ]; then
        echo "cpu lock still held after 15 minutes: $LOCK" >&2
        exit 75
    fi
    sleep 10
done
printf '%s pid %s at %s\n' "$(id -un)" "$$" "$(date)" > "$LOCK/owner"
trap 'rm -rf "$LOCK"' EXIT INT TERM

echo "cpu lock: $LOCK"
python3 "$HERE/gates.py" --root "$NATIVE_ROOT" --label native1 --reps "$REPS" --gates "$GATES"
python3 "$HERE/gates.py" --root "$COWFS_ROOT"  --label cowfs1  --reps "$REPS" --gates "$GATES"
python3 "$HERE/gates.py" --root "$NATIVE_ROOT" --label native2 --reps "$REPS" --gates "$GATES"
python3 "$HERE/gates.py" --root "$COWFS_ROOT"  --label cowfs2  --reps "$REPS" --gates "$GATES"
