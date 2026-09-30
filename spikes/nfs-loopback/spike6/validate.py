#!/usr/bin/env python3
"""validate.py <pool name> : independent check. (1) whole-file unique raw bytes vs tool (2) 5 random files: cmp vs compare.py verdict"""
import hashlib, json, os, random, subprocess, sys, collections
OUT = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike6"
name = sys.argv[1]
pool = f"{OUT}/pool/{name}"
seen, uniq, raw, nfiles = set(), {}, 0, 0
for s in sorted(os.listdir(pool)):
    if not os.path.isdir(f"{pool}/{s}"): continue
    for dp, dn, fn in os.walk(f"{pool}/{s}"):
        for f in fn:
            p = os.path.join(dp, f)
            if os.path.islink(p): continue
            st = os.stat(p)
            if (st.st_dev, st.st_ino) in seen: continue
            seen.add((st.st_dev, st.st_ino)); nfiles += 1; raw += st.st_size
            uniq.setdefault(hashlib.sha256(open(p, "rb").read()).digest(), st.st_size)
t = json.load(open(f"{OUT}/dedup/{name}/summary.json"))
print("python: files", nfiles, "raw", raw, "whole-file unique", sum(uniq.values()))
print("tool  : files", sum(x["files"] for x in t["slots"]), "raw", t["raw"], "whole-file unique", t["whole_file_unique_raw"])
d = json.load(open(f"{OUT}/cmp-{name}.json"))
random.seed(1)
t1, t2 = f"{pool}/1/target", f"{pool}/2/target"
files = [os.path.relpath(os.path.join(dp, f), t1) for dp, _, fn in os.walk(t1) for f in fn if os.path.isfile(os.path.join(dp, f)) and "incremental" not in dp]
for rel in random.sample(files, 5):
    same = subprocess.run(["cmp", "-s", f"{t1}/{rel}", f"{t2}/{rel}"]).returncode == 0 if os.path.exists(f"{t2}/{rel}") else None
    print("cmp-identical" if same else "cmp-differs/missing", rel)
