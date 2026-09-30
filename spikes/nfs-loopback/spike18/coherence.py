#!/usr/bin/env python3
"""usage: coherence.py <mnt> <backing>  - checks under long attr caching (single writer through the mount)"""
import os, subprocess, sys, time

M, B = sys.argv[1], sys.argv[2]
W = f"{M}/coh{os.getpid()}"
res = []


def ok(name, cond, detail=""):
    res.append(cond)
    print("PASS" if cond else "FAIL", name, detail, flush=True)


os.makedirs(f"{W}/c/src")
open(f"{W}/c/Cargo.toml", "w").write('[package]\nname="coh"\nversion="0.0.0"\nedition="2021"\n')
for k in range(3):
    open(f"{W}/c/src/main.rs", "w").write(f'fn main() {{ println!("v{k}"); }}\n')
    subprocess.run(["cargo", "build", "-q"], cwd=f"{W}/c", check=True)
    out = subprocess.run([f"{W}/c/target/debug/coh"], capture_output=True, text=True).stdout.strip()
    ok(f"edit-then-build sees edit #{k}", out == f"v{k}", out)

p = f"{W}/f"
open(p, "w").write("one")
os.rename(p, p + "2")
ok("rename: old gone", not os.path.exists(p))
ok("rename: new has data", open(p + "2").read() == "one")
open(p, "w").write("two")
ok("recreate old name after rename", open(p).read() == "two")
os.unlink(p + "2")
ok("unlink: stat ENOENT", not os.path.exists(p + "2"))
try:
    os.stat(p + "2")
    ok("unlink: os.stat raises", False)
except FileNotFoundError:
    ok("unlink: os.stat raises", True)
q = f"{W}/neg"
ok("negative lookup before create", not os.path.exists(q))
open(q, "w").write("x")
ok("create after cached negative lookup visible", os.path.exists(q) and open(q).read() == "x")
os.makedirs(f"{W}/d1/sub")
open(f"{W}/d1/sub/x", "w").write("y")
os.rename(f"{W}/d1", f"{W}/d2")
ok("dir rename: child reachable at new path", open(f"{W}/d2/sub/x").read() == "y")
ok("dir rename: old path gone", not os.path.exists(f"{W}/d1/sub/x"))
open(p, "a").write("more")
ok("size after append", os.stat(p).st_size == 7)
t0 = os.stat(p).st_mtime_ns
time.sleep(0.01)
open(p, "a").write("!")
ok("mtime advances after write through mount", os.stat(p).st_mtime_ns > t0)

rel = os.path.relpath(W, M)
bp = f"{B}/{rel}/behind"
ok("behind-mount pre: absent via mount", not os.path.exists(f"{W}/behind"))
open(bp, "w").write("b")
t = time.time()
seen = None
while time.time() - t < 10:
    if os.path.exists(f"{W}/behind"):
        seen = time.time() - t
        break
    time.sleep(0.2)
print("INFO write behind mount (backing dir) visible after", seen, "s (None = not within 10s; expected with long caches)")
subprocess.run(["rm", "-rf", W])
print(f"{sum(res)}/{len(res)} passed")
sys.exit(0 if all(res) else 1)
