#!/usr/bin/env python3
"""usage: check.py <set> - fsck each layout, compare HEAD and status --porcelain across layouts"""
import hashlib, json, sys
from lib import *
S = sys.argv[1]
res = {}
for L in "NMO":
    p = paths(S, L)["repo"]
    x = git(p, "fsck", "--full", check=False)
    head = git(p, "rev-parse", "HEAD").stdout.strip()
    tree = git(p, "rev-parse", "HEAD^{tree}").stdout.strip()
    st = git(p, "status", "--porcelain").stdout
    cnt = git(p, "rev-list", "--count", "HEAD").stdout.strip()
    res[L] = {"fsck_rc": x.returncode, "fsck_bad": [l for l in (x.stdout + x.stderr).split("\n") if l and "dangling" not in l][:5],
              "head": head, "tree": tree, "commits": cnt, "status_md5": hashlib.md5(st.encode()).hexdigest(), "status_lines": st.count("\n")}
    print(L, res[L])
print("HEAD equal:", len({r["head"] for r in res.values()}) == 1, "status equal:", len({r["status_md5"] for r in res.values()}) == 1)
json.dump(res, open(f"{OUT}/check_{S}.json", "w"), indent=1)
