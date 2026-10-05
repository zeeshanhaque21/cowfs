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
# Every run gets its own immutable attempt directory under REPO_ROOT/bench/out/ns17/, holding its own
# store, mount point, control socket, daemon, treehouse HOME, canonical directory, fixture repository and
# logs. The path components are short on purpose: a Unix socket path is limited to about 108 bytes, and
# a longer one is refused by the kernel at bind time with nothing but "path must be shorter than
# SUN_LEN", which is why the length is checked here first. An earlier run's
# attempt is never read, written or removed, so this script can be run again in the same checkout and
# each run stands on its own fixtures: without that, the second run recreated the fixture repository
# while the store persisted, the seed import collided, and the run failed for a reason that had nothing
# to do with the product. Nothing outside that attempts directory is created or removed, apart from the
# shared cargo build cache, and the only processes this script signals are the ones it started itself.
#
# The verdict is PASS, FAIL or UNMEASURABLE. A real failure of the product is FAIL; only a missing
# prerequisite is UNMEASURABLE.
set -eu

repo=${1:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}
root=$repo/bench/out/ns17
attempts=$root
# The attempt is named for when it started and which process started it, so it cannot already exist and
# two runs can never share one.
out=$attempts/a$(date -u +%m%dT%H%M%S)-$$
canonical=$out/canonical
store=$out/store
mnt=$out/mnt
sock=$out/c.sock
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

mkdir -p "$attempts"
# A refusal, not an overwrite: if this attempt exists, something is wrong with the naming and the run
# must not adopt a directory it did not create.
[ ! -e "$out" ] || fail "$out already exists, refusing to run inside it"
mkdir -p "$out"
# The daemon refuses a control socket whose directory is not private to this user.
chmod 700 "$out"
: >"$out/run.log"
prior=$(ls -1 "$attempts" 2>/dev/null | grep -c . || true)
say "run: repo=$repo attempt=$out ($prior earlier attempt(s) preserved)"
# A Unix socket address is at most 108 bytes including its terminator, so this is checked before a
# daemon is started rather than discovered from the kernel's refusal to bind.
# Measured in bytes, because that is the unit the kernel limit is in and not every shell counts
# characters. A pipe is needed since the value may hold characters a here-string would mangle.
sock_len=$(printf %s "$sock" | wc -c | tr -d ' ')
[ "$sock_len" -le 100 ] ||
  fail "the control socket path is $sock_len bytes, and a Unix socket path cannot exceed 107: $sock"
say "run: the control socket path is $sock_len bytes of the 107 a Unix socket allows"
[ "$(uname -s)" = Linux ] || unmeasurable "this run needs Linux, this is $(uname -s)"
[ -x "$helper" ] || fail "$helper is missing or not executable"
command -v cargo >/dev/null || unmeasurable "cargo is not on PATH"
command -v rustc >/dev/null || unmeasurable "rustc is not on PATH"
[ -c /dev/fuse ] || unmeasurable "/dev/fuse is not a character device, so no cowfs mount is possible"

# The build cache is shared between attempts on purpose: it is a cargo target directory, not a fixture,
# and rebuilding it per run would cost minutes for no extra evidence.
export CARGO_TARGET_DIR="$root/target"
bin=$CARGO_TARGET_DIR/debug/cowfs
# The build is pinned to this repository, never to whatever workspace the caller happens to be
# standing in. Without `--manifest-path`, running this script from another directory built the wrong
# workspace, which showed up as a confusing `cargo build` failure.
cargo_build() { # cargo_build PKGS...
  (cd "$repo" && cargo build --manifest-path "$repo/Cargo.toml" -j 4 "$@") >>"$out/build.log" 2>&1
}

say "build: cargo build -p cowfs-cli -p cowfs-treehouse -p cowfs-daemon"
cargo_build -p cowfs-cli -p cowfs-treehouse -p cowfs-daemon ||
  fail "cargo build failed, see $out/build.log"
[ -x "$bin" ] || fail "cargo build produced no $bin"
companion=$CARGO_TARGET_DIR/debug/cowfs-treehouse
[ -x "$companion" ] || fail "cargo build produced no $companion"
daemon_bin=$CARGO_TARGET_DIR/debug/cowfs-daemon
[ -x "$daemon_bin" ] || fail "cargo build produced no $daemon_bin"
version=$("$bin" --version 2>&1 || true)
say "build: cowfs ${version%%:*}, companion present"

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
# Never recreated over: the attempt directory is new, so this repository is this run's alone and its
# commits mean what this run says they mean.
[ ! -e "$repo_dir" ] || fail "$repo_dir already exists, refusing to recreate it"
mkdir -p "$repo_dir"
cp "$out/fixture/main.rs" "$repo_dir/main.rs"
git -C "$repo_dir" init -q -b main 2>/dev/null
git -C "$repo_dir" config user.email ns17@example.invalid
git -C "$repo_dir" config user.name ns17
git -C "$repo_dir" add main.rs
git -C "$repo_dir" commit -q -m "the warm base fixture"
commit=$(git -C "$repo_dir" rev-parse HEAD)
say "repo: $repo_dir at $commit"

