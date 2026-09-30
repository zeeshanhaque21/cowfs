#!/usr/bin/env python3
"""usage: run_cases.py SIDE CASE[,CASE...]   SIDE = native|mount. Appends JSON lines to out/spike5/cases.jsonl"""
import json, os, subprocess, sys, time

H = os.path.dirname(os.path.abspath(__file__))
S = os.path.abspath(f"{H}/../out/spike5")
REPO = f"{S}/repo"
ROOTS = {"native": f"{S}/native/pool", "mount": f"{S}/mnt/pool"}
side, cases = sys.argv[1], sys.argv[2].split(",")
R = ROOTS[side]
ENV = {**os.environ, "HOME": f"{S}/home", "TREEHOUSE_ROOT": R}
assert R.startswith(S) and "/.treehouse" not in R.replace(S, "")


def th(*args, stdin=subprocess.DEVNULL, timeout=60):
    assert args[0] in ("get", "return", "status"), args
    cmd = ["treehouse", *args, "--root", R]
    t = time.time()
    p = subprocess.run(cmd, cwd=REPO, env=ENV, capture_output=True, text=True, stdin=stdin, timeout=timeout)
    out = dict(cmd=" ".join(cmd[:-2]), rc=p.returncode, sec=round(time.time() - t, 2), out=p.stdout.strip(), err=p.stderr.strip())
    for k in ("out", "err"):
        assert "/Users/zeeshanhaque/.treehouse" not in out[k], out
    return out


def ps(pids):
    if not pids:
        return []
    r = subprocess.run(["ps", "-o", "pid=,ppid=,pgid=,sess=,stat=,command=", "-p", ",".join(map(str, pids))], capture_output=True, text=True)
    return [l.strip() for l in r.stdout.splitlines()]


def alive(pid):
    r = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
    return bool(r) and not r.startswith("Z")


def lsof_cwd(pid):
    r = subprocess.run(["lsof", "-a", "-d", "cwd", "-p", str(pid)], capture_output=True, text=True, timeout=30)
    return r.stdout.strip().splitlines()[1:]


def spawn(kind, slot):
    pf = f"{S}/pids-{side}-{kind}.json"
    if os.path.exists(pf):
        os.remove(pf)
    subprocess.run([sys.executable, f"{H}/fixtures.py", kind, slot, pf], check=True, timeout=10)
    for _ in range(50):
        if os.path.exists(pf) and os.path.getsize(pf):
            try:
                return json.load(open(pf))
            except ValueError:
                pass
        time.sleep(0.1)
    raise RuntimeError("fixture no pidfile " + kind)


def kill_mine(pids):
    for p in pids:
        if not alive(p):
            continue
        cmd = subprocess.run(["ps", "-o", "command=", "-p", str(p)], capture_output=True, text=True).stdout
        if "sleep 600" in cmd or "fixtures.py" in cmd:
            os.kill(p, 9)


def slot_state(slot):
    if not os.path.isdir(slot):
        return dict(exists=False)
    g = subprocess.run(["git", "-C", slot, "status", "--porcelain", "-uall"], capture_output=True, text=True, env=ENV).stdout.strip()
    return dict(exists=True, porcelain=g.splitlines(), nfs_silly=[n for n in os.listdir(slot) if n.startswith(".nfs")])


def lease():
    r = th("get", "--lease", "--json", "--no-fetch")
    assert r["rc"] == 0, r
    return json.loads(r["out"].splitlines()[-1])["path"], r


SPEC = {  # case -> (fixture kinds, dirty file setup, expected killed per kind)
    "a": (["cwd_root"], "cwd = slot root"),
    "b": (["cwd_nested"], "cwd = nested subdir"),
    "c": (["fd_out"], "chdir out, open fd on untracked file in slot"),
    "d": (["flock_out"], "chdir out, flock on file in slot"),
    "e": (["setsid_child"], "parent+child in different sessions, both cwd in slot"),
    "e2": (["setsid_child_out"], "parent cwd in slot, setsid child cwd outside"),
    "f": (["trap"], "ignores SIGTERM, cwd in slot"),
    "g": ([], "no lingering process, clean return without --force, then re-get"),
}

for c in cases:
    kinds, desc = SPEC[c]
    rec = dict(side=side, case=c, desc=desc, load=os.getloadavg()[0])
    slot, r0 = lease()
    rec["slot"] = slot
    os.makedirs(f"{slot}/src/deep/er", exist_ok=True)
    if c == "c":
        open(f"{slot}/held.txt", "w").write("held\n")
    pids = []
    for k in kinds:
        pids += spawn(k, slot)
    time.sleep(0.5)
    rec["before_ps"] = ps(pids)
    rec["before_lsof_cwd"] = {p: lsof_cwd(p) for p in pids}
    rec["slot_before"] = slot_state(slot)
    ret = th("return", slot) if c == "g" else th("return", slot, "--force")
    rec["return"] = ret
    time.sleep(0.5)
    rec["after_ps"] = ps([p for p in pids if alive(p)])
    rec["killed"] = {p: not alive(p) for p in pids}
    rec["slot_after"] = slot_state(slot)
    rec["status_after"] = th("status")["out"]
    if c == "g":
        slot2, _ = lease()
        rec["reget_same_slot"] = slot2 == slot
        rec["reget_path"] = slot2
        th("return", slot2)
    elif os.path.isdir(slot) and rec["return"]["rc"] != 0:
        kill_mine(pids)
        rec["cleanup_return"] = th("return", slot, "--force")
    rec["leftover_pids"] = [p for p in pids if alive(p)]
    open(f"{S}/cases.jsonl", "a").write(json.dumps(rec) + "\n")
    print(side, c, "rc", ret["rc"], "killed", rec["killed"], "left", rec["leftover_pids"], flush=True)
    if c not in ("c", "d", "e2"):
        kill_mine(pids)
