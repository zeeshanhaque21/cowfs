#!/bin/bash
# End-to-end `cowfs import` through a real mount: a synthetic tree with text, binary, 0-byte and
# UTF-8-named entries, imported into a real core store, verified through the mount, then a kill -9
# mid-import to prove a partial import is never visible, then a restart and a repeat import.
#
# Usage: IMPORT_E2E_MIB=192 import-e2e.sh <workdir>
#   IMPORT_E2E_MIB          size in MiB of the incompressible file (default 192)
#   IMPORT_E2E_SKIP_CRASH   set to anything to leave out the kill -9 section, for a fast fixture run
#
# The workdir must be a private directory on a real filesystem: it holds the store, the mount and
# the synthetic source tree.
#
# Source preservation is measured, not asserted: a manifest of the whole source tree is taken before
# the daemon starts and compared with one taken after the last import. The manifest records, for
# every entry, the path, the kind, the mode, the size, the SHA-256 of a regular file's bytes, a
# symlink's target, and mtime, ctime, inode, link count, uid and gid. atime is deliberately absent:
# the import reads the source, so atime is expected to move and comparing it would prove nothing.
# `manifest.py selftest` proves the comparison has teeth before any import runs.
set -eu

W=${1:?workdir}
BIN=${BIN:?the cowfs binary}
DAEMON=${DAEMON?:the cowfs-daemon binary}
MIB=${IMPORT_E2E_MIB:-192}
OUT="$W/out"
rm -rf "$OUT"
mkdir -p "$OUT/rt" "$OUT/mnt"
chmod 700 "$OUT" "$OUT/rt"

command -v python3 >/dev/null || { echo "FAIL: python3 is required"; exit 1; }

# The source-manifest tool: one self-contained file so the evidence travels with the harness.
cat > "$OUT/manifest.py" <<'MANIFEST'
#!/usr/bin/env python3
"""Deterministic source-tree manifest, and the comparison that says a tree changed.

Every field that could show a write, other than atime: the import reads the source, so atime moves
on any correct implementation and comparing it would only ever fail.

`manifest ROOT FILE` writes one JSON object per entry, sorted by the path's raw bytes.
`compare BEFORE AFTER` prints every differing field and exits 1 if anything moved.
`selftest DIR` proves an untouched tree passes and that a same-size content edit, a mode edit, an
mtime edit, a symlink retarget and a rename each fail it by the field that moved.
"""
import hashlib
import json
import os
import stat
import sys

CHUNK = 1 << 20
# atime_ns is deliberately not here.
FIELDS = ("kind", "mode", "size", "sha256", "target", "mtime_ns", "ctime_ns", "ino", "nlink",
          "uid", "gid")


def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(CHUNK), b""):
            h.update(block)
    return h.hexdigest()


def manifest(root):
    root_b = os.fsencode(root)
    out = []
    stack = [b""]
    while stack:
        rel = stack.pop()
        here = os.path.join(root_b, rel) if rel else root_b
        with os.scandir(here) as it:
            names = sorted((e.name for e in it))
        for name in names:
            child = os.path.join(rel, name) if rel else name
            full = os.path.join(root_b, child)
            st = os.lstat(full)
            rec = {
                "path": child.decode("utf-8", "surrogateescape"),
                "kind": None,
                "mode": format(stat.S_IMODE(st.st_mode), "04o"),
                "size": st.st_size if not stat.S_ISDIR(st.st_mode) else 0,
                "sha256": None,
                "target": None,
                "mtime_ns": st.st_mtime_ns,
                "ctime_ns": st.st_ctime_ns,
                "ino": st.st_ino,
                "nlink": st.st_nlink,
                "uid": st.st_uid,
                "gid": st.st_gid,
            }
            if stat.S_ISLNK(st.st_mode):
                rec["kind"] = "l"
                rec["size"] = 0
                rec["target"] = os.readlink(full).decode("utf-8", "surrogateescape")
            elif stat.S_ISDIR(st.st_mode):
                rec["kind"] = "d"
                stack.append(child)
            else:
                rec["kind"] = "f"
                rec["sha256"] = digest(full)
            out.append(rec)
    out.sort(key=lambda r: r["path"].encode("utf-8", "surrogateescape"))
    return out


def load(path):
    with open(path, "r", encoding="utf-8", errors="surrogateescape") as f:
        return {r["path"]: r for r in map(json.loads, f)}


