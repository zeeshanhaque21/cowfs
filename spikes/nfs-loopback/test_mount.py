"""Spike #2 test battery. Usage: test_mount.py <mount_dir> [<backing_dir>]

Each check prints PASS, FAIL or ERR with detail. Lock checks run in subprocesses with a timeout
so a hung lock call is reported instead of wedging the run.
"""
import fcntl, hashlib, mmap, os, subprocess, sys, tempfile, textwrap, time, traceback

MNT = os.path.abspath(sys.argv[1])
BACK = os.path.abspath(sys.argv[2]) if len(sys.argv) > 2 else None
WORK = os.path.join(MNT, f"t{os.getpid()}")
results = []


def check(name):
    def deco(fn):
        t0 = time.time()
        try:
            detail = fn()
            status = "PASS"
        except AssertionError as e:
            status, detail = "FAIL", str(e) or traceback.format_exc(limit=1)
        except Exception as e:
            status, detail = "ERR", f"{type(e).__name__}: {e}"
        print(f"{status:<5} {name:<44} {time.time() - t0:5.1f}s {detail or ''}", flush=True)
        results.append((name, status))
        return fn
    return deco


def sub(code, timeout=15):
    try:
        r = subprocess.run([sys.executable, "-c", textwrap.dedent(code)], capture_output=True, text=True, timeout=timeout)
        return r.returncode, (r.stdout + r.stderr).strip()
    except subprocess.TimeoutExpired:
        return None, "TIMEOUT"


os.makedirs(WORK)
P = lambda n: os.path.join(WORK, n)


@check("create/write/read/stat roundtrip")
def _():
    with open(P("a"), "wb") as f:
        f.write(b"hello")
    assert open(P("a"), "rb").read() == b"hello"
    assert os.stat(P("a")).st_size == 5


@check("mkdir/readdir/rmdir/rename dir")
def _():
    os.makedirs(P("d/e"))
    open(P("d/e/f"), "w").write("x")
    assert sorted(os.listdir(P("d"))) == ["e"]
    os.rename(P("d"), P("d2"))
    assert open(P("d2/e/f")).read() == "x"


@check("large file 256 MiB sha256 roundtrip")
def _():
    h = hashlib.sha256()
    with open(P("big"), "wb") as f:
        for i in range(256):
            b = os.urandom(1 << 20)
            h.update(b)
            f.write(b)
    h2 = hashlib.sha256()
    with open(P("big"), "rb") as f:
        while b := f.read(1 << 20):
            h2.update(b)
    assert h.digest() == h2.digest(), "hash mismatch"


@check("readdir with 5000 entries")
def _():
    os.makedirs(P("many"))
    for i in range(5000):
        open(P(f"many/f{i}"), "w").close()
    assert len(os.listdir(P("many"))) == 5000, len(os.listdir(P("many")))


@check("symlink + readlink")
def _():
    os.symlink("a", P("ln"))
    assert os.readlink(P("ln")) == "a"
    assert open(P("ln")).read() == "hello"


