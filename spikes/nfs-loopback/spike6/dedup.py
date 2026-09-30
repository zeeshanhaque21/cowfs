#!/usr/bin/env python3
"""dedup.py <pool name> [--resume] : run spike1 tool over out/spike6/pool/<name>, print per-slot new_comp and marginal % ; saves out/spike6/dedup/<name>/summary.json"""
import json, os, shutil, subprocess, sys

OUT = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike6"
TOOL = "/Users/zeeshanhaque/Projects/cowfs/spikes/dedup-corpus/target/release/dedup-corpus"
name = sys.argv[1]
label = sys.argv[2] if len(sys.argv) > 2 else name
o = f"{OUT}/dedup/{label}"
shutil.rmtree(o, ignore_errors=True)
p = subprocess.run([TOOL, "--out", o, "--pool", f"{OUT}/pool/{name}"], capture_output=True, text=True)
open(f"{o}.log", "w").write(p.stderr + p.stdout)
if p.returncode:
    sys.exit(p.stderr[-500:])
slots = [json.loads(l) for l in open(f"{o}/slots.jsonl")]
rep = json.load(open(f"{o}/report.json"))
s = {"pool": name, "slots": [{k: x[k] for k in ("name", "files", "raw_bytes", "new_comp", "read_errors")} for x in slots],
     "total_unique_comp": rep["cdc_unique_comp_bytes"], "perfile_zstd": rep["baseline_perfile_zstd_bytes"],
     "raw": rep["raw_bytes"], "whole_file_unique_raw": rep["baseline_whole_file_dedup_bytes"], "cdc_unique_raw": rep["cdc_unique_raw_bytes"]}
s["marginal_pct_of_slot1"] = [round(100 * x["new_comp"] / slots[0]["new_comp"], 2) for x in slots]
json.dump(s, open(f"{o}/summary.json", "w"), indent=1)
print(json.dumps(s))
