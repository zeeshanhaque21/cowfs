set -e
export PATH=$HOME/.cargo/bin:$PATH
cd /mnt/docs/Projects/cowfs-cargo171
C=/mnt/docs/Projects/cowfs-cargo171/canon; rm -rf slotA slotB nsout; mkdir -p $C nsout
for s in slotA slotB; do mkdir $s; cp -a proj/. $s/; rm -rf $s/target; done
h() { (cd $1/target && find . -type f ! -name .cargo-lock -print0 | sort -z | xargs -0 sha256sum); }
for cfg in "CARGO_INCREMENTAL=0" "CARGO_INCREMENTAL=1"; do
 L=${cfg#*=}
 for pair in A1:slotA A2:slotA B1:slotB; do
  n=${pair%%:*}; s=${pair##*:}; rm -rf $s/target
  ./cowfs-ns-run.sh --src $PWD/$s --canonical $C -- env $cfg cargo build --workspace >/dev/null 2>&1
  h $s > nsout/inc$L-$n.sha
 done
 rm -rf slotA/target; (cd slotA && env $cfg cargo build --workspace >/dev/null 2>&1); h slotA > nsout/inc$L-N1.sha
 echo "== $cfg"; for p in A2 B1 N1; do echo -n "A1 vs $p: differing/missing paths = "; diff nsout/inc$L-A1.sha nsout/inc$L-$p.sha | grep -c '^>' || true; done
done
echo "-- INCREMENTAL=0 files differing A1 vs N1:"; diff nsout/inc0-A1.sha nsout/inc0-N1.sha | grep '^>' | awk '{print $3}' | head
