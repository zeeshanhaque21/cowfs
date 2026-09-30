#!/usr/bin/env python3
import json, sys
d = json.load(open(sys.argv[1]))
T = d["A_total"]["bytes"]; F = d["A_total"]["files"]
for k in ["A_total", "same", "path_only_diff", "content_diff", "only_in_A", "only_in_B"]:
    v = d[k]; print(f"{k:15} files={v['files']:6} ({100*v['files']/F:5.1f}%) bytes={v['bytes']:12} ({100*v['bytes']/T:5.1f}%)")
print("by kind (files/bytes): total | same | path_only | diff | onlyA | onlyB")
for k, v in d["by_kind"].items():
    print(f"{k:16}", " | ".join(f"{v[a]['files']}/{v[a]['bytes']}" for a in ["total", "same", "path_only", "diff", "onlyA", "onlyB"]))
print("residual diff bytes after path-norm:", d["residual_diff_bytes"], d["residual_diff_bytes_by_kind"])
print(d["diff_examples"])
