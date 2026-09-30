#!/bin/sh
cd "$(dirname "$0")"
python3 run.py toy stress 3 || echo "FAIL toy stress"
python3 run.py real worktree 5 || echo "FAIL worktree"
python3 run.py real stress 3 || echo "FAIL stress"
python3 settings.py real 5 || echo "FAIL settings"
python3 correctness.py real || echo "FAIL correctness"
echo CHAIN2_DONE
