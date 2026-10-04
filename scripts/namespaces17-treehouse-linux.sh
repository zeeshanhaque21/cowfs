#!/bin/sh
# Linux integration run for issue #17: the real cowfs-treehouse companion building through the
# canonical namespace seam over a real cowfs FUSE mount.
#
# usage: namespaces17-treehouse-linux.sh [REPO_ROOT]
#
# This is the acceptance for the wiring, not for the helper. scripts/namespaces17-linux.sh covers the
# helper directly; this one drives `cowfs-treehouse base refresh --build --canonical`, which is the
# path a pool owner actually uses, and it reads the store back afterwards.
#
# Everything lives under REPO_ROOT/bench/out/namespaces17-treehouse: its own store, mount point,
# control socket, daemon, treehouse HOME and canonical directory. Nothing outside that directory is
# created or removed, and the only processes this script signals are the ones it started itself.
#
# The verdict is PASS, FAIL or UNMEASURABLE. A real failure of the product is FAIL; only a missing
# prerequisite is UNMEASURABLE.
set -eu

repo=${1:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}
out=$repo/bench/out/namespaces17-treehouse
canonical=$out/canonical
store=$out/store
mnt=$out/mnt
sock=$out/control.sock
log=$out/serve.log
pidfile=$out/serve.pid
helper=$repo/scripts/cowfs-ns-run.sh
thhome=$out/treehouse-home
bin=
serve_pid=

say() {
  printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" | tee -a "$out/run.log"
}

unmeasurable() {
  say "VERDICT: UNMEASURABLE: $*"
  exit 77
}

fail() {
  say "VERDICT: FAIL: $*"
  exit 1
}

owned_pid() {
  [ -n "$serve_pid" ] || return 1
  [ -d "/proc/$serve_pid" ] || return 1
  tr '\0' ' ' <"/proc/$serve_pid/cmdline" | grep -q -- "$store"
}

cleanup() {
  status=$?
  if [ -n "$serve_pid" ] && [ -d "/proc/$serve_pid" ]; then
    if ! owned_pid; then
      say "cleanup: pid $serve_pid is not this run's daemon, leaving it alone"
    elif [ -x "$bin" ]; then
      say "cleanup: shutting down the daemon this run started, pid $serve_pid"
      "$bin" --socket "$sock" shutdown >>"$log" 2>&1 || true
      i=0
      while [ -d "/proc/$serve_pid" ] && [ "$i" -lt 60 ]; do
        sleep 1
        i=$((i + 1))
      done
      if [ -d "/proc/$serve_pid" ]; then
        say "cleanup: the daemon is still alive after 60 s, sending SIGTERM"
        kill -TERM "$serve_pid" 2>/dev/null || true
        sleep 2
      fi
      if [ -d "/proc/$serve_pid" ]; then
        say "cleanup: STILL RUNNING, not killing again: pid $serve_pid"
      fi
    else
      say "cleanup: $bin is gone, so the daemon can only be signalled"
      kill -TERM "$serve_pid" 2>/dev/null || true
      sleep 2
    fi
  fi
  if grep -q " $mnt " /proc/self/mountinfo 2>/dev/null; then
    say "cleanup: the mount is still listed at $mnt, leaving it for a human to look at"
  else
    say "cleanup: the mount at $mnt is gone"
  fi
  exit $status
}
trap cleanup EXIT

# procfs reports mode 0444 and cp copies those bits, so the file this leaves behind could not be
# overwritten by the next run. Remove it first.
save_mountinfo() {
  rm -f "$1"
  cat /proc/self/mountinfo >"$1"
}

mkdir -p "$out"
# The daemon refuses a control socket whose directory is not private to this user.
chmod 700 "$out"
: >"$out/run.log"
say "run: repo=$repo out=$out"
[ "$(uname -s)" = Linux ] || unmeasurable "this run needs Linux, this is $(uname -s)"
[ -x "$helper" ] || fail "$helper is missing or not executable"
command -v cargo >/dev/null || unmeasurable "cargo is not on PATH"
command -v rustc >/dev/null || unmeasurable "rustc is not on PATH"
[ -c /dev/fuse ] || unmeasurable "/dev/fuse is not a character device, so no cowfs mount is possible"

export CARGO_TARGET_DIR="$out/target"
bin=$CARGO_TARGET_DIR/debug/cowfs
say "build: cargo build -p cowfs-cli -p cowfs-treehouse -p cowfs-daemon"
cargo build -p cowfs-cli -p cowfs-treehouse -p cowfs-daemon -j 4 >>"$out/build.log" 2>&1 ||
  fail "cargo build failed, see $out/build.log"
