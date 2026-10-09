#!/usr/bin/env bash
# Issue 173: the store's process-exit fault seam (feature `fault-injection`) must be absent from a
# build that does not ask for it, and this check must be able to tell the difference.
# Builds only the library crates, so no dev-dependency edge can switch the feature on, and uses its
# own target dir so it never touches the shared one.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/seam-check}"
NEEDLE=C7D_EXIT

count() { grep -ac "$NEEDLE" "$CARGO_TARGET_DIR"/debug/libcowfs_store.rlib || true; }

cargo build -q -p cowfs-store -p cowfs-gc --lib
if cargo tree -q -p cowfs-daemon -e features -i cowfs-store | grep -q fault-injection; then
  echo "FAIL: cowfs-daemon's graph enables fault-injection" >&2; exit 1
fi
cargo tree -q -p cowfs-store -e features --features fault-injection | grep -q fault-injection \
  || { echo "FAIL: cargo tree positive control found nothing" >&2; exit 1; }
off=$(count)
[ "$off" = 0 ] || { echo "FAIL: seam present in a build without the feature ($off hits)" >&2; exit 1; }

touch crates/cowfs-store/src/lib.rs
cargo build -q -p cowfs-store --lib --features fault-injection
on=$(count)
[ "$on" -gt 0 ] || { echo "FAIL: positive control found no seam with the feature on; the check is blind" >&2; exit 1; }
echo "ok: seam absent without the feature (0 hits), present with it ($on hits)"
