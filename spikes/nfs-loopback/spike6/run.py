#!/usr/bin/env python3
"""run.py <crate> <variant> [--noctl] : build slots 1,2; compare; dedup; same-path rebuild control. Writes out/spike6/res/<crate>-<variant>.json"""
import json, os, shutil, subprocess, sys
sys.path.insert(0, os.path.dirname(__file__))
import build, compare

OUT = build.OUT
crate, variant = sys.argv[1], sys.argv[2]
name = f"{crate}-{variant}"
pool = f"{OUT}/pool/{name}"
os.makedirs(f"{OUT}/res", exist_ok=True)
py = sys.executable
here = os.path.dirname(os.path.abspath(__file__))

builds = [json.loads(l) for l in subprocess.run([py, f"{here}/build.py", crate, variant, "1", "2"], capture_output=True, text=True, check=True).stdout.splitlines()]
d1, d2 = f"{pool}/1", f"{pool}/2"
cmp = compare.main(d1, d2)
json.dump(cmp, open(f"{OUT}/cmp-{name}.json", "w"))
dd = json.loads(subprocess.run([py, f"{here}/dedup.py", name], capture_output=True, text=True, check=True).stdout)
res = {"name": name, "builds": builds, "compare": {k: cmp[k] for k in ("A_total", "same", "path_only_diff", "content_diff", "only_in_A", "only_in_B", "residual_diff_bytes", "residual_diff_bytes_by_kind")},
       "by_kind": cmp["by_kind"], "dedup": dd}
T = cmp["A_total"]
res["identical_files_pct"] = round(100 * cmp["same"]["files"] / T["files"], 1)
res["identical_bytes_pct"] = round(100 * cmp["same"]["bytes"] / T["bytes"], 1)
res["residual_diff_bytes_pct"] = round(100 * cmp["residual_diff_bytes"] / T["bytes"], 3)
if "--noctl" not in sys.argv:
    ctl = f"{OUT}/ctl/{name}/run1"
    shutil.rmtree(f"{OUT}/ctl/{name}", ignore_errors=True)
    os.makedirs(ctl)
    subprocess.run(["cp", "-c", "-R", "-p", f"{d1}/target", f"{ctl}/target"], check=True)
    shutil.rmtree(f"{d1}/target")
    build.lock()
    try:
        cb = build.build(d1, variant)
    finally:
        build.unlock()
    c = compare.main(d1, ctl)
    res["control_same_path_rebuild"] = {"build": cb, "same_files_pct": round(100 * c["same"]["files"] / c["A_total"]["files"], 1),
                                        "same_bytes_pct": round(100 * c["same"]["bytes"] / c["A_total"]["bytes"], 1),
                                        "residual_diff_bytes": c["residual_diff_bytes"], "by_kind_diff": {k: v["diff"] for k, v in c["by_kind"].items() if v["diff"]["files"]},
                                        "examples": c["diff_examples"]}
json.dump(res, open(f"{OUT}/res/{name}.json", "w"), indent=1)
print(name, "identical files %", res["identical_files_pct"], "bytes %", res["identical_bytes_pct"], "resid%", res["residual_diff_bytes_pct"],
      "marginal2 %", dd["marginal_pct_of_slot1"][1], "ctl same bytes %", res.get("control_same_path_rebuild", {}).get("same_bytes_pct"))
