#!/usr/bin/env python3
"""FUSE-specific extras. usage: test_extra.py <mnt> <backing>"""
import fcntl, os, shutil, subprocess, sys

MNT, BACK = map(os.path.abspath, sys.argv[1:3])
W = os.path.join(MNT, "extra")
shutil.rmtree(W, ignore_errors=True)
os.makedirs(W)
T = 1_000_000_000
fails = 0


def check(name, ok, detail=""):
    global fails
    fails += not ok
    print(f"{'PASS' if ok else 'FAIL'} {name} {detail}")


P = lambda n: os.path.join(W, n)
open(P("real"), "w").write("x")
os.utime(P("real"), (5, 5))
os.symlink("real", P("ok"))
os.symlink("missing", P("dangling"))
for n in ("ok", "dangling"):
    try:
        os.utime(P(n), (T, T), follow_symlinks=False)
        check(f"utime nofollow on symlink {n}", int(os.lstat(P(n)).st_mtime) == T and int(os.stat(P("real")).st_mtime) == 5,
              f"lmtime={os.lstat(P(n)).st_mtime} target={os.stat(P('real')).st_mtime}")
    except OSError as e:
        check(f"utime nofollow on symlink {n}", False, str(e))
    try:
        os.chown(P(n), os.getuid(), os.getgid(), follow_symlinks=False)
        check(f"lchown on symlink {n}", True)
    except OSError as e:
        check(f"lchown on symlink {n}", False, str(e))
b = os.lstat(os.path.join(BACK, "extra", "dangling"))
check("backing symlink mtime matches", int(b.st_mtime) == T, str(b.st_mtime))

fd = os.open(P("real"), os.O_RDWR)
os.unlink(P("real"))
check("fstat of unlinked open file nlink 0", os.fstat(fd).st_nlink == 0, str(os.fstat(fd).st_nlink))
os.write(fd, b"abc"); os.fsync(fd); os.close(fd)

with open(P("ap"), "wb") as f:
    f.write(b"1")
for c in b"234":
    with open(P("ap"), "ab") as f:
        f.write(bytes([c]))
check("O_APPEND writes", open(P("ap"), "rb").read() == b"1234")

os.makedirs(P("d1")); os.makedirs(P("d2")); open(P("d1/f"), "w").close()
os.rename(P("d1"), P("d2"))
check("rename dir over empty dir", os.path.exists(P("d2/f")) and not os.path.exists(P("d1")))
open(P("rn"), "w").write("q")
f = open(P("rn"))
os.rename(P("rn"), P("rn2"))
check("rename while open, read ok", f.read() == "q"); f.close()

open(P("h1"), "w").write("x"); os.link(P("h1"), P("h2")); os.link(P("h1"), P("h3"))
os.unlink(P("h1"))
check("nlink after link/unlink", os.stat(P("h2")).st_nlink == 2)
r = subprocess.run(["find", W, "-name", "h*", "-links", "2"], capture_output=True, text=True)
check("find -links via readdir(plus)", len(r.stdout.split()) == 2, r.stdout.strip())

sv = os.statvfs(MNT)
check("statvfs matches backing", sv.f_bsize == os.statvfs(BACK).f_bsize and sv.f_blocks == os.statvfs(BACK).f_blocks)

os.chmod(P("d2"), 0o700)
check("chmod dir", os.stat(P("d2")).st_mode & 0o777 == 0o700)
check("stat of missing -> ENOENT twice", all(not os.path.exists(P("nope")) for _ in range(2)))
open(P("nope"), "w").write("now")
check("negative-cached name visible after create", open(P("nope")).read() == "now")

fd = os.open(P("lk"), os.O_CREAT | os.O_RDWR)
fcntl.lockf(fd, fcntl.LOCK_EX)
r = subprocess.run([sys.executable, "-c", f"import fcntl,os\nfd=os.open({P('lk')!r},os.O_RDWR)\ntry:\n fcntl.lockf(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);print('ACQ')\nexcept OSError:\n print('BLK')"], capture_output=True, text=True)
check("posix lock blocks other process", r.stdout.strip() == "BLK", r.stdout)
os.close(fd)
print("extra failures:", fails)
sys.exit(1 if fails else 0)