# A seed, and only that. `import` copies a directory in; it does not publish a warm base and records
# no commit, so nothing may be cloned from this snapshot for the acceptance. The published base comes
# from the `base refresh` below, and `base status` has to confirm it before anything is cloned.
say "seed: importing the repo as a snapshot to refresh from"
cowfs import "$repo_dir" --name seed >>"$out/import.log" 2>&1 ||
  fail "the seed import failed, see $out/import.log"
say "seed: imported as 'seed'; no base is claimed from it"

# One `base refresh`, and its real exit code.
#
# There is no `|| true` and no tolerance here. `base refresh` is the operation that publishes the warm
# base, so a refresh that fails is a failure. An earlier version of this script accepted a failed
# refresh as long as the build's artifact had landed, and printed PASS with exit 0 while every
# product operation it claimed to accept had failed. An artifact cannot stand in for the postcondition
# it was supposed to establish.
#
# The build runs in the snapshot itself, through --slot, because this host has no treehouse binary to
# lease a slot with. That is the same run_build call site a leased slot takes, so the seam under test
# is identical; only the slot provider differs, and the doc says so.
refresh() { # refresh SLOT_DIR OUT_NAME
  companion_run --json base refresh --repo "$repo_dir" --ref main --slot "$1" --build \
    'rustc -g --edition 2021 main.rs -o app' --canonical "$canonical" --ns-helper "$helper" \
    >"$2" 2>&1
}

refresh_ok() { # refresh_ok SLOT OUT_NAME
  if refresh "$mnt/$1" "$2"; then
    say "refresh: $1 published its base and exited 0"
    return 0
  fi
  if grep -q UNMEASURABLE "$2"; then
    tail -5 "$2" | tee -a "$out/run.log"
    unmeasurable "the companion reported no namespace: $(tail -1 "$2")"
  fi
  fail "base refresh for $1 exited nonzero, so no warm base was published: $(tail -2 "$2" | tr '\n' ' ')"
}

# The published warm base, read back through the control API.
#
# `base status` is the product's own answer to "is there a base for this repository", so it, and not a
# file seen on the mount, is what this asserts. A base that was imported from a directory would fail
# here, which is the point: an import is a seed, not a refresh.
published_base() { # published_base OUT_NAME
  companion_run --json base status --repo "$repo_dir" --ref main >"$1" 2>&1 || true
  python3 - "$1" <<'PYCHECK'
import json, sys

last = [l for l in open(sys.argv[1]).read().splitlines() if l.strip()]
if not last:
    raise SystemExit("base status printed nothing")
status = json.loads(last[-1])
# The human line goes to stderr, so this prints the snapshot name and nothing else and its caller
# can use it as a command substitution.
print(
    f"base status: pool_id={status['pool_id']} snapshot={status['snapshot']} "
    f"base_commit={status['base_commit']} fresh={status['fresh']}",
    file=sys.stderr,
)
if status["base_commit"] is None:
    raise SystemExit(f"no base commit is recorded, so no warm base was published: {status}")
if status["snapshot"] != f"{status['pool_id']}-base":
    raise SystemExit(f"the base snapshot name is not the derived one: {status}")
if not status["fresh"]:
    raise SystemExit(f"the base is not fresh for this ref: {status}")
# The commit goes to a sidecar rather than to stdout, so this function prints the snapshot name and
# nothing else and can be used directly as a command substitution.
open(sys.argv[1] + ".commit", "w").write(f'{status["base_commit"]}\n')
print(status["snapshot"])
PYCHECK
}

# The commit the control API reported, which is what a reopen has to report again.
reported_commit() { # reported_commit STATUS_LOG
  cat "$1.commit"
}

say "warm base: refreshing through cowfs-treehouse base refresh --canonical $canonical"
refresh_ok seed "$out/refresh.log"

# The postcondition, asserted before anything is cloned from it.
base_snap=$(published_base "$out/base-status.log") ||
  fail "the control API does not report a published warm base: $(tail -1 "$out/base-status.log")"
