#!/usr/bin/env python3
"""usage: silentloss.py <set> [k] - k git pack-objects writes per layout; rc vs sha1 of resulting .idx/.pack (does git report success on a corrupt idx?)"""
import glob, hashlib, json, os, subprocess, sys, time
from lib import *

S = sys.argv[1]
K = int(sys.argv[2]) if len(sys.argv) > 2 else 8


def sha_ok(fn):
    d = open(fn, "rb").read()
    return hashlib.sha1(d[:-20]).digest() == d[-20:]


res = {}
for L in "MON":
    p = paths(S, L)
    repo, gd = p["repo"], p["gitdir"]
    pd = f"{gd}/objects/pack"
    bpd = pd.replace(MNT, BACK) if L != "N" and L != "O" else pd
    out = []
    objs = "".join(l.split()[0] + "\n" for l in git(repo, "rev-list", "--objects", "--all").stdout.splitlines() if l)
    for a in range(K):
        for f in glob.glob(f"{pd}/zz-*") + glob.glob(f"{pd}/tmp_*"):
            os.chmod(f, 0o644)
            os.remove(f)
        t = time.perf_counter()
        r = subprocess.run(["git", "-C", repo, "pack-objects", "-q", "--delta-base-offset", "--window=0", f"{pd}/zz"],
                           input=objs, capture_output=True, text=True, env=ENV)
        idxs = glob.glob(f"{pd}/zz-*.idx")
        rec = {"rc": r.returncode, "s": round(time.perf_counter() - t, 1), "err": r.stderr[:100], "load": round(load(), 1)}
        if idxs:
            rec["idx_sha_ok"] = sha_ok(idxs[0])
            rec["pack_sha_ok"] = sha_ok(idxs[0][:-4] + ".pack")
            if L == "M":
                bi = idxs[0].replace(MNT, BACK)
                rec["backing_idx_sha_ok"] = sha_ok(bi)
        rec["SILENT_CORRUPT"] = r.returncode == 0 and (not rec.get("idx_sha_ok", False) or not rec.get("pack_sha_ok", False))
        out.append(rec)
        print(L, a, rec, flush=True)
    for f in glob.glob(f"{pd}/zz-*") + glob.glob(f"{pd}/tmp_*"):
        os.chmod(f, 0o644)
        os.remove(f)
    res[L] = out
json.dump(res, open(f"{OUT}/silentloss_{S}.json", "w"), indent=1)
