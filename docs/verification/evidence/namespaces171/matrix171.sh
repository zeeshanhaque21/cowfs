#!/bin/bash
# usage: matrix171.sh BASE_DIR N INCR   (BASE_DIR on the filesystem under test; run as the normal user)
# Builds the cargo171 fixture N times through scripts/cowfs-ns-run.sh at one canonical dir, alternating
# two copies (slotA odd, slotB even), plus one native build at slotA's own path. Records a sha256
# manifest of every file under target per build. CARGO_INCREMENTAL=INCR.
set -eu
unset CARGO_TARGET_DIR
B=$1; N=$2; INCR=$3
SRC=/mnt/docs/Projects/cowfs-171/src
FIX=$SRC/docs/verification/evidence/cargo171/fixture
HELPER=$SRC/scripts/cowfs-ns-run.sh
C=$B/canon; O=$B/out-inc$INCR
rm -rf "$B/slotA" "$B/slotB" "$O"; mkdir -p "$C" "$O"
[ -z "$(ls -A "$C")" ] || { echo "canon not empty"; exit 1; }
for s in slotA slotB; do mkdir "$B/$s"; cp -a "$FIX/." "$B/$s/"; done
man() { (cd "$1/target" && find . -type f ! -name .cargo-lock -print0 | sort -z | xargs -0 sha256sum); }
for i in $(seq 1 "$N"); do
  if [ $((i % 2)) = 1 ]; then s=slotA; else s=slotB; fi
  rm -rf "$B/$s/target"
  "$HELPER" --src "$B/$s" --canonical "$C" -- env CARGO_INCREMENTAL="$INCR" cargo build --workspace >"$O/c$i.log" 2>&1 || { echo "canonical build $i FAILED"; tail -5 "$O/c$i.log"; exit 1; }
  man "$B/$s" > "$O/c$i.sha"
done
rm -rf "$B/slotA/target"
(cd "$B/slotA" && env CARGO_INCREMENTAL="$INCR" cargo build --workspace >"$O/n1.log" 2>&1) || { echo "native build FAILED"; exit 1; }
man "$B/slotA" > "$O/n1.sha"
echo "fs: $(findmnt -no FSTYPE -T "$B") ; INCR=$INCR ; N=$N canonical builds; files per build: $(wc -l <"$O/c1.sha")"
echo "distinct canonical manifests: $(cat "$O"/c[0-9]*.sha | wc -l >/dev/null; for f in "$O"/c[0-9]*.sha; do sha256sum <"$f"; done | sort -u | wc -l)"
for i in $(seq 2 "$N"); do echo -n "c1 vs c$i differing paths: "; diff "$O/c1.sha" "$O/c$i.sha" | grep -c '^>' || true; done
echo -n "c1 vs native(own path) differing paths: "; diff "$O/c1.sha" "$O/n1.sha" | grep -c '^>' || true
echo "native-differing files:"; diff "$O/c1.sha" "$O/n1.sha" | grep '^>' | awk '{print "  " $3}' | head -20
echo "c1 vs c2 differing files:"; diff "$O/c1.sha" "$O/c2.sha" | grep '^>' | awk '{print "  " $3}' | head -10
# embedded path check on the final canonical build (slotB, if N is even) and the native build
last=slotA; [ $((N % 2)) = 0 ] && last=slotB
echo "embedded paths in last canonical build ($last) debug/app: canonical=$(grep -c -aF "$C" "$B/$last/target/debug/app" || true) own_slot=$(grep -c -aF "$B/$last" "$B/$last/target/debug/app" || true)"
echo "embedded paths in native slotA debug/app: canonical=$(grep -c -aF "$C" "$B/slotA/target/debug/app" || true) own_slot=$(grep -c -aF "$B/slotA" "$B/slotA/target/debug/app" || true)"