@check("O_EXCL create fails when file exists")
def _():
    fd = os.open(P("excl"), os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    os.close(fd)
    try:
        os.open(P("excl"), os.O_CREAT | os.O_EXCL | os.O_WRONLY)
    except FileExistsError:
        return
    raise AssertionError("second O_EXCL succeeded")


@check("rename over existing file (atomic replace)")
def _():
    open(P("r1"), "w").write("new")
    open(P("r2"), "w").write("old")
    os.rename(P("r1"), P("r2"))
    assert open(P("r2")).read() == "new" and not os.path.exists(P("r1"))


@check("unlink while open, still readable")
def _():
    open(P("u"), "w").write("data")
    f = open(P("u"))
    os.unlink(P("u"))
    assert f.read() == "data"
    f.close()


@check("hardlink (os.link) and nlink")
def _():
    open(P("h1"), "w").write("x")
    os.link(P("h1"), P("h2"))
    assert os.stat(P("h1")).st_nlink == 2, f"nlink={os.stat(P('h1')).st_nlink}"


@check("chmod and utimes")
def _():
    open(P("m"), "w").close()
    os.chmod(P("m"), 0o751)
    os.utime(P("m"), (1_000_000_000, 1_000_000_000))
    st = os.stat(P("m"))
    assert st.st_mode & 0o777 == 0o751, oct(st.st_mode)
    assert int(st.st_mtime) == 1_000_000_000, st.st_mtime


@check("truncate shrink and extend")
def _():
    with open(P("t"), "wb") as f:
        f.write(b"x" * 1000)
    os.truncate(P("t"), 10)
    assert os.stat(P("t")).st_size == 10
    os.truncate(P("t"), 5000)
    assert open(P("t"), "rb").read()[10:20] == b"\0" * 10


@check("mmap read-only matches file contents")
def _():
    data = os.urandom(3 << 20)
    open(P("mm"), "wb").write(data)
    with open(P("mm"), "rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
        assert m[:] == data, "mapped bytes differ"


@check("mmap MAP_SHARED write + msync visible via read()")
def _():
    open(P("ms"), "wb").write(b"\0" * (1 << 20))
    with open(P("ms"), "r+b") as f, mmap.mmap(f.fileno(), 0) as m:
        m[100:105] = b"HELLO"
        m.flush()
    assert open(P("ms"), "rb").read()[100:105] == b"HELLO", "write via mmap not visible after msync+close"


@check("mmap MAP_SHARED write visible in backing store")
def _():
    if not BACK:
        return "skipped (no backing dir)"
    rel = os.path.relpath(P("ms"), MNT)
    got = open(os.path.join(BACK, rel), "rb").read()[100:105]
    assert got == b"HELLO", f"backing has {got!r}"


@check("mmap MAP_PRIVATE copy-on-write does not leak")
def _():
    open(P("mp"), "wb").write(b"A" * 4096)
    with open(P("mp"), "r+b") as f, mmap.mmap(f.fileno(), 0, flags=mmap.MAP_PRIVATE) as m:
        m[0:4] = b"ZZZZ"
    assert open(P("mp"), "rb").read()[:4] == b"AAAA"


@check("mmap grows: extend file then map new region")
def _():
    with open(P("mg"), "wb") as f:
        f.write(b"a" * 4096)
    with open(P("mg"), "r+b") as f:
        f.truncate(1 << 20)
        with mmap.mmap(f.fileno(), 0) as m:
            m[(1 << 20) - 3:] = b"end"
            m.flush()
    assert open(P("mg"), "rb").read()[-3:] == b"end"


LOCK_CHILD = """
import fcntl, os, sys, time
mode, path, hold = sys.argv[1], sys.argv[2], float(sys.argv[3])
"""


def lock_pair(kind, first, second):
    path = P(f"lock_{kind}_{first}_{second}")
    open(path, "w").close()
    fl = {"ex": fcntl.LOCK_EX, "sh": fcntl.LOCK_SH}
    fd = os.open(path, os.O_RDWR)
    if kind == "flock":
        fcntl.flock(fd, fl[first])
    else:
        fcntl.lockf(fd, fl[first])
    if kind == "flock":
        code = f"""
        import fcntl, os
        fd = os.open({path!r}, os.O_RDWR)
        try:
            fcntl.flock(fd, fcntl.{'LOCK_EX' if second == 'ex' else 'LOCK_SH'} | fcntl.LOCK_NB)
            print('ACQUIRED')
        except OSError as e:
            print('BLOCKED', e.errno)
        """
    else:
        code = f"""
        import fcntl, os
        fd = os.open({path!r}, os.O_RDWR)
        try:
            fcntl.lockf(fd, fcntl.{'LOCK_EX' if second == 'ex' else 'LOCK_SH'} | fcntl.LOCK_NB)
            print('ACQUIRED')
        except OSError as e:
            print('BLOCKED', e.errno)
        """
    rc, out = sub(code)
    os.close(fd)
    return out


for kind in ("flock", "fcntl"):
    @check(f"{kind} EX then other process EX (must block)")
    def _(kind=kind):
        out = lock_pair(kind, "ex", "ex")
        assert out.startswith("BLOCKED"), out
        return out

    @check(f"{kind} SH then other process SH (must acquire)")
    def _(kind=kind):
        out = lock_pair(kind, "sh", "sh")
        assert out.startswith("ACQUIRED"), out
        return out

    @check(f"{kind} SH then other process EX (must block)")
    def _(kind=kind):
        out = lock_pair(kind, "sh", "ex")
        assert out.startswith("BLOCKED"), out
        return out


@check("git init/add/commit/status/gc")
def _():
    g = P("repo")
    os.makedirs(g)
    env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null")
    run = lambda *a: subprocess.run(["git", *a], cwd=g, env=env, capture_output=True, text=True, timeout=120)
    assert run("init", "-q").returncode == 0
    run("config", "user.email", "t@t")
    run("config", "user.name", "t")
    for i in range(300):
        open(os.path.join(g, f"f{i}.txt"), "w").write(f"file {i}\n" * 50)
    assert run("add", "-A").returncode == 0, run("add", "-A").stderr
    r = run("commit", "-qm", "c")
    assert r.returncode == 0, r.stderr
    assert run("status", "--porcelain").stdout == "", "status not clean"
    r = run("gc", "-q")
    assert r.returncode == 0, r.stderr
    r = run("fsck")
    assert r.returncode == 0, r.stderr


@check("cargo build of a small crate with target dir on mount")
def _():
    c = P("crate")
    os.makedirs(os.path.join(c, "src"))
    open(os.path.join(c, "Cargo.toml"), "w").write('[package]\nname="t"\nversion="0.1.0"\nedition="2021"\n')
    open(os.path.join(c, "src/main.rs"), "w").write('fn main(){ println!("ok"); }\n')
    r = subprocess.run(["cargo", "run", "-q"], cwd=c, capture_output=True, text=True, timeout=300)
    assert r.returncode == 0 and r.stdout.strip() == "ok", (r.stdout + r.stderr)[-400:]


@check("read-only-mode files: 8 MiB write+fsync x20 each (0444, 0400)")
def _():
    data = os.urandom(8 << 20)
    fails = []
    for mode in (0o444, 0o400):
        for i in range(20):
            path = P(f"ro_{oct(mode)[2:]}_{i}")
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
            try:
                os.write(fd, data)
                os.fsync(fd)
            except OSError as e:
                fails.append(f"{oct(mode)}#{i}:{e.errno}")
            finally:
                os.close(fd)
            assert os.stat(path).st_mode & 0o777 == mode, f"mode drifted to {oct(os.stat(path).st_mode)}"
    assert not fails, f"{len(fails)}/40 failed: {fails[:4]}"


bad = [n for n, s in results if s != "PASS"]
print(f"\n{len(results) - len(bad)}/{len(results)} passed. Not passing: {bad}")
