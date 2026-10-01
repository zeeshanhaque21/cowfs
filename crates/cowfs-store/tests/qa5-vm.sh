set -eu
. /Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/spike3/env.sh
SOURCE=$1
REV=$2
WORK=/home/zeeshanhaque/cowfs-spike3/round5-$REV
if test -e "$WORK"; then
    test "${COWFS_QA5_RESUME:-0}" = 1
    git -C "$SOURCE" show "$REV:crates/cowfs-store/src/store.rs" | cmp - "$WORK/source/crates/cowfs-store/src/store.rs"
else
    mkdir -p "$WORK/source" "$WORK/build" "$WORK/logs"
    git -C "$SOURCE" archive "$REV" | tar -x -C "$WORK/source"
fi
cd "$WORK/source"
# The home subvolume silently caps a single file write at about 6.5 MiB, so cargo fails with
# ENOSPC part way through linking while `df` still reports free space. A tmpfs target dir has no
# such cap, so use one when it is available and fall back to the work directory otherwise.
BUILD=${COWFS_QA5_BUILD:-/tmp/cowfs-qa5-$REV}
if mkdir -p "$BUILD" 2>/dev/null && dd if=/dev/zero of="$BUILD/.probe" bs=1M count=64 2>/dev/null; then
    rm -f "$BUILD/.probe"
    echo "build dir: $BUILD"
else
    BUILD=$WORK/build
    echo "build dir: $BUILD (tmpfs unavailable)"
fi
mkdir -p "$BUILD"
export CARGO_TARGET_DIR="$BUILD"
LOGS=$BUILD/logs
mkdir -p "$LOGS"
check() {
    label=$1
    shift
    echo "START $label"
    if "$@" > "$LOGS/$label.log" 2>&1; then
        echo "PASS $label"
        tail -n 4 "$LOGS/$label.log"
    else
        echo "FAIL $label"
        tail -n 45 "$LOGS/$label.log"
        exit 1
    fi
}
check sample cargo test -j4 -p cowfs-store --features fault-injection --test round5 a_torn_watermark_slot_plus_a_valid_checkpoint
check components flock -w 120 "$RUSTUP_HOME/cowfs-components.lock" rustup component add rustfmt clippy
check fmt cargo fmt --all --check
check clippy-store cargo clippy -j4 -p cowfs-store --all-targets --all-features -- -D warnings
check round5 cargo test -j4 -p cowfs-store --features fault-injection --test round5 -- --test-threads=1
check round4 cargo test -j4 -p cowfs-store --features fault-injection --test round4 -- --test-threads=1
check round3 cargo test -j4 -p cowfs-store --test round3
check integrity cargo test -j4 -p cowfs-store --test integrity
check lock cargo test -j4 -p cowfs-store --test lock
check workspace cargo test -j4 --workspace
check crash4000 env C7C_SEEDS=1000 cargo test -j4 --release -p cowfs-store --test crash -- --nocapture
check power1800 env COWFS_POWERLOSS_SEEDS=400 cargo test -j4 --release -p cowfs-store --test round2 power_loss_orderings -- --nocapture
check docs cargo doc -j4 --workspace --no-deps
echo "VM QA complete: $LOGS"