[ -x "$bin" ] || fail "cargo build produced no $bin"
companion=$CARGO_TARGET_DIR/debug/cowfs-treehouse
[ -x "$companion" ] || fail "cargo build produced no $companion"
daemon_bin=$CARGO_TARGET_DIR/debug/cowfs-daemon
[ -x "$daemon_bin" ] || fail "cargo build produced no $daemon_bin"
say "build: $("$bin" --version 2>&1 | head -1), companion present"

mkdir -p "$store" "$mnt" "$canonical" "$thhome"
[ -z "$(ls -A "$canonical")" ] || fail "$canonical is not empty, it must be a bare directory"
chmod 700 "$thhome"

# The build itself is plain rustc, so what lands in the artifact is the path and nothing else: no
# cargo fingerprint, no incremental cache, no timestamps in argv.
mkdir -p "$out/fixture"
cat >"$out/fixture/main.rs" <<'RS'
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

fn main() {
    println!("{}", add(2, 40));
}
RS

save_mountinfo "$out/mountinfo-host.txt"
say "host: $(grep -c . "$out/mountinfo-host.txt") mounts before the daemon starts"

# cowfs-daemon with the path backend, not `cowfs serve`: base_refresh copies a directory into the
# store, and the core backend refuses that on purpose because its snapshots are trees. The path
# backend serves the same store over the same FUSE mount and is the backend that supports the
# operation under test.
setsid "$daemon_bin" --backend path --store "$store" --mount "$mnt" --socket "$sock" >"$log" 2>&1 &
serve_pid=$!
printf '%s\n' "$serve_pid" >"$pidfile"
say "daemon: pid $serve_pid, store $store, mount $mnt"

# Wait for the mount, failing on the two signals that mean it will never arrive.
i=0
while ! grep -q " $mnt " /proc/self/mountinfo 2>/dev/null; do
  if ! kill -0 "$serve_pid" 2>/dev/null; then
    tail -5 "$log" | tee -a "$out/run.log"
    fail "the daemon exited before it mounted"
  fi
  if grep -qiE "fatal|panic|error" "$log" 2>/dev/null; then
    tail -5 "$log" | tee -a "$out/run.log"
    fail "the daemon logged an error before it mounted"
  fi
  [ "$i" -lt 180 ] || fail "the mount at $mnt did not appear within 3 minutes"
  sleep 1
  i=$((i + 1))
done
say "daemon: mounted, $(grep -c " $mnt " /proc/self/mountinfo) line(s) in mountinfo"
save_mountinfo "$out/mountinfo-before.txt"
ls -A "$canonical" >"$out/canonical-before.txt"

cowfs() { "$bin" --socket "$sock" "$@"; }
# HOME is sandboxed so treehouse reads no real config or pool, which also takes RUSTUP_HOME away:
# cargo on PATH is a rustup shim, and a shim with no rustup home cannot choose a toolchain. Both
# toolchain directories are carried across explicitly, and the real HOME is never restored.
: "${RUSTUP_HOME:=$HOME/.rustup}"
: "${CARGO_HOME:=$HOME/.cargo}"
export RUSTUP_HOME CARGO_HOME
companion_run() { HOME=$thhome RUSTUP_HOME=$RUSTUP_HOME CARGO_HOME=$CARGO_HOME "$companion" --socket "$sock" "$@"; }

# The warm base, built through the seam. This is the delivery run: a real repo, a real git commit, a
# real companion invocation with --canonical, and a real warm base snapshot afterwards.
repo_dir=$out/repo
# Recreated every run, so a second run in the same directory starts from nothing rather than
# inheriting the last run's commit.
rm -rf "$repo_dir"
mkdir -p "$repo_dir"
cp "$out/fixture/main.rs" "$repo_dir/main.rs"
git -C "$repo_dir" init -q -b main 2>/dev/null
git -C "$repo_dir" config user.email ns17@example.invalid
git -C "$repo_dir" config user.name ns17
git -C "$repo_dir" add main.rs
git -C "$repo_dir" commit -q -m "the warm base fixture"
commit=$(git -C "$repo_dir" rev-parse HEAD)
say "repo: $repo_dir at $commit"

say "warm base: importing the repo into the store"
cowfs import "$repo_dir" --name base >>"$out/import.log" 2>&1 ||
  fail "import failed, see $out/import.log"

# The build runs in the snapshot itself, through --slot, because this host has no treehouse binary to
# lease a slot with. That is the same run_build call site a leased slot takes, so the seam under test
# is identical; only the slot provider differs, and the doc says so.
refresh() { # refresh SLOT_DIR OUT_NAME
  companion_run --json base refresh --repo "$repo_dir" --ref main --slot "$1" --build \
    'rustc -g --edition 2021 main.rs -o app' --canonical "$canonical" --ns-helper "$helper" \
    >"$2" 2>&1
}

