#!/bin/bash
# Build the pinned xfstests tree, unprivileged, so g5 has ltp/fsstress, ltp/fsx and
# tests/generic/group.list. Issue 101. Linux only: the helpers need Linux headers.
#
#   bench/xfstests_build.sh XFS_DIR
#
# XFS_DIR absent: init it and fetch exactly the pinned commit. XFS_DIR present: no fetch,
# but the same checks run before make: the tree must hold NOTHING outside the commit,
# ignored files included (every build output is gitignored upstream, so a stale or
# hand-made ltp/fsx or include/config.h would otherwise pass), and HEAD must be the pin.
# A second run in the same directory is therefore refused until `git clean -xdf`; that is
# the point, the recorded digests must be of binaries this run built.
# Writes next to XFS_DIR: .build.log, .smoke-fsx.txt, .smoke-fsstress.txt and
# .identity.txt (key=value) with the helper sha256s the g5 meta records.
# The digests embed the build path (DW_AT_comp_dir from -g), so they are per host AND per
# path: build where the harness runs (g5_box.sh uses $G5_W/ref/xfstests).
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
UPSTREAM=${XFSTESTS_UPSTREAM:-https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git}
# The commit pin is the allowlist's, so the build and the reviewed cases cannot drift apart.
COMMIT=$(sed -n 's/^tree_sha \([0-9a-f]\{40\}\)$/\1/p' "$HERE/xfstests-allowlist.txt")
XFS=${1:?usage: xfstests_build.sh XFS_DIR}
[ -n "$COMMIT" ] || { echo "refusing: no tree_sha in xfstests-allowlist.txt" >&2; exit 1; }

if [ ! -d "$XFS/.git" ]; then
  git init -q "$XFS"
  git -C "$XFS" remote add origin "$UPSTREAM"
  # git checks every fetched object's hash, and HEAD is compared with the pin below.
  GIT_TERMINAL_PROMPT=0 git -C "$XFS" fetch -q --depth 1 origin "$COMMIT" \
    || { rm -rf -- "$XFS/.git"; echo "refusing: could not fetch $COMMIT from $UPSTREAM" >&2; exit 1; }
  git -C "$XFS" checkout -q FETCH_HEAD
fi
dirty=$(git -C "$XFS" status --porcelain --ignored)
[ -z "$dirty" ] || { printf 'refusing: %s holds files outside the commit, ignored ones included (git clean -xdf):\n%s\n' \
  "$XFS" "$(printf '%s\n' "$dirty" | head -n 20)" >&2; exit 1; }
got=$(git -C "$XFS" rev-parse HEAD)
[ "$got" = "$COMMIT" ] || { echo "refusing: $XFS is at $got, the pin is $COMMIT" >&2; exit 1; }
# The commit id already binds the tree; it is recorded, not separately pinned.
TREE=$(git -C "$XFS" rev-parse "HEAD^{tree}")

# The suite's own top-level build, unmodified flags. -k: src/locktest does not compile on
# kernel 7.2 headers, and one broken src/ helper must not hide the rest. Failed targets
# are recorded, not hidden.
log=$XFS.build.log
set +e
nice make -C "$XFS" -k -j"${JOBS:-4}" > "$log" 2>&1
make_rc=$?
set -e
failed=$(sed -n 's/^make\[[0-9]*\]: \*\*\* \[[^:]*:[0-9]*: \([^]]*\)\] Error.*/\1/p' "$log" | sort -u | tr '\n' ' ')

for f in ltp/fsstress ltp/fsx include/builddefs tests/generic/group.list; do
  [ -s "$XFS/$f" ] || { echo "build failed: $f missing, see $log" >&2; exit 1; }
done
for f in ltp/fsstress ltp/fsx; do
  [ -x "$XFS/$f" ] || { echo "build failed: $f not executable" >&2; exit 1; }
done
dirty=$(git -C "$XFS" status --porcelain)
[ -z "$dirty" ] || { echo "build dirtied the tree: $dirty" >&2; exit 1; }

# Smoke on the native filesystem under the build dir: a no-op helper must not pass.
sm=$(mktemp -d "$XFS.smoke.XXXXXX")
trap 'rm -rf -- "$sm"' EXIT
"$XFS/ltp/fsx" -N 200 -S 1 "$sm/fsx.dat" > "$XFS.smoke-fsx.txt" 2>&1
grep -q "All 200 operations completed A-OK" "$XFS.smoke-fsx.txt" \
  || { echo "fsx smoke failed, see $XFS.smoke-fsx.txt" >&2; exit 1; }
"$XFS/ltp/fsstress" -v -d "$sm/fss" -n 50 -p 1 -s 1 > "$XFS.smoke-fsstress.txt" 2>&1
ops=$(grep -o '^0/[0-9]*: ' "$XFS.smoke-fsstress.txt" | sort -u | wc -l)
# Not every op logs a 0/N line. Measured on the cachyos box with -s 1: 45 of 50; op 27
# (btrfs subvol_delete) prints `0:27:` instead, and ops 20, 36, 37 and 38 print nothing.
# The bar is half of -n, which a no-op or a helper that stops early cannot meet; it is
# not a claim that every op ran. The count is recorded.
[ "$ops" -ge 25 ] || { echo "fsstress smoke logged $ops ops, want at least 25 of 50" >&2; exit 1; }

{
  echo "xfstests_commit=$COMMIT"
  echo "xfstests_tree=$TREE"
  echo "xfstests_path=$(cd "$XFS" && pwd)"
  echo "make_rc=$make_rc"
  echo "make_failed_targets=${failed% }"
  echo "compiler=$(cc --version | head -n1)"
  echo "kernel=$(uname -r)"
  # Same keys as the g5_root.sh meta, so `grep -Fx` of these lines in a run's meta.txt
  # proves the run used this build.
  echo "fsstress_sha256=$(sha256sum "$XFS/ltp/fsstress" | cut -d' ' -f1)"
  echo "fsx_sha256=$(sha256sum "$XFS/ltp/fsx" | cut -d' ' -f1)"
  echo "group_list_sha256=$(sha256sum "$XFS/tests/generic/group.list" | cut -d' ' -f1)"
  echo "smoke_fsx=200 ops A-OK"
  echo "smoke_fsstress_ops=$ops"
} > "$XFS.identity.txt"
cat "$XFS.identity.txt"
