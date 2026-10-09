#!/bin/bash
# Gate g5, the ONLY root-needing code: one xfstests `check` per case per arm.
# Design: docs/g5-harness-redesign.md. Judging is done by bench/g5_diff.py, never here.
#
#   sudo env XFS=<xfstests tree> BIN=<dir with cowfs and cowfs-daemon> [ARMS="native cowfs control"] \
#        [TMO=300] [SIZE=4G] bash g5_root.sh OUT CASES_FILE
#
# CASES_FILE: one `generic/NNN` per line. Everything is written under OUT and chowned
# back to $SUDO_USER on exit. No password is read or written here.
set -u
umask 022

record_identity() { # arm cdir mnt dev [snapshot]
  local arm=$1 cd=$2 mnt=$3 dev=$4 snap=${5:-}
  {
    echo "arm=$arm"
    echo "test_dir=$mnt"
    echo "test_dev=$dev"
    echo "target_line=$(findmnt -no FSTYPE,SOURCE,TARGET -T "$mnt" | awk 'NR==1{sub(/\[.*\]$/,"",$2); print $1"|"$2"|"$3}')"
    echo "fsroot=$(findmnt -no FSROOT -T "$mnt" | head -n1)"
    echo "source_mounts=$(findmnt -rn -S "$dev" | wc -l)"
    echo "backing=$(losetup -nO BACK-FILE "$dev" 2>/dev/null | head -n1)"
    echo "snapshot=$snap"
  } > "$cd/identity.txt"
}

run_check() { # cdir id path_prefix fstyp dev mnt
  local cd=$1 id=$2 pre=$3 fstyp=$4 dev=$5 mnt=$6
  mkdir -p "$cd/results"
  cd "$XFS" || return 1
  env PATH="$pre$PATH:/usr/sbin:/sbin" TMPDIR="$OUT/tmp" RESULT_BASE="$cd/results" \
      SCRATCH_DEV= SCRATCH_MNT= TEST_DIR="$mnt" TEST_DEV="$dev" FSTYP="$fstyp" \
      timeout -k 10 "${TMO:-300}" ./check "$id" </dev/null > "$cd/console.txt" 2>&1
  echo $? > "$cd/rc"
}

make_shim() { # shimdir mnt dev stash log
  local sd=$1 mnt=$2 dev=$3 stash=$4 log=$5
  local rm_ ru
  rm_=$(type -P mount); ru=$(type -P umount)
  mkdir -p "$sd"
  # Every call that names TEST_DEV or TEST_DIR is logged with its caller, whether or not
  # anything moved, so a missing restore cannot hide a cycle (see the design doc).
  cat > "$sd/umount" <<EOF
#!/bin/bash
caller=\$(tr '\\0' ' ' </proc/\$PPID/cmdline)
for a in "\$@"; do
  if [ "\$a" = "$mnt" ] || [ "\$a" = "$dev" ]; then
    if findmnt -n "$mnt" >/dev/null 2>&1; then
      echo "op=umount moved=1 caller=\$caller args=\$*" >> "$log"
      exec $rm_ --move "$mnt" "$stash"
    fi
    echo "op=umount moved=0 caller=\$caller args=\$*" >> "$log"
    break
  fi
done
exec $ru "\$@"
EOF
  cat > "$sd/mount" <<EOF
#!/bin/bash
n=\$#
if [ \$n -ge 2 ] && [ "\${@: -2:1}" = "$dev" ] && [ "\${@: -1}" = "$mnt" ]; then
  caller=\$(tr '\\0' ' ' </proc/\$PPID/cmdline)
  if ! findmnt -n "$mnt" >/dev/null 2>&1 && findmnt -n "$stash" >/dev/null 2>&1; then
    echo "op=mount moved=1 caller=\$caller args=\$*" >> "$log"
    exec $rm_ --move "$stash" "$mnt"
  fi
  echo "op=mount moved=0 caller=\$caller args=\$*" >> "$log"
fi
exec $rm_ "\$@"
EOF
  chmod +x "$sd/umount" "$sd/mount"
}

if [ "${1:-}" = inns ]; then # re-exec target inside the private mount namespace
  set -e
  mkdir -p "$MNT" "$STASH"
  mount --bind "$MAIN/$SN" "$MNT"
  [ "$ARM" != control ] || mount -o remount,ro,bind "$MNT"
  umount -l "$MAIN"
  make_shim "$CD/shim" "$MNT" cowfs "$STASH" "$CD/mountcycle.log"
  record_identity "$ARM" "$CD" "$MNT" cowfs "$SN"
  set +e
  run_check "$CD" "$ID" "$CD/shim:" fuse cowfs "$MNT"
  exit 0
fi

OUT=$(realpath -m "${1:?OUT}"); CASES=${2:?CASES_FILE}
: "${XFS:?}" "${BIN:?}"
[ "$(id -u)" = 0 ] || { echo "g5_root.sh must run as root" >&2; exit 2; }
ARMS=${ARMS:-native cowfs control}
mkdir -p "$OUT/tmp" "$OUT/native" "$OUT/cowfs" "$OUT/control"
cp "$CASES" "$OUT/cases.txt"
progress() { echo "$(date +%T) $*" >> "$OUT/progress.txt"; }

