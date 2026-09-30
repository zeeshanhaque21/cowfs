#!/usr/bin/env python3
"""usage: run.py <toy|real> <workload> [n] - interleaved N/M/O timed batch under the cpu lock; appends to out/spike4/runs_<set>.jsonl"""
import glob, json, os, re, shutil, subprocess, sys, threading, time
from lib import *

S, W = sys.argv[1], sys.argv[2]
N = int(sys.argv[3]) if len(sys.argv) > 3 else 5
LS = ("N", "M", "O")
RUNS = f"{OUT}/runs_{S}.jsonl"
STRESS_A, STRESS_B = (50, 50) if S == "real" else (50, 50)


def files200():
    fn = f"{OUT}/files200_{S}.json"
    if not os.path.exists(fn):
        p = paths(S, "N")["repo"]
        fs = sorted(git(p, "ls-files").stdout.split("\n"))
        ok = [f for f in fs if re.search(r"\.(ts|tsx|js|json|md|txt)$", f) and os.path.isfile(f"{p}/{f}")
              and not os.path.islink(f"{p}/{f}") and os.path.getsize(f"{p}/{f}") < 100000]
        json.dump(ok[:200], open(fn, "w"))
    return json.load(open(fn))


def pair():
    fn = f"{OUT}/pair.json"
    if not os.path.exists(fn):
        p = paths(S, "N")["repo"]
        best = None
        for k in (100, 200, 400, 800, 1600):
            c = git(p, "diff", "--name-only", f"bench~{k}", "bench").stdout.count("\n")
            print("pair", k, c)
            if best is None or (best[1] < 1500 and c > best[1]):
                best = (k, c)
            if c >= 1500:
                best = (k, c)
                break
        a = git(p, "rev-parse", f"bench~{best[0]}").stdout.strip()
        b = git(p, "rev-parse", "bench").stdout.strip()
        json.dump({"A": a, "B": b, "files": best[1]}, open(fn, "w"))
    return json.load(open(fn))


class Rep:
    def __init__(self, L, rep):
        self.L, self.rep = L, rep
        self.p = paths(S, L)
        self.repo = self.p["repo"]
        self.m = []

    def T(self, name, *a, ok=(0,)):
        l0 = load()
        t, r = timed(self.repo, *a)
        self.m.append((name, t, l0, load()))
        if r.returncode not in ok:
            raise RuntimeError(f"{self.L} {name} rc={r.returncode} {r.stderr[:300]}")
        return r

    def X(self, name, fn):
        l0 = load()
        t = time.perf_counter()
        v = fn()
        self.m.append((name, time.perf_counter() - t, l0, load()))
        return v


def w_status(r, first):
    if first:
        git(r.repo, "status", "--porcelain")
    r.T("status", "status", "--porcelain")
    r.T("refresh", "update-index", "--really-refresh")
    r.T("status_after_refresh", "status", "--porcelain")


def w_status_ignored(r, first):
    if first:
        git(r.repo, "status", "--ignored", "--porcelain")
    r.T("status_ignored", "status", "--ignored", "--porcelain")


def w_log(r, first):
    if first:
        git(r.repo, "log", "--oneline", "-n", "2000")
    r.T("log2000", "log", "--oneline", "-n", "2000")


def w_diffstat(r, first):
    pr = pair()
    if first:
        git(r.repo, "diff", "--stat", pr["A"], pr["B"])
    r.T("diffstat_commits", "diff", "--stat", pr["A"], pr["B"])
    r.T("diffstat_wt_clean", "diff", "--stat")


def w_checkout(r, first):
    pr = pair()
    if first:
        git(r.repo, "checkout", "-q", pr["A"])
        git(r.repo, "checkout", "-q", pr["B"])
    r.T("checkout_fwd", "checkout", "-q", pr["A"])
    r.T("checkout_back", "checkout", "-q", pr["B"])
    r.T("status_after_checkout", "status", "--porcelain")
    git(r.repo, "checkout", "-q", "bench")


def w_addcommit(r, first):
    fs = files200()
    def mod():
        for f in fs:
            with open(f"{r.repo}/{f}", "a") as h:
                h.write(f"\n// rep{r.rep}\n")
    r.X("modify200", mod)
    r.T("diffstat_wt_200", "diff", "--stat")
    r.T("status_200", "status", "--porcelain")
    r.T("add_A", "add", "-A")
    r.T("commit", "commit", "-qm", f"rep{r.rep}")


def w_cold(r, first):
    r.T("status_cold", "status", "--porcelain")
    r.T("log_cold", "log", "--oneline", "-n", "2000")


def w_gc(r, first):
    for name, a in (("gc", ("gc", "--quiet")), ("repack_adf", ("repack", "-adfq"))):
        n = 0
        while True:
            n += 1
            x = r.T(name, *a, ok=(0, 128))
            for f in glob.glob(f"{r.p['gitdir']}/objects/pack/tmp_*"):
                os.remove(f)
            if x.returncode == 0 or n >= 6:
                break
            r.m.pop()
        r.m.append((name + "_attempts", n, 0, 0))
        r.m.append((name + "_rc", x.returncode, 0, 0))
    x = r.T("fsck_full", "fsck", "--full", ok=(0, 1, 16))
    bad = [l for l in (x.stdout + x.stderr).split("\n") if l and "dangling" not in l]
    r.m.append(("fsck_bad_lines", len(bad), 0, 0))
    r.m.append(("fsck_rc", x.returncode, 0, 0))