say "warm base: the control API reports it published and fresh; snapshot $base_snap"
base_commit=$(reported_commit "$out/base-status.log")
[ -n "$base_commit" ] || fail "base status printed no commit to compare"
say "warm base: the reported commit is $base_commit"

# Publishing a base must not leave the caller's repository changed. This is where the provenance is
# checked to be a record and not a side effect: the record is written to the store, never into the
# repository the refresh read.
say "warm base: the repository still holds only its own worktree"
git -C "$repo_dir" worktree list --porcelain >"$out/worktrees-after-refresh.txt" 2>&1 ||
  fail "git worktree list failed after the refresh"
if [ "$(grep -c '^worktree ' "$out/worktrees-after-refresh.txt")" -ne 1 ]; then
  cat "$out/worktrees-after-refresh.txt" | tee -a "$out/run.log"
  fail "the refresh left extra worktrees registered: $(grep '^worktree ' "$out/worktrees-after-refresh.txt" | tr '\n' ' ')"
fi
[ -z "$(git -C "$repo_dir" status --porcelain)" ] ||
  fail "the refresh dirtied the repository: $(git -C "$repo_dir" status --porcelain | tr '\n' ' ')"
[ "$(git -C "$repo_dir" rev-parse HEAD)" = "$commit" ] ||
  fail "the refresh moved the repository's HEAD"
say "warm base: one worktree, a clean tree, HEAD still $commit"

# What a warm base holds: the repository's committed source at the ref, published so a fresh slot can
# start from it. `base refresh` builds in a leased slot but publishes a fresh checkout of the ref, so
# the base carries the committed files and nothing that was only built. Asserting an `app` here would
# be asserting a different product.
[ -f "$mnt/$base_snap/main.rs" ] || fail "the published warm base $base_snap has no main.rs"
repo_src=$(sha256sum "$repo_dir/main.rs" | cut -d' ' -f1)
base_src=$(sha256sum "$mnt/$base_snap/main.rs" | cut -d' ' -f1)
[ "$base_src" = "$repo_src" ] ||
  fail "the published warm base's main.rs is not the repository's at $commit"
cp "$mnt/$base_snap/main.rs" "$out/source-base"
say "warm base: the published base holds the repository's committed source at $base_src"

# Two fresh slots cloned from the PUBLISHED warm base, never from the imported seed. Both must carry
# the base's own source.
for name in slotA slotB; do
  say "slot: cloning $name from the published warm base $base_snap"
  cowfs snapshot create "$name" --from "$base_snap" >"$out/snapshot-$name.log" 2>&1 ||
    fail "snapshot create $name failed, see $out/snapshot-$name.log"
done
cowfs snapshot list | tee -a "$out/run.log"
base_main=$(sha256sum "$mnt/$base_snap/main.rs" | cut -d' ' -f1)
for name in slotA slotB; do
  [ -f "$mnt/$name/main.rs" ] || fail "$name has no main.rs, so it is not a clone of $base_snap"
  got=$(sha256sum "$mnt/$name/main.rs" | cut -d' ' -f1)
  [ "$got" = "$base_main" ] || fail "$name main.rs differs from the warm base, so it is not a clone"
done
say "slots: slotA and slotB each hold the warm base's main.rs at $base_main"

# Two fresh slots cloned from that warm base, each built through the same seam at the same canonical
# path. These are the two artifacts the issue is about.
hash_of() { sha256sum "$1" | cut -d' ' -f1; }
for name in slotA slotB; do
  say "slot: building $name through the companion at the canonical path"
  refresh_ok "$name" "$out/build-$name.log"
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

hA=$(hash_of "$out/app-slotA")
hB=$(hash_of "$out/app-slotB")
hNA=$(hash_of "$out/app-N-slotA")
hNB=$(hash_of "$out/app-N-slotB")
{
  printf 'source   warm base source at %s %s %s\n' "$commit" "$(hash_of "$out/source-base")" "$(wc -c <"$out/source-base")"
  printf 'slotA    canonical, fresh clone        %s %s\n' "$hA" "$(wc -c <"$out/app-slotA")"
  printf 'slotB    canonical, fresh clone        %s %s\n' "$hB" "$(wc -c <"$out/app-slotB")"
  printf 'N-slotA  own path, do-nothing baseline %s %s\n' "$hNA" "$(wc -c <"$out/app-N-slotA")"
  printf 'N-slotB  own path, do-nothing baseline %s %s\n' "$hNB" "$(wc -c <"$out/app-N-slotB")"
} >"$out/hashes.txt"
say "hashes:" && cat "$out/hashes.txt" | tee -a "$out/run.log"

python3 - "$out" "$canonical" "$mnt" <<'PY'
import sys

