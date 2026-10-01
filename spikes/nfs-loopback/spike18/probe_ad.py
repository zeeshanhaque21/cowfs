#!/usr/bin/env python3
"""which mount options / server args stop AppleDouble ._ companions. usage: probe_ad.py 'extra1' 'extra2' ..."""
import os, subprocess, sys, shutil
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import srv

for extra in sys.argv[1:]:
    if srv.mounted():
        srv.stop()
    try:
        srv.start(os.environ.get("SRV_ARGS", "").split(), extra or None)
    except Exception as e:
        print(repr(extra), "MOUNT FAILED", e)
        continue
    d = f"{srv.MNT}/adprobe"
    subprocess.run(["rm", "-rf", d])
    os.makedirs(d)
    open(f"{d}/plain", "w").write("x")
    shutil.copy("/bin/echo", f"{d}/exe")
    subprocess.run([f"{d}/exe", "hi"], capture_output=True)
    w = subprocess.run(["xattr", "-w", "user.t", "v", f"{d}/plain"], capture_output=True, text=True)
    xa = subprocess.run(["xattr", "-l", f"{d}/plain", f"{d}/exe"], capture_output=True, text=True).stdout
    back = sorted(os.listdir(f"{srv.BACK}/adprobe"))
    print(repr(extra), "backing:", back, "| xattr -w rc", w.returncode, w.stderr.strip()[:80], "| xattrs:", xa.replace("\n", "; ")[:200])
    print("  stats", srv.parse(srv.stats()))
    subprocess.run(["rm", "-rf", d])
    srv.stop()
