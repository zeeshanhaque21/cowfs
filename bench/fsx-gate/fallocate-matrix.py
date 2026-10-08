#!/usr/bin/env python3
"""Print the fallocate matrix for two roots, so a support gap has a shape rather than a rumour.

    fallocate-matrix.py NATIVE_ROOT COWFS_ROOT

One row per (mode, offset, length, file state) with the errno from each root. The same probe that
fsx runs at startup is one of the rows, because that is the one fsx's own coverage decision rests
on: if fsx's probe succeeds and a wider probe does not, the tool believes in a capability the
filesystem only half has.
"""

import ctypes
import ctypes.util
import errno
import os
import sys

FALLOC_FL_KEEP_SIZE = 0x01
FALLOC_FL_PUNCH_HOLE = 0x02
FALLOC_FL_ZERO_RANGE = 0x10

MODES = [
    ("allocate", 0),
    ("keep_size", FALLOC_FL_KEEP_SIZE),
    ("punch_hole", FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE),
    ("zero_range_keep", FALLOC_FL_ZERO_RANGE | FALLOC_FL_KEEP_SIZE),
    ("zero_range", FALLOC_FL_ZERO_RANGE),
]
# (offset, length) pairs: the one fsx itself probes at startup comes first, on an empty file.
RANGES = [(0, 1), (0, 4096), (4096, 4096), (4096, 1), (65536, 4096)]
STATES = ["empty", "sparse_64k", "written_64k"]


def prepare(root, name, state):
    path = os.path.join(root, name)
    if os.path.exists(path):
        os.unlink(path)
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
    try:
        if state in ("sparse_64k", "written_64k"):
            os.ftruncate(fd, 65536)
        if state == "written_64k":
            for off in range(0, 65536, 4096):
                os.pwrite(fd, b"\xa5" * 4096, off)
            os.fsync(fd)
    finally:
        os.close(fd)
    st = os.stat(path)
    return path, st.st_size, st.st_blocks * 512


def probe(libc, path, mode, offset, length):
    fd = os.open(path, os.O_RDWR)
    try:
        ctypes.set_errno(0)
        rc = libc.fallocate(fd, ctypes.c_int(mode), ctypes.c_longlong(offset), ctypes.c_longlong(length))
        if rc == 0:
            return "ok"
        err = ctypes.get_errno()
        return errno.errorcode.get(err, str(err))
    finally:
        os.close(fd)


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    libc = ctypes.CDLL(ctypes.util.find_library("c") or "libc.so.6", use_errno=True)
    roots = [("native", sys.argv[1]), ("cowfs", sys.argv[2])]
    print("%-8s %-11s %-8s %-7s %-6s %-12s %-12s" %
          ("state", "mode", "offset", "length", "size", "native", "cowfs"))
    for state in STATES:
        for mode_name, mode in MODES:
            for offset, length in RANGES:
                results = []
                for _arm, root in roots:
                    path, size, allocated = prepare(root, ".fallocate-matrix", state)
                    results.append(probe(libc, path, mode, offset, length))
                    if _arm == "cowfs":
                        if os.path.exists(path):
                            os.unlink(path)
                print("%-8s %-11s %-8d %-7d %-6d %-12s %-12s" %
                      (state, mode_name, offset, length, size, results[0], results[1]))
    for _arm, root in roots:
        leftover = os.path.join(root, ".fallocate-matrix")
        if os.path.exists(leftover):
            os.unlink(leftover)


if __name__ == "__main__":
    main()