out, canonical, mnt = sys.argv[1:4]
names = ("slotA", "slotB", "N-slotA", "N-slotB")
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

# The record was written to the store, so a daemon that has never seen this refresh in memory must
# still find the base and report the same commit. Before the fix the tree survived this restart and
# the provenance did not, which is why the readback has to ask the API again rather than trust it.
say "readback: asking the control API for the base again, from a daemon that never saw the refresh"
reopen_snap=$(published_base "$out/base-status-reopened.log") ||
  fail "the reopened daemon does not report a published warm base: $(tail -1 "$out/base-status-reopened.log")"
reopen_commit=$(reported_commit "$out/base-status-reopened.log")
[ "$reopen_commit" = "$base_commit" ] ||
  fail "the reopened daemon reports commit $reopen_commit, the refresh published $base_commit"
[ "$reopen_snap" = "$base_snap" ] ||
  fail "the reopened daemon reports base $reopen_snap, the refresh published $base_snap"
say "readback: the reopened daemon reports $reopen_snap at the same commit $reopen_commit"

# The published base and the two slots, each read back through the mount after the store was reloaded
# by a daemon that never saw the refresh in memory. The base is named from the pool, so it is named
# here from what the control API reported rather than assumed.
say "readback: per-snapshot main.rs, and app in the slots, read back through the mount after a reload"
: >"$out/readback.txt"
for name in "$base_snap" slotA slotB; do
  [ -f "$mnt/$name/main.rs" ] || fail "$name has no main.rs after the reload"
  sha256sum "$mnt/$name/main.rs" >>"$out/readback.txt"
done
for name in slotA slotB; do
  [ -f "$mnt/$name/app" ] || fail "$name has no app after the reload"
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

# The counterexample a stored commit has to survive: a base whose repository has moved on is stale.
# Without this, a base that merely records a commit would look exactly as good as one that is
# compared against the repository, and "fresh" would be a word rather than a check.
say "control: moving the repository forward must make the recorded base stale"
printf '// moved on\n' >>"$repo_dir/main.rs"
git -C "$repo_dir" add main.rs
git -C "$repo_dir" commit -q -m "moved on after the base was published"
new_commit=$(git -C "$repo_dir" rev-parse HEAD)
[ "$new_commit" != "$commit" ] || fail "the control commit did not move the repository"
companion_run --json base status --repo "$repo_dir" --ref main >"$out/base-status-stale.log" 2>&1 || true
python3 - "$out/base-status-stale.log" "$base_commit" <<'PYSTALE'
import json, sys

last = [l for l in open(sys.argv[1]).read().splitlines() if l.strip()]
status = json.loads(last[-1])
print(
    f"base status after the repository moved: base_commit={status['base_commit']} "
    f"fresh={status['fresh']} reason={status.get('reason')!r}"
)
if status["base_commit"] != sys.argv[2]:
    raise SystemExit(f"the stored commit changed: {status}")
if status["fresh"]:
    raise SystemExit(f"a base whose repository moved on still reports fresh: {status}")
PYSTALE
say "control: the same base, the same commit, and fresh=false because the repository moved"

# And a repository that was never refreshed must not borrow another repository's base.
say "control: a repository that was never refreshed has no base"
other=$out/other-repo
[ ! -e "$other" ] || fail "$other already exists, refusing to recreate it"
mkdir -p "$other"
cp "$out/fixture/main.rs" "$other/main.rs"
git -C "$other" init -q -b main 2>/dev/null
git -C "$other" config user.email ns17@example.invalid
git -C "$other" config user.name ns17
git -C "$other" add main.rs
git -C "$other" commit -q -m "a different repository"
companion_run --json base status --repo "$other" --ref main >"$out/base-status-other.log" 2>&1 || true
python3 - "$out/base-status-other.log" <<'PYOTHER'
import json, sys

last = [l for l in open(sys.argv[1]).read().splitlines() if l.strip()]
status = json.loads(last[-1])
print(
    f"base status for an unrefreshed repository: snapshot={status['snapshot']} "
    f"base_commit={status['base_commit']} fresh={status['fresh']}"
)
if status["base_commit"] is not None:
    raise SystemExit(f"an unrefreshed repository reported a commit: {status}")
if status["fresh"]:
    raise SystemExit(f"an unrefreshed repository reported fresh: {status}")
PYOTHER
say "control: an unrefreshed repository has no commit and is not fresh"

say "VERDICT: PASS: warm base published with durable provenance, surviving a daemon reopen, and two fresh slots built through cowfs-treehouse at one canonical path over a real cowfs FUSE mount"
say "attempt: $out is this run's own; earlier attempts under $attempts are untouched"
say "artifacts: $out"