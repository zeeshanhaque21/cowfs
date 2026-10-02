#!/bin/sh
set -eu
ulimit -c 0
. /Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/spike3/env.sh
source_dir=/Users/zeeshanhaque/.treehouse/cowfs-7c1bf8/9/cowfs
work=/home/zeeshanhaque/cowfs-core-work
mkdir -p "$work"
tar -C "$source_dir" --exclude=.git --exclude=target --exclude=spikes -cf "$work/src.tar" Cargo.toml Cargo.lock crates docs
tar -C "$work" -xf "$work/src.tar"
rm -f "$work/src.tar"
cd "$work"
# A tmpfs target was tried because the home subvolume is reported to cap single file writes near
# 6.5 MiB, but the tmpfs runs out of memory instead: compiling redb in release with -j4 there was
# SIGKILLed. The VM-local target below completed this whole script, so it stays.
export CARGO_TARGET_DIR="$work/target"
export TMPDIR="$work/target/tmp"
mkdir -p "$CARGO_TARGET_DIR" "$TMPDIR"
phase=${1:-all}
if [ "$phase" = hammer ]; then
    timeout 300 env C2B_STRESS_SECS=3 cargo test -j4 -p cowfs-core --release --test critic2b -- --ignored stress --nocapture
    timeout 420 env C2B_STRESS_SECS=180 cargo test -j4 -p cowfs-core --release --test critic2b -- --ignored stress --nocapture
    echo 'LINUX HAMMER PASSED'
    exit
fi
timeout 300 cargo test -j4 -p cowfs-core --test flush_boundary -- --nocapture
timeout 300 env FSX_OPS=100 FSX_SEED=1 cargo test -j4 -p cowfs-core --release --test critic -- --ignored fsx --nocapture
timeout 1800 env INVARIANT_ITERS=100 INVARIANT_CRASHES=3 cargo test -j4 -p cowfs-core --release
if [ "$phase" = crate ]; then
    echo 'LINUX CRATE VALIDATION PASSED'
    exit
fi
for seed in 1 2; do
    timeout 1800 env FSX_OPS=100000 FSX_SEED="$seed" cargo test -j4 -p cowfs-core --release --test critic -- --ignored fsx --nocapture
done
timeout 300 env C2B_STRESS_SECS=3 cargo test -j4 -p cowfs-core --release --test critic2b -- --ignored stress --nocapture
timeout 420 env C2B_STRESS_SECS=180 cargo test -j4 -p cowfs-core --release --test critic2b -- --ignored stress --nocapture
echo 'LINUX CORE VALIDATION PASSED'
