#!/bin/sh
# Small-corpus harness check inside the cowfs-spike3 OrbStack VM.
#
# usage (from the Mac): orb -m cowfs-spike3 sh <repo>/bench/linux-sample.sh
#
# The measured root is VM-local, never a shared host mount, and g1 and g2 build with a tmpfs
# CARGO_TARGET_DIR so the number is of the filesystem under test and not of the VM's own disk.
set -eu

REPO=${COWFS_BENCH_REPO:?set COWFS_BENCH_REPO to the repo path as the VM sees it}
# The VM's btrfs subvolume for /home refuses new files with ENOSPC while df reports 200 GiB free,
# so /tmp (tmpfs) is the default here and COWFS_BENCH_WORK moves it.
WORK=${COWFS_BENCH_WORK:-/tmp/cowfs-bench-work}
export COWFS_BENCH_SCALE="${COWFS_BENCH_SCALE:-2}"
export COWFS_BENCH_CARGO_JOBS="${COWFS_BENCH_CARGO_JOBS:-4}"
# RUSTUP_HOME too, or cargo is a rustup shim with no default toolchain in the VM.
export COWFS_BENCH_CARGO_HOME="${COWFS_BENCH_CARGO_HOME:-$WORK/cargo-home}"
export RUSTUP_HOME="${RUSTUP_HOME:-/home/zeeshanhaque/cowfs-spike3/rustup-home}"
export CARGO_TERM_COLOR=never
export CARGO_TARGET_DIR=/tmp/cowfs-bench-target

mkdir -p "$WORK" "$CARGO_TARGET_DIR"
python3 "$REPO/bench/gates.py" --root "$WORK/root-a" --label linux-native-a --reps 1
python3 "$REPO/bench/gates.py" --root "$WORK/root-b" --label linux-native-b --reps 1

A=$(find "$REPO/bench/out" -name 'linux-native-a-*.jsonl' -print | sort | tail -1)
B=$(find "$REPO/bench/out" -name 'linux-native-b-*.jsonl' -print | sort | tail -1)
python3 "$REPO/bench/compare.py" --native "$A" --cowfs "$B" --noise-floor "$B"
