"""builds out/spike3/results.json from out/spike3/vm/*.jsonl and macload.log"""
import json, statistics as st, glob, os
O = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike3"
mac = [tuple(map(float, l.split())) for l in open(f"{O}/macload.log") if l.strip()]
def macmax(t0, t1):
    v = [l for t, l in mac if t0 - 5 <= t <= t1 + 5]
    return max(v) if v else None
def table(label):
    rows = [json.loads(l) for l in open(f"{O}/vm/{label}.jsonl")]
    cells = {}
    for r in rows:
        cells.setdefault(r["workload"], {}).setdefault(r["side"], []).append(r)
    out = {}
    for w, sd in cells.items():
        n, f = sd.get("native", []), sd.get("fuse", [])
        nk = {r["rep"]: r["secs"] for r in n}
        pr = [r["secs"] / nk[r["rep"]] for r in f if r["rep"] in nk and nk[r["rep"]] > 0]
        allr = n + f
        mm = [macmax(r["t0"], r["t1"]) for r in allr]
        out[w] = dict(n_native=len(n), n_fuse=len(f),
                      native_median_s=st.median(x["secs"] for x in n) if n else None,
                      fuse_median_s=st.median(x["secs"] for x in f) if f else None,
                      ratio_median=st.median(pr) if pr else None, ratio_min=min(pr) if pr else None, ratio_max=max(pr) if pr else None,
                      vm_load1_range=[min(x["load_pre"] for x in allr), max(x["load_pre"] for x in allr)],
                      mac_load1_max=max(m for m in mm if m is not None) if any(m is not None for m in mm) else None,
                      runs_mac_load_gt30=sum(1 for m in mm if m is not None and m > 30), runs=len(allr))
        ex = [r for r in f if "fresh" in r]
        if ex: out[w]["fresh_rustc_fuse"] = [(r["fresh"], r["rustc_runs"]) for r in ex]; out[w]["fresh_rustc_native"] = [(r["fresh"], r["rustc_runs"]) for r in n if "fresh" in r]
    return out
labels = [os.path.basename(f)[:-6] for f in sorted(glob.glob(f"{O}/vm/*.jsonl"))]
res = dict(
    env=dict(vm="OrbStack cowfs-spike3 Debian bookworm arm64 15 vCPU 16GB kernel 7.0.14-orbstack", native_fs="btrfs (VM root, /dev/vdb1)", cargo="1.98.1", fuser="0.15.1 default-features=false abi-7-31", cargo_jobs=4,
              note="absolute times are VM times; Mac load1 was 20-40 for most runs and 150-370 later; per-run t0/t1 in vm/*.jsonl"),
    configs={l: table(l) for l in labels if not l.startswith("smoke")},
    final_config="--ttl 3600 --neg --keep-cache (label f1); baseline (label f_base) has default options",
    ext4_comparison="run e_ext4 discarded: VM load1 up to 20, Mac load1 above 30; not usable",
)
json.dump(res, open(f"{O}/results.json", "w"), indent=1)
print("labels", labels)
for l in ("f1",):
    print(f"-- {l}")
    for w, c in res["configs"][l].items():
        v = "ok" if c["ratio_median"] and c["ratio_median"] <= 1.5 else "OVER"
        print(f"{w:26} nat {c['native_median_s']:.3f} fuse {c['fuse_median_s']:.3f} ratio {c['ratio_median']:.2f} [{c['ratio_min']:.2f}-{c['ratio_max']:.2f}] n={c['n_native']} {v}")
