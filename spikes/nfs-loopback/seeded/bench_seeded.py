#!/usr/bin/env python3
"""Seeded-build bench: native APFS vs private NFS loopback (port 11112).

usage: bench_seeded.py STAGE   (seed | modeb | modea | git)
Appends one JSON line per run to out/seeded/runs.jsonl.
Contamination guard: waits for no foreign cargo/rustc/cc/clang/ld before each timed run,
polls ps every 1s during it, redoes contaminated runs (max 3 redos each).
"""
import json, os, shutil, subprocess, sys, threading, time

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/seeded"))
ROOT = {"native": f"{S}/native", "nfs": f"{S}/mnt"}
BACK = f"{S}/backing"
SEED = f"{S}/native-seed"
CTX_SRC = "/Users/zeeshanhaque/Projects/context-mode/"
NAMES = {"cargo", "rustc", "cc", "clang", "ld"}
RUNS = f"{S}/runs.jsonl"
ME = os.getpid()
CONTAM = {"n": 0}
if os.environ.get("BENCH_SDI_OFF"):
    os.environ["CARGO_PROFILE_DEV_SPLIT_DEBUGINFO"] = "off"


def procs():
    out = subprocess.run(["ps", "-axo", "pid=,ppid=,comm="], capture_output=True, text=True).stdout
    r = {}
    for line in out.splitlines():
        p = line.split(None, 2)
        if len(p) == 3:
            r[int(p[0])] = (int(p[1]), p[2])
    return r


def foreign():
    pr = procs()
    def mine(pid):
        while pid in pr:
            if pid == ME:
                return True
            pid = pr[pid][0]
        return False
    return [(pid, c) for pid, (pp, c) in pr.items() if os.path.basename(c) in NAMES and not mine(pid)]


def wait_idle(limit=900):
    t0 = time.time()
    said = False
    while True:
        f = foreign()
        if not f:
            return time.time() - t0
        if not said:
            print(f"waiting for idle: {f[:4]}", flush=True)
            said = True
        if time.time() - t0 > limit:
            sys.exit("TIMEOUT waiting for idle machine")
        time.sleep(2)


def timed(cmd, cwd, shell=False, env=None):
    waited = wait_idle()
    la = os.getloadavg()
    seen = []
    stop = threading.Event()
    def mon():
        while not stop.wait(1.0):
            seen.extend(foreign())
    th = threading.Thread(target=mon, daemon=True)
    th.start()
    seen.extend(foreign())
    e = dict(os.environ, **(env or {}))
    t = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, shell=shell, capture_output=True, text=True, env=e)
    dt = time.perf_counter() - t
    stop.set()
    th.join()
    seen.extend(foreign())
    srv = sh(f"ps -p $(cat {S}/server.pid) -o rss=,etime=").stdout.split()
    return dict(srv=srv, sec=dt, rc=p.returncode, out=p.stdout + p.stderr, load=la, waited=round(waited, 1), foreign=seen[:5])


def done(cell, side, i):
    if not os.path.exists(RUNS):
        return False
    for l in open(RUNS):
        r = json.loads(l)
        if (r["cell"], r["side"], r.get("i")) == (cell, side, i) and not r.get("contaminated"):
            return True
    return False


def run(cell, side, cmd, cwd, i, shell=False, setup=None, teardown=None, env=None, **extra):
    if os.environ.get("RESUME") and done(cell, side, i):
        print("skip done", cell, side, i, flush=True)
        return None
    for attempt in range(4):
        if setup:
            setup()
        r = timed(cmd, cwd, shell, env)
        if teardown:
            teardown()
        contam = bool(r["foreign"])
        rec = dict(cell=cell, side=side, i=i, sec=round(r["sec"], 3), rc=r["rc"], load=r["load"],
                   waited=r["waited"], srv_rss_kb_etime=r["srv"], contaminated=contam, attempt=attempt,
                   compiling=r["out"].count("Compiling"), fresh=r["out"].count("Fresh"), **extra)
        if contam:
            rec["foreign"] = r["foreign"]
            CONTAM["n"] += 1
        with open(RUNS, "a") as f:
            f.write(json.dumps(rec) + "\n")
        print(cell, side, i, rec["sec"], "rc", r["rc"], "compiling", rec["compiling"], "fresh", rec["fresh"],
              "CONTAM" if contam else "", flush=True)
        if not contam:
            return rec
    print("cell gave up after 3 redos", cell, side, i, flush=True)
    return None


def sh(cmd, cwd=None):
    return subprocess.run(cmd, shell=True, cwd=cwd, capture_output=True, text=True)


def interleave(n, fn):
    for i in range(n):
        for side in (("native", "nfs") if i % 2 == 0 else ("nfs", "native")):
            fn(side, i)


def rmtree(p):
    if os.path.exists(p):
        subprocess.run(["rm", "-rf", p], check=True)


