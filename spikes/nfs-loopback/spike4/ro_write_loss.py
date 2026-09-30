#!/usr/bin/env python3
"""usage: ro_write_loss.py [n] - create files with mode 0444/0644 via O_CREAT, write 8 MiB random, check every syscall, verify bytes in the backing dir"""
import hashlib, json, os, sys
from lib import *

N = int(sys.argv[1]) if len(sys.argv) > 1 else 20
D = f"{MNT}/probe/rw"
os.makedirs(D, exist_ok=True)
res = {}
for mode in (0o444, 0o644):
    c = {"files": 0, "syscall_err": 0, "silent_bad": 0, "ok": 0, "err_sites": {}}
    for i in range(N):
        p = f"{D}/f{mode:o}_{i}"
        data = os.urandom(8 << 20)
        site = None
        try:
            fd = os.open(p, os.O_CREAT | os.O_WRONLY | os.O_EXCL, mode)
            try:
                site = "write"
                mv = memoryview(data)
                while mv:
                    mv = mv[os.write(fd, mv):]
                site = "fsync"
                os.fsync(fd)
            finally:
                try:
                    os.close(fd)
                except OSError:
                    site = site or "close"
                    raise
        except OSError:
            c["syscall_err"] += 1
            c["err_sites"][site] = c["err_sites"].get(site, 0) + 1
            c["files"] += 1
            continue
        c["files"] += 1
        back = open(f"{BACK}/probe/rw/f{mode:o}_{i}", "rb").read()
        if hashlib.sha1(back).digest() != hashlib.sha1(data).digest():
            c["silent_bad"] += 1
            z = sum(1 for k in range(0, len(back), 4096) if len(back) > k and not any(back[k:k + 4096]))
            c.setdefault("zero_4k_blocks", []).append(z)
        else:
            c["ok"] += 1
    res[oct(mode)] = c
    print(oct(mode), c, flush=True)
json.dump(res, open(f"{OUT}/ro_write_loss.json", "w"), indent=1)
