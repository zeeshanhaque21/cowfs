#!/usr/bin/env python3
"""Paired nfs/native ratios per label and kind from out/bench.csv (each nfs row pairs with the preceding native row)."""
import collections, os, statistics as st

rows = [l.strip().split(",") for l in open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "out/bench.csv")) if l.strip()]
pend, data = {}, collections.defaultdict(lambda: collections.defaultdict(list))
for lab, run, side, kind, t in rows:
    t = float(t)
    if side == "native":
        pend[(lab, kind)] = t
    elif (lab, kind) in pend:
        n = pend.pop((lab, kind))
        data[lab][kind].append((n, t, t / n))
for lab, d in data.items():
    for kind, v in d.items():
        f = lambda xs: f"{st.median(xs):.2f} [{min(xs):.2f}-{max(xs):.2f}]"
        print(f"{lab:20s} {kind:13s} n={len(v)} native {f([x[0] for x in v])} nfs {f([x[1] for x in v])} ratio {f([x[2] for x in v])}")