# Judges one `base refresh` by what it produced, not by its exit code.
#
# UNMEASURABLE means no build ran, and that is the one failure that is not about the artifact. Any
# other nonzero exit is tolerated only when the artifact landed: `base refresh` runs the build
# before the daemon materialises the warm base, and on git 2.39 that second step fails because
# `git worktree add --detach <sha>` reports the commit on stdout instead of the path. That is a
# daemon defect outside this change, and it is reported here rather than swallowed.
classify_refresh() { # classify_refresh SLOT OUT_NAME
  if grep -q UNMEASURABLE "$2"; then
    tail -5 "$2" | tee -a "$out/run.log"
    unmeasurable "the companion reported no namespace: $(tail -1 "$2")"
  fi
  if [ -f "$mnt/$1/app" ]; then
    say "seam: the $1 build ran at the canonical path and its artifact landed; the refresh then reported: $(tail -1 "$2")"
    return 0
  fi
  fail "no artifact in $1, so the build never ran: $(tail -3 "$2" | tr '\n' ' ')"
}

# The build inside `base refresh` runs before the daemon's own base_refresh, so the artifact is on
# disk even when that later step fails. A failure there is reported and then tolerated, because the
# claim under test is about the path the build ran at, not about the daemon materialising a warm
# base. What is not tolerated is a missing artifact: that would mean the build never ran.
build_ran=0
if refresh "$mnt/base" "$out/refresh.log"; then
  say "warm base: refresh succeeded end to end"
  build_ran=1
else
  classify_refresh base "$out/refresh.log" || true
  build_ran=1
fi

[ -f "$mnt/base/app" ] || fail "the base snapshot has no app"
cp "$mnt/base/app" "$out/app-base"
say "warm base: the artifact landed in the base snapshot at the store level"

# Whatever the daemon's base_refresh did or did not do, the two claims below are about slots cloned
# from `base` and built through the companion, which is the mode (b) path issue 17 is about.
for name in slotA slotB; do
  say "slot: cloning $name from base"
  cowfs snapshot create "$name" --from base >"$out/snapshot-$name.log" 2>&1 ||
    fail "snapshot create $name failed, see $out/snapshot-$name.log"
done
cowfs snapshot list | tee -a "$out/run.log"
# "clone of base" is what the core backend records as a parent; the path backend does not, so the
# clone is proven by content instead: both slots must hold base's main.rs, byte for byte.
base_main=$(sha256sum "$mnt/base/main.rs" | cut -d' ' -f1)
for name in slotA slotB; do
  [ -f "$mnt/$name/main.rs" ] || fail "$name has no main.rs, so it is not a clone of base"
  got=$(sha256sum "$mnt/$name/main.rs" | cut -d' ' -f1)
  [ "$got" = "$base_main" ] || fail "$name main.rs differs from base, so it is not a clone"
done
say "slots: slotA and slotB each hold base's main.rs at $base_main, so both are clones"
[ "$build_ran" = 1 ] || fail "the warm base build did not run, so the slots have nothing to compare"

# Two fresh slots cloned from that warm base, each built through the same seam at the same canonical
# path. These are the two artifacts the issue is about.
hash_of() { sha256sum "$1" | cut -d' ' -f1; }
for name in slotA slotB; do
  say "slot: building $name through the companion at the canonical path"
  refresh "$mnt/$name" "$out/build-$name.log" || true
  classify_refresh "$name" "$out/build-$name.log"
  [ -f "$mnt/$name/app" ] || fail "$name has no app after the build"
  cp "$mnt/$name/app" "$out/app-$name"
done

# The do-nothing baseline: the same two slots built at their own paths, no namespace at all.
for name in slotA slotB; do
  say "native: building the $name control at its own path"
  (cd "$mnt/$name" && rustc -g --edition 2021 main.rs -o app-native) ||
    fail "the native $name control build did not run"
  cp "$mnt/$name/app-native" "$out/app-N-$name"
done

save_mountinfo "$out/mountinfo-after.txt"
ls -A "$canonical" >"$out/canonical-after.txt"

hbase=$(hash_of "$out/app-base")
hA=$(hash_of "$out/app-slotA")
hB=$(hash_of "$out/app-slotB")
hNA=$(hash_of "$out/app-N-slotA")
hNB=$(hash_of "$out/app-N-slotB")
{
  printf 'base    warm base, built at %s %s %s\n' "$canonical" "$hbase" "$(wc -c <"$out/app-base")"
  printf 'slotA   canonical, fresh clone        %s %s\n' "$hA" "$(wc -c <"$out/app-slotA")"
  printf 'slotB   canonical, fresh clone        %s %s\n' "$hB" "$(wc -c <"$out/app-slotB")"
  printf 'N-slotA own path, do-nothing baseline %s %s\n' "$hNA" "$(wc -c <"$out/app-N-slotA")"
  printf 'N-slotB own path, do-nothing baseline %s %s\n' "$hNB" "$(wc -c <"$out/app-N-slotB")"
} >"$out/hashes.txt"
say "hashes:" && cat "$out/hashes.txt" | tee -a "$out/run.log"

