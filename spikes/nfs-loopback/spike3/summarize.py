"""usage: summarize.py <label>... ; per cell median/min/max, paired ratio fuse/native, load ranges, mac load"""
import json, statistics as st, sys, collections
O = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike3"
mac = [tuple(map(float, l.split())) for l in open(f"{O}/macload.log") if l.strip()]
def macl(t0, t1):
    v = [l for t, l in mac if t0 - 5 <= t <= t1 + 5]
    return (min(v), max(v)) if v else (None, None)
for label in sys.argv[1:]:
    rows = [json.loads(l) for l in open(f"{O}/vm/{label}.jsonl")]
    cells = collections.OrderedDict()
    for r in rows:
        cells.setdefault(r["workload"], {}).setdefault(r["side"], []).append(r)
    print(f"== {label} flags='{rows[0]['flags']}'")
    print(f"{'workload':24}{'nat med [min-max]':>24}{'fuse med [min-max]':>24}{'ratio med [min-max]':>24} n   vmload   macload>30")
    for w, sd in cells.items():
        n, f = sd.get("native", []), sd.get("fuse", [])
        fr = lambda x: f"{st.median(x):.3f} [{min(x):.3f}-{max(x):.3f}]"
        fm = lambda x: f"{st.median(x):.3f} [{min(x):.3f}-{max(x):.3f}]" if x else "-"
        ns, fs = [r["secs"] for r in n], [r["secs"] for r in f]
        pr = []
        nk = {r["rep"]: r["secs"] for r in n}
        for r in f:
            if r["rep"] in nk and nk[r["rep"]] > 0: pr.append(r["secs"] / nk[r["rep"]])
        ml = [macl(r["t0"], r["t1"]) for r in n + f]
        hi = sum(1 for a, b in ml if b is not None and b > 30)
        vl = [r["load_pre"] for r in n + f]
        extra = ""
        if f and "fresh" in f[0]: extra = " fresh/rustc fuse=%s native=%s" % ([(r["fresh"], r["rustc_runs"]) for r in f], [(r["fresh"], r["rustc_runs"]) for r in n])
        if f and "tracked" in f[0]: extra = f" tracked={f[0]['tracked']}"
        if w.startswith("lookup_"):
            fm = lambda x: f"{st.median(x)/3000*1e6:.1f}us [{min(x)/3000*1e6:.1f}-{max(x)/3000*1e6:.1f}]" if x else "-"
            extra = ""
        print(f"{w:24}{fm(ns):>24}{fm(fs):>24}{(fr(pr) if pr else '-'):>24} {len(ns)}/{len(fs)} {min(vl):.1f}-{max(vl):.1f}  {hi}/{len(ml)}{extra}")
