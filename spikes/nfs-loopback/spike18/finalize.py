#!/usr/bin/env python3
"""usage: finalize.py TAG_PREFIX... -> out/spike18/results.json (per tag/ws/cell medians, op counts)"""
import json, os, statistics as st, sys
from collections import defaultdict

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/spike18"))
rows = [json.loads(l) for l in open(f"{S}/runs.jsonl")]
rows = [r for r in rows if r["rc"] == 0 and any(r["tag"].startswith(a) for a in sys.argv[1:])]
g = defaultdict(list)
for r in rows:
    g[(r["tag"], r["ws"], r["cell"], r["side"])].append(r)
out = []
for (tag, ws, cell, side) in sorted(g):
    if side != "nfs" or (tag, ws, cell, "native") not in g:
        continue
    nat, nfs = g[(tag, ws, cell, "native")], g[(tag, ws, cell, "nfs")]
    ns, fs = [r["sec"] for r in nat], [r["sec"] for r in nfs]
    ops = [r["ops"] for r in nfs if r["ops"]]
    procs = sorted({p for o in ops for p in o if isinstance(o[p], list)})
    out.append(dict(tag=tag, ws=ws, cell=cell, mount=nfs[0]["mount"], srv_args=nfs[0]["srv_args"],
                    native_med=st.median(ns), nfs_med=st.median(fs), nfs_min=min(fs), nfs_max=max(fs),
                    ratio=round(st.median(fs) / st.median(ns), 2), n=len(fs), n_native=len(ns),
                    load1_med=st.median(r["load0"][0] for r in nat + nfs), hot_runs=sum(r["hot"] for r in nat + nfs),
                    ops_med={p: st.median(o.get(p, [0])[0] for o in ops) for p in procs},
                    total_ops_med=st.median(o["TOTAL"] for o in ops),
                    lookup_hit_med=st.median(o.get("LOOKUP_HIT", 0) for o in ops),
                    lookup_miss_med=st.median(o.get("LOOKUP_MISS", 0) for o in ops),
                    server_busy_s_med=st.median(sum(v[0] * v[1] for v in o.values() if isinstance(v, list)) / 1e6 for o in ops)))
json.dump(dict(generated_from="out/spike18/runs.jsonl", note="hot = load1 > 30 at start or end of run", cells=out),
          open(f"{S}/results.json", "w"), indent=1)
print(len(out), "cells ->", f"{S}/results.json")
