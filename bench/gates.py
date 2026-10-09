#!/usr/bin/env python3
"""cowfs success criterion 2 gate harness: one arm, one directory, six gates.

Usage: gates.py --root DIR --label NAME --reps N [--gates g1,g3] [--scale PCT]

Every gate runs inside DIR, so the same script produces the native arm (a plain
directory on APFS or ext4) and the cowfs arm (a mount point), and the only
difference between the two runs is the path.

Gates, in fixed order:

  g1  clean `cargo build` of a pinned clone of this repo, CARGO_TARGET_DIR in DIR
  g2  warm edit-and-rebuild: edit cowfs-ctl, `cargo build` again (3 libs rebuilt, 2 bins relinked)
  g3  `git status` on a generated 100k+ file tree, warm and with 1% touched
  g4  tree walk plus a small-file read pass over 20k files
  g5  large sequential 1 GiB write and read back, fsync, MiB/s
  g6  metadata storm: create, stat and unlink 50k files

Every rep is one JSONL line, appended and flushed as it finishes, so a kill -9
loses at most the rep in flight and the file stays valid JSONL.
Re-running with the same root, label, scale and corpus reuses that file and
skips the reps already recorded.

Environment:
  COWFS_BENCH_SCALE         percent of the full file counts (default 100)
  COWFS_BENCH_CORPUS_SHA    pinned commit for the g1/g2 corpus (default the
                            sha this harness was written against)
  COWFS_BENCH_CARGO_HOME    shared registry cache, outside the measured dir
  COWFS_BENCH_CARGO_JOBS    cargo -j, same for both arms (default 4)
  COWFS_BENCH_FAKE_LOAD1    test hook: recorded load1, to exercise the
                            refuse-on-load path in compare.py
"""

import argparse
import json
import os
import platform
import random
import shutil
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "bench" / "out"
DEFAULT_SHA = "c1619ec16df3a6b11dd5a1e08e8a512b4fedd240"

FULL = {
    "small_files": 100_000,
    "large_files": 64,
    "large_bytes": 8 << 20,
    "read_files": 20_000,
    "big_bytes": 1 << 30,
    "meta_files": 50_000,
}
SEED = 0xC0FFEE
# A byte count is written in whole chunks of this size, so counts() rounds down to it.
UNIT = {"large_bytes": 1 << 16, "big_bytes": 1 << 20}
MIN_BYTES = 1 << 20
SMALL_BYTES = 256
SMALL_PER_DIR = 256
GATES = ["g1", "g2", "g3", "g4", "g5", "g6"]

# g2 edits this file: cowfs-ctl is the one crate at the pinned sha whose
# dependents (cowfs-cli, cowfs-treehouse) are the workspace's only binaries, so a
# one-line change here rebuilds three libs and relinks two executables, which is
# what an edit-rebuild loop does. (The earlier target, cowfs-vfs-path/src/cookies.rs,
# had no binary downstream at that sha and cost cargo ~0.3 s: see
# docs/verification/g1-g2-readiness-20261009.md.) EDIT_CRATE is its lib target name.
EDIT_TARGET = "crates/cowfs-ctl/src/lib.rs"
EDIT_CRATE = "cowfs_ctl"
# A g2 rep that rebuilt fewer units than this, or relinked no executable, is refused.
# Measured at the pinned sha: 5 units (3 libs, 2 bins). compare.py repeats this number.
G2_MIN_UNITS = 3


def load1() -> float:
    fake = os.environ.get("COWFS_BENCH_FAKE_LOAD1")
    if fake:
        return float(fake)
    try:
        return os.getloadavg()[0]
    except OSError:
        return float("nan")


