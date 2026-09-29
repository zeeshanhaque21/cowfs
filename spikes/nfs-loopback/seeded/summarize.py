#!/usr/bin/env python3
import json, os, statistics as st, collections

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/seeded"))
import sys
name_in = sys.argv[1] if len(sys.argv) > 1 else "runs.jsonl"
runs = [json.loads(l) for l in open(f"{S}/{name_in}")]
checks = [r for r in runs if r["cell"] == "check"]
timed = [r for r in runs if r["cell"] != "check"]
print("failed (rc!=0, excluded):", collections.Counter((r["cell"], r["side"]) for r in timed if r["rc"] != 0 and not r["contaminated"] and "G2" not in r["cell"]))
timed = [r for r in timed if r["rc"] == 0 or r["contaminated"] or "G2" in r["cell"]]
cells = collections.OrderedDict()
for r in timed:
    c = cells.setdefault(r["cell"], {"native": [], "nfs": [], "cont": {"native": 0, "nfs": 0}, "load": []})
    if r["contaminated"]:
        c["cont"][r["side"]] += 1
    else:
        c[r["side"]].append(r["sec"])
        c["load"].append(r["load"][0])
summary = {}
for name, c in cells.items():
    row = {}
    for s in ("native", "nfs"):
        v = c[s]
        row[s] = dict(n=len(v), median=round(st.median(v), 3) if v else None,
                      min=min(v) if v else None, max=max(v) if v else None, contaminated=c["cont"][s])
    a, b = row["native"]["median"], row["nfs"]["median"]
    row["ratio"] = round(b / a, 2) if a and b else None
    row["load1_median"] = round(st.median(c["load"]), 1) if c["load"] else None
    summary[name] = row
    print(f"{name:42s} nat {a} [{row['native']['min']}-{row['native']['max']}] n{row['native']['n']} c{row['native']['contaminated']}"
          f" | nfs {b} [{row['nfs']['min']}-{row['nfs']['max']}] n{row['nfs']['n']} c{row['nfs']['contaminated']}"
          f" | x{row['ratio']} load {row['load1_median']}")
print("total contaminated:", sum(1 for r in timed if r["contaminated"]), "of", len(timed))
json.dump(dict(summary=summary, checks=checks, runs=runs), open(f"{S}/results_{name_in.replace('.jsonl','')}.json", "w"), indent=1)
