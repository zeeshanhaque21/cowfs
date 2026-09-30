#!/usr/bin/env python3
"""usage: correctness.py <set> - 20 repeated commits per layout, kill -9 mid git add / mid git commit, then fsck + index/ref sanity"""
import glob, json, os, re, signal, subprocess, sys, time
from lib import *

S = sys.argv[1]
out = {}


def files(n):
    fn = f"{OUT}/files3000_{S}.json"
    if not os.path.exists(fn):
        p = paths(S, "N")["repo"]
        fs = sorted(git(p, "ls-files").stdout.split("\n"))
        ok = [f for f in fs if re.search(r"\.(ts|tsx|js|json|md|txt)$", f) and os.path.isfile(f"{p}/{f}")
              and not os.path.islink(f"{p}/{f}") and os.path.getsize(f"{p}/{f}") < 100000]
        json.dump(ok[:3000], open(fn, "w"))
    return json.load(open(fn))[:n]


def leftovers(gd):
    return sorted(os.path.relpath(x, gd) for x in glob.glob(f"{gd}/**/*.lock", recursive=True) + glob.glob(f"{gd}/**/tmp_*", recursive=True)
                  + glob.glob(f"{gd}/**/.tmp*", recursive=True))


def sane(repo, gd, base_count):
    r = {"leftovers": leftovers(gd)}
    for l in r["leftovers"]:
        if l.endswith("index.lock") or l.endswith(".lock"):
            try:
                os.remove(f"{gd}/{l}")
            except OSError:
                pass
    x = git(repo, "fsck", "--full", check=False)
    r["fsck_rc"] = x.returncode
    r["fsck_bad"] = [l for l in (x.stdout + x.stderr).split("\n") if l and "dangling" not in l][:3]
    s = git(repo, "status", "--porcelain", check=False)
    r["status_rc"] = s.returncode
    r["status_err"] = s.stderr[:200]
    r["head_ok"] = git(repo, "rev-parse", "--verify", "HEAD", check=False).returncode == 0
    r["ls_files_rc"] = git(repo, "ls-files", "--stage", check=False).returncode
    return r


for L in "NMO":
    p = paths(S, L)
    repo, gd = p["repo"], p["gitdir"]
    res = {}
    fs = files(200)
    c0 = int(git(repo, "rev-list", "--count", "HEAD").stdout)
    for i in range(20):
        with open(f"{repo}/{fs[i]}", "a") as h:
            h.write(f"\n// c20 {i}\n")
        git(repo, "commit", "-qam", f"c20 {i}")
    res["commit20"] = {"count_delta": int(git(repo, "rev-list", "--count", "HEAD").stdout) - c0, "head": git(repo, "rev-parse", "HEAD").stdout.strip(), **sane(repo, gd, c0)}
    big = files(3000 if S == "real" else 200)
    kills = {"add": [], "commit": []}
    base = git(repo, "rev-parse", "HEAD").stdout.strip()
    for mode in ("add", "commit"):
        for i, delay in enumerate((0.02, 0.05, 0.1, 0.2, 0.4)):
            for f in big:
                with open(f"{repo}/{f}", "a") as h:
                    h.write(f"\n// k {mode}{i}\n")
            if mode == "commit":
                git(repo, "add", "-A")
            a = ["add", "-A"] if mode == "add" else ["commit", "-qm", "k"]
            pr = subprocess.Popen(["git", "-C", repo, *a], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=ENV)
            time.sleep(delay)
            killed = pr.poll() is None
            if killed:
                assert subprocess.run(["ps", "-p", str(pr.pid), "-o", "command="], capture_output=True, text=True).stdout.find(repo) > 0, "pid check"
                os.kill(pr.pid, signal.SIGKILL)
            pr.wait()
            r = sane(repo, gd, 0)
            r["delay"], r["killed"] = delay, killed
            r["rerun_ok"] = git(repo, *a, check=False).returncode == 0 if mode == "add" else git(repo, "commit", "-qm", "k2", "--allow-empty", check=False).returncode == 0
            kills[mode].append(r)
            git(repo, "reset", "-q", "--hard", base)
    res["kill9"] = kills
    out[L] = res
    print(L, "commit20", res["commit20"]["count_delta"], res["commit20"]["fsck_rc"], "kills:",
          {m: (sum(k["killed"] for k in v), sum(bool(k["leftovers"]) for k in v), sum(k["fsck_rc"] != 0 for k in v), sum(k["status_rc"] != 0 for k in v), sum(not k["rerun_ok"] for k in v)) for m, v in kills.items()},
          flush=True)
print("commit20 HEAD equal across layouts:", len({out[L]["commit20"]["head"] for L in out}) == 1)
json.dump(out, open(f"{OUT}/correctness_{S}.json", "w"), indent=1)
