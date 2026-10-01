set -eu
. /Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/spike3/env.sh
SOURCE=$1
REV=$2
WORK=/home/zeeshanhaque/cowfs-spike3/round4-$REV
if test -e "$WORK"; then
    echo "Refusing existing VM QA directory: $WORK"
    exit 1
fi
mkdir -p "$WORK/source" "$WORK/build" "$WORK/logs"
git -C "$SOURCE" archive "$REV" | tar -x -C "$WORK/source"
cd "$WORK/source"
export CARGO_TARGET_DIR="$WORK/build"
check() {
    label=$1
    shift
    echo "START $label"
    if "$@" > "$WORK/logs/$label.log" 2>&1; then
        echo "PASS $label"
        tail -n 4 "$WORK/logs/$label.log"
    else
        echo "FAIL $label"
        tail -n 45 "$WORK/logs/$label.log"
        exit 1
    fi
}
check sample cargo test -j4 -p cowfs-store --features fault-injection --test round4 cut_crash_must_keep_pending_loss
check fmt cargo fmt --all --check
check clippy cargo clippy -j4 --workspace --all-targets --all-features -- -D warnings
check workspace cargo test -j4 --workspace
check recovery cargo test -j4 -p cowfs-store --features fault-injection --test round4 -- --nocapture
check crash4000 env C7C_SEEDS=1000 cargo test -j4 --release -p cowfs-store --test crash -- --nocapture
check power1800 env COWFS_POWERLOSS_SEEDS=400 cargo test -j4 --release -p cowfs-store --test round2 power_loss_orderings -- --nocapture
check docs cargo doc -j4 --workspace --no-deps
echo "VM QA complete: $WORK/logs"
