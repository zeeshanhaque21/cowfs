#!/usr/bin/env python3
"""grow a hardlinked deps dir natively (default profile), rsync -aH it onto the mount, list it there.
Run after a server restart so the client cache is cold."""
import os, subprocess, sys

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/seeded"))
env = {k: v for k, v in os.environ.items() if k != "CARGO_PROFILE_DEV_SPLIT_DEBUGINFO"}
d = f"{S}/native/probe"
if sys.argv[1] == "grow":
    subprocess.run(["rm", "-rf", d], check=True)
    subprocess.run(["rsync", "-a", "--exclude", "target", f"{S}/native-seed/", d + "/"], check=True)
    subprocess.run(["cargo", "build", "--frozen"], cwd=d, env=env, capture_output=True, check=True)
    for i in range(25):
        open(f"{d}/src/main.rs", "a").write(f"// e{i}\n")
        subprocess.run(["cargo", "build", "--frozen"], cwd=d, env=env, capture_output=True, check=True)
    dd = f"{d}/target/debug/deps"
    print("native deps entries", len(os.listdir(dd)),
          "hardlinked", subprocess.run(f"find {dd} -type f -links +1 | wc -l", shell=True, capture_output=True, text=True).stdout.strip())
    subprocess.run(["rm", "-rf", f"{S}/mnt/t_deps"])
    r = subprocess.run(["rsync", "-aH", dd + "/", f"{S}/mnt/t_deps/"])
    print("rsync rc", r.returncode)
else:
    try:
        n = os.listdir(f"{S}/mnt/t_deps")
        print("mount listing", len(n), "unique", len(set(n)))
    except OSError as e:
        print("mount listing ERR", e)
