#!/bin/sh
# tuned-server chain, default cargo profile, restart server before every stage
cd "$(dirname "$0")/.." || exit 1
export SRV_BIN="$PWD/out/seeded/nfs-loopback-tuned" RUNS_FILE="${RUNS_FILE:-$PWD/out/seeded/runs_tuned.jsonl}"
unset CARGO_PROFILE_DEV_SPLIT_DEBUGINFO BENCH_SDI_OFF
for s in seed modeb modea git gitignored; do
  python3 seeded/srv.py stop || exit 1
  sleep 1
  python3 seeded/srv.py start || exit 1
  python3 -u seeded/bench_seeded.py $s || exit 1
done
