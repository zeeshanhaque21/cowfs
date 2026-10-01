#!/usr/bin/env python3
"""usage: srv.py start [server args] | stop | stats
spike18 private server on port 11116. MOUNT_EXTRA env appends mount options, SRV_ENV_NAME_STATS=1 enables name stats."""
import os, signal, subprocess, sys, time

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/spike18"))
BIN = os.environ.get("SRV_BIN", f"{S}/target/release/nfs-loopback")
BACK, MNT, PIDF, LOG = f"{S}/backing", f"{S}/mnt", f"{S}/server.pid", f"{S}/server.log"
PORT = "11116"
BASE_OPTS = f"locallocks,vers=3,tcp,rsize=131072,actimeo=120,port={PORT},mountport={PORT}"


def mounted():
    return f" on {MNT} " in subprocess.run(["mount"], capture_output=True, text=True).stdout


def pid():
    p = open(PIDF).read().strip()
    cmd = subprocess.run(["ps", "-p", p, "-o", "command="], capture_output=True, text=True).stdout
    return int(p) if BIN in cmd and f"--port {PORT}" in cmd else None


def start(args=(), extra=None, name_stats=False):
    assert not mounted(), "already mounted"
    os.makedirs(BACK, exist_ok=True)
    os.makedirs(MNT, exist_ok=True)
    env = dict(os.environ)
    if name_stats:
        env["NAME_STATS"] = "1"
    p = subprocess.Popen([BIN, "--root", BACK, "--port", PORT, "--hide-appledouble", *args],
                         stdout=open(LOG, "a"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                         start_new_session=True, env=env)
    open(PIDF, "w").write(str(p.pid))
    time.sleep(1.0)
    assert p.poll() is None, "server died"
    opts = BASE_OPTS + ("," + extra if extra else "")
    subprocess.run(["mount_nfs", "-o", opts, "localhost:/", MNT], timeout=30, check=True)
    assert mounted()
    return p.pid, opts


def stop():
    if mounted() and subprocess.run(["umount", MNT], timeout=60).returncode != 0:
        subprocess.run(["umount", "-f", MNT], timeout=60)
    assert not mounted(), "still mounted"
    p = pid() if os.path.exists(PIDF) else None
    if p:
        os.kill(p, signal.SIGTERM)
        for _ in range(50):
            if subprocess.run(["ps", "-p", str(p)], capture_output=True).returncode:
                break
            time.sleep(0.1)
    return p


def stats():
    """USR1 the server, return the stats block it printed (also resets counters)"""
    p = pid()
    off = os.path.getsize(LOG)
    os.kill(p, signal.SIGUSR1)
    t0 = time.time()
    while time.time() - t0 < 10:
        with open(LOG) as f:
            f.seek(off)
            s = f.read()
        if "\nEND\n" in s:
            return s[s.index("STATS\n") + 6:s.index("\nEND\n")]
        time.sleep(0.05)
    raise RuntimeError("no stats reply")


def parse(block):
    r = {}
    for l in block.splitlines():
        w = l.split()
        if len(w) == 4 and w[0].startswith("NFSPROC3_"):
            r[w[0][9:]] = [int(w[1]), float(w[3])]
        elif w and w[0] == "TOTAL":
            r["TOTAL"] = int(w[1])
        elif w and w[0] == "LOOKUP_HIT":
            r["LOOKUP_HIT"], r["LOOKUP_MISS"] = int(w[1]), int(w[3])
        elif w and w[0] == "LOOKUP_DISTINCT":
            r["LK"] = {w[k]: int(w[k + 1]) for k in range(1, len(w) - 1, 2)}
        elif w and w[0] in ("PROC1", "PROC4"):
            r[w[0] + "_distinct"] = int(w[2])
    return r


if __name__ == "__main__":
    c = sys.argv[1]
    if c == "start":
        print(start(sys.argv[2:], os.environ.get("MOUNT_EXTRA"), bool(os.environ.get("SRV_ENV_NAME_STATS"))))
    elif c == "stop":
        print("stopped", stop())
    else:
        print(stats())
