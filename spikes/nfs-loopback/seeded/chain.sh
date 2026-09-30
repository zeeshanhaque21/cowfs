#!/bin/sh
# fresh-server chain: restart server before every stage
cd "$(dirname "$0")/.." || exit 1
export BENCH_SDI_OFF=1
for s in seed modeb modea git; do
  python3 seeded/srv.py stop || exit 1
  sleep 1
  python3 seeded/srv.py start || exit 1
  python3 -u seeded/bench_seeded.py $s || exit 1
done
