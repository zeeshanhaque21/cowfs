#!/bin/sh
# Linux acceptance run for issue #17: a private cowfs FUSE mount, two fresh snapshots, the same
# canonical path for both, and a small Rust artifact built at each path.
#
# usage: namespaces17-linux.sh [REPO_ROOT]
#
# Everything lives under REPO_ROOT/bench/out/namespaces17: its own store, its own mount point, its
# own control socket, its own daemon. Nothing outside that directory is created or removed, and the
# only process this script ever signals is the daemon it started itself.
#
# The verdict is PASS, FAIL or UNMEASURABLE. A real failure of the product under test is FAIL; only a
# missing prerequisite is UNMEASURABLE. Every claim is checked against a control: the two canonical
# builds are compared with each other, with a same-path rebuild of one of them, and with a native
# build at the snapshot's own path.
set -eu

repo=${1:-$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)}
out=$repo/bench/out/namespaces17
canonical=$out/canonical
store=$out/store
mnt=$out/mnt
sock=$out/control.sock
log=$out/serve.log
pidfile=$out/serve.pid
helper=$repo/scripts/cowfs-ns-run.sh
canonical_builds=0
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

# The helper forwards the command's own exit code, and 77 is also the code it uses for its own
# refusal, so the code alone cannot classify a failure. probe_namespace settles it once: after the
# probe has said a namespace exists, every later nonzero exit from the helper is the payload's own,
# and must fail loudly rather than be filed as unmeasurable.
ns_available=unknown
ns_refusal=
probe_namespace() {
  if ns_err=$("$helper" --src "$mnt" --canonical "$canonical" -- /bin/true 2>&1); then
    ns_available=yes
    say "probe: a mount namespace is available, so a nonzero helper exit from here is the payload's own"
    return 0
  fi
  ns_available=no
  ns_refusal=$ns_err
  say "probe: no mount namespace: $ns_refusal"
  return 1
}

# classify RC DESCRIPTION
classify() {
  if [ "$ns_available" = no ]; then
    unmeasurable "no mount namespace on this host: $ns_refusal"
  fi
  fail "$2 exited $1"
}

# Only ever signals the daemon this script started, and only after proving its command line names
# this run's own store.
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
      say "cleanup: $bin is gone, so the daemon can only be signalled, not asked to shut down"
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

mkdir -p "$out"
# The daemon refuses a control socket whose directory is not private to this user.
chmod 700 "$out"
: >"$out/run.log"
say "run: repo=$repo out=$out"
[ "$(uname -s)" = Linux ] || unmeasurable "this run needs Linux, this is $(uname -s)"
[ -x "$helper" ] || fail "$helper is missing or not executable"
command -v rustc >/dev/null || unmeasurable "rustc is not on PATH"
command -v cargo >/dev/null || unmeasurable "cargo is not on PATH"
[ -r /dev/fuse ] || unmeasurable "/dev/fuse is missing, so no cowfs FUSE mount is possible"
[ -c /dev/fuse ] || unmeasurable "/dev/fuse is not a character device"

# procfs reports mode 0444 and cp copies those bits, so the file this leaves behind could not be
# overwritten by the next run. Remove it first, and keep the copy out of cp's way.
save_mountinfo() {
  rm -f "$1"
  cat /proc/self/mountinfo >"$1"
}

# The build goes in this run's own output directory so nothing shared is written to.
export CARGO_TARGET_DIR="$out/target"
bin=$CARGO_TARGET_DIR/debug/cowfs
say "build: cargo build -p cowfs-cli (target $CARGO_TARGET_DIR)"
cargo build -p cowfs-cli -j 4 >>"$out/build.log" 2>&1 ||
  fail "cargo build -p cowfs-cli failed, see $out/build.log"
[ -x "$bin" ] || fail "cargo build produced no $bin"
say "build: $("$bin" --version 2>&1 | head -1)"

mkdir -p "$store" "$mnt" "$canonical"
[ -z "$(ls -A "$canonical")" ] || fail "$canonical is not empty, it must be a bare directory"

# Mount state is the thing this run promises not to disturb, so it is sampled here, after this run's
# own daemon is up and before any namespace runs.
save_mountinfo "$out/mountinfo-host.txt"
say "host: $(grep -c . "$out/mountinfo-host.txt") mounts before the daemon starts"

setsid "$bin" serve --store "$store" --mount "$mnt" --socket "$sock" >"$log" 2>&1 &
serve_pid=$!
printf '%s\n' "$serve_pid" >"$pidfile"
say "daemon: pid $serve_pid, store $store, mount $mnt, socket $sock"

# Wait for the mount, and fail on the two signals that mean it will never arrive: the process is
# gone, or there is an error line in its log. The budget is 3 minutes of no progress.
i=0
while ! grep -q " $mnt " /proc/self/mountinfo 2>/dev/null; do
  if ! kill -0 "$serve_pid" 2>/dev/null; then
    tail -5 "$log" | tee -a "$out/run.log"
    unmeasurable "the daemon exited before it mounted"
  fi
  if grep -qiE "fatal|panic|error" "$log" 2>/dev/null; then
    tail -5 "$log" | tee -a "$out/run.log"
    unmeasurable "the daemon logged an error before it mounted"
  fi
  [ "$i" -lt 180 ] || unmeasurable "the mount at $mnt did not appear within 3 minutes"
  sleep 1
  i=$((i + 1))
done
say "daemon: mounted, $(grep "cowfs" /proc/self/mountinfo | grep -c " $mnt " ) line(s) in mountinfo"
say "host: $(grep -c . /proc/self/mountinfo) mounts after the daemon started"

