#!/usr/bin/env python3
"""probe: N edit-builds on the NFS mount, then list target/debug/deps. usage: probe_readdir.py [ENV=VAL ...]"""
import os, subprocess, sys, time

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/seeded"))
env = dict(os.environ, **dict(a.split("=", 1) for a in sys.argv[1:]))
d = f"{S}/mnt/probe"
subprocess.run(["rm", "-rf", d], timeout=120)
subprocess.run(["rsync", "-a", "--exclude", "target", f"{S}/native-seed/", d + "/"], check=True, timeout=120)
t = time.time()
p = subprocess.run(["cargo", "build", "--frozen"], cwd=d, env=env, capture_output=True, text=True, timeout=300)
print("clean build rc", p.returncode, round(time.time() - t, 1), flush=True)
for i in range(6):
    open(f"{d}/src/main.rs", "a").write(f"// e{i}\n")
    p = subprocess.run(["cargo", "build", "--frozen"], cwd=d, env=env, capture_output=True, text=True, timeout=300)
    dd = f"{d}/target/debug/deps"
    try:
        n = len(os.listdir(dd))
    except OSError as e:
        n = f"ERR {e}"
    print("edit", i, "rc", p.returncode, "deps entries", n, flush=True)
    if p.returncode:
        print(p.stderr[-300:])
        break
print("hardlinked files in backing deps:",
      subprocess.run(f"find {S}/backing/probe/target/debug/deps -type f -links +1 | wc -l", shell=True, capture_output=True, text=True).stdout.strip())
