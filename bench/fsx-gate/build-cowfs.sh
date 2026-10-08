#!/bin/sh
# Build the two binaries a real mounted run needs: the daemon and the control client.
#
# usage: build-cowfs.sh SRC_DIR TARGET_DIR LOCK_PATH
#
# SRC_DIR is a tree with this workspace's Cargo.toml, Cargo.lock and crates/. The build is a
# serialized heavy workload, so it goes through the wave lock as one foreground invocation.
# Release debug info is off: the gate needs behaviour, not symbols, and the wave caps each
# worker's build artifacts at 1 GiB.
set -eu

SRC=${1:?usage: build-cowfs.sh SRC_DIR TARGET_DIR LOCK_PATH}
TARGET=${2:?usage: build-cowfs.sh SRC_DIR TARGET_DIR LOCK_PATH}
LOCK=${3:?usage: build-cowfs.sh SRC_DIR TARGET_DIR LOCK_PATH}
HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

mkdir -p "$TARGET"
cd "$SRC"
exec "$HERE/locked-run.sh" "$LOCK" env \
    CARGO_TARGET_DIR="$TARGET" \
    CARGO_PROFILE_RELEASE_DEBUG=false \
    CARGO_PROFILE_RELEASE_INCREMENTAL=false \
    cargo build --release --locked -j"$(nproc)" -p cowfs-daemon -p cowfs-cli
