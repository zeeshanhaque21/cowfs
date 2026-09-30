#!/usr/bin/env python3
"""usage: summarize.py <set> - medians [min-max] N/M/O + ratios, load range, flagged runs (load1>30 excluded from stats)"""
import json, sys, statistics as st
from collections import defaultdict
S = sys.argv[1]
import os
OUT = os.path.abspath(f"{os.path.dirname(os.path.abspath(__file__))}/../out/spike4")
d, flag, loads, ex = defaultdict(list), defaultdict(int), defaultdict(list), defaultdict(list)
order = []
for l in open(f"{OUT}/runs_{S}.jsonl"):
    r = json.loads(l)
    k = (r["workload"] + ("*" if r["cold"] else ""), r["metric"])
    if k not in order:
        order.append(k)
    if r["metric"].startswith(("stress_A_fail", "stress_B_", "stress_stale", "stress_fsck", "fsck_", "wt_remove_rc", "gc_rc", "repack_adf_rc", "gc_attempts", "repack_adf_attempts")) and r["metric"] != "stress_B_samewt":
        ex[(k, r["layout"])].append(r["value"])
        continue
    loads[k] += [r["load0"], r["load1"]]
    if max(r["load0"], r["load1"]) > 30:
        flag[(k, r["layout"])] += 1
        continue
    d[(k, r["layout"])].append(r["value"])
out = {}
def f(xs):
    return f"{st.median(xs):.3f}[{min(xs):.3f}-{max(xs):.3f}]n{len(xs)}" if xs else "-"
for k in order:
    if all((k, L) not in d for L in "NMO"):
        print(k[1], {L: ex[(k, L)] for L in "NMO"})
        out["/".join(k)] = {L: ex[(k, L)] for L in "NMO"}
        continue
    med = {L: st.median(d[(k, L)]) if d[(k, L)] else None for L in "NMO"}
    rat = {L: (round(med[L] / med["N"], 2) if med[L] and med["N"] else None) for L in "MO"}
    ls = loads[k]
    out["/".join(k)] = {"median": med, "ratio_vs_N": rat, "raw": {L: d[(k, L)] for L in "NMO"},
                        "load": [min(ls), max(ls)], "flagged_excluded": {L: flag[(k, L)] for L in "NMO"}}
    print(f"{k[0]:14s} {k[1]:22s} N {f(d[(k,'N')])}  M {f(d[(k,'M')])} x{rat['M']}  O {f(d[(k,'O')])} x{rat['O']}  load {min(ls):.0f}-{max(ls):.0f}"
          + (f" FLAG {({L: flag[(k, L)] for L in 'NMO' if flag[(k, L)]})}" if any(flag[(k, L)] for L in 'NMO') else ""))
json.dump(out, open(f"{OUT}/summary_{S}.json", "w"), indent=1)