def w_worktree(r, first):
    pr = pair() if S == "real" else {"B": "bench"}
    d = f"{r.p['root']}/slot{r.rep}/repo"
    os.makedirs(os.path.dirname(d), exist_ok=True)
    r.T("wt_add", "worktree", "add", "-q", "-b", f"wtb{r.rep}", d, pr["B"])
    t, x = timed(d, "status", "--porcelain")
    r.m.append(("wt_status_first", t, load(), load()))
    t, x = timed(d, "status", "--porcelain")
    r.m.append(("wt_status_warm", t, load(), load()))
    f = files200()[0]
    open(f"{d}/{f}", "a").write(f"\n// wt{r.rep}\n")
    t, x = timed(d, "commit", "-qam", f"wt{r.rep}")
    r.m.append(("wt_commit", t, load(), load()))
    assert x.returncode == 0, x.stderr
    x = r.T("wt_remove", "worktree", "remove", "--force", d, ok=(0, 255))
    r.m.append(("wt_remove_rc", x.returncode, 0, 0))
    if x.returncode:
        r.X("wt_rmrf_fallback", lambda: subprocess.run(["rm", "-rf", os.path.dirname(d)]))
        git(r.repo, "worktree", "prune")
    git(r.repo, "branch", "-D", f"wtb{r.rep}", "-q")
    shutil.rmtree(os.path.dirname(d), ignore_errors=True)


def run_par(cmds_per_worker):
    fails = []
    def worker(cmds):
        for repo, a in cmds:
            r = subprocess.run(["git", "-C", repo, *a], capture_output=True, text=True, env=ENV)
            if r.returncode:
                fails.append(r.stderr)
    ts = [threading.Thread(target=worker, args=(c,)) for c in cmds_per_worker]
    t = time.perf_counter()
    [x.start() for x in ts]
    [x.join() for x in ts]
    return time.perf_counter() - t, fails


def w_stress(r, first):
    B = pair()["B"] if S == "real" else "bench"
    git(r.repo, "checkout", "-q", "bench")
    wts = [f"{r.p['root']}/sw{r.rep}_{k}" for k in range(4)]
    for k, w in enumerate(wts):
        git(r.repo, "worktree", "add", "-q", "--detach", w, "HEAD")
    cs = []
    for k, w in enumerate(wts):
        c = []
        for i in range(STRESS_A):
            c += [(w, ["checkout", "-q", "-b", f"s{r.rep}_{k}_{i}"]), (w, ["commit", "-q", "--allow-empty", "-m", f"a{k}_{i}"])]
        cs.append(c)
    l0 = load()
    dtA, fA = run_par(cs)
    r.m.append(("stress_A_worktrees", dtA, l0, load()))
    r.m.append(("stress_A_fail", len(fA), 0, 0))
    cs = [[(r.repo, ["commit", "-q", "--allow-empty", "-m", f"b{p}_{i}"]) for i in range(STRESS_B)] for p in range(2)]
    dtB, fB = run_par(cs)
    r.m.append(("stress_B_samewt", dtB, load(), load()))
    r.m.append(("stress_B_lockfail", sum(".lock" in f for f in fB), 0, 0))
    r.m.append(("stress_B_refracefail", sum(".lock" not in f and "cannot lock ref" in f for f in fB), 0, 0))
    r.m.append(("stress_B_otherfail", sum(".lock" not in f and "cannot lock ref" not in f for f in fB), 0, 0))
    for w in wts:
        if git(r.repo, "worktree", "remove", "--force", w, check=False).returncode:
            subprocess.run(["rm", "-rf", w])
            git(r.repo, "worktree", "prune")
    locks = subprocess.run(["find", r.p["gitdir"], "-name", "*.lock"], capture_output=True, text=True).stdout.split()
    r.m.append(("stress_stale_locks", len(locks), 0, 0))
    x = git(r.repo, "fsck", "--full", check=False)
    r.m.append(("stress_fsck_rc", x.returncode, 0, 0))
    names = git(r.repo, "for-each-ref", "--format=%(refname)", f"refs/heads/s{r.rep}_*").stdout.split()
    subprocess.run(["git", "-C", r.repo, "update-ref", "--stdin"], input="".join(f"delete {n}\n" for n in names), text=True, env=ENV, check=True)
    git(r.repo, "reset", "-q", "--hard", B)


WL = {k[2:]: v for k, v in globals().items() if k.startswith("w_")}
COLD = W in ("cold",)
STATEFUL_FIRST = {"status", "status_ignored", "log", "diffstat", "checkout"}
fn = WL[W]
with cpulock():
    t0 = time.time()
    while load() > 28 and time.time() - t0 < 300:
        time.sleep(10)
    print("lock acquired", W, S, "load", load(), flush=True)
    for L in LS:
        if W in STATEFUL_FIRST:
            fn(Rep(L, -1), True)
    for rep in range(N):
        if COLD:
            srv_restart()
        order = LS[rep % 3:] + LS[:rep % 3]
        for L in order:
            r = Rep(L, rep)
            fn(r, False)
            for name, v, l0, l1 in r.m:
                record(RUNS, {"set": S, "workload": W, "rep": rep, "layout": L, "metric": name, "value": v,
                              "load0": l0, "load1": l1, "cold": COLD})
        print("rep", rep, "done load", load(), flush=True)
