#!/usr/bin/env python3
"""aggregate.py : merge out/spike6/res/*.json, fresh-*.json into out/spike6/results.json and print a table"""
import glob, json, os
OUT = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike6"
R = {"variants": {}, "seed": {}, "fresh": {}}
for f in sorted(glob.glob(f"{OUT}/res/*.json")):
    r = json.load(open(f))
    n = os.path.basename(f)[:-5]
    if n.startswith("seed-"):
        R["seed"][n[5:]] = r
        continue
    c = r.get("control_same_path_rebuild", {})
    loads = [b["load1_before"] for b in r["builds"]] + [b["load1_after"] for b in r["builds"]]
    R["variants"][n] = {"identical_files_pct": r["identical_files_pct"], "identical_bytes_pct": r["identical_bytes_pct"],
        "path_only_bytes_pct": round(100 * r["compare"]["path_only_diff"]["bytes"] / r["compare"]["A_total"]["bytes"], 1),
        "residual_diff_bytes_pct": r["residual_diff_bytes_pct"], "slot1_comp": r["dedup"]["slots"][0]["new_comp"], "slot2_comp": r["dedup"]["slots"][1]["new_comp"],
        "slot2_marginal_pct": r["dedup"]["marginal_pct_of_slot1"][1], "slot1_raw": r["dedup"]["slots"][0]["raw_bytes"],
        "ctl_same_files_pct": c.get("same_files_pct"), "ctl_same_bytes_pct": c.get("same_bytes_pct"), "ctl_residual_diff_bytes": c.get("residual_diff_bytes"),
        "ctl_diff_kinds": {k: v["files"] for k, v in c.get("by_kind_diff", {}).items()},
        "max_load1": max(loads), "build_secs": [b["secs"] for b in r["builds"]],
        "by_kind": {k: {"total": v["total"]["bytes"], "same": v["same"]["bytes"], "path_only": v["path_only"]["bytes"], "diff": v["diff"]["bytes"]} for k, v in r["by_kind"].items()}}
for f in glob.glob(f"{OUT}/fresh-*.json"):
    R["fresh"].update(json.load(open(f)))
json.dump(R, open(f"{OUT}/results.json", "w"), indent=1)
print("variant  files%  bytes%  pathonly%  resid%  s2marg%  ctl_files% ctl_bytes%  maxload1")
for n, v in R["variants"].items():
    print(f"{n:6} {v['identical_files_pct']:6} {v['identical_bytes_pct']:6} {v['path_only_bytes_pct']:6} {v['residual_diff_bytes_pct']:7} {v['slot2_marginal_pct']:7} {v['ctl_same_files_pct']:8} {v['ctl_same_bytes_pct']:8}  {v['max_load1']}")
