#!/bin/sh
set -eu
. /Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/spike3/env.sh
source_dir=/Users/zeeshanhaque/.treehouse/cowfs-7c1bf8/9/cowfs
work=/home/zeeshanhaque/cowfs-core-work
mkdir -p "$work"
tar -C "$source_dir" --exclude=.git --exclude=target --exclude=spikes -cf - Cargo.toml Cargo.lock crates docs | tar -C "$work" -xf -
cd "$work"
export CARGO_TARGET_DIR="$work/target"
export TMPDIR="$work/target/tmp"
mkdir -p "$TMPDIR"
timeout 300 cargo test -j4 -p cowfs-core --test flush_boundary -- --nocapture
timeout 300 env FSX_OPS=100 FSX_SEED=1 cargo test -j4 -p cowfs-core --release --test critic -- --ignored fsx --nocapture
timeout 1800 cargo test -j4 -p cowfs-core
for seed in 1 2; do
    timeout 1800 env FSX_OPS=100000 FSX_SEED="$seed" cargo test -j4 -p cowfs-core --release --test critic -- --ignored fsx --nocapture
done
timeout 300 env C2B_STRESS_SECS=3 cargo test -j4 -p cowfs-core --release --test critic2b -- --ignored stress --nocapture
timeout 420 env C2B_STRESS_SECS=180 cargo test -j4 -p cowfs-core --release --test critic2b -- --ignored stress --nocapture
echo 'LINUX CORE VALIDATION PASSED'