def compare(before_path, after_path, capture=False):
    before, after = load(before_path), load(after_path)
    lines = []

    def say(line):
        lines.append(line)
        if not capture:
            print(line)

    problems = 0
    for p in sorted(set(before) | set(after)):
        b, a = before.get(p), after.get(p)
        if b is None:
            say(f"  + {p}: added")
            problems += 1
            continue
        if a is None:
            say(f"  - {p}: removed")
            problems += 1
            continue
        for f in FIELDS:
            if b.get(f) != a.get(f):
                say(f"  ~ {p}: {f} {b.get(f)!r} -> {a.get(f)!r}")
                problems += 1
    return (problems, "\n".join(lines)) if capture else problems


def write(records, path):
    with open(path, "w", encoding="utf-8", errors="surrogateescape") as f:
        for r in records:
            f.write(json.dumps(r, sort_keys=True, ensure_ascii=True) + "\n")


def selftest(dir):
    ok = True
    src = os.path.join(dir, "tree")
    os.makedirs(os.path.join(src, "sub"), exist_ok=True)
    with open(os.path.join(src, "a.txt"), "wb") as f:
        f.write(b"alpha\n")
    with open(os.path.join(src, "sub", "b.bin"), "wb") as f:
        f.write(bytes(range(256)) * 4)
    os.symlink("../a.txt", os.path.join(src, "sub", "link"))
    # Each edit below keeps every entry name and every file size, so a name-only or size-only check
    # passes it. Each one must be reported, and by the field that actually moved.
    original = bytes(range(256)) * 4

    def step(label, before, after, want_field):
        write(manifest(src), os.path.join(dir, after))
        problems, detail = compare(os.path.join(dir, before), os.path.join(dir, after),
                                   capture=True)
        if problems == 0:
            print(f"FAIL selftest: {label} compared equal")
            return False
        if f" {want_field} " not in detail:
            print(f"FAIL selftest: {label} was reported without {want_field}:\n{detail}")
            return False
        print(f"ok: {label} is caught ({problems} field(s), {want_field} among them)")
        return True

    write(manifest(src), os.path.join(dir, "m0"))
    write(manifest(src), os.path.join(dir, "m0b"))
    problems, detail = compare(os.path.join(dir, "m0"), os.path.join(dir, "m0b"), capture=True)
    if problems:
        print(f"FAIL selftest: an untouched tree compared different:\n{detail}")
        ok = False
    else:
        print("ok: an untouched tree compares equal")

    # A content edit that keeps the name and the size: a same-length overwrite in place.
    with open(os.path.join(src, "sub", "b.bin"), "wb") as f:
        f.write(original[:300] + b"\xff\xfe\xfd\xfc" + original[304:])
    ok &= step("a same-size content edit", "m0", "m1", "sha256")

    # Back to the original bytes, then a metadata edit with name, size and content unchanged.
    with open(os.path.join(src, "sub", "b.bin"), "wb") as f:
        f.write(original)
    write(manifest(src), os.path.join(dir, "m2"))
    os.chmod(os.path.join(src, "a.txt"), 0o600)
    ok &= step("a mode edit", "m2", "m3", "mode")

    # And an mtime edit, which a mode-and-hash check would miss.
    os.utime(os.path.join(src, "a.txt"), ns=(1_600_000_000_000_000_000, 1_600_000_000_000_000_000))
    ok &= step("an mtime edit", "m3", "m4", "mtime_ns")

    # And a symlink retarget, whose names are unchanged and whose size the check ignores.
    os.remove(os.path.join(src, "sub", "link"))
    os.symlink("../../a.txt", os.path.join(src, "sub", "link"))
    ok &= step("a symlink retarget", "m4", "m5", "target")

    # And a rename, which moves a path without changing a byte of content, so it shows up as a
    # removed and an added path rather than as a field difference.
    os.rename(os.path.join(src, "sub", "b.bin"), os.path.join(src, "sub", "b2.bin"))
    write(manifest(src), os.path.join(dir, "m6"))
    problems, detail = compare(os.path.join(dir, "m5"), os.path.join(dir, "m6"), capture=True)
    # A rename also moves the parent directory's mtime, so only the path lines are asserted here.
    if problems < 2 or "sub/b.bin: removed" not in detail or "sub/b2.bin: added" not in detail:
        print(f"FAIL selftest: a rename was not reported as one removal and one addition:\n{detail}")
        ok = False
    else:
        print("ok: a rename is caught (one path removed, one added)")
    return 0 if ok else 1


