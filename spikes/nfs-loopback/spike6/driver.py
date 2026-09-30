#!/usr/bin/env python3
"""driver.py <crate> <variant>... : run.py per variant sequentially (detached-friendly); removes pool after recording if disk use > 30 GiB"""
import shutil, subprocess, sys, os
here = os.path.dirname(os.path.abspath(__file__))
OUT = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike6"
crate, variants = sys.argv[1], sys.argv[2:]
for v in variants:
    n = f"{crate}-{v}"
    for d in (f"pool/{n}", f"ctl/{n}", f"dedup/{n}"):
        shutil.rmtree(f"{OUT}/{d}", ignore_errors=True)
    p = subprocess.run([sys.executable, f"{here}/run.py", crate, v], capture_output=True, text=True)
    print(p.stdout.strip() or "FAIL " + n + " " + p.stderr[-400:], flush=True)
