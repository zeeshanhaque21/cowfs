#!/usr/bin/env python3
"""usage: summarize.py [tag_prefix...] [--ops]  -> table from out/spike18/runs.jsonl"""
import json, os, statistics as st, sys
from collections import defaultdict

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/spike18"))
args = [a for a in sys.argv[1:] if not a.startswith("--")]
rows = [json.loads(l) for l in open(f"{S}/runs.jsonl")]
rows = [r for r in rows if r["rc"] == 0 and (not args or any(r["tag"].startswith(a) for a in args))]
g = defaultdict(list)
for r in rows:
    g[(r["tag"], r["ws"], r["cell"], r["side"])].append(r)
keys = sorted({k[:3] for k in g}, key=lambda k: (k[1], k[2], k[0]))
print("tag ws cell | native_med | nfs_med [min-max] | ratio | n | load1 med | hot | nfs_ops med | lookups (miss) | getattr")
for k in keys:
    nat, nfs = g.get(k + ("native",), []), g.get(k + ("nfs",), [])
    if not nat or not nfs:
        continue
    ns, fs = [r["sec"] for r in nat], [r["sec"] for r in nfs]
    ops = [r["ops"] for r in nfs if r["ops"]]
    med = lambda f: st.median([f(o) for o in ops]) if ops else 0
    loads = [r["load0"][0] for r in nat + nfs]
    print(f"{k[0]} {k[1]} {k[2]} | {st.median(ns):.3f} | {st.median(fs):.3f} [{min(fs):.3f}-{max(fs):.3f}] | "
          f"{st.median(fs) / st.median(ns):.2f} | {len(fs)} | {st.median(loads):.0f} | {sum(r['hot'] for r in nat + nfs)} | "
          f"{med(lambda o: o['TOTAL']):.0f} | {med(lambda o: o.get('LOOKUP', [0])[0]):.0f} ({med(lambda o: o.get('LOOKUP_MISS', 0)):.0f}) | "
          f"{med(lambda o: o.get('GETATTR', [0])[0]):.0f}")
    if "--ops" in sys.argv and ops:
        procs = sorted({p for o in ops for p in o if isinstance(o[p], list)}, key=lambda p: -med(lambda o: o.get(p, [0])[0]))
        print("    " + " ".join(f"{p}={med(lambda o: o.get(p, [0])[0]):.0f}@{med(lambda o: o.get(p, [0, 0])[1]):.0f}us" for p in procs))
        lk = [o["LK"] for o in ops if "LK" in o]
        if lk:
            print("    lookup keys:", {k2: st.median([x[k2] for x in lk]) for k2 in lk[0]},
                  "getattr distinct", med(lambda o: o.get("PROC1_distinct", 0)))
