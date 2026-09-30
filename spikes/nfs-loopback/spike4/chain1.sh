#!/bin/sh
cd "$(dirname "$0")"
for w in status_ignored log diffstat checkout cold addcommit worktree; do
  python3 run.py real $w 5 || echo "FAIL $w"
done
echo CHAIN1_DONE