DPID=; LOOP=
teardown() {
  local rc=$?
  if [ -n "$DPID" ]; then
    "$BIN/cowfs" --socket "$OUT/cowfs/run/c.sock" shutdown >/dev/null 2>&1
    for _ in $(seq 20); do grep -q " $OUT/cowfs/main " /proc/self/mountinfo || break; sleep 1; done
    kill -0 "$DPID" 2>/dev/null && kill "$DPID" 2>/dev/null
    for _ in $(seq 20); do kill -0 "$DPID" 2>/dev/null || break; sleep 1; done
  fi
  mountpoint -q "$OUT/native/mnt" && { umount "$OUT/native/mnt" || umount -l "$OUT/native/mnt"; }
  [ -n "$LOOP" ] && losetup -d "$LOOP" 2>/dev/null
  {
    echo "mounts_left=$(grep -c " $OUT" /proc/self/mountinfo)"
    echo "loops_left=$(losetup -a | grep -c "$OUT")"
    echo "daemons_left=$(pgrep -fc "^$BIN/cowfs-daemon.*$OUT")"
  } > "$OUT/teardown.txt"
  [ "$rc" = 0 ] && progress ALLDONE || progress "ABORT rc=$rc"
  [ -n "${SUDO_USER:-}" ] && chown -R "$SUDO_USER:" "$OUT"
}
trap teardown EXIT

# Pin evidence is captured BEFORE the first case, by the run itself.
: > "$OUT/cleanup.txt"
for p in $(git -C "$XFS" status --porcelain | sed -n 's/^?? \(tmp\.[^ ]*\)$/\1/p'); do
  echo "removed stray root-owned $p" >> "$OUT/cleanup.txt"; rm -rf -- "${XFS:?}/$p"
done
profile=unknown
case "$BIN" in */release|*/release/) profile=release;; */debug|*/debug/) profile=debug;; esac
{
  echo "tree_head=$(git -C "$XFS" rev-parse HEAD)"
  echo "tree_porcelain=$(git -C "$XFS" status --porcelain | tr '\n' ' ' | sed 's/ *$//')"
  echo "check_sha256=$(sha256sum "$XFS/check" | cut -d' ' -f1)"
  echo "cowfs_bin=$BIN/cowfs-daemon"
  echo "cowfs_bin_sha256=$(sha256sum "$BIN/cowfs-daemon" | cut -d' ' -f1)"
  echo "cowfs_profile=$profile"
  echo "cowfs_rev=${COWFS_REV:-unrecorded}"
  echo "kernel=$(uname -r)"
  for id in $(sed 's#.*/##' "$CASES"); do
    echo "case_sha.$id=$(sha256sum "$XFS/tests/generic/$id" | cut -d' ' -f1)"
  done
} > "$OUT/meta.txt"
progress "meta recorded"

case " $ARMS " in *" native "*)
  img=$OUT/native/x.img; mnt=$OUT/native/mnt; mkdir -p "$mnt"
  truncate -s "${SIZE:-4G}" "$img"
  LOOP=$(losetup --find --show "$img") || exit 3
  for c in $(cat "$CASES"); do
    id=${c#generic/}; cd_=$OUT/native/$id; mkdir -p "$cd_"
    mountpoint -q "$mnt" && umount "$mnt"
    mkfs.ext4 -q -F "$LOOP" && mount "$LOOP" "$mnt" || { progress "native $id mkfs/mount failed"; continue; }
    record_identity native "$cd_" "$mnt" "$LOOP"
    run_check "$cd_" "$c" "" ext4 "$LOOP" "$mnt"
    progress "native $id rc=$(cat "$cd_/rc")"
  done
  mountpoint -q "$mnt" && umount "$mnt"; losetup -d "$LOOP"; LOOP=; rm -f "$img"
esac

case " $ARMS " in *" cowfs "*|*" control "*)
  mkdir -p "$OUT/cowfs/run" "$OUT/cowfs/main" "$OUT/cowfs/store"; chmod 700 "$OUT/cowfs/run"
  CLI="$BIN/cowfs --socket $OUT/cowfs/run/c.sock"
  setsid "$BIN/cowfs-daemon" --backend core --store "$OUT/cowfs/store" --mount "$OUT/cowfs/main" \
        --socket "$OUT/cowfs/run/c.sock" </dev/null > "$OUT/cowfs/daemon.log" 2>&1 &
  DPID=$!
  for _ in $(seq 30); do grep -q " $OUT/cowfs/main " /proc/self/mountinfo && break; sleep 1; done
  grep -q " $OUT/cowfs/main " /proc/self/mountinfo || { tail -n 20 "$OUT/cowfs/daemon.log" >&2; exit 4; }
  one() { # arm id case
    local arm=$1 id=$2 c=$3 cd_=$OUT/$1/$2 sn
    sn=g5-$arm-$id; mkdir -p "$cd_"
    $CLI snapshot create "$sn" >/dev/null 2>"$cd_/snapshot.err" || { progress "$arm $id snapshot failed"; return; }
    ARM=$arm CD=$cd_ ID=$c SN=$sn MAIN=$OUT/cowfs/main MNT=$OUT/cowfs/mnt STASH=$OUT/cowfs/stash \
      XFS=$XFS OUT=$OUT TMO=${TMO:-300} \
      unshare -m --propagation private bash "$0" inns
    # what is left in the bare directories, seen from OUTSIDE the case's namespace
    { ls -A "$OUT/cowfs/mnt"; ls -A "$OUT/cowfs/stash"; } > "$cd_/residue.txt" 2>&1
    $CLI snapshot rm "$sn" >/dev/null 2>&1
    progress "$arm $id rc=$(cat "$cd_/rc" 2>/dev/null)"
  }
  case " $ARMS " in *" cowfs "*) for c in $(cat "$CASES"); do one cowfs "${c#generic/}" "$c"; done;; esac
  case " $ARMS " in *" control "*) one control 005 generic/005;; esac
esac
exit 0
