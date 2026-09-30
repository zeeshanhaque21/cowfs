#!/usr/bin/env python3
"""usage: lsof_timing.py - time lsof +D, lsof -p/-d cwd and proc_pidinfo(PROC_PIDVNODEPATHINFO) on native vs NFS dirs; writes out/spike5/lsof_timing.json"""
import ctypes, json, os, statistics, subprocess, sys, tempfile, time

H = os.path.dirname(os.path.abspath(__file__))
S = os.path.abspath(f"{H}/../out/spike5")
POOL = "repo-ab9db5/1/repo"
DIRS = {"native": f"{S}/native", "mount": f"{S}/mnt"}
N_DIRS, N_FILES = 100, 100
libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
libproc.proc_pidinfo.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_int]


def pidinfo_cwd(pid):
    buf = ctypes.create_string_buffer(2352)
    n = libproc.proc_pidinfo(pid, 9, 0, buf, 2352)
    return n, buf.raw[152:152 + 1024].split(b"\0")[0].decode()


def timed(cmd, timeout=180):
    t = time.time()
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return round(time.time() - t, 3), r.returncode, r.stdout
    except subprocess.TimeoutExpired:
        return round(time.time() - t, 3), "TIMEOUT", ""


def spawn(d):
    pf = tempfile.mktemp(dir=S)
    subprocess.run([sys.executable, f"{H}/fixtures.py", "cwd_root", d, pf], check=True, timeout=10)
    for _ in range(50):
        if os.path.exists(pf) and os.path.getsize(pf):
            return json.load(open(pf))[0], pf
        time.sleep(0.1)
    raise RuntimeError("no pid")


out = {"loadavg_start": os.getloadavg()}
for side, base in DIRS.items():
    big = f"{base}/bigtree"
    if not os.path.isdir(big):
        for i in range(N_DIRS):
            os.makedirs(f"{big}/d{i}")
            for j in range(N_FILES):
                open(f"{big}/d{i}/f{j}.txt", "w").write("x" * 64)
    slot = f"{base}/pool/.treehouse/{POOL}"
    res = out[side] = {}
    for label, d in (("slot_small", slot), ("bigtree_10k", big)):
        pid, pf = spawn(d)
        try:
            r = res[label] = {"files": sum(len(f) for _, _, f in os.walk(d)), "pid": pid}
            r["lsof_+D"] = []
            for _ in range(3):
                sec, rc, o = timed(["lsof", "+D", d])
                r["lsof_+D"].append({"sec": sec, "rc": rc, "lines": len(o.splitlines()), "lists_pid": str(pid) in o})
            for name, cmd in (("lsof_-a_-d_cwd_-p", ["lsof", "-a", "-d", "cwd", "-p", str(pid)]), ("lsof_-p", ["lsof", "-p", str(pid)])):
                r[name] = []
                for _ in range(3):
                    sec, rc, o = timed(cmd)
                    r[name].append({"sec": sec, "rc": rc, "lines": len(o.splitlines()), "has_dir": d in o})
            ts, ok = [], 0
            for _ in range(200):
                t = time.perf_counter()
                n, p = pidinfo_cwd(pid)
                ts.append(time.perf_counter() - t)
                ok += p == os.path.realpath(d)
            r["proc_pidinfo_cwd"] = {"n": 200, "correct": ok, "median_us": round(statistics.median(ts) * 1e6, 1), "max_us": round(max(ts) * 1e6, 1)}
        finally:
            cmd = subprocess.run(["ps", "-o", "command=", "-p", str(pid)], capture_output=True, text=True).stdout
            if "sleep 600" in cmd:
                os.kill(pid, 9)
out["loadavg_end"] = os.getloadavg()
json.dump(out, open(f"{S}/lsof_timing.json", "w"), indent=1)
print(json.dumps(out, indent=1))