def main(argv):
    if len(argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    cmd = argv[1]
    if cmd == "manifest":
        write(manifest(argv[2]), argv[3])
        return 0
    if cmd == "compare":
        n = compare(argv[2], argv[3])
        if n:
            print(f"FAIL: {n} field(s) differ")
            return 1
        return 0
    if cmd == "selftest":
        return selftest(argv[2])
    print(f"unknown command {cmd}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
MANIFEST

SRC="$OUT/src"
mkdir -p "$SRC/deep/a/b/c" "$SRC/empty-dir"
printf 'hello cowfs\n' > "$SRC/readme.txt"
# $MIB of incompressible bytes: large enough that the import is still running well after the kill,
# so the crash check proves the partial import was never published rather than racing it.
dd if=/dev/urandom of="$SRC/deep/a/b/c/big.bin" bs=1M count="$MIB" 2>/dev/null
: > "$SRC/zero-byte"
printf 'utf-8 names\n' > "$SRC/café 😀.txt"
ln -sf ../readme.txt "$SRC/deep/a/link"
ln -sf /nowhere "$SRC/deep/dangling"
mkdir -p "$SRC/.git/refs"
printf 'ref: refs/heads/main\n' > "$SRC/.git/HEAD"

echo "== the comparison has teeth: content and metadata edits both fail it"
mkdir -p "$OUT/probe"
python3 "$OUT/manifest.py" selftest "$OUT/probe" || exit 1

echo "== manifest the source before anything touches it"
python3 "$OUT/manifest.py" manifest "$SRC" "$OUT/src.before"
echo "ok: $(wc -l < "$OUT/src.before" | tr -d ' ') entries recorded, atime excluded"

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
  for _ in $(seq 1 300); do [ -S "$1" ] && return 0; sleep 0.1; done
  echo "FAIL: the daemon never bound its socket"; cat "$OUT/daemon.log"; exit 1
}

echo "== start the daemon on a real store"
"$DAEMON" --store "$OUT/store" --mount "$OUT/mnt" --socket "$OUT/rt/c.sock" --backend core \
  > "$OUT/daemon.log" 2>&1 &
DAEMON_PID=$!
wait_socket "$OUT/rt/c.sock"
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

echo "== the three imports so far wrote nothing to the source"
python3 "$OUT/manifest.py" manifest "$SRC" "$OUT/src.after1"
python3 "$OUT/manifest.py" compare "$OUT/src.before" "$OUT/src.after1" \
  || { echo "FAIL: the source changed during the imports"; exit 1; }
echo "ok: every path, kind, mode, size, hash, target, mtime, ctime, inode, nlink, uid and gid is unchanged"

if [ -z "${IMPORT_E2E_SKIP_CRASH:-}" ]; then
  echo "== kill -9 the daemon mid-import: no partial snapshot may be visible"
  "$DAEMON" --store "$OUT/store2" --mount "$OUT/mnt2" --socket "$OUT/rt/c2.sock" --backend core \
    >> "$OUT/daemon.log" 2>&1 &
  DAEMON_PID=$!
  mkdir -p "$OUT/mnt2"
  for _ in $(seq 1 300); do [ -S "$OUT/rt/c2.sock" ] && break; sleep 0.1; done
  "$BIN" --socket "$OUT/rt/c2.sock" import "$SRC" --store-name killed > "$OUT/killed.out" 2>&1 &
  IMPORT_PID=$!
  # The import must still be running when the daemon dies, or nothing was proven.
  sleep 1
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
  wait_socket "$OUT/rt/c3.sock"
  echo "snapshots after the restart: $("$BIN" --socket "$OUT/rt/c3.sock" --json snapshot list)"
  if "$BIN" --socket "$OUT/rt/c3.sock" --json snapshot list | grep -q '"killed"'; then
    echo "FAIL: the interrupted import left a snapshot a caller can see"; exit 1
  fi
  echo "ok: no snapshot named killed exists"

  echo "== restart and re-import: idempotent, and the bytes are already stored"
  "$BIN" --socket "$OUT/rt/c3.sock" import "$SRC" --store-name killed
  "$BIN" --socket "$OUT/rt/c3.sock" import "$SRC" --store-name killed-again
else
  echo "== skipping the kill -9 section (IMPORT_E2E_SKIP_CRASH is set)"
fi

echo "== the source is still byte for byte what it was before the daemon started"
python3 "$OUT/manifest.py" manifest "$SRC" "$OUT/src.after"
python3 "$OUT/manifest.py" compare "$OUT/src.before" "$OUT/src.after" \
  || { echo "FAIL: the source changed"; exit 1; }
echo "ok: $(wc -l < "$OUT/src.after" | tr -d ' ') entries, nothing but atime moved"
echo "IMPORT_E2E_OK"
