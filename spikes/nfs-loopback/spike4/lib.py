import json, os, shutil, statistics, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.abspath(f"{HERE}/../out/spike4")
LOCK = os.path.abspath(f"{HERE}/../out/cpu.lock")
BIN, BACK, MNT, PIDF, PORT = f"{OUT}/srv-bin", f"{OUT}/backing", f"{OUT}/mnt", f"{OUT}/server.pid", 11114
NAT = f"{OUT}/native"
ENV = {**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1", "GIT_PAGER": "cat",
       "GIT_TERMINAL_PROMPT": "0", "LC_ALL": "C",
       "GIT_AUTHOR_NAME": "b", "GIT_AUTHOR_EMAIL": "b@x", "GIT_COMMITTER_NAME": "b", "GIT_COMMITTER_EMAIL": "b@x",
       "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"}


def paths(S, L):
    """S = sample set (toy|real), L = N|M|O. returns dict root, repo, gitdir"""
    root = f"{NAT if L == 'N' else MNT}/{S}/{L}"
    return {"root": root, "repo": f"{root}/repo", "gitdir": f"{NAT}/{S}/O.git" if L == "O" else f"{root}/repo/.git"}


def git(repo, *a, check=True, env=None, timeout=1800):
    r = subprocess.run(["git", "-C", repo, *a], capture_output=True, text=True, env=env or ENV, timeout=timeout)
    if check and r.returncode:
        raise RuntimeError(f"git {a} rc={r.returncode} {r.stderr[:500]}")
    return r


def timed(repo, *a, env=None):
    t = time.perf_counter()
    r = subprocess.run(["git", "-C", repo, *a], capture_output=True, text=True, env=env or ENV, timeout=3600)
    return time.perf_counter() - t, r


def mounted():
    return MNT in subprocess.run(["mount"], capture_output=True, text=True).stdout


def srv_start():
    os.makedirs(BACK, exist_ok=True)
    os.makedirs(MNT, exist_ok=True)
    log = open(f"{OUT}/server.log", "a")
    p = subprocess.Popen([BIN, "--root", BACK, "--port", str(PORT), "--hide-appledouble"], stdout=log, stderr=log,
                         stdin=subprocess.DEVNULL, start_new_session=True)
    open(PIDF, "w").write(str(p.pid))
    time.sleep(1.5)
    assert subprocess.run(["ps", "-p", str(p.pid)], capture_output=True).returncode == 0, "server died"
    subprocess.run(["mount_nfs", "-o", f"locallocks,vers=3,tcp,rsize=131072,actimeo=120,port={PORT},mountport={PORT}",
                    "localhost:/", MNT], timeout=20, check=True)
    assert mounted()


def srv_stop():
    if mounted() and subprocess.run(["umount", MNT], timeout=120, capture_output=True).returncode != 0:
        subprocess.run(["umount", "-f", MNT], timeout=120)
    assert not mounted(), "still mounted"
    pid = open(PIDF).read().strip()
    cmd = subprocess.run(["ps", "-p", pid, "-o", "command="], capture_output=True, text=True).stdout
    if BIN in cmd:
        os.kill(int(pid), 15)
        time.sleep(0.5)


def srv_restart():
    srv_stop()
    srv_start()


class cpulock:
    def __enter__(self):
        t0 = time.time()
        while True:
            try:
                os.mkdir(LOCK)
                break
            except FileExistsError:
                try:
                    pid = int(open(f"{LOCK}/owner").read().split()[1])
                    alive = subprocess.run(["ps", "-p", str(pid)], capture_output=True).returncode == 0
                    if not alive and time.time() - os.path.getmtime(LOCK) > 1200:
                        shutil.rmtree(LOCK, ignore_errors=True)
                        continue
                except Exception:
                    pass
                if time.time() - t0 > 900:
                    raise TimeoutError("cpu lock 15min")
                time.sleep(10)
        open(f"{LOCK}/owner", "w").write(f"spike4 {os.getpid()} {time.strftime('%F %T')}\n")
        return self

    def __exit__(self, *a):
        shutil.rmtree(LOCK, ignore_errors=True)


def load():
    return os.getloadavg()[0]


def summ(xs):
    xs = sorted(xs)
    return {"n": len(xs), "median": statistics.median(xs), "min": xs[0], "max": xs[-1]} if xs else {"n": 0}


def record(fn, rec):
    with open(fn, "a") as f:
        f.write(json.dumps(rec) + "\n")
        f.flush()