def rsync_seed(dst):
    return ["rsync", "-aH", SEED + "/", dst + "/"]


def checks(side, tree, tag):
    """correctness on a fresh seeded copy, before cargo touches it"""
    res = {}
    real = tree if side == "native" else tree.replace(ROOT["nfs"], BACK)
    bin_ = f"{tree}/target/debug/dedup-corpus"
    p = subprocess.run([bin_, "--bogus"], capture_output=True, text=True)
    res["bogus_rc"] = p.returncode
    def nlinks(d):
        return int(sh(f"find target -type f -links +1 | wc -l", cwd=d).stdout)
    res["hardlinks_real"] = nlinks(real)
    res["hardlinks_mount_view"] = nlinks(tree)
    res["hardlinks_seed"] = nlinks(SEED)
    files = sorted(sh("find target -type f", cwd=SEED).stdout.split("\n"))
    files = [f for f in files if f]
    step = max(1, len(files) // 20)
    sample = files[::step][:20]
    bad = [f for f in sample if sh(f"cmp -s '{SEED}/{f}' '{tree}/{f}'").returncode != 0]
    res["cmp_sample"] = len(sample)
    res["cmp_mismatch"] = bad
    res["total_target_files"] = len(files)
    res["tag"] = tag
    with open(RUNS, "a") as f:
        f.write(json.dumps(dict(cell="check", side=side, **res)) + "\n")
    print("check", side, res, flush=True)


def stage_seed():
    N = 3
    for side in ("native", "nfs"):
        for i in range(N):
            rmtree(f"{ROOT[side]}/seed{i}")
    rmtree(f"{ROOT['native']}/clone0")
    def w5(side, i):
        d = f"{ROOT[side]}/seed{i}"
        run("W5_seed_rsync_aH", side, ["rsync", "-aH", SEED + "/", d + "/"], S, i, setup=lambda: rmtree(d))
    interleave(N, w5)
    run("W5_seed_cp_c_p_native_only", "native", ["cp", "-c", "-R", "-p", SEED, f"{ROOT['native']}/clone0"], S, 0,
        setup=lambda: rmtree(f"{ROOT['native']}/clone0"))
    for side in ("native", "nfs"):
        checks(side, f"{ROOT[side]}/seed0", "seed0_before_cargo")
    attempts = __import__("collections").defaultdict(int)
    def first(side, i):
        d = f"{ROOT[side]}/seed{i}"
        def reseed():
            if attempts[(side, i)]:
                rmtree(d)
                subprocess.run(["rsync", "-aH", SEED + "/", d + "/"], check=True)
            attempts[(side, i)] += 1
        run("ModeA_first_build_after_copy", side, ["cargo", "build", "--frozen"], d, i, setup=reseed)
    interleave(N, first)
    run("ModeA_first_build_after_cp_c_p", "native", ["cargo", "build", "--frozen"], f"{ROOT['native']}/clone0", 0)
    for side in ("native", "nfs"):
        rmtree(f"{ROOT[side]}/slotA")
        os.rename(f"{ROOT[side]}/seed0", f"{ROOT[side]}/slotA")
        for i in (1, 2):
            rmtree(f"{ROOT[side]}/seed{i}")
    rmtree(f"{ROOT['native']}/clone0")


def workloads(mode, name, n=5):
    tree = lambda side: f"{ROOT[side]}/{name}"
    main = lambda side: f"{tree(side)}/src/main.rs"
    orig = {s: open(main(s), "rb").read() for s in ROOT}
    def w1(side, i):
        run(f"{mode}_W1_noop_build", side, ["cargo", "build", "--frozen"], tree(side), i)
    interleave(n, w1)
    def w2(side, i):
        def setup():
            with open(main(side), "ab") as f:
                f.write(f"// edit {i}\n".encode())
        def teardown():
            open(main(side), "wb").write(orig[side])
            subprocess.run(["cargo", "build", "--frozen"], cwd=tree(side), capture_output=True)
        run(f"{mode}_W2_edit_build", side, ["cargo", "build", "--frozen"], tree(side), i, setup=setup, teardown=teardown)
    interleave(n, w2)
    def w3first(side, i):
        run(f"{mode}_W3a_test_no_run_first", side, ["cargo", "test", "--frozen", "--no-run"], tree(side), i)
    interleave(1, w3first)
    def w3edit(side, i):
        def setup():
            with open(main(side), "ab") as f:
                f.write(f"// tedit {i}\n".encode())
        def teardown():
            open(main(side), "wb").write(orig[side])
            subprocess.run(["cargo", "test", "--frozen", "--no-run"], cwd=tree(side), capture_output=True)
        run(f"{mode}_W3b_test_no_run_after_edit", side, ["cargo", "test", "--frozen", "--no-run"], tree(side), i,
            setup=setup, teardown=teardown)
    interleave(3, w3edit)
    def w3warm(side, i):
        run(f"{mode}_W3c_test_no_run_warm", side, ["cargo", "test", "--frozen", "--no-run"], tree(side), i)
    interleave(3, w3warm)


def stage_modeb():
    for side in ROOT:
        if os.environ.get("RESUME") and os.path.exists(f"{ROOT[side]}/modeB/target"):
            continue
        rmtree(f"{ROOT[side]}/modeB")
        subprocess.run(["rsync", "-a", "--exclude", "target", SEED + "/", f"{ROOT[side]}/modeB/"], check=True)
    def clean(side, i):
        run("ModeB_W0_clean_build_in_place", side, ["cargo", "build", "--frozen"], f"{ROOT[side]}/modeB", i,
            setup=lambda: rmtree(f"{ROOT[side]}/modeB/target"))
    interleave(3, clean)
    workloads("ModeB", "modeB")


def stage_modea():
    workloads("ModeA", "slotA")


def git_cell(name, side_dirs, n=5):
    """first status after copy (stale index), refresh x3, warm status x n"""
    attempts = __import__("collections").defaultdict(int)
    def first(side, i):
        run(f"{name}_G1_status_first_after_copy", side, ["git", "status", "--porcelain"], side_dirs[side], 0)
    interleave(1, first)
    def refresh(side, i):
        run(f"{name}_G2_update_index_really_refresh", side, ["git", "update-index", "--really-refresh"], side_dirs[side], i)
    interleave(3, refresh)
    def warm(side, i):
        run(f"{name}_G3_status_warm", side, ["git", "status", "--porcelain"], side_dirs[side], i)
    interleave(n, warm)


def stage_gitctx():
    dirs = {s: f"{ROOT[s]}/ctx" for s in ROOT}
    for s, d in dirs.items():
        print("ctx files", s, sh("find . -type f | wc -l", cwd=d).stdout.strip(), flush=True)
    git_cell("GitCtx", dirs)


def stage_gitignored():
    dirs = {s: f"{ROOT[s]}/ctx" for s in ROOT}
    def f(side, i):
        run("GitCtx_G4_status_ignored", side, ["git", "status", "--porcelain", "--ignored"], dirs[side], i)
    interleave(5, f)


def stage_w12():
    pfx = os.environ["CELLPFX"]
    tree = lambda side: f"{ROOT[side]}/modeB"
    main = lambda side: f"{tree(side)}/src/main.rs"
    orig = {s: open(main(s), "rb").read() for s in ROOT}
    def w1(side, i):
        run(f"{pfx}_W1_noop_build", side, ["cargo", "build", "--frozen"], tree(side), i)
    interleave(3, w1)
    def w2(side, i):
        def setup():
            with open(main(side), "ab") as f:
                f.write(f"// edit {i}\n".encode())
        def teardown():
            open(main(side), "wb").write(orig[side])
            subprocess.run(["cargo", "build", "--frozen"], cwd=tree(side), capture_output=True)
        run(f"{pfx}_W2_edit_build", side, ["cargo", "build", "--frozen"], tree(side), i, setup=setup, teardown=teardown)
    interleave(3, w2)


def stage_age():
    tree = lambda side: f"{ROOT[side]}/slotA"
    def w1(pfx):
        def f(side, i):
            run(f"{pfx}_W1_noop_build", side, ["cargo", "build", "--frozen"], tree(side), i)
        interleave(5, f)
    w1("Age0")
    for k in range(2):
        rmtree(f"{tree('nfs')}/target")
        subprocess.run(["cargo", "build", "--frozen"], cwd=tree("nfs"), capture_output=True)
    w1("AfterTwoCleanBuildsChurn")


def stage_git():
    G = ("git", "-c", "user.name=b", "-c", "user.email=b@b")
    dirs = {}
    for side in ROOT:
        d = f"{ROOT[side]}/gitcopy"
        rmtree(d)
        subprocess.run(["rsync", "-aH", SEED + "/", d + "/"], check=True)
        subprocess.run(["sh", "-c", "echo target/ > .gitignore && git init -q && git add -A && git -c user.name=b -c user.email=b@b commit -qm x"], cwd=d, check=True)
        dirs[side] = d
    git_cell("GitSmall", dirs)
    for side in ROOT:
        d = f"{ROOT[side]}/ctx"
        t = time.time()
        if not os.path.exists(d):
            rc = subprocess.run(["rsync", "-aH", CTX_SRC, d + "/"]).returncode
            print("ctx copy", side, round(time.time() - t, 1), "rsync rc", rc, flush=True)
        dirs[side] = d
    git_cell("GitCtx", dirs)


if __name__ == "__main__":
    globals()["stage_" + sys.argv[1]]()
    print("STAGE DONE", sys.argv[1], "contaminated_runs_this_stage", CONTAM["n"], flush=True)