save_mountinfo "$out/mountinfo-before.txt"
ls -A "$canonical" >"$out/canonical-before.txt"

# Once this has run, a nonzero helper exit is the payload's own code, so the checks below classify
# against the probe rather than against 77.
probe_namespace || true

mkdir -p "$out/fixture"
cat >"$out/fixture/main.rs" <<'RS'
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}

fn main() {
    println!("{}", add(2, 40));
}
RS

cowfs() { "$bin" --socket "$sock" "$@"; }

say "store: importing the fixture into a base snapshot"
cowfs import "$out/fixture" --name base >"$out/import.log" 2>&1 ||
  fail "import failed, see $out/import.log"
for slot in slotA slotB; do
  say "store: creating $slot as a fresh clone of base"
  cowfs snapshot create "$slot" --from base >"$out/snapshot-$slot.log" 2>&1 ||
    fail "snapshot create $slot failed, see $out/snapshot-$slot.log"
done
cowfs snapshot list | tee -a "$out/run.log"

# The build is a plain rustc call, so what lands in the artifact is the path and nothing else: no
# cargo fingerprint, no incremental cache, no timestamps in argv.
build() { # build SRC_DIR OUT_NAME
  "$helper" --src "$1" --canonical "$canonical" -- rustc -g --edition 2021 main.rs -o "$2"
}

for name in A1 A2 B1; do
  case $name in
  A1 | A2) slot=slotA ;;
  B1) slot=slotB ;;
  esac
  say "canonical: building $name in $slot at $canonical"
  # The helper's own code is captured before classify runs, because $? inside the guard would be
  # the classify call's own status.
  build "$mnt/$slot" app || { rc=$?; classify "$rc" "the canonical build $name"; }
  canonical_builds=$((canonical_builds + 1))
  cp "$mnt/$slot/app" "$out/app-$name"
done

say "native: building the control in slotA at its own path $mnt/slotA"
(cd "$mnt/slotA" && rustc -g --edition 2021 main.rs -o app-native) ||
  fail "the native control build did not run, see $out/run.log"
cp "$mnt/slotA/app-native" "$out/app-N1"

save_mountinfo "$out/mountinfo-after.txt"
ls -A "$canonical" >"$out/canonical-after.txt"

hash_of() { sha256sum "$out/$1" | cut -d' ' -f1; }
hA1=$(hash_of app-A1)
hA2=$(hash_of app-A2)
hB1=$(hash_of app-B1)
hN1=$(hash_of app-N1)
sizeA1=$(wc -c <"$out/app-A1")
sizeN1=$(wc -c <"$out/app-N1")

cat >"$out/hashes.txt" <<EOF
A1 canonical slotA $(printf '%s %s' "$hA1" "$sizeA1")
A2 canonical slotA again (same-path rebuild control) $(printf '%s %s' "$hA2" "$(wc -c <"$out/app-A2")")
B1 canonical slotB $(printf '%s %s' "$hB1" "$(wc -c <"$out/app-B1")")
N1 native slotA at $mnt/slotA (do-nothing baseline) $(printf '%s %s' "$hN1" "$sizeN1")
EOF
say "hashes:" && cat "$out/hashes.txt" | tee -a "$out/run.log"

python3 - "$out" "$canonical" "$mnt/slotA" <<'PY'
import sys

out, canonical, slot = sys.argv[1:4]
artifacts = {name: open(f"{out}/app-{name}", "rb").read() for name in ("A1", "A2", "B1", "N1")}
lines = []
for name, data in sorted(artifacts.items()):
    lines.append(
        f"{name}: canonical_path_embedded={canonical.encode() in data} "
        f"slot_path_embedded={slot.encode() in data} bytes={len(data)}"
    )
open(f"{out}/embedded-paths.txt", "w").write("\n".join(lines) + "\n")
print("\n".join(lines))
PY

say "host: $(grep -c . "$out/mountinfo-after.txt") mounts after $canonical_builds canonical builds"

# The claims, each against its own control.
cmp -s "$out/mountinfo-before.txt" "$out/mountinfo-after.txt" ||
  fail "the caller's mounts changed across the canonical builds"
say "check: the caller's mounts are unchanged across $canonical_builds canonical builds"
cmp -s "$out/canonical-before.txt" "$out/canonical-after.txt" ||
  fail "the canonical directory changed on the host"
[ ! -s "$out/canonical-after.txt" ] || fail "the canonical directory is not empty outside the namespace"
say "check: $canonical is still empty outside every namespace"
grep -q "canonical_path_embedded=True" "$out/embedded-paths.txt" ||
  fail "no artifact records the canonical path, so the build proved nothing"
[ "$hA1" = "$hA2" ] ||
  unmeasurable "a same-path rebuild is not byte-identical here, so hash equality across slots would prove nothing"
[ "$hA1" = "$hB1" ] ||
  fail "two canonical builds at the same path differ, while a same-path rebuild does not"
say "check: two snapshots built at one canonical path are byte-identical, and a same-path rebuild of one of them is too"
[ "$hA1" != "$hN1" ] ||
  fail "the native build at its own path is identical to the canonical build, so the control did not run"
say "check: the native build at $mnt/slotA differs from the canonical build"

# say already appends to run.log, so this block is not piped again: one verdict, one line.
say "VERDICT: PASS: $canonical_builds canonical builds over a private cowfs FUSE mount, two fresh snapshots, one canonical path"
say "artifacts: $out"