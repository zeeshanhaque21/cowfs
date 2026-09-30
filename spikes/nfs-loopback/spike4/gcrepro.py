#!/usr/bin/env python3
"""usage: gcrepro.py <set> [n] - repack -adf then fsck --full at once, retry fsck after wait, then after server restart; per layout"""
import glob, json, os, sys, time
from lib import *

S = sys.argv[1]
N = int(sys.argv[2]) if len(sys.argv) > 2 else 3
OUTF = f"{OUT}/gcrepro_{S}.jsonl"


def fsck(repo):
    t = time.perf_counter()
    x = git(repo, "fsck", "--full", check=False)
    bad = [l for l in (x.stdout + x.stderr).split("\n") if l and "dangling" not in l]
    return x.returncode, len(bad), bad[:2], round(time.perf_counter() - t, 1)


for rep in range(N):
    for L in "NMO":
        p = paths(S, L)
        repo, gd = p["repo"], p["gitdir"]
        lk = cpulock().__enter__()
        rec = {"rep": rep, "layout": L, "load": load()}
        tries = []
        for a in range(4):
            t = time.perf_counter()
            x = git(repo, "repack", "-adfq", check=False)
            tries.append({"rc": x.returncode, "s": round(time.perf_counter() - t, 1), "err": x.stderr[:120]})
            for f in glob.glob(f"{gd}/objects/pack/tmp_pack_*"):
                os.remove(f)
            if x.returncode == 0:
                break
        rec["repack_tries"] = tries
        rec["fsck_immediate"] = fsck(repo)
        if rec["fsck_immediate"][0] != 0:
            time.sleep(150)
            rec["fsck_after_150s"] = fsck(repo)
            if rec["fsck_after_150s"][0] != 0:
                srv_restart()
                rec["fsck_after_restart"] = fsck(repo)
        rec["head"] = git(repo, "rev-parse", "HEAD").stdout.strip()
        lk.__exit__()
        print(json.dumps(rec), flush=True)
        record(OUTF, rec)
