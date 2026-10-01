#!/usr/bin/env python3
"""spike18 bench: native APFS vs port-11116 NFS loopback, per mount/server config.

usage: bench.py CONFIG_TAG [cells] [n]
  env MOUNT_EXTRA (mount opts appended), SRV_ARGS (server args), NAME_STATS=1, WS (X,Y)
  cells: comma list of noop,edit,tedit,modea (default noop,edit)
Appends one JSON line per run to out/spike18/runs.jsonl; nfs runs carry server op counts.
"""
import json, os, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import srv

S = srv.S
ROOT = {"native": f"{S}/native", "nfs": srv.MNT}
RUNS = f"{S}/runs.jsonl"
LOCK = os.path.abspath(f"{S}/../cpu.lock")
AGENT = "spike18-roundtrips"


def lock():
    t0 = time.time()
    while True:
        try:
            os.mkdir(LOCK)
            open(f"{LOCK}/owner", "w").write(f"{AGENT} pid={os.getpid()} t={time.time():.0f} {time.ctime()}\n")
            return time.time() - t0
        except FileExistsError:
            try:
                o = open(f"{LOCK}/owner").read()
                opid = int(o.split("pid=")[1].split()[0])
                age = time.time() - os.stat(LOCK).st_mtime
                alive = subprocess.run(["ps", "-p", str(opid)], capture_output=True).returncode == 0
                if not alive and age > 1200:
                    subprocess.run(["rm", "-rf", LOCK])
                    continue
            except (FileNotFoundError, IndexError, ValueError):
                pass
            if time.time() - t0 > 900:
                sys.exit("TIMEOUT waiting for cpu.lock")
            time.sleep(10)


def unlock():
    if os.path.exists(f"{LOCK}/owner") and AGENT in open(f"{LOCK}/owner").read():
        subprocess.run(["rm", "-rf", LOCK])


def sh(cmd, cwd):
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


def timed(tag, cell, side, ws, i, cmd, cwd, setup=None, teardown=None):
    if setup:
        setup()
    if side == "nfs":
        srv.stats()
    la0 = os.getloadavg()
    t = time.perf_counter()
    p = sh(cmd, cwd)
    dt = time.perf_counter() - t
    la1 = os.getloadavg()
    ops = srv.parse(srv.stats()) if side == "nfs" else None
    if teardown:
        teardown()
    rec = dict(tag=tag, cell=cell, side=side, ws=ws, i=i, sec=round(dt, 4), rc=p.returncode,
               load0=[round(x, 1) for x in la0], load1=[round(x, 1) for x in la1],
               hot=max(la0[0], la1[0]) > 30, compiling=p.stderr.count("Compiling"), ops=ops,
               mount=os.environ.get("MOUNT_EXTRA", ""), srv_args=os.environ.get("SRV_ARGS", ""))
    with open(RUNS, "a") as f:
        f.write(json.dumps(rec) + "\n")
    print(cell, ws, side, i, rec["sec"], "rc", p.returncode, "comp", rec["compiling"],
          "ops", ops and ops.get("TOTAL"), "load", rec["load0"][0], flush=True)
    return rec


def prep(ws):
    for side, root in ROOT.items():
        d = f"{root}/{ws}"
        if os.environ.get("FRESH"):
            subprocess.run(["rm", "-rf", d], check=True)
        if not os.path.exists(f"{d}/target"):
            subprocess.run(["rsync", "-aH", f"{S}/seed/{ws}/", d + "/"], check=True)
        for c in (["cargo", "build", "--frozen"], ["cargo", "test", "--frozen", "--no-run"]):
            assert sh(c, d).returncode == 0, (c, d)


def pairs(n, fn):
    for i in range(n):
        for side in (("native", "nfs") if i % 2 == 0 else ("nfs", "native")):
            fn(side, i)


def cells(tag, ws, which, n):
    tree = lambda side: f"{ROOT[side]}/{ws}"
    main = lambda side: f"{tree(side)}/src/main.rs"
    orig = {s: open(main(s), "rb").read() for s in ROOT}
    B, T = ["cargo", "build", "--frozen"], ["cargo", "test", "--frozen", "--no-run"]

    def edit(side, i, rebuild):
        def setup():
            with open(main(side), "ab") as f:
                f.write(f"// edit {i}\n".encode())
        def teardown():
            open(main(side), "wb").write(orig[side])
            sh(rebuild, tree(side))
        return setup, teardown

    if "noop" in which:
        pairs(n, lambda side, i: timed(tag, "noop", side, ws, i, B, tree(side)))
    if "edit" in which:
        pairs(n, lambda side, i: timed(tag, "edit", side, ws, i, B, tree(side), *edit(side, i, B)))
    if "tedit" in which:
        pairs(n, lambda side, i: timed(tag, "tedit", side, ws, i, T, tree(side), *edit(side, i, T)))
    if "modea" in which:
        def ma(side, i):
            d = f"{ROOT[side]}/modeA_{ws}"
            def setup():
                subprocess.run(["rm", "-rf", d])
                subprocess.run(["rsync", "-aH", f"{S}/seed/{ws}/", d + "/"], check=True)
            timed(tag, "modea", side, ws, i, B, d, setup=setup, teardown=lambda: subprocess.run(["rm", "-rf", d]))
        pairs(n, ma)


if __name__ == "__main__":
    tag = sys.argv[1]
    which = (sys.argv[2] if len(sys.argv) > 2 else "noop,edit").split(",")
    n = int(sys.argv[3]) if len(sys.argv) > 3 else 5
    wss = os.environ.get("WS", "X").split(",")
    if srv.mounted():
        srv.stop()
    pid, opts = srv.start(os.environ.get("SRV_ARGS", "").split(), os.environ.get("MOUNT_EXTRA"),
                          bool(os.environ.get("NAME_STATS")))
    print("server", pid, opts, flush=True)
    try:
        for ws in wss:
            prep(ws)
            waited = lock()
            print("lock waited", round(waited), flush=True)
            try:
                cells(tag, ws, which, n)
            finally:
                unlock()
    finally:
        print("stopped", srv.stop(), flush=True)