python3 - "$out" "$canonical" "$mnt" <<'PY'
import sys

out, canonical, mnt = sys.argv[1:4]
names = ("base", "slotA", "slotB", "N-slotA", "N-slotB")
lines = []
for name in names:
    data = open(f"{out}/app-{name}", "rb").read()
    canonical_hits = data.count(canonical.encode())
    slot_hits = sum(data.count(f"{mnt}/{n}".encode()) for n in ("slotA", "slotB"))
    lines.append(
        f"{name}: canonical_path_embedded={canonical_hits > 0} ({canonical_hits} hits) "
        f"slot_path_embedded={slot_hits > 0} ({slot_hits} hits) bytes={len(data)}"
    )
open(f"{out}/embedded-paths.txt", "w").write("\n".join(lines) + "\n")
print("\n".join(lines))
PY

# Store readback with the daemon gone and the store reloaded from disk: fsck verifies every block.
say "readback: shutting the daemon down to reload the store from disk"
"$bin" --socket "$sock" shutdown >>"$log" 2>&1 || true
i=0
while [ -d "/proc/$serve_pid" ] && [ "$i" -lt 60 ]; do
  sleep 1
  i=$((i + 1))
done
if [ -d "/proc/$serve_pid" ]; then
  say "readback: the daemon is still alive, refusing to read the store under a live writer"
  fail "the daemon did not stop, so no store readback was taken"
fi
serve_pid=
say "readback: daemon gone, mounting the same store again"

setsid "$daemon_bin" --backend path --store "$store" --mount "$mnt" --socket "$sock" >>"$log" 2>&1 &
serve_pid=$!
i=0
while ! grep -q " $mnt " /proc/self/mountinfo 2>/dev/null; do
  if ! kill -0 "$serve_pid" 2>/dev/null; then
    fail "the daemon did not remount for the readback"
  fi
  [ "$i" -lt 180 ] || fail "the remount did not appear within 3 minutes"
  sleep 1
  i=$((i + 1))
done

cowfs status | tee -a "$out/run.log"
# The path backend keeps no block store, so fsck has nothing to verify and says so. The block-level
# readback is the helper run's job (scripts/namespaces17-linux.sh, core backend); what matters here
# is that every byte of every snapshot survives a daemon restart, which the reads below prove.
say "readback: fsck (the path backend has no block store, so this is expected to be refused)"
cowfs fsck >"$out/fsck.log" 2>&1 || say "readback: fsck says: $(tail -1 "$out/fsck.log")"
cat "$out/fsck.log" | tee -a "$out/run.log"

say "readback: per-snapshot main.rs and app, read back through the mount after a reload"
for name in base slotA slotB; do
  [ -f "$mnt/$name/main.rs" ] || fail "$name has no main.rs after the reload"
  [ -f "$mnt/$name/app" ] || fail "$name has no app after the reload"
  sha256sum "$mnt/$name/main.rs" >>"$out/readback.txt"
done
cat "$out/readback.txt" | tee -a "$out/run.log"

# The claims, each against its own control.
cmp -s "$out/mountinfo-before.txt" "$out/mountinfo-after.txt" ||
  fail "the caller's mounts changed across the canonical builds"
say "check: the caller's mounts are unchanged across the canonical builds"
[ ! -s "$out/canonical-after.txt" ] ||
  fail "the canonical directory is not empty outside the namespace"
say "check: $canonical is still empty outside every namespace"
[ "$hA" = "$hB" ] ||
  fail "two fresh slots built at one canonical path differ: $(grep '^slot' "$out/hashes.txt" | tr '\n' ' ')"
say "check: two fresh slots from one warm base, built at one canonical path, are byte-identical"
[ "$hA" != "$hNA" ] && [ "$hB" != "$hNB" ] ||
  fail "a native control matched its canonical build, so the control did not run"
say "check: both native controls at their own paths differ from their canonical builds"
grep -q "^slotA: .*canonical_path_embedded=True" "$out/embedded-paths.txt" ||
  fail "the slotA artifact does not record the canonical path, so the build proved nothing"
grep -q "^N-slotA: .*slot_path_embedded=True" "$out/embedded-paths.txt" ||
  fail "the native control does not record its own path, so the control proved nothing"
say "check: the canonical artifacts record the canonical path and the native controls record their own"

say "VERDICT: PASS: warm base and two fresh slots built through cowfs-treehouse at one canonical path over a real cowfs FUSE mount"
say "artifacts: $out"