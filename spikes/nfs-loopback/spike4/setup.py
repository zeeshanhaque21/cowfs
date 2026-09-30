#!/usr/bin/env python3
"""usage: setup.py toy|real  - build layouts N, M, O for a sample set (server must be up for M, O)"""
import glob, os, shutil, subprocess, sys, time
from lib import *

SRC = os.path.expanduser("~/Projects/ai-tools/OmniRoute")
S = sys.argv[1]


def prep(repo):
    git(repo, "config", "gc.auto", "0")
    git(repo, "config", "maintenance.auto", "false")


def toy(L):
    p = paths(S, L)
    os.makedirs(p["repo"], exist_ok=True)
    a = ["init", "-q", "-b", "bench"] + ([f"--separate-git-dir={p['gitdir']}"] if L == "O" else []) + [p["repo"]]
    subprocess.run(["git", *a], check=True, env=ENV)
    prep(p["repo"])
    for i in range(500):
        d = f"{p['repo']}/d{i % 25}"
        os.makedirs(d, exist_ok=True)
        open(f"{d}/f{i}.txt", "w").write("".join(f"line {i} {j} {(i * 7919 + j * 104729) % 99991}\n" for j in range(80)))
    git(p["repo"], "add", "-A")
    git(p["repo"], "commit", "-qm", "toy")


def real(L):
    p = paths(S, L)
    T = f"{NAT}/{S}/T"
    os.makedirs(p["root"], exist_ok=True)
    a = ["clone", "-q", "--no-hardlinks", "-b", "bench"] + ([f"--separate-git-dir={p['gitdir']}"] if L == "O" else []) + [T, p["repo"]]
    t = time.perf_counter()
    subprocess.run(["git", *a], check=True, env=ENV)
    print(L, "clone s", round(time.perf_counter() - t, 1))
    prep(p["repo"])
    ign = f"{p['repo']}/node_modules"
    for i in range(5000):
        d = f"{ign}/p{i % 50}"
        os.makedirs(d, exist_ok=True)
        open(f"{d}/m{i}.js", "w").write(f"module.exports={i};\n" * 20)


if S == "real":
    T = f"{NAT}/real/T"
    if not os.path.isdir(T):
        os.makedirs(f"{NAT}/real", exist_ok=True)
        subprocess.run(["git", "clone", "-q", "--no-hardlinks", SRC, T], check=True, env=ENV)
        for f in glob.glob(f"{T}/.git/objects/pack/tmp_pack_*"):
            os.remove(f)
        git(T, "branch", "-f", "bench", "HEAD")
        print("T", git(T, "rev-parse", "bench").stdout.strip(), git(T, "ls-files").stdout.count("\n"), "files")
for L in "NMO":
    (toy if S == "toy" else real)(L)
    print(L, git(paths(S, L)["repo"], "rev-parse", "HEAD").stdout.strip())
