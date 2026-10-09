#!/bin/bash
# Row 3 of issue 171: real treehouse leases through `cowfs-treehouse base refresh --build --canonical`.
# usage: lease171.sh OUT_DIR N     (OUT_DIR must not exist; keep it short: unix socket path limit)
set -eu
unset CARGO_TARGET_DIR
OUT=$1; N=$2
SRC=/mnt/docs/Projects/cowfs-171/src
TGT=/mnt/docs/Projects/cowfs-171/target/debug
TH=/mnt/docs/Projects/cowfs-171/bin/treehouse
FIX=$SRC/docs/verification/evidence/cargo171/fixture
HELPER=$SRC/scripts/cowfs-ns-run.sh
[ ! -e "$OUT" ] || { echo "$OUT exists"; exit 1; }
mkdir -p "$OUT"/{store,mnt,canon,home,root,repo}; chmod 700 "$OUT" "$OUT/home"
SOCK=$OUT/c.sock; pid=
cleanup() {
  rc=$?
  if [ -n "$pid" ] && [ -d /proc/$pid ] && tr '\0' ' ' </proc/$pid/cmdline | grep -q -- "$OUT/store"; then
    "$TGT/cowfs" --socket "$SOCK" shutdown >>"$OUT/serve.log" 2>&1 || true
    for i in $(seq 60); do [ -d /proc/$pid ] || break; sleep 1; done
    [ -d /proc/$pid ] && { echo "daemon still alive, TERM"; kill -TERM $pid; sleep 2; }
  fi
  if grep -q " $OUT/mnt " /proc/self/mountinfo; then echo "WARN: mount still listed"; else echo "cleanup: mount gone"; fi
  exit $rc
}
trap cleanup EXIT
echo "treehouse: $($TH --version) ; mounts before: $(grep -c . /proc/self/mountinfo)"
cp /proc/self/mountinfo $OUT/mi-before.txt
setsid "$TGT/cowfs-daemon" --backend path --store "$OUT/store" --mount "$OUT/mnt" --socket "$SOCK" >"$OUT/serve.log" 2>&1 &
pid=$!
for i in $(seq 120); do grep -q " $OUT/mnt " /proc/self/mountinfo && break; kill -0 $pid || { tail -5 $OUT/serve.log; exit 1; }; sleep 1; done
grep -q " $OUT/mnt " /proc/self/mountinfo || { echo "no mount"; exit 1; }
echo "daemon pid $pid mounted at $OUT/mnt"
ls -A "$OUT/canon" > "$OUT/canon-before.txt"
# repo = the cargo171 fixture, one git commit
cp -a "$FIX/." "$OUT/repo/"
G="git -C $OUT/repo -c user.email=a@example.invalid -c user.name=a"
git -C "$OUT/repo" init -q -b main
$G add -A
$G commit -q -m fixture
export TREEHOUSE_NO_UPDATE=1
comp() { HOME=$OUT/home "$TGT/cowfs-treehouse" --socket "$SOCK" --treehouse-bin "$TH" --json "$@"; }
"$TGT/cowfs" --socket "$SOCK" import "$OUT/repo" --name seed >"$OUT/import.log" 2>&1
man() { (cd "$1/target" && find . -type f ! -name .cargo-lock -print0 | sort -z | xargs -0 sha256sum); }
slots=()
for i in $(seq "$N"); do
  comp base refresh --repo "$OUT/repo" --ref main --build 'cargo build --workspace' --root "$OUT/root" \
    --canonical "$OUT/canon" --ns-helper "$HELPER" >"$OUT/refresh$i.json" 2>"$OUT/refresh$i.err" || { echo "refresh $i FAILED"; tail -5 "$OUT/refresh$i.err"; tail -3 "$OUT/refresh$i.json"; exit 1; }
  s=$(python3 -c "import json; d=json.loads(open('$OUT/refresh$i.json').read().strip().splitlines()[-1]); print(d['slot'])")
  slots+=("$s"); man "$s" > "$OUT/l$i.sha"
  echo "lease $i: slot $s ; files $(wc -l <$OUT/l$i.sha) ; $(grep -o '"built_in_slot":[a-z]*' $OUT/refresh$i.json)"
done
# native control: a further real lease, built at its own path with the same CARGO_INCREMENTAL=0 and no helper
ns=$(cd "$OUT/repo" && "$TH" get --lease --root "$OUT/root")
(cd "$ns" && CARGO_INCREMENTAL=0 cargo build --workspace >"$OUT/native.log" 2>&1) ; man "$ns" > "$OUT/n.sha"
echo "native control lease: $ns"
cp /proc/self/mountinfo $OUT/mi-after.txt
ls -A "$OUT/canon" > "$OUT/canon-after.txt"
echo "distinct canonical-lease manifests: $(for f in $OUT/l*.sha; do sha256sum <$f; done | sort -u | wc -l) of $N"
for i in $(seq 2 "$N"); do echo -n "lease1 vs lease$i differing paths: "; diff $OUT/l1.sha $OUT/l$i.sha | grep -c '^>' || true; done
echo -n "lease1 vs native(own path) differing paths: "; diff $OUT/l1.sha $OUT/n.sha | grep -c '^>' || true
diff $OUT/l1.sha $OUT/n.sha | grep '^>' | awk '{print "  " $3}' || true
echo -n "slot paths distinct: "; printf '%s\n' "${slots[@]}" "$ns" | sort -u | wc -l
echo -n "mounts: "; if cmp -s <(grep -v " $OUT/mnt " $OUT/mi-before.txt) <(grep -v " $OUT/mnt " $OUT/mi-after.txt); then echo "host mounts (excluding this daemon's) unchanged"; else echo "host mounts CHANGED"; fi
echo -n "canon bytes listed before+after (0 = empty): "; cat $OUT/canon-before.txt $OUT/canon-after.txt | wc -c
echo "base status:"; comp base status --repo "$OUT/repo" --ref main 2>&1 | tail -1
(cd "$OUT/repo" && "$TH" --root "$OUT/root" status 2>&1 | head -12) || true
for s in "${slots[@]}" "$ns"; do (cd "$OUT/repo" && "$TH" return "$s" --root "$OUT/root" 2>&1 | tail -1); done
echo "after return:"; (cd "$OUT/repo" && "$TH" --root "$OUT/root" status 2>&1 | head -12) || true
