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

# Slice 2: the whole workspace's normal and build edges must not enable the feature anywhere; only a
# dev-dependency edge (and the daemon's own opt-in feature) may. The dev-inclusive tree is the
# positive control, so a grep that matches nothing cannot pass for the right reason.
cargo tree -q --workspace -e normal,build,features -i cowfs-store | grep -q fault-injection \
  && { echo "FAIL: a normal or build edge in the workspace enables fault-injection" >&2; exit 1; }
cargo tree -q --workspace -e normal,build,dev,features -i cowfs-store | grep -q fault-injection \
  || { echo "FAIL: workspace tree positive control (dev edges) found nothing" >&2; exit 1; }
cargo tree -q -p cowfs-daemon --features fault-injection -e normal,build,features -i cowfs-store \
  | grep -q 'cowfs-daemon feature "fault-injection"' \
  || { echo "FAIL: the daemon feature does not forward the store feature" >&2; exit 1; }

# A built RELEASE cowfs-daemon without the feature carries no seam string; with it, it does.
count_bin() { grep -ac "$NEEDLE" "$CARGO_TARGET_DIR"/release/cowfs-daemon || true; }
cargo build -q --release -p cowfs-daemon
rel_off=$(count_bin)
[ "$rel_off" = 0 ] || { echo "FAIL: release cowfs-daemon carries the seam ($rel_off hits)" >&2; exit 1; }
touch crates/cowfs-store/src/lib.rs
cargo build -q --release -p cowfs-daemon --features fault-injection
rel_on=$(count_bin)
[ "$rel_on" -gt 0 ] || { echo "FAIL: release daemon with the feature has no seam; the binary check is blind" >&2; exit 1; }
echo "ok: release cowfs-daemon seam absent without the feature (0 hits), present with it ($rel_on hits)"
echo "ok: seam absent without the feature (0 hits), present with it ($on hits)"
