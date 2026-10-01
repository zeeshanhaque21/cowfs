"""Write files through mmap with NO msync, then re-read from a fresh process.

Usage: mmap_race.py <dir> [<n>] [<mode>]
mode: nosync (default), msync, fsync.  Prints mismatch counts by kind.
"""
import hashlib, mmap, os, random, subprocess, sys

d = sys.argv[1]
n = int(sys.argv[2]) if len(sys.argv) > 2 else 100
mode = sys.argv[3] if len(sys.argv) > 3 else "nosync"
os.makedirs(d, exist_ok=True)
random.seed(1)
bad = {"size": 0, "content": 0, "missing_tail": 0}
for i in range(n):
    size = random.randint(50_000, 1_000_000)
    data = os.urandom(size)
    p = os.path.join(d, f"f{i}")
    fd = os.open(p, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
    os.ftruncate(fd, size)
    m = mmap.mmap(fd, size)
    m[:] = data
    if mode == "msync":
        m.flush()
    m.close()
    if mode == "fsync":
        os.fsync(fd)
    os.close(fd)
    r = subprocess.run(["cat", p], capture_output=True)
    got = r.stdout
    if len(got) != size:
        bad["size"] += 1
        if size - len(got) < 65536:
            bad["missing_tail"] += 1
    elif hashlib.md5(got).digest() != hashlib.md5(data).digest():
        bad["content"] += 1
print(f"{d} mode={mode} n={n}: size_mismatch={bad['size']} (tail_only={bad['missing_tail']}) content_mismatch={bad['content']}")
