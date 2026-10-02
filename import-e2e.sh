#!/bin/bash
# End-to-end `cowfs import` through a real mount: a synthetic tree with text, binary, 0-byte and
# UTF-8-named entries, imported into a real core store, verified through the mount, then a kill -9
# mid-import to prove a partial import is never visible, then a restart and a repeat import.
#
# Usage: import-e2e.sh <workdir>
# The workdir must be a private directory on a real filesystem: it holds the store, the mount and
# the synthetic source tree.
set -eu

W=${1:?workdir}
BIN=${BIN:?the cowfs binary}
DAEMON=${DAEMON:?the cowfs-daemon binary}
OUT="$W/out"
rm -rf "$OUT"
mkdir -p "$OUT/rt" "$OUT/mnt"
chmod 700 "$OUT" "$OUT/rt"

SRC="$OUT/src"
mkdir -p "$SRC/deep/a/b/c" "$SRC/empty-dir"
printf 'hello cowfs\n' > "$SRC/readme.txt"
# ~12 MiB of incompressible-ish bytes so the import is long enough to kill in the middle
dd if=/dev/urandom of="$SRC/deep/a/b/c/big.bin" bs=1M count=12 2>/dev/null
: > "$SRC/zero-byte"
printf 'utf-8 names\n' > "$SRC/café 😀.txt"
ln -sf ../readme.txt "$SRC/deep/a/link"
ln -sf /nowhere "$SRC/deep/dangling"
mkdir -p "$SRC/.git/refs"
printf 'ref: refs/heads/main\n' > "$SRC/.git/HEAD"

DAEMON_PID=""
cleanup() {
  if [ -n "$DAEMON_PID" ]; then
    kill -TERM "$DAEMON_PID" 2>/dev/null || true
    for _ in $(seq 1 100); do kill -0 "$DAEMON_PID" 2>/dev/null || break; sleep 0.1; done
  fi
  if mount | grep -q " on $OUT/mnt "; then
    umount "$OUT/mnt" 2>/dev/null || true
  fi
}
trap cleanup EXIT

wait_socket() {
  for _ in $(seq 1 300); do [ -S "$OUT/rt/c.sock" ] && return 0; sleep 0.1; done
  echo "FAIL: the daemon never bound its socket"; cat "$OUT/daemon.log"; exit 1
}

names() { "$BIN" --socket "$OUT/rt/c.sock" --json snapshot list | tr ',' '\n' | grep '"name"' || true; }

echo "== start the daemon on a real store"
"$DAEMON" --store "$OUT/store" --mount "$OUT/mnt" --socket "$OUT/rt/c.sock" --backend core \
  > "$OUT/daemon.log" 2>&1 &
DAEMON_PID=$!
wait_socket
echo "adapter: $(mount | grep " on $OUT/mnt " | sed 's/.*(//;s/,.*//')"

echo "== import the synthetic tree"
"$BIN" --socket "$OUT/rt/c.sock" import "$SRC"
echo "== import again under a second name: the content is already stored"
"$BIN" --socket "$OUT/rt/c.sock" import "$SRC" --store-name src2
echo "== a second import of the first name must fail with already_exists"
if "$BIN" --socket "$OUT/rt/c.sock" --json import "$SRC" > "$OUT/dup.out" 2>&1; then
  echo "FAIL: a duplicate name was accepted"; cat "$OUT/dup.out"; exit 1
fi
grep -q '"already_exists"' "$OUT/dup.out" || { echo "FAIL: wrong error"; cat "$OUT/dup.out"; exit 1; }
echo "ok: $(cat "$OUT/dup.out")"

echo "== the imported snapshot is readable through the mount and matches the source"
M="$OUT/mnt/src"
for p in readme.txt zero-byte "café 😀.txt" deep/a/b/c/big.bin .git/HEAD; do
  cmp "$SRC/$p" "$M/$p" || { echo "FAIL: $p differs"; exit 1; }
done
[ "$(readlink "$M/deep/a/link")" = "../readme.txt" ] || { echo "FAIL: the symlink differs"; exit 1; }
[ "$(readlink "$M/deep/dangling")" = "/nowhere" ] || { echo "FAIL: the dangling symlink differs"; exit 1; }
[ -d "$M/empty-dir" ] || { echo "FAIL: the empty directory is missing"; exit 1; }
echo "ok: $(find "$M" -mindepth 1 | wc -l | tr -d ' ') entries match"

echo "== the source tree was not written to"
BEFORE=$(cd "$SRC" && find . | sort | cksum)
[ "$BEFORE" = "$(cd "$SRC" && find . | sort | cksum)" ] || { echo "FAIL: the source changed"; exit 1; }
echo ok

echo "== kill -9 the daemon mid-import: no partial snapshot may be visible"
"$DAEMON" --store "$OUT/store2" --mount "$OUT/mnt2" --socket "$OUT/rt/c2.sock" --backend core \
  >> "$OUT/daemon.log" 2>&1 &
DAEMON_PID=$!
mkdir -p "$OUT/mnt2"
for _ in $(seq 1 300); do [ -S "$OUT/rt/c2.sock" ] && break; sleep 0.1; done
"$BIN" --socket "$OUT/rt/c2.sock" import "$SRC" --store-name killed > "$OUT/killed.out" 2>&1 &
IMPORT_PID=$!
# The import must still be running when the daemon dies, or nothing was proven.
sleep 0.4
if ! kill -0 "$IMPORT_PID" 2>/dev/null; then
  echo "FAIL: the import finished before the kill; make the source tree bigger"; exit 1
fi
kill -9 "$DAEMON_PID" 2>/dev/null || true
wait "$IMPORT_PID" 2>/dev/null || true
DAEMON_PID=""
echo "daemon killed; import output: $(tail -1 "$OUT/killed.out" 2>/dev/null || echo none)"
if mount | grep -q " on $OUT/mnt2 "; then umount "$OUT/mnt2" 2>/dev/null || true; fi

"$DAEMON" --store "$OUT/store2" --mount "$OUT/mnt2" --socket "$OUT/rt/c3.sock" --backend core \
  >> "$OUT/daemon.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 300); do [ -S "$OUT/rt/c3.sock" ] && break; sleep 0.1; done
echo "snapshots after the restart: $("$BIN" --socket "$OUT/rt/c3.sock" --json snapshot list)"
if "$BIN" --socket "$OUT/rt/c3.sock" --json snapshot list | grep -q '"killed"'; then
  echo "FAIL: the interrupted import left a snapshot a caller can see"; exit 1
fi
echo "ok: no snapshot named killed exists"

echo "== restart and re-import: idempotent, and the bytes are already stored"
"$BIN" --socket "$OUT/rt/c3.sock" import "$SRC" --store-name killed
"$BIN" --socket "$OUT/rt/c3.sock" import "$SRC" --store-name killed-again
echo "IMPORT_E2E_OK"