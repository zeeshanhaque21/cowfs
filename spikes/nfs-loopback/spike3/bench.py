#!/usr/bin/env python3
"""spike3 bench driver, runs inside the OrbStack VM cowfs-spike3.
usage: bench.py <label> <fusepass flags or -> <workloads,comma,sep> [--no-native] [--fast N] [--slow N]
workloads (comma = separate cpu.lock batch, + = same batch): x_clean x_noop x_gitstatus x_incr x_test x_seed x_rel y_clean y_noop y_incr y_test y_seed git
Results append to results/<label>.jsonl, one line per timed run, resumable."""
import argparse, json, os, resource, shutil, subprocess, sys, time

D = "/home/zeeshanhaque/cowfs-spike3"
ap = argparse.ArgumentParser()
ap.add_argument("label"); ap.add_argument("flags"); ap.add_argument("workloads")
ap.add_argument("--no-native", action="store_true")
ap.add_argument("--nolock", action="store_true")
ap.add_argument("--no-fuse", action="store_true")
ap.add_argument("--native-root")
ap.add_argument("--back-root")
ap.add_argument("--fast", type=int, default=5)
ap.add_argument("--slow", type=int, default=3)
A = ap.parse_args()
label, flags = A.label, ("" if A.flags.strip() == "-" else A.flags.strip())
WL = A.workloads.split(",")
ROOT = {"native": A.native_root or f"{D}/native_{label}", "fuse": f"{D}/mnt_{label}"}
BACK = f"{A.back_root or D}/back_{label}"
OUT = f"{D}/results/{label}.jsonl"
ENV = dict(os.environ, CARGO_HOME=f"{D}/cargo-home", RUSTUP_HOME="/home/zeeshanhaque/cowfs-spike3/rustup-home",
           CARGO_TERM_COLOR="never", PATH="/home/zeeshanhaque/cowfs-spike3/cargo-home/bin:" + os.environ["PATH"],
           GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null")
ENV.pop("CARGO_TARGET_DIR", None)
done = set()
if os.path.exists(f"{D}/results/{label}.complete"):
    print("already complete", label)
    sys.exit(0)
if os.path.exists(OUT):
    for l in open(OUT):
        r = json.loads(l)
        done.add((r["workload"], r["side"], r["rep"]))
SIDES = ["fuse"] if A.no_native else ["native"] if A.no_fuse else ["native", "fuse"]


def load1():
    return float(open("/proc/loadavg").read().split()[0])


def temp():
    try:
        return int(open("/sys/class/thermal/thermal_zone0/temp").read()) / 1000
    except OSError:
        return None


def used_gb():
    tot = 0
    for p in [BACK, ROOT["native"], f"{D}/bigtree", f"{D}/cargo-home", f"{D}/target-fusepass"]:
        if os.path.exists(p):
            tot += int(subprocess.run(["du", "-sk", p], capture_output=True, text=True).stdout.split()[0])
    return tot / 1048576


GATES = [0]


def gate():
    GATES[0] += 1
    if GATES[0] % 25 == 1 and used_gb() > 7.0:
        print("DISK BUDGET EXCEEDED", flush=True)
        sys.exit(4)
    time.sleep(0.3)


LOCK = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/cpu.lock"
MACPID = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/spike3/macpid"


def acquire():
    t0 = time.time()
    while True:
        try:
            os.mkdir(LOCK)
            break
        except FileExistsError:
            if time.time() - t0 > 900:
                print("CPU LOCK BUSY 15 MIN, STOPPING", open(f"{LOCK}/owner").read(), flush=True)
                sys.exit(6)
            if int(time.time() - t0) % 60 < 10:
                print(f"waiting for cpu.lock ({time.time() - t0:.0f}s) owner: {open(f'{LOCK}/owner').read().strip() if os.path.exists(f'{LOCK}/owner') else '?'}", flush=True)
            time.sleep(10)
    pid = open(MACPID).read().strip() if os.path.exists(MACPID) else "0"
    open(f"{LOCK}/owner", "w").write(f"spike3 {pid} {time.strftime('%Y-%m-%d %H:%M:%S')}\n")
    print(f"lock acquired after {time.time() - t0:.0f}s", flush=True)


def release():
    shutil.rmtree(LOCK, ignore_errors=True)


def sh(cmd, cwd=None, shell=False):
    return subprocess.run(cmd, cwd=cwd, env=ENV, capture_output=True, text=True, shell=shell)


def timed(workload, side, rep, fn):
    if (workload, side, rep) in done:
        return
    gate()
    lp, tp, ts0 = load1(), temp(), time.time()
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    t0 = time.perf_counter()
    ok, extra = fn()
    dt = time.perf_counter() - t0
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    rec = dict(config=label, flags=flags, workload=workload, side=side, rep=rep, secs=round(dt, 3),
               cpu_user=round(r1.ru_utime - r0.ru_utime, 2), cpu_sys=round(r1.ru_stime - r0.ru_stime, 2),
               load_pre=lp, load_post=load1(), t0=round(ts0, 2), t1=round(time.time(), 2), ok=ok, **extra)
    with open(OUT, "a") as f:
        f.write(json.dumps(rec) + "\n")
        f.flush()
        os.fsync(f.fileno())
    print(f"{workload:16} {side:6} rep{rep:<2} {dt:8.2f}s load {lp:.2f}->{rec['load_post']:.2f} ok={ok} {extra}", flush=True)
    if not ok:
        sys.exit(2)


def order(rep):
    return SIDES if rep % 2 == 0 else SIDES[::-1]


def interleave(workload, reps, fn, prep=None):
    for r in range(reps):
        for s in order(r):
            if (workload, s, r) in done:
                continue
            if prep:
                prep(s)
            timed(workload, s, r, lambda s=s: fn(s))


def setup_crate(s, c):
    if not os.path.exists(f"{ROOT[s]}/{c}"):
        assert sh(["cp", "-a", f"{D}/src/{c}", f"{ROOT[s]}/{c}"]).returncode == 0


def cargo(s, c, *a):
    p = sh(["cargo", *a, "--frozen", "-j4"], cwd=f"{ROOT[s]}/{c}")
    if p.returncode:
        print(p.stderr[-1500:], flush=True)
    return p


def warm(s, c, test=False):
    setup_crate(s, c)
    p = cargo(s, c, "test", "--no-run") if test else cargo(s, c, "build")
    assert p.returncode == 0


def main_rs(s, c):
    return f"{ROOT[s]}/{c}/src/main.rs"


def restore(c):
    pristine = open(f"{D}/src/{c}/src/main.rs").read()
    for s in SIDES:
        if os.path.exists(main_rs(s, c)):
            open(main_rs(s, c), "w").write(pristine)


def bin_ok(s, c, release=False):
    name = {"x": "dedup-corpus", "y": "nfs-loopback"}[c]
    b = f"{ROOT[s]}/{c}/target/{'release' if release else 'debug'}/{name}"
    p = sh([b, "--bogus"])
    return os.path.exists(b) and os.path.getsize(b) > 100000 and p.returncode == 101 and "unknown arg" in p.stderr


def w_clean(c):
    def prep(s):
        setup_crate(s, c)
        shutil.rmtree(f"{ROOT[s]}/{c}/target", ignore_errors=True)
    interleave(f"{c}_clean_debug", A.slow, lambda s: (cargo(s, c, "build").returncode == 0 and bin_ok(s, c), {}), prep)


def w_rel(c):
    def prep(s):
        warm(s, c)
        shutil.rmtree(f"{ROOT[s]}/{c}/target/release", ignore_errors=True)
    interleave(f"{c}_clean_release", A.slow, lambda s: (cargo(s, c, "build", "--release").returncode == 0, {}), prep)
    for s in SIDES:
        shutil.rmtree(f"{ROOT[s]}/{c}/target/release", ignore_errors=True)


def w_noop(c):
    for s in SIDES:
        warm(s, c)
    interleave(f"{c}_noop", A.fast, lambda s: (cargo(s, c, "build").returncode == 0, {}))


def edit(s, c, rep, tag):
    with open(main_rs(s, c), "a") as f:
        f.write(f"\n// bench {label} {tag} {rep}\n")


def w_incr(c):
    for s in SIDES:
        warm(s, c)
    def fn(s, rep=[0]):
        return None
    for r in range(A.fast):
        for s in order(r):
            def go(s=s, r=r):
                edit(s, c, r, "incr")
                return cargo(s, c, "build").returncode == 0, {}
            timed(f"{c}_incr_edit", s, r, go)
    restore(c)
    for s in SIDES:
        warm(s, c)


def w_test(c):
    for s in SIDES:
        warm(s, c, test=True)
    for r in range(A.fast):
        for s in order(r):
            def go(s=s, r=r):
                edit(s, c, r, "test")
                return cargo(s, c, "test", "--no-run").returncode == 0, {}
            timed(f"{c}_test_compile_edit", s, r, go)
    restore(c)
    for s in SIDES:
        warm(s, c)


def w_seed(c):
    for s in SIDES:
        warm(s, c)
    for r in range(A.slow):
        for s in order(r):
            dst = f"{ROOT[s]}/{c}_seed"
            shutil.rmtree(dst, ignore_errors=True)
            timed(f"{c}_seed_cp", s, r, lambda s=s: (sh(["cp", "-a", "--reflink=never", f"{ROOT[s]}/{c}", dst]).returncode == 0, {}))
            def build(s=s):
                p = sh(["cargo", "build", "--frozen", "-j4", "-v"], cwd=dst)
                fresh = sum(1 for l in p.stderr.splitlines() if l.strip().startswith("Fresh"))
                rustc = sum(1 for l in p.stderr.splitlines() if "Running `" in l and "rustc" in l)
                return p.returncode == 0, dict(fresh=fresh, rustc_runs=rustc)
            timed(f"{c}_seed_first_build", s, r, build)
            shutil.rmtree(dst, ignore_errors=True)


def w_lookup():
    N = 3000
    for s in SIDES:
        d = f"{ROOT[s]}/lk"
        os.makedirs(d, exist_ok=True)
        for i in range(N):
            open(f"{d}/e{i}", "a").close()
    def loop(names):
        t0 = time.perf_counter()
        for p in names:
            try:
                os.stat(p)
            except FileNotFoundError:
                pass
        return True, dict(us_per_op=round((time.perf_counter() - t0) / len(names) * 1e6, 2))
    for r in range(A.slow):
        for s in order(r):
            d = f"{ROOT[s]}/lk"
            time.sleep(1.2)
            timed("lookup_exist_distinct", s, r, lambda d=d: loop([f"{d}/e{i}" for i in range(N)]))
            timed("lookup_missing_distinct", s, r, lambda d=d, r=r: loop([f"{d}/m{r}_{i}" for i in range(N)]))
            timed("lookup_missing_same", s, r, lambda d=d: loop([f"{d}/nofile"] * N))
            timed("lookup_exist_same", s, r, lambda d=d: loop([f"{d}/e0"] * N))


def dropcaches():
    sh("sudo sh -c 'sync; echo 3 > /proc/sys/vm/drop_caches'", shell=True)


def w_seqio():
    for r in range(A.slow):
        for s in order(r):
            f = f"{ROOT[s]}/io.bin"
            timed("io_write_256M_fsync", s, r, lambda f=f: (sh(f"dd if=/dev/zero of={f} bs=1M count=256 conv=fsync", shell=True).returncode == 0, {}))
            dropcaches()
            timed("io_read_256M_cold", s, r, lambda f=f: (sh(f"dd if={f} of=/dev/null bs=1M", shell=True).returncode == 0, {}))
            timed("io_read_256M_warm", s, r, lambda f=f: (sh(f"dd if={f} of=/dev/null bs=1M", shell=True).returncode == 0, {}))
            os.unlink(f)


def w_xgit():
    for s in SIDES:
        warm(s, "x")
        d = f"{ROOT[s]}/x"
        if not os.path.exists(f"{d}/.git"):
            open(f"{d}/.gitignore", "w").write("target\n")
            for cmd in (["git", "init", "-q"], ["git", "config", "gc.auto", "0"], ["git", "config", "maintenance.auto", "false"], ["git", "add", "-A"], ["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "c"]):
                assert sh(cmd, cwd=d).returncode == 0
        sh(["git", "status"], cwd=d)
    interleave("x_git_status", A.fast, lambda s: (sh(["git", "status"], cwd=f"{ROOT[s]}/x").returncode == 0, {}))


def w_git():
    if not os.path.exists(f"{D}/bigtree"):
        reg = subprocess.run(f"ls -d {D}/cargo-home/registry/src/*/", shell=True, capture_output=True, text=True).stdout.split()[0]
        assert sh(["cp", "-a", reg, f"{D}/bigtree"]).returncode == 0
        for c in ("x", "y"):
            assert sh(["cp", "-a", f"{D}/src/{c}", f"{D}/bigtree/{c}"]).returncode == 0
    for r in range(A.slow):
        for s in order(r):
            g = f"{ROOT[s]}/gitbig"
            shutil.rmtree(g, ignore_errors=True)
            timed("bt_cp", s, r, lambda s=s, g=g: (sh(["cp", "-a", "--reflink=never", f"{D}/bigtree", g]).returncode == 0, {}))
            def add(g=g):
                ok = all(sh(c, cwd=g).returncode == 0 for c in (["git", "init", "-q"], ["git", "config", "gc.auto", "0"], ["git", "config", "maintenance.auto", "false"], ["git", "add", "-A"],
                         ["git", "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "c"]))
                return ok, dict(tracked=int(sh("git ls-files | wc -l", cwd=g, shell=True).stdout))
            timed("bt_git_add_commit", s, r, add)
            sh(["git", "status"], cwd=g)
            for j in range(A.fast):
                timed("bt_git_status", s, r * 100 + j, lambda g=g: (sh(["git", "status"], cwd=g).returncode == 0, {}))
            for j in range(3):
                timed("bt_find", s, r * 100 + j, lambda g=g: (sh(f"find {g} -printf . | wc -c", shell=True).returncode == 0, {}))
            def rm(g=g):
                p = sh(["rm", "-rf", g])
                if p.returncode:
                    print(p.stderr, flush=True)
                return p.returncode == 0, {}
            timed("bt_rm", s, r, rm)


GROUPS = {
    "x_clean": lambda: w_clean("x"), "x_noop": lambda: w_noop("x"), "x_gitstatus": w_xgit,
    "x_incr": lambda: w_incr("x"), "x_test": lambda: w_test("x"), "x_seed": lambda: w_seed("x"),
    "x_rel": lambda: w_rel("x"),
    "y_clean": lambda: w_clean("y"), "y_noop": lambda: w_noop("y"), "y_incr": lambda: w_incr("y"),
    "y_test": lambda: w_test("y"), "y_seed": lambda: w_seed("y"), "git": w_git, "lookup": w_lookup, "seqio": w_seqio,
}

os.makedirs(f"{D}/results", exist_ok=True)
shutil.rmtree(ROOT["native"], ignore_errors=True) if not os.path.exists(OUT) else None
os.makedirs(ROOT["native"], exist_ok=True)
mounted = False
if "fuse" in SIDES:
    if not os.path.exists(OUT):
        shutil.rmtree(BACK, ignore_errors=True)
    p = subprocess.run(["bash", f"{D}/mount.sh", BACK, ROOT["fuse"], *flags.split()], capture_output=True, text=True)
    print(p.stdout, p.stderr, flush=True)
    if p.returncode:
        sys.exit(5)
    mounted = True
try:
    print(f"start {label} flags='{flags}' workloads={WL} uptime={open('/proc/loadavg').read().strip()}", flush=True)
    for w in WL:
        if not A.nolock:
            acquire()
        try:
            for g in w.split("+"):
                GROUPS[g]()
        finally:
            if not A.nolock:
                release()
    open(f"{D}/results/{label}.complete", "w").write(A.workloads)
    print("ALL DONE", flush=True)
finally:
    if mounted:
        print(subprocess.run(["bash", f"{D}/umount.sh", ROOT["fuse"]], capture_output=True, text=True).stdout, flush=True)