def scaled_bytes(key: str, scale: float) -> int:
    want = int(FULL[key] * (scale / 100)) if -1e300 < scale < 1e300 else None  # a huge scale overflows int() on the product
    if want is None:
        raise OverflowError(f"scale {scale} has no representable byte count")
    return max(MIN_BYTES, want // UNIT[key] * UNIT[key])


def counts() -> dict:
    scale = float(os.environ.get("COWFS_BENCH_SCALE", "100"))
    out = {}
    for key, value in FULL.items():
        if key.endswith("_bytes"):
            out[key] = scaled_bytes(key, scale)
        else:
            out[key] = max(10, int(value * scale / 100))
    return out


def cargo_env(root: Path) -> dict:
    home = os.environ.get(
        "COWFS_BENCH_CARGO_HOME", str(OUT / "cargo-home")
    )
    env = dict(os.environ)
    env["CARGO_HOME"] = home
    env["CARGO_TARGET_DIR"] = str(root / "corpus-target")
    env["CARGO_TERM_COLOR"] = "never"
    env.pop("CARGO_INCREMENTAL", None)
    env.pop("RUSTC_WRAPPER", None)
    env.pop("RUSTFLAGS", None)
    return env


def run(cmd, cwd=None, env=None, timeout=7200):
    return subprocess.run(
        cmd,
        cwd=None if cwd is None else str(cwd),
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def checked(cmd, cwd=None, env=None, timeout=7200):
    p = run(cmd, cwd=cwd, env=env, timeout=timeout)
    if p.returncode != 0:
        sys.stderr.write(f"FAIL {cmd}\n{p.stdout[-4000:]}\n{p.stderr[-4000:]}\n")
        raise SystemExit(70)
    return p


def rebuilt_units(stdout: str) -> list:
    """Units cargo actually rebuilt (not fresh), from `--message-format=json` output."""
    out = []
    for line in stdout.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        if not isinstance(m, dict) or m.get("reason") != "compiler-artifact" or m.get("fresh"):
            continue
        target = m.get("target") or {}
        if "custom-build" in (target.get("kind") or []):
            continue
        out.append({"name": target.get("name"), "exe": bool(m.get("executable"))})
    return out


def g2_rebuild_problem(units: list):
    """Why this g2 rep measured a trivial workload, or None."""
    names = [u["name"] for u in units]
    if EDIT_CRATE not in names:
        return f"the edited crate {EDIT_CRATE} was not rebuilt (rebuilt: {names})"
    if len(units) < G2_MIN_UNITS:
        return f"only {len(units)} unit(s) rebuilt, need at least {G2_MIN_UNITS} (rebuilt: {names})"
    if not any(u["exe"] for u in units):
        return f"no executable was relinked (rebuilt: {names})"
    return None


def jobs() -> list:
    return ["-j", os.environ.get("COWFS_BENCH_CARGO_JOBS", "4")]


class Ctx:
    def __init__(self, root: Path, n: dict):
        self.root = root
        self.n = n
        self.sha = os.environ.get("COWFS_BENCH_CORPUS_SHA", DEFAULT_SHA)
        self.env = cargo_env(root)
        self.corpus = root / "corpus"
        self.tree = root / "tree"

    # --- setup, all untimed -------------------------------------------------

    def ensure_corpus(self):
        git = str(self.corpus / ".git")
        if not Path(git).exists():
            self.root.mkdir(parents=True, exist_ok=True)
            checked(
                [
                    "git",
                    "clone",
                    "--no-hardlinks",
                    "--quiet",
                    str(REPO),
                    str(self.corpus),
                ]
            )
        head = run(["git", "-C", str(self.corpus), "rev-parse", "HEAD"]).stdout.strip()
        if head != self.sha:
            checked(["git", "-C", str(self.corpus), "fetch", "--quiet", "origin", self.sha])
            checked(["git", "-C", str(self.corpus), "checkout", "--quiet", self.sha])
        # Populate the shared registry cache once, untimed, so no build is ever
        # waiting on the network.
        checked(
            ["cargo", "fetch", "--locked", "--manifest-path", str(self.corpus / "Cargo.toml")],
            env=self.env,
        )

    def warm_target(self):
        if not (self.root / "corpus-target" / "debug").exists():
            checked(
                ["cargo", "build", "--offline", "--locked", *jobs()],
                cwd=self.corpus,
                env=self.env,
            )

    def ensure_tree(self):
        marker = self.tree / ".generated"
        want = f"{self.n['small_files']} {self.n['large_files']} {self.n['large_bytes']} {SEED}"
        if marker.exists() and marker.read_text().strip() == want:
            self.verify_tree()
            return
        if self.tree.exists():
            shutil.rmtree(self.tree)
        rng = random.Random(SEED)
        per = SMALL_PER_DIR
        total = self.n["small_files"]
        bodies = [bytes(rng.getrandbits(8) for _ in range(SMALL_BYTES)) for _ in range(64)]
        for d in range(per):
            sub = self.tree / f"d{d:03d}"
            sub.mkdir(parents=True)
            for i in range(max(1, total // per)):
                (sub / f"f{i:05d}").write_bytes(bodies[(i + d) % 64])
        big = self.tree / "big"
        big.mkdir()
        chunk = bytes(rng.getrandbits(8) for _ in range(UNIT["large_bytes"]))
        for i in range(self.n["large_files"]):
            with open(big / f"b{i:03d}", "wb") as fh:
                fh.writelines(chunk for _ in range(self.n["large_bytes"] // len(chunk)))
        git = [
            "git",
            "-c",
            "user.email=bench@cowfs.invalid",
            "-c",
            "user.name=cowfs bench",
            "-c",
            "core.fsmonitor=false",
            "-C",
            str(self.tree),
        ]
        self.verify_tree()
        checked([*git, "init", "--quiet"])
        checked([*git, "add", "-A"])
        checked([*git, "commit", "--quiet", "--no-verify", "-m", "bench tree"])
        marker.write_text(want)

    def verify_tree(self):
        """Refuse, never repair: a short or stray fixture file on the cowfs arm may be data loss.

        Checks the exact entry set and file lengths, not contents: a same-length corruption is not detected,
        because hashing 100k files on every run would dominate setup.
        """
        sub = max(1, self.n["small_files"] // SMALL_PER_DIR)
        fix = f"remove {self.tree}"
        want_dirs = {f"d{d:03d}" for d in range(SMALL_PER_DIR)}
        want_small = {f"f{i:05d}" for i in range(sub)}
        want_large = {f"b{i:03d}" for i in range(self.n["large_files"])}
        try:
            with os.scandir(self.tree) as it:
                top = {e.name for e in it}
            extra = top - want_dirs - {"big", ".git", ".generated"}
            if extra or not (want_dirs | {"big"}) <= top:
                raise SystemExit(f"tree top level differs: stray {sorted(extra)[:3]} missing {sorted((want_dirs | {'big'}) - top)[:3]}; {fix}")
            for d in sorted(want_dirs):
                with os.scandir(self.tree / d) as it:
                    ents = [(e.name, e.is_file(follow_symlinks=False) and e.stat().st_size) for e in it]
                if {n for n, _ in ents} != want_small or any(z != SMALL_BYTES for _, z in ents):
                    raise SystemExit(f"tree {d}: want exactly {sub} files of {SMALL_BYTES} bytes, got {sorted(ents)[:3]}.. ({len(ents)} entries); {fix}")
            with os.scandir(self.tree / "big") as it:
                big = {e.name: e.is_file(follow_symlinks=False) and e.stat().st_size for e in it}
            if set(big) != want_large or any(z != self.n["large_bytes"] for z in big.values()):
                raise SystemExit(f"tree big: want exactly {len(want_large)} files of {self.n['large_bytes']} bytes, got {sorted(big.items())[:3]}.. ({len(big)} entries); {fix}")
        except OSError as e:
            raise SystemExit(f"tree fixture unreadable: {e}; {fix}")

    # --- gates ---------------------------------------------------------------

    def g1(self):
        target = self.root / "corpus-target"
        if target.exists():
            shutil.rmtree(target)
        checked(
            ["cargo", "build", "--offline", "--locked", *jobs()],
            cwd=self.corpus,
            env=self.env,
        )

    def g2(self):
        leaf = self.corpus / EDIT_TARGET
        leaf.write_text(
            leaf.read_text() + f"\n// cowfs bench g2 edit {time.time_ns()}\n"
        )
        p = checked(
            ["cargo", "build", "--offline", "--locked", *jobs(), "--message-format=json"],
            cwd=self.corpus,
            env=self.env,
        )
        units = rebuilt_units(p.stdout)
        why = g2_rebuild_problem(units)
        if why:
            raise SystemExit(f"g2 invalid, no rep recorded: {why}")
        return {
            "rebuilt_count": len(units),
            "bins_relinked": sum(1 for u in units if u["exe"]),
            "rebuilt_units": sorted(u["name"] for u in units),
        }

    def g3(self):
        git = [
            "git",
            "-c",
            "user.email=bench@cowfs.invalid",
            "-c",
            "user.name=cowfs bench",
            "-c",
            "core.fsmonitor=false",
            "-C",
            str(self.tree),
        ]
        warm_t = time.monotonic()
        p = checked([*git, "status", "--porcelain"])
        warm = time.monotonic() - warm_t
        warm_lines = len(p.stdout.splitlines())
        sub = self.tree / "d000"
        names = sorted(p.name for p in sub.iterdir())
        touch = names[::100][: max(1, len(names) // 100)]
        for name in touch:
            os.utime(sub / name, None)
        dirty_t = time.monotonic()
        p = checked([*git, "status", "--porcelain"])
        dirty = time.monotonic() - dirty_t
        dirty_lines = len(p.stdout.splitlines())
        if touch:
            checked([*git, "checkout", "--", "."])
        return {
            "status_warm_s": warm,
            "status_dirty_s": dirty,
            "touched": len(touch),
            "warm_lines": warm_lines,
            "dirty_lines": dirty_lines,
        }

    def g4(self):
        count = 0
        read = 0
        names = []
        t = time.monotonic()
        for dirpath, dirnames, filenames in os.walk(self.tree):
            dirnames.sort()
            for name in sorted(filenames):
                count += 1
                names.append(os.path.join(dirpath, name))
        walk = time.monotonic() - t
        limit = min(self.n["read_files"], len(names))
        step = max(1, len(names) // max(1, limit))
        sample = names[::step][:limit]
        t = time.monotonic()
        for path in sample:
            with open(path, "rb") as fh:
                read += len(fh.read(4096))
        rd = time.monotonic() - t
        return {
            "walk_s": walk,
            "entries": count,
            "read_s": rd,
            "read_files": len(sample),
            "read_bytes": read,
        }

    def g5(self):
        d = self.root / "big"
        d.mkdir(parents=True, exist_ok=True)
        path = d / "seq.bin"
        chunk = os.urandom(UNIT["big_bytes"])
        expected = self.n["big_bytes"]
        try:
            t = time.monotonic()
            with open(path, "wb") as fh:
                fh.writelines(chunk for _ in range(expected // len(chunk)))
                fh.flush()
                os.fsync(fh.fileno())
            write = time.monotonic() - t
            written = path.stat().st_size
            if written != expected:
                raise SystemExit(f"g5 wrote {written} bytes, counts() says {expected}")
            t = time.monotonic()
            got = 0
            with open(path, "rb") as fh:
                while True:
                    b = fh.read(UNIT["big_bytes"])
                    if not b:
                        break
                    got += len(b)
            read = time.monotonic() - t
            if got != written:
                raise SystemExit(f"g5 read back {got} bytes of the {written} it wrote: invalid run, no rep recorded")
            fd = os.open(d, os.O_RDONLY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
        finally:
            path.unlink(missing_ok=True)
            if path.exists():
                raise SystemExit(f"g5 could not remove its own file {path}")
        mib = expected / (1 << 20)
        return {
            "bytes": expected,
            "written_bytes": written,
            "read_bytes": got,
            "write_s": write,
            "read_s": read,
            "write_mib_s": mib / write if write else 0.0,
            "read_mib_s": mib / read if read else 0.0,
            "read_matches": True,
        }

    def g6(self):
        d = self.root / "meta"
        if d.exists():
            shutil.rmtree(d)
        d.mkdir(parents=True)
        n = self.n["meta_files"]
        t = time.monotonic()
        for i in range(n):
            with open(d / f"m{i:06d}", "wb") as fh:
                fh.write(b"x")
        create = time.monotonic() - t
        t = time.monotonic()
        for i in range(n):
            os.stat(d / f"m{i:06d}")
        stat = time.monotonic() - t
        t = time.monotonic()
        for i in range(n):
            os.unlink(d / f"m{i:06d}")
        unlink = time.monotonic() - t
        d.rmdir()
        return {
            "files": n,
            "create_s": create,
            "stat_s": stat,
            "unlink_s": unlink,
            "ops_per_s": n / (create + stat + unlink) if create + stat + unlink else 0.0,
        }


SETUP = {
    "g1": ("ensure_corpus", "warm_target"),
    "g2": ("ensure_corpus", "warm_target"),
    "g3": ("ensure_corpus", "ensure_tree"),
    "g4": ("ensure_corpus", "ensure_tree"),
    "g5": (),
    "g6": (),
}


def meta(root: Path, label: str, reps: int, gates: list, n: dict) -> dict:
    return {
        "kind": "meta",
        "label": label,
        "root": str(root),
        "reps": reps,
        "gates": gates,
        "counts": n,
        "scale": float(os.environ.get("COWFS_BENCH_SCALE", "100")),
        "corpus_sha": os.environ.get("COWFS_BENCH_CORPUS_SHA", DEFAULT_SHA),
        "cargo_home": ctx_env_cargo_home(),
        "cargo_jobs": os.environ.get("COWFS_BENCH_CARGO_JOBS", "4"),
        "host": platform.node(),
        "platform": platform.platform(),
        "python": platform.python_version(),
        "started": time.time(),
    }


def ctx_env_cargo_home() -> str:
    return os.environ.get("COWFS_BENCH_CARGO_HOME", str(OUT / "cargo-home"))


def read_jsonl(path: Path) -> list:
    lines = []
    if not path.exists():
        return lines
    for line in path.read_text().splitlines():
        line = line.strip()
        if line:
            lines.append(json.loads(line))
    return lines


def open_out(root: Path, label: str, want: dict, resume: bool) -> Path:
    OUT.mkdir(parents=True, exist_ok=True)
    if resume:
        for path in sorted(OUT.glob(f"{label}-*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True):
            rows = read_jsonl(path)
            if not rows or rows[0].get("kind") != "meta":
                continue
            old = rows[0]
            same = all(old.get(k) == want.get(k) for k in ("root", "counts", "corpus_sha", "gates"))
            if same:
                return path
    stamp = time.strftime("%Y%m%d-%H%M%S")
    path = OUT / f"{label}-{stamp}.jsonl"
    with open(path, "a") as fh:
        fh.write(json.dumps(want) + "\n")
        fh.flush()
        os.fsync(fh.fileno())
    return path


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--root", required=True)
    ap.add_argument("--label", required=True)
    ap.add_argument("--reps", type=int, required=True)
    ap.add_argument("--gates", default=",".join(GATES))
    ap.add_argument("--no-resume", action="store_true")
    args = ap.parse_args()

    gates = [g for g in args.gates.split(",") if g]
    for g in gates:
        if g not in GATES:
            raise SystemExit(f"unknown gate {g}")
    root = Path(args.root).resolve()
    n = counts()
    want = meta(root, args.label, args.reps, gates, n)
    path = open_out(root, args.label, want, not args.no_resume)

    done = set()
    for row in read_jsonl(path):
        if row.get("kind") == "rep":
            done.add((row["gate"], row["rep"]))

    ctx = Ctx(root, n)
    for gate in GATES:
        if gate not in gates:
            continue
        for step in SETUP[gate]:
            getattr(ctx, step)()
        for rep in range(args.reps):
            if (gate, rep) in done:
                continue
            before = load1()
            t0 = time.monotonic()
            metrics = getattr(ctx, gate)()
            wall = time.monotonic() - t0
            after = load1()
            row = {
                "kind": "rep",
                "label": args.label,
                "root": str(root),
                "gate": gate,
                "rep": rep,
                "wall_s": round(wall, 6),
                "load1_before": round(before, 3),
                "load1_after": round(after, 3),
                "metrics": metrics,
                "ts": time.time(),
            }
            with open(path, "a") as fh:
                fh.write(json.dumps(row) + "\n")
                fh.flush()
                os.fsync(fh.fileno())
            print(f"{args.label} {gate} rep {rep} {wall:.3f}s load {before:.1f}->{after:.1f}", flush=True)
    print(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
