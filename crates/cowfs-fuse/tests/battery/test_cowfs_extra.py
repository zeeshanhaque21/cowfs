#!/usr/bin/env python3
"""cowfs-fuse extras that need no backing directory. usage: test_cowfs_extra.py <mnt>

Covers symlink-safe setattr, unlinked-open files, O_APPEND, rename flags, xattrs, mknod, statfs, access.
"""
import ctypes, errno, os, shutil, stat, sys

MNT = os.path.abspath(sys.argv[1])
W = os.path.join(MNT, f"extra{os.getpid()}")
os.makedirs(W)
P = lambda n: os.path.join(W, n)
fails = 0
libc = ctypes.CDLL(None, use_errno=True)
AT_FDCWD = -100


def check(name, ok, detail=""):
    global fails
    fails += not ok
    print(f"{'PASS' if ok else 'FAIL'} {name} {detail}", flush=True)


def errno_of(fn, *a, **k):
    try:
        fn(*a, **k)
        return 0
    except OSError as e:
        return e.errno


def renameat2(a, b, flags):
    r = libc.renameat2(AT_FDCWD, a.encode(), AT_FDCWD, b.encode(), flags)
    return 0 if r == 0 else ctypes.get_errno()


T = 1_000_000_000
open(P("real"), "w").write("x")
os.utime(P("real"), (5, 5))
os.symlink("real", P("ok"))
os.symlink("missing", P("dangling"))
for n in ("ok", "dangling"):
    e = errno_of(os.utime, P(n), (T, T), follow_symlinks=False)
    check(f"utime nofollow on symlink {n}", e == 0 and int(os.lstat(P(n)).st_mtime) == T and int(os.stat(P("real")).st_mtime) == 5, f"errno={e}")
    e = errno_of(os.chown, P(n), os.getuid(), os.getgid(), follow_symlinks=False)
    check(f"lchown on symlink {n}", e == 0, f"errno={e}")
check("readlink", os.readlink(P("ok")) == "real" and os.readlink(P("dangling")) == "missing")

fd = os.open(P("real"), os.O_RDWR)
os.unlink(P("real"))
os.write(fd, b"hello")
os.lseek(fd, 0, 0)
st = os.fstat(fd)
check("unlinked open file: fstat nlink 0", st.st_nlink == 0 and st.st_size == 5, f"nlink={st.st_nlink} size={st.st_size}")
check("unlinked open file: still readable", os.read(fd, 10) == b"hello")
os.close(fd)
check("unlinked file gone after close", not os.path.exists(P("real")))

fd = os.open(P("app"), os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
os.write(fd, b"aaa")
os.lseek(fd, 0, 0)
os.write(fd, b"bbb")
os.close(fd)
check("O_APPEND appends", open(P("app"), "rb").read() == b"aaabbb")

os.mkdir(P("d1"))
os.mkdir(P("d2"))
open(P("d1/f"), "w").close()
os.rename(P("d1"), P("d2"))
check("rename dir over empty dir", os.listdir(P("d2")) == ["f"] and not os.path.exists(P("d1")))
os.mkdir(P("d3"))
open(P("d3/x"), "w").close()
os.mkdir(P("d4"))
check("rename over non-empty dir is ENOTEMPTY", errno_of(os.rename, P("d4"), P("d3")) == errno.ENOTEMPTY)
check("rename dir into itself is EINVAL", errno_of(os.rename, P("d3"), P("d3/x2")) in (errno.EINVAL,))

open(P("r1"), "w").write("1")
open(P("r2"), "w").write("2")
check("RENAME_NOREPLACE onto existing is EEXIST", renameat2(P("r1"), P("r2"), 1) == errno.EEXIST and open(P("r2")).read() == "2")
check("RENAME_NOREPLACE onto free name works", renameat2(P("r1"), P("r3"), 1) == 0 and open(P("r3")).read() == "1")
check("RENAME_EXCHANGE is ENOTSUP", renameat2(P("r3"), P("r2"), 2) == errno.ENOTSUP and open(P("r2")).read() == "2")

open(P("x"), "w").close()
os.setxattr(P("x"), "user.a", b"one")
check("xattr get", os.getxattr(P("x"), "user.a") == b"one")
check("xattr list", os.listxattr(P("x")) == ["user.a"])
check("xattr missing is ENODATA", errno_of(os.getxattr, P("x"), "user.zz") == errno.ENODATA)
check("xattr XATTR_CREATE on existing is EEXIST", errno_of(os.setxattr, P("x"), "user.a", b"2", os.XATTR_CREATE) == errno.EEXIST)
check("xattr XATTR_REPLACE on missing is ENODATA", errno_of(os.setxattr, P("x"), "user.b", b"2", os.XATTR_REPLACE) == errno.ENODATA)
os.setxattr(P("x"), "user.big", b"z" * 1000)
check("xattr large value", os.getxattr(P("x"), "user.big") == b"z" * 1000)
os.removexattr(P("x"), "user.a")
check("xattr remove", sorted(os.listxattr(P("x"))) == ["user.big"] and errno_of(os.removexattr, P("x"), "user.a") == errno.ENODATA)

os.mknod(P("reg"), stat.S_IFREG | 0o644)
check("mknod regular file", stat.S_ISREG(os.stat(P("reg")).st_mode))
os.mkfifo(P("fifo"), 0o640)
check("mknod fifo is a fifo", stat.S_ISFIFO(os.lstat(P("fifo")).st_mode) and stat.S_IMODE(os.lstat(P("fifo")).st_mode) == 0o640)

sv = os.statvfs(W)
check("statfs", sv.f_bsize > 0 and sv.f_blocks > 0 and sv.f_namemax == 255, str(sv))
check("name too long", errno_of(open, P("n" * 256), "w") == errno.ENAMETOOLONG)

with open(P("sparse"), "wb") as f:
    f.seek(1 << 20)
    f.write(b"end")
d = open(P("sparse"), "rb").read()
check("hole reads as zeros", len(d) == (1 << 20) + 3 and d[: 1 << 20] == bytes(1 << 20) and d.endswith(b"end"))
os.truncate(P("sparse"), 10)
check("truncate shrinks", os.stat(P("sparse")).st_size == 10)
os.truncate(P("sparse"), 100)
check("truncate extends with zeros", open(P("sparse"), "rb").read() == bytes(100))

open(P("perm"), "w").close()
os.chmod(P("perm"), 0o4755)
check("chmod keeps setuid bit", stat.S_IMODE(os.stat(P("perm")).st_mode) == 0o4755)
os.chmod(P("perm"), 0o000)
check("access honours mode bits", (os.access(P("perm"), os.R_OK) is False) or os.getuid() == 0)

shutil.rmtree(W)
check("cleanup", not os.path.exists(W))
print(f"\n{fails} failures")
sys.exit(1 if fails else 0)
