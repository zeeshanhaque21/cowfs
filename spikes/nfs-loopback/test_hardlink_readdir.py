#!/usr/bin/env python3
"""Regression: multi-page readdir of a dir with same-directory hardlinks (rustc incremental deps/ does this).
usage: test_hardlink_readdir.py <mnt> [pairs]"""
import os, shutil, sys

pairs = int(sys.argv[2]) if len(sys.argv) > 2 else 4000
d = os.path.join(sys.argv[1], "hl_readdir")
shutil.rmtree(d, ignore_errors=True)
os.makedirs(d)
for i in range(pairs):
    a = os.path.join(d, f"f{i:05d}a")
    open(a, "w").write("x")
    os.link(a, os.path.join(d, f"f{i:05d}b"))
try:
    names = os.listdir(d)
    got, uniq = len(names), len(set(names))
    err = ""
except OSError as e:
    got = uniq = -1
    err = f" listdir error: {e}"
try:
    shutil.rmtree(d)
    gone = not os.path.exists(d)
except OSError as e:
    gone = False
    err += f" rmtree error: {e}"
want = pairs * 2
ok = got == want and uniq == want and gone
print(f"{'PASS' if ok else 'FAIL'} hardlink readdir: listed {got}/{want} unique {uniq}, removed={gone}{err}")
sys.exit(0 if ok else 1)
