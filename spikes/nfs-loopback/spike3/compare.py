"""usage: compare.py <baseline> <label>... ; fuse/native median ratio per cell"""
import json, statistics as st, sys
O = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike3/vm"
cells = ["x_clean_debug", "x_noop", "x_incr_edit", "x_test_compile_edit", "x_seed_cp", "bt_cp", "bt_git_add_commit", "bt_git_status", "bt_find", "bt_rm",
         "lookup_exist_distinct", "lookup_missing_distinct", "lookup_missing_same", "lookup_exist_same", "io_write_256M_fsync", "io_read_256M_cold", "io_read_256M_warm"]
short = [c.replace("x_", "").replace("_debug", "").replace("test_compile_edit", "testc").replace("incr_edit", "incr").replace("lookup_", "lk_").replace("exist", "ex").replace("missing", "mi").replace("distinct", "d").replace("io_", "").replace("_256M", "").replace("bt_git_", "git_").replace("add_commit", "addc") for c in cells]
def load(l):
    d = {}
    for line in open(f"{O}/{l}.jsonl"):
        r = json.loads(line); d.setdefault(r["workload"], {}).setdefault(r["side"], []).append(r["secs"])
    return d
def ratio(d, c):
    n, f = d.get(c, {}).get("native"), d.get(c, {}).get("fuse")
    return st.median(f) / st.median(n) if n and f else float("nan")
print(f"{'config':11}" + "".join(f"{s:>9}" for s in short))
for l in sys.argv[1:]:
    d = load(l)
    print(f"{l:11}" + "".join(f"{ratio(d, c):9.2f}" for c in cells))
b = load(sys.argv[1])
print("fuse abs median us/op (lookup) or s:")
for l in sys.argv[1:]:
    d = load(l)
    print(f"{l:11}" + "".join(f"{(st.median(d[c]['fuse']) / (3000/1e6) if c.startswith('lookup') else st.median(d[c]['fuse'])):9.3f}" for c in cells))
