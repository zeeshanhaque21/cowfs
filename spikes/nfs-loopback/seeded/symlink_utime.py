#!/usr/bin/env python3
"""os.utime(follow_symlinks=False) on symlinks: native vs mount. usage: symlink_utime.py"""
import os, shutil

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/seeded"))
T = 1_000_000_000
for side, root in (("native", f"{S}/native"), ("nfs", f"{S}/mnt")):
    d = f"{root}/symtest"
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    open(f"{d}/real", "w").write("x")
    os.symlink("real", f"{d}/ok")
    os.symlink("missing", f"{d}/dangling")
    for name in ("ok", "dangling"):
        try:
            os.utime(f"{d}/{name}", (T, T), follow_symlinks=False)
            r = "ok lmtime=%d" % os.lstat(f"{d}/{name}").st_mtime
        except OSError as e:
            r = f"ERR {e}"
        print(side, name, r, "target_mtime_changed=", os.stat(f"{d}/real").st_mtime == T)
