#!/usr/bin/env python3
"""seed.py <crate> <variant> : clone-seeded slot (cp -c -R -p) vs independently built slot, same one-line edit, incremental rebuild.
Clone slot reuses the seed slot's RUSTFLAGS prefix (treehouse pool env stays constant)."""
import json, os, shutil, subprocess, sys
sys.path.insert(0, os.path.dirname(__file__))
import build, compare

OUT = build.OUT
crate, variant = sys.argv[1], sys.argv[2]
newflags = len(sys.argv) > 3 and sys.argv[3] == "newflags"
tag = f"{crate}-{variant}" + ("-newflags" if newflags else "")
here = os.path.dirname(os.path.abspath(__file__))
pc, pi = f"{tag}-seedclone", f"{tag}-seedindep"
for p in (pc, pi):
    shutil.rmtree(f"{OUT}/pool/{p}", ignore_errors=True)
    for d in ("dedup",):
        shutil.rmtree(f"{OUT}/{d}/{p}-pre", ignore_errors=True); shutil.rmtree(f"{OUT}/{d}/{p}-post", ignore_errors=True)
res = {"tag": tag, "builds": {}}


def dd(pool, label):
    r = json.loads(subprocess.run([sys.executable, f"{here}/dedup.py", pool, f"{pool}-{label}"], capture_output=True, text=True, check=True).stdout)
    return {"slots": [(s["name"], s["new_comp"]) for s in r["slots"]], "marginal_pct_of_slot1": r["marginal_pct_of_slot1"][1], "unique_comp": r["total_unique_comp"]}


def cmp(pool, a, b):
    c = compare.main(f"{OUT}/pool/{pool}/{a}", f"{OUT}/pool/{pool}/{b}")
    T = c["A_total"]
    return {"identical_files_pct": round(100 * c["same"]["files"] / T["files"], 1), "identical_bytes_pct": round(100 * c["same"]["bytes"] / T["bytes"], 1),
            "path_only_bytes_pct": round(100 * c["path_only_diff"]["bytes"] / T["bytes"], 1), "residual_diff_bytes": c["residual_diff_bytes"],
            "by_kind": {k: {"total": v["total"]["bytes"], "same": v["same"]["bytes"], "path_only": v["path_only"]["bytes"], "diff": v["diff"]["bytes"], "only": v["onlyA"]["bytes"] + v["onlyB"]["bytes"]} for k, v in c["by_kind"].items()}}


d1c = build.ensure_slot(crate, pc, "1")
d1i, d2i = build.ensure_slot(crate, pi, "1"), build.ensure_slot(crate, pi, "2")
build.lock()
try:
    for d in (d1c, d1i, d2i):
        res["builds"][d] = build.build(d, variant)
finally:
    build.unlock()
d3c = f"{OUT}/pool/{pc}/3"
subprocess.run(["cp", "-c", "-R", "-p", d1c, d3c], check=True)
res["pre_edit"] = {"clone": {"dedup": dd(pc, "pre"), "cmp": cmp(pc, "1", "3")}, "indep": {"dedup": dd(pi, "pre"), "cmp": cmp(pi, "1", "2")}}

for d in (d1c, d3c, d1i, d2i):
    with open(f"{d}/src/main.rs", "a") as f:
        f.write("\n// spike6 edit\n")
build.lock()
try:
    for d in (d1c, d3c, d1i, d2i):
        res["builds"][d + "-edit"] = build.build(d, variant, flags_slot=d1c if d == d3c and not newflags else None)
finally:
    build.unlock()
res["post_edit"] = {"clone": {"dedup": dd(pc, "post"), "cmp": cmp(pc, "1", "3")}, "indep": {"dedup": dd(pi, "post"), "cmp": cmp(pi, "1", "2")}}
json.dump(res, open(f"{OUT}/res/seed-{tag}.json", "w"), indent=1)
for st in ("pre_edit", "post_edit"):
    for k in ("clone", "indep"):
        r = res[st][k]
        print(tag, st, k, "marginal2 %", r["dedup"]["marginal_pct_of_slot1"], "identical files %", r["cmp"]["identical_files_pct"], "bytes %", r["cmp"]["identical_bytes_pct"], "path-only bytes %", r["cmp"]["path_only_bytes_pct"], "resid", r["cmp"]["residual_diff_bytes"])
