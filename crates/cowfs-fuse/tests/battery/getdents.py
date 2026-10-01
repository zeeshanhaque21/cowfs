#!/usr/bin/env python3
"""Prints `name ino` for every raw getdents64 entry of a directory, `.` and `..` included."""
import ctypes, os, platform, struct, sys

NR = {"aarch64": 61, "x86_64": 217}[platform.machine()]
libc = ctypes.CDLL(None, use_errno=True)
fd = os.open(sys.argv[1], os.O_RDONLY | os.O_DIRECTORY)
buf = ctypes.create_string_buffer(65536)
while True:
    n = libc.syscall(NR, fd, buf, len(buf))
    if n <= 0:
        break
    raw, off = buf.raw, 0
    while off < n:
        ino, _, reclen, _t = struct.unpack_from("<QqHB", raw, off)
        print(raw[off + 19 : off + reclen].split(b"\0")[0].decode(), ino)
        off += reclen
