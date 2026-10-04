#!/usr/bin/env python3
"""Ready-task #21: Git index and pack-index integrity over a private Core NFS mount.

Answers one question with evidence instead of inference: on the *real* cowfs Core backend served
over the macOS NFS loopback, does a real `git` workload lose or corrupt anything that the path
backend spike reported as an unexplained corrupt `.idx` (issue #21, bullet 2)?

Layout, both arms, same seed, same declared op sequence, same git:

    N  native:   a real git repo on APFS, cloned from the native seed
    M  mount:    the same repo cloned from the imported snapshot, `.git` and worktree both on a
                 private cowfs Core NFS mount

Every step is one subprocess with its exit code read from `subprocess.returncode` directly, never
from a pipeline. Every step appends and flushes one JSONL record before the next starts, so an
interrupted run keeps everything up to the last completed step.

Checks are semantic and use real git, not byte comparisons alone:

  * `git fsck --full --no-progress` exit code and output
  * `git show-index < a.idx` exit code (idx magic + trailing SHA-1 trailer)
  * `git verify-pack -v a.idx` exit code
  * `git log --format=%H` count, and HEAD / HEAD^{tree} identical across arms
  * `git status --porcelain=v1` exit code and content
  * long-zero-run structure of every `.pack` and `.idx` (the shape the spike observed)
  * sha256 of every tracked worktree file against the native seed
  * a store reopen: private daemon stopped, a second daemon on the same store, `cowfs fsck`, and
    the full git check set again through the fresh mount
  * the readdir scan-and-unlink probe, reported for the #19 slot-1 owner, never patched here

Isolation: private store, private mount, private socket in a short mode-0700 directory. The shared
daemon and its mount are snapshotted before and after and never addressed. No signal is sent to any
pid whose command line does not carry this run's exact store and socket.

usage:
    verify-git-index-integrity.py [--ops N] [--out DIR] [--keep-daemon]

`--ops` is the declared operation-window size and is clamped to the 30..60 band the task allows.
This is a bounded window, not a soak.

Exits 0 when every semantic check passed on both arms, 1 otherwise.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import random
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

OPS_MIN, OPS_MAX = 30, 60
ZERO_RUN_MIN = 4096
SHARED_DAEMON_PID = 15263

GIT_ENV = {
    "GIT_AUTHOR_NAME": "ready21",
    "GIT_AUTHOR_EMAIL": "ready21@invalid",
    "GIT_COMMITTER_NAME": "ready21",
    "GIT_COMMITTER_EMAIL": "ready21@invalid",
    "GIT_AUTHOR_DATE": "2020-01-01T00:00:00 +0000",
    "GIT_COMMITTER_DATE": "2020-01-01T00:00:00 +0000",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_TERMINAL_PROMPT": "0",
    "LC_ALL": "C",
    "TZ": "UTC",
}


# ---------------------------------------------------------------- log / run


class Run:
    """Append-and-flush JSONL, so an interruption keeps every completed record."""

    def __init__(self, path: Path):
        self.path = path
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.fh = self.path.open("a", buffering=1)
        self.seq = 0

    def rec(self, kind: str, **fields) -> dict:
        self.seq += 1
        r = {"seq": self.seq, "t": time.time(), "kind": kind}
        r.update(fields)
        self.fh.write(json.dumps(r, sort_keys=True, default=str) + "\n")
        self.fh.flush()
        os.fsync(self.fh.fileno())
        return r

    def close(self) -> None:
        self.fh.close()


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with p.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ---------------------------------------------------------------- process


def proc(argv, cwd=None, env=None, timeout=900) -> dict:
    """One subprocess, one real exit code, no shell, no pipeline."""
    e = dict(os.environ)
    e.update(GIT_ENV)
    if env:
        e.update(env)
    t0 = time.monotonic()
    try:
        p = subprocess.run(
            argv, cwd=cwd, env=e, capture_output=True, timeout=timeout
        )
    except subprocess.TimeoutExpired as exc:
        return {
            "argv": list(argv),
            "rc": None,
            "timed_out": True,
            "stdout": (exc.stdout or b"").decode("utf8", "replace")[-4000:],
            "stderr": (exc.stderr or b"").decode("utf8", "replace")[-4000:],
            "secs": round(time.monotonic() - t0, 3),
        }
    return {
        "argv": list(argv),
        "rc": p.returncode,
        "timed_out": False,
        "stdout": p.stdout.decode("utf8", "replace")[-8000:],
        "stderr": p.stderr.decode("utf8", "replace")[-8000:],
        "secs": round(time.monotonic() - t0, 3),
    }


def mount_line_for(mountpoint: Path) -> str | None:
    mp = str(mountpoint)
    out = subprocess.run(["mount"], capture_output=True).stdout.decode("utf8", "replace")
    for line in out.splitlines():
        if f" on {mp} " in line:
            return line
    return None


def pid_argv(pid: int) -> str | None:
    out = subprocess.run(
        ["ps", "-o", "command=", "-p", str(pid)], capture_output=True
    ).stdout.decode("utf8", "replace")
    return out.strip() or None


def pid_lstart(pid: int) -> str | None:
    out = subprocess.run(
        ["ps", "-o", "lstart=", "-p", str(pid)], capture_output=True
    ).stdout.decode("utf8", "replace")
    return out.strip() or None


# ---------------------------------------------------------------- content


def make_seed(repo: Path) -> dict:
    """A real native git repo with real history, a real pack and a real idx.

    Content is deterministic, so both arms produce identical object ids and identical packs, which
    turns "are the packs the same" into a byte comparison with a known-good native answer.
    """
    repo.mkdir(parents=True)
    run = proc(["git", "init", "-q", "-b", "main"], cwd=repo)
    assert run["rc"] == 0, run
    for kv in (
        ("gc.auto", "0"),
        ("core.autocrlf", "false"),
        ("core.fsync", "committed"),
        ("pack.threads", "1"),
        ("commit.gpgsign", "false"),
        ("core.logAllRefUpdates", "true"),
    ):
        r = proc(["git", "config", kv[0], kv[1]], cwd=repo)
        assert r["rc"] == 0, r

    (repo / "README.md").write_text("# ready-21 seed\n")
    (repo / "src").mkdir()
    (repo / "src" / "main.txt").write_text("".join(f"line {i}\n" for i in range(4000)))
    (repo / "docs").mkdir()
    (repo / "docs" / "a.md").write_text("alpha\n" * 500)
    # Incompressible blobs: real packs then carry real entropy, so a zeroed region cannot hide
    # inside plausible compressed output.
    rng = random.Random(20261004)
    (repo / "blobs").mkdir()
    for name, n in (("r1.bin", 512 * 1024), ("r2.bin", 256 * 1024)):
        (repo / "blobs" / name).write_bytes(rng.randbytes(n))
    (repo / "sparse.bin").write_bytes(b"\0" * (64 * 1024) + rng.randbytes(64 * 1024))

    assert proc(["git", "add", "-A"], cwd=repo)["rc"] == 0
    assert proc(["git", "commit", "-q", "-m", "seed"], cwd=repo)["rc"] == 0
    assert proc(["git", "gc", "-q", "--aggressive"], cwd=repo)["rc"] == 0
    assert proc(["git", "fsck", "--full", "--no-progress"], cwd=repo)["rc"] == 0

    head = proc(["git", "rev-parse", "HEAD"], cwd=repo)["rc"]
    head_sha = proc(["git", "rev-parse", "HEAD"], cwd=repo)["stdout"].strip()
    files = {}
    for rel in tracked_files(repo):
        files[rel] = sha256_file(repo / rel)
    return {
        "head": head_sha,
        "tree": proc(["git", "rev-parse", "HEAD^{tree}"], cwd=repo)["stdout"].strip(),
        "tracked": files,
        "init_rc": head,
    }


def tracked_files(repo: Path) -> list[str]:
    r = proc(["git", "ls-files", "-z"], cwd=repo)
    assert r["rc"] == 0, r
    return [x for x in r["stdout"].split("\0") if x]


# ---------------------------------------------------------------- windows


class OpWindow:
    """The declared operation window. `ops` is bounded to the 30..60 band."""

    def __init__(self, size: int):
        self.ops = build_ops()[:size]
        assert OPS_MIN <= len(self.ops) <= OPS_MAX, len(self.ops)

    def names(self) -> list[str]:
        return [o["name"] for o in self.ops]


def build_ops() -> list[dict]:
    """Every entry is exactly one subprocess, or one deterministic fixture write.

    The git invocations are the ones that exercise the git index and the pack index: index writes,
    index refreshes, repack, gc, fsck, and a worktree add/remove pair that drives the readdir
    cookie path with a real git command.
    """
    return [
        {"name": "status.clean", "git": ["status", "--porcelain=v1"]},
        {"name": "write.text", "write": ("text", "README.md", "ready-21 edit 1\n")},
        {"name": "add", "git": ["add", "-A"]},
        {"name": "status.staged", "git": ["status", "--porcelain=v1"]},
        {"name": "commit.op1", "git": ["commit", "-q", "-m", "op1"]},
        {"name": "log", "git": ["log", "--format=%H"]},
        {"name": "write.text", "write": ("text", "docs/a.md", "beta\n" * 900)},
        {"name": "add", "git": ["add", "-A"]},
        {"name": "commit.op2", "git": ["commit", "-q", "-m", "op2"]},
        {"name": "fsck", "git": ["fsck", "--full", "--no-progress"]},
        {"name": "gc", "git": ["gc", "-q"]},
        {"name": "idx.check", "check": "show-index"},
        {"name": "pack.verify", "check": "verify-pack"},
        {"name": "write.text", "write": ("text", "src/main.txt", "changed\n" * 3000)},
        # An unstaged edit makes `--refresh` report "needs update" and exit 1 on a correct
        # filesystem, so this op declares 1 as its expected exit code. Demanding 0 was a harness
        # bug, not a cowfs bug: native returned 1 as well.
        {"name": "index.refresh.dirty", "git": ["update-index", "--refresh"], "expect": [0, 1]},
        {"name": "diff.stat", "git": ["diff", "--stat"]},
        {"name": "add", "git": ["add", "-A"]},
        {"name": "index.refresh.clean", "git": ["update-index", "--refresh"], "expect": [0]},
        {"name": "commit.am", "git": ["commit", "-q", "-m", "op3"]},
        {"name": "repack.adf", "git": ["repack", "-adf"]},
        {"name": "idx.check", "check": "show-index"},
        {"name": "fsck", "git": ["fsck", "--full", "--no-progress"]},
        {"name": "worktree.add", "git": ["worktree", "add", "-q", "../wt1", "-b", "wtbranch"]},
        {"name": "worktree.remove", "git": ["worktree", "remove", "../wt1"],
         "needs": "worktree.add"},
        {"name": "write.text", "write": ("text", "docs/b.md", "gamma\n" * 1200)},
        {"name": "checkout.branch", "git": ["checkout", "-q", "-b", "topic"]},
        {"name": "add", "git": ["add", "-A"]},
        {"name": "commit.topic", "git": ["commit", "-q", "-m", "op4-topic"]},
        {"name": "checkout.main", "git": ["checkout", "-q", "-"]},
        {"name": "merge.topic", "git": ["merge", "--no-edit", "-q", "topic"]},
        {"name": "log.oneline", "git": ["log", "--oneline", "-n", "5"]},
        {"name": "repack.adfl", "git": ["repack", "-adfl"]},
        {"name": "idx.check", "check": "show-index"},
        {"name": "fsck", "git": ["fsck", "--full", "--no-progress"]},
        # `stash pop` needs a stash entry. The written path is untracked, and `stash push` ignores
        # untracked files unless told not to, so `-u` is required; without it the push exits 0
        # having stashed nothing and the pop then exits 128. Both arms agreed on 128, so this was a
        # window bug, not a cowfs bug.
        {"name": "write.text", "write": ("text", "docs/stash-me.md", "stash\n" * 700)},
        {"name": "stash.push", "git": ["stash", "push", "-q", "-u"], "expect": [0]},
        {"name": "stash.pop", "git": ["stash", "pop", "-q"], "expect": [0]},
        {"name": "status.after-pop", "git": ["status", "--porcelain=v1"], "expect": [0]},
        {"name": "reflog.expire", "git": [
            "reflog", "expire", "--all", "--expire=now", "--expire-unreachable=now"]},
        {"name": "gc.aggressive", "git": ["gc", "-q", "--aggressive", "--prune=now"]},
        {"name": "idx.check", "check": "show-index"},
        {"name": "pack.verify", "check": "verify-pack"},
        {"name": "fsck", "git": ["fsck", "--full", "--no-progress"]},
        {"name": "count-objects", "git": ["count-objects", "-v"]},
    ]


def pack_dir(repo: Path) -> Path:
    return repo / ".git" / "objects" / "pack"


def idx_files(repo: Path) -> list[Path]:
    d = pack_dir(repo)
    return sorted(d.glob("*.idx")) if d.is_dir() else []


def run_idx_check(repo: Path, kind: str) -> list[dict]:
    """`git show-index` validates the idx magic and its own trailing SHA-1; verify-pack reads
    every object the idx points at. Both are real git, both return real exit codes."""
    out = []
    for idx in idx_files(repo):
        if kind == "show-index":
            with idx.open("rb") as fh:
                p = subprocess.run(
                    ["git", "show-index"],
                    stdin=fh,
                    cwd=repo,
                    env={**os.environ, **GIT_ENV},
                    capture_output=True,
                    timeout=900,
                )
            r = {
                "argv": ["git", "show-index", "<", str(idx.relative_to(repo))],
                "rc": p.returncode,
                "stdout_lines": len(p.stdout.splitlines()),
                "stderr": p.stderr.decode("utf8", "replace")[-2000:],
            }
        else:
            r = proc(["git", "verify-pack", "-v", str(idx)], cwd=repo)
            r["argv"] = ["git", "verify-pack", "-v", str(idx.relative_to(repo))]
        r["idx"] = idx.name
        r["idx_sha256"] = sha256_file(idx)
        r["idx_bytes"] = idx.stat().st_size
        out.append(r)
    return out


def zero_runs(p: Path) -> dict:
    """Structure of long zero runs, not a verdict. The spike saw 101 zeroed runs in an idx."""
    data = p.read_bytes()
    runs = []
    for m in re.finditer(b"\x00{16,}", data):
        runs.append((m.start(), len(m.group(0))))
    long_runs = [(o, n) for o, n in runs if n >= ZERO_RUN_MIN]
    return {
        "file": p.name,
        "bytes": len(data),
        "sha256": sha256_bytes(data),
        "total_zero_bytes": len(data) - len(data.replace(b"\x00", b"")),
        "runs_ge16": len(runs),
        "runs_ge4096": len(long_runs),
        "longest_run": max([n for _, n in runs], default=0),
        "first_long_runs": [{"offset": o, "length": n} for o, n in long_runs[:8]],
    }


def pack_zero_report(repo: Path) -> list[dict]:
    d = pack_dir(repo)
    if not d.is_dir():
        return []
    return [zero_runs(p) for p in sorted(d.iterdir()) if p.is_file()]


# ---------------------------------------------------------------- arms


def clone_arm(source: Path, dest: Path, label: str) -> dict:
    """A real `git clone`, `.git` and worktree both inside `dest`."""
    r = proc(["git", "clone", "-q", str(source), str(dest)])
    if r["rc"] != 0:
        return {"arm": label, "ok": False, "clone": r}
    for kv in (
        ("gc.auto", "0"),
        ("core.autocrlf", "false"),
        ("core.fsync", "committed"),
        ("pack.threads", "1"),
        ("commit.gpgsign", "false"),
        ("core.logAllRefUpdates", "true"),
    ):
        proc(["git", "config", kv[0], kv[1]], cwd=dest)
    head = proc(["git", "rev-parse", "HEAD"], cwd=dest)
    return {
        "arm": label,
        "ok": True,
        "clone_rc": r["rc"],
        "head": head["stdout"].strip(),
        "clone": r,
    }


def do_write(repo: Path, spec) -> dict:
    kind, rel, payload = spec
    p = repo / rel
    if kind == "text":
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(payload)
        return {"wrote": rel, "bytes": len(payload.encode()), "sha256": sha256_file(p)}
    raise ValueError(kind)


def run_window(win: OpWindow, repo: Path, label: str, run: Run) -> dict:
    """The declared window, one subprocess per git op, append-and-flush per op."""
    executed, skipped = 0, 0
    results = []
    failed: set[str] = set()
    for i, op in enumerate(win.ops, 1):
        need = op.get("needs")
        if need and need in failed:
            skipped += 1
            results.append({"i": i, "name": op["name"], "rc": None,
                            "skipped_because": f"{need} failed"})
            run.rec("op", arm=label, i=i, name=op["name"], rc=None, skipped=True,
                    skipped_because=f"{need} failed")
            continue
        if "write" in op:
            try:
                r = do_write(repo, op["write"])
                rc = 0
            except Exception as exc:  # a fixture write failure is an op failure, not a crash
                r = {"error": repr(exc)}
                rc = None
        elif "check" in op:
            r = run_idx_check(repo, op["check"])
            rc = 0 if r and all(x["rc"] == 0 for x in r) else (None if not r else 1)
        else:
            r = proc(["git"] + op["git"], cwd=repo)
            rc = r["rc"]
        expect = op.get("expect", [0])
        within = rc in expect
        if not within:
            failed.add(op["name"])
        executed += 1
        results.append({"i": i, "name": op["name"], "rc": rc, "expect": expect,
                        "within_expectation": within})
        run.rec(
            "op",
            arm=label,
            i=i,
            name=op["name"],
            argv=(r if isinstance(r, list) else r).get("argv")
            if isinstance(r, dict)
            else ["git"] + op.get("git", []),
            rc=rc,
            detail=_trim(r),
        )
    return {"arm": label, "declared": len(win.ops), "executed": executed,
            "skipped": skipped, "failed_ops": sorted(failed), "results": results}


def _trim(r) -> object:
    if isinstance(r, list):
        return [{k: v for k, v in x.items() if k in ("idx", "rc", "stdout_lines", "stderr")}
                for x in r]
    if isinstance(r, dict):
        return {k: v for k, v in r.items() if k != "stdout"}
    return r


WINDOW_WRITES = ("README.md", "docs/a.md", "src/main.txt", "docs/b.md", "docs/stash-me.md")


def worktree_map(repo: Path) -> tuple[dict, list]:
    """sha256 of every tracked worktree file, and the tracked paths that are absent."""
    out, missing = {}, []
    for rel in tracked_files(repo):
        p = repo / rel
        if p.is_file():
            out[rel] = sha256_file(p)
        else:
            missing.append(rel)
    return out, missing


def semantic_checks(repo: Path, label: str, seed: dict) -> dict:
    """Real git, real exit codes, expected history and counts, and source-hash equality.

    `seed` supplies the expected hashes. Paths the window deliberately rewrites are compared
    against the matched native arm instead, so a rewritten file is never counted as corruption.
    """
    out = {"arm": label, "repo": str(repo), "checks": {}}

    def add(name, r, expect_rc=0, extra=None):
        out["checks"][name] = {
            "rc": r["rc"],
            "expect_rc": expect_rc,
            "pass": r["rc"] == expect_rc,
            "stdout": r["stdout"][-3000:],
            "stderr": r["stderr"][-2000:],
            **(extra or {}),
        }

    add("status.clean", proc(["git", "status", "--porcelain=v1"], cwd=repo))
    add("fsck.full", proc(["git", "fsck", "--full", "--no-progress"], cwd=repo))
    add("fsck.strict", proc(["git", "fsck", "--strict", "--no-progress"], cwd=repo))

    log = proc(["git", "log", "--format=%H"], cwd=repo)
    commits = [x for x in log["stdout"].splitlines() if re.fullmatch(r"[0-9a-f]{40}", x)]
    out["checks"]["log"] = {"rc": log["rc"], "pass": log["rc"] == 0, "commit_count": len(commits)}
    out["commits"] = commits

    head = proc(["git", "rev-parse", "HEAD"], cwd=repo)["stdout"].strip()
    tree = proc(["git", "rev-parse", "HEAD^{tree}"], cwd=repo)["stdout"].strip()
    out["head"], out["tree"] = head, tree
    out["checks"]["head"] = {"pass": True, "head": head, "tree": tree}
    out["checks"]["head_matches_seed_ancestry"] = {
        "pass": seed["head"] in
        proc(["git", "rev-list", "--all"], cwd=repo)["stdout"].split(),
        "seed_head": seed["head"],
    }

    for kind in ("show-index", "verify-pack"):
        rows = run_idx_check(repo, kind)
        out["checks"][f"idx.{kind}"] = {
            "count": len(rows),
            "pass": bool(rows) and all(x["rc"] == 0 for x in rows),
            "rows": [{k: v for k, v in x.items() if k != "stdout"} for x in rows],
        }
    out["pack_zero_report"] = pack_zero_report(repo)
    out["idx_by_name"] = {p.name: sha256_file(p) for p in idx_files(repo)}

    # Untouched tracked files must still equal the native seed byte for byte. Paths the window
    # rewrites are excluded here and compared arm-against-arm instead, so a planned edit is never
    # mistaken for corruption.
    mismatches, missing, checked = [], [], 0
    for rel, want in seed["tracked"].items():
        if rel in WINDOW_WRITES:
            continue
        p = repo / rel
        if not p.is_file():
            missing.append(rel)
            continue
        checked += 1
        got = sha256_file(p)
        if got != want:
            mismatches.append({"path": rel, "want": want, "got": got,
                               "bytes": p.stat().st_size})
    out["source_hash"] = {
        "checked": checked,
        "compared_against": "native seed (window-rewritten paths excluded)",
        "excluded_paths": list(WINDOW_WRITES),
        "mismatches": mismatches,
        "missing": missing,
        "pass": not mismatches and not missing,
    }
    wt, wt_missing = worktree_map(repo)
    out["worktree_map"] = wt
    out["worktree_missing"] = wt_missing

    add("count-objects", proc(["git", "count-objects", "-v"], cwd=repo))
    add("rev-list.all", proc(["git", "rev-list", "--all", "--objects"], cwd=repo))
    add("worktree.list", proc(["git", "worktree", "list"], cwd=repo))
    out["pass"] = all(
        c["pass"] for c in out["checks"].values() if isinstance(c, dict) and "pass" in c
    ) and out["source_hash"]["pass"]
    return out


# ---------------------------------------------------------------- daemon


class PrivateDaemon:
    """A private cowfs-daemon on a private store, mount and socket.

    Every signal is gated on this run's own store and socket appearing in the target's command
    line, so a pid collision with the shared daemon cannot become a signal.
    """

    def __init__(self, binaries: Path, root: Path, run: Run, tag: str):
        self.bin = binaries / "cowfs-daemon"
        self.cli = binaries / "cowfs"
        self.store = root / f"store-{tag}"
        self.mount = root / f"mnt-{tag}"
        self.sock_dir = Path(f"/private/tmp/cowfs-r21-{tag}-{os.getpid()}")
        self.sock = self.sock_dir / "d.sock"
        self.log = root / f"daemon-{tag}.log"
        self.pid: int | None = None
        self.run = run
        self.root = root

    def start(self) -> dict:
        self.store.mkdir(parents=True, exist_ok=True)
        self.mount.mkdir(parents=True, exist_ok=True)
        if self.sock_dir.exists():
            shutil.rmtree(self.sock_dir)
        self.sock_dir.mkdir(mode=0o700)
        self.log.parent.mkdir(parents=True, exist_ok=True)
        fh = self.log.open("a", buffering=1)
        p = subprocess.Popen(
            [str(self.bin), "--store", str(self.store), "--mount", str(self.mount),
             "--socket", str(self.sock), "--backend", "core"],
            stdout=fh, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
            start_new_session=True,
        )
        self.pid = p.pid
        self.run.rec("daemon.start", pid=self.pid, store=str(self.store),
                     mount=str(self.mount), socket=str(self.sock), log=str(self.log))
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            if p.poll() is not None:
                return {"ok": False, "why": "daemon exited", "rc": p.returncode,
                        "log": self.log.read_text()[-4000:]}
            line = mount_line_for(self.mount)
            if line:
                return {"ok": True, "pid": self.pid, "mount_line": line,
                        "socket_exists": self.sock.exists()}
            time.sleep(0.5)
        return {"ok": False, "why": "mount never appeared", "log": self.log.read_text()[-4000:]}

    def owned(self, pid: int) -> bool:
        argv = pid_argv(pid) or ""
        return str(self.store) in argv and str(self.sock) in argv

    def ctl(self, *args, timeout=1800) -> dict:
        return proc([str(self.cli), "--socket", str(self.sock), "--json", *args],
                    timeout=timeout)

    def stop(self) -> dict:
        if self.pid is None:
            return {"stopped": False, "why": "never started"}
        argv = pid_argv(self.pid)
        if argv is None:
            # Already reaped. A second stop after an explicit one is normal, not a refusal.
            return {"stopped": True, "pid": self.pid, "why": "already gone",
                    "mount_line_after": mount_line_for(self.mount)}
        if not self.owned(self.pid):
            return {"stopped": False, "why": "pid is not this run's daemon", "argv": argv}
        try:
            os.kill(self.pid, signal.SIGTERM)
        except ProcessLookupError:
            return {"stopped": True, "why": "already gone"}
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if not self.owned(self.pid):
                break
            time.sleep(0.25)
        still = mount_line_for(self.mount)
        return {"stopped": not still, "pid": self.pid, "mount_line_after": still,
                "log": self.log.read_text()[-2000:]}


def shared_snapshot() -> dict:
    return {
        "pid": SHARED_DAEMON_PID,
        "alive": Path(f"/proc/{SHARED_DAEMON_PID}").exists() or bool(pid_argv(SHARED_DAEMON_PID)),
        "argv": pid_argv(SHARED_DAEMON_PID),
        "lstart": pid_lstart(SHARED_DAEMON_PID),
        "mount": mount_line_for(Path("/Users/zeeshanhaque/.cowfs/mnt")),
    }


# ---------------------------------------------------------------- cookie


def _getdents_scan(path: Path, buf_size: int, unlink_inside: bool) -> dict:
    """Drive `__getdirentries64` directly so the page size and the unlink are under harness control.

    `os.scandir` picks its own buffer, so a directory that fits one libc buffer never makes the
    kernel resume a scan, and then the NFS cookie is never exercised at all. A small buffer plus an
    unlink of the last name of each page forces a resume from a cookie across a directory whose
    contents just changed: exactly the shape issue #21 bullet 3 describes.

    The record layout is `__DARWIN_STRUCT_DIRENTRY` from the SDK's `sys/dirent.h`, which is what
    `__DARWIN_64_BIT_INO_T` selects on arm64: `d_ino u64 @0, d_seekoff u64 @8, d_reclen u16 @16,
    d_namlen u16 @18, d_type u8 @20, d_name char[1024] @21`.

    Returns the page count, the names seen and the unlinks, so a caller can prove the scan really
    paged instead of inferring it from a zero.
    """
    import ctypes

    libc = ctypes.CDLL("libc.dylib", use_errno=True)
    libc.__getdirentries64.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_int]
    libc.__getdirentries64.restype = ctypes.c_int
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        buf = ctypes.create_string_buffer(buf_size)
        pages = names = unlinked = 0
        while True:
            n = libc.__getdirentries64(fd, ctypes.byref(buf), buf_size)
            if n < 0:
                err = ctypes.get_errno()
                raise OSError(err, os.strerror(err), str(path))
            if n == 0:
                break
            pages += 1
            pos = 0
            while pos < n:
                reclen = int.from_bytes(buf.raw[pos + 16:pos + 18], "little")
                namlen = int.from_bytes(buf.raw[pos + 18:pos + 20], "little")
                if reclen < 21 or namlen > reclen - 21:
                    raise ValueError(
                        f"bad getdirentries64 record reclen={reclen} namlen={namlen} in {path}")
                name = buf.raw[pos + 21:pos + 21 + namlen]
                pos += reclen
                if name in (b".", b".."):
                    continue
                names += 1
                # Unlink every name, not just the last of the page: the whole directory shifts under
                # the scan, so a cookie that is a live position rather than a stable handle skips.
                if unlink_inside:
                    try:
                        os.unlink(os.path.join(os.fsencode(path), name))
                        unlinked += 1
                    except FileNotFoundError:
                        pass
        return {"pages": pages, "names": names, "unlinked": unlinked, "buf_size": buf_size}
    finally:
        os.close(fd)


def cookie_probe_at(native_dir: Path, mount_dir: Path, entries: int, buf_size: int) -> dict:
    """Fill both directories with the same names, then run the identical paged scan-and-unlink."""
    out = {}
    for label, d in (("native", native_dir), ("mount", mount_dir)):
        if d.exists():
            shutil.rmtree(d)
        d.mkdir(parents=True)
        # Creation order is deliberately not byte order, so a cookie that is really a slot index
        # gets its slots reused the moment a name is unlinked.
        for i in range(entries):
            (d / f"f{(i * 7919) % entries:05d}").write_text(str(i))
        r = _getdents_scan(d, buf_size, unlink_inside=True)
        remaining = sorted(p.name for p in d.iterdir())
        r["created"] = entries
        r["remaining"] = len(remaining)
        r["remaining_sample"] = remaining[:10]
        out[label] = r
    out["entries"] = entries
    out["diverges"] = out["mount"]["remaining"] != out["native"]["remaining"]
    out["mount_left_native_left"] = [out["mount"]["remaining"], out["native"]["remaining"]]
    return out


def cookie_sweep(native_base: Path, mount_base: Path, entries: int,
                 buffers=(512, 1024, 4096, 16384)) -> dict:
    """Sweep buffer sizes and entry counts.

    The NFS adapter serves `READDIR_PAGE = 512` entries per READDIR and the client resumes with the
    cookie of the last entry it saw, so a single entry count is not a verdict: this probes a grid
    and reports the first cell where the two arms diverge. `pages > 1` is the proof that the client
    really resumed from a cookie rather than draining one libc buffer.
    """
    rows = []
    for buf in buffers:
        for n in (64, 256, 1024, 2500, entries):
            if n > entries or any(r["buf_size"] == buf and r["entries"] == n for r in rows):
                continue
            r = cookie_probe_at(native_base / "cd", mount_base / "cd", n, buf)
            rows.append({"buf_size": buf, "entries": n,
                         "native_pages": r["native"]["pages"],
                         "mount_pages": r["mount"]["pages"],
                         "native_remaining": r["native"]["remaining"],
                         "mount_remaining": r["mount"]["remaining"],
                         "diverges": r["diverges"]})
    return {"grid": rows, "any_divergence": any(r["diverges"] for r in rows),
            "first_diverging": next((r for r in rows if r["diverges"]), None),
            "note": "pages>1 means the client resumed from a cookie, not one libc buffer"}


def readdir_cookie_probe(native_base: Path, mount_base: Path, entries: int = 2500) -> dict:
    """Reproduce issue #21 bullet 3 for the #19 slot-1 owner.

    The shape that failed in the spike is an *incremental* scan-and-unlink: unlink each entry while
    the directory iterator is still open, so the reader must resume from a cookie across a directory
    whose contents keep shifting. `os.listdir` cannot express that, because it drains the whole
    directory into memory before the caller sees the first name, so nothing is ever unlinked mid
    scan. `os.scandir` is lazy and does express it.

    A cookie that is a bare position skips every entry whose slot moved after the unlink, so files
    are left behind. The same loop on APFS leaves nothing.
    """
    out = {"dirs": {}, "method": "os.scandir lazy iterator, unlink inside the loop",
           "page_hint": "NFS READDIR_PAGE is 512 entries in crates/cowfs-nfs/src/adapter.rs"}
    for label, base in (("native", native_base / "cookie-native"),
                        ("mount", mount_base / "cookie-mount")):
        d = base / "d"
        if d.exists():
            shutil.rmtree(d)
        d.mkdir(parents=True)
        out["dirs"][label] = str(d)
        # Names must not sort in creation order. A positional cookie that is really a byte-offset
        # or an index is only stable while the set is static, and byte-sorted creation order makes
        # an index-based cookie reuse slots the moment a name is removed.
        for i in range(entries):
            (d / f"f{(i * 7919) % entries:05d}").write_text(str(i))
        iterated = unlinked = 0
        with os.scandir(d) as it:
            for entry in it:
                iterated += 1
                try:
                    os.unlink(entry.path)
                    unlinked += 1
                except FileNotFoundError:
                    pass
        remaining = sorted(p.name for p in d.iterdir())
        out[label] = {
            "created": entries,
            "iterated": iterated,
            "unlinked": unlinked,
            "remaining": len(remaining),
            "remaining_sample": remaining[:10],
        }
    # The mount arm must match the native arm. The known-good baseline is native leaving 0, so a
    # nonzero mount delta is the cookie defect, not a property of the loop.
    out["pass"] = out["mount"]["remaining"] == out["native"]["remaining"] == 0
    out["delta_mount_minus_native"] = (
        out["mount"]["remaining"] - out["native"]["remaining"]
    )
    return out


def cookie_sweep_probe(native_base: Path, mount_base: Path, entries: int) -> dict:
    """Ladder of entry counts through the lazy `os.scandir` loop, then the raw `getdents64` grid."""
    ladder = []
    n = 64
    while n <= entries:
        ladder.append(n)
        n *= 4
    if ladder[-1] != entries:
        ladder.append(entries)
    rows = []
    for size in ladder:
        r = readdir_cookie_probe(native_base, mount_base, size)
        rows.append({
            "entries": size,
            "native_remaining": r["native"]["remaining"],
            "mount_remaining": r["mount"]["remaining"],
            "native_iterated": r["native"]["iterated"],
            "mount_iterated": r["mount"]["iterated"],
            "diverges": r["mount"]["remaining"] != r["native"]["remaining"],
        })
    return {
        "method": "os.scandir lazy iterator",
        "ladder": rows,
        "first_diverging_entries": next((r["entries"] for r in rows if r["diverges"]), None),
        "any_divergence": any(r["diverges"] for r in rows),
        "getdents64_grid": cookie_sweep(native_base, mount_base, entries),
    }


# ---------------------------------------------------------------- main


class Leaked:
    """Track this run's private daemons so an exception can never leave one serving.

    A crashed harness that leaks a daemon also leaks an NFS mount, and macOS 26 makes every `ls` on
    an abandoned mount hang for twenty seconds or more. Teardown runs from `finally`, and each stop
    re-verifies that the pid's argv still carries this run's store and socket.
    """

    def __init__(self, run: Run, keep: bool = False):
        self.run = run
        self.keep = keep
        self.daemons: list[PrivateDaemon] = []
        self.done = False

    def add(self, d: PrivateDaemon) -> PrivateDaemon:
        self.daemons.append(d)
        return d

    def sweep(self) -> list[dict]:
        if self.keep:
            live = [{"pid": d.pid, "store": str(d.store), "mount": str(d.mount),
                     "socket": str(d.sock), "note": "KEPT: --keep-daemon, stop it yourself"}
                    for d in self.daemons if d.pid]
            self.done = True
            self.run.rec("daemon.sweep", kept=live)
            return [{"stopped": False, "kept": live}]
        out = []
        for d in reversed(self.daemons):
            out.append(d.stop())
        self.done = True
        self.run.rec("daemon.sweep", results=out)
        return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ops", type=int, default=41)
    ap.add_argument("--out", default=None)
    ap.add_argument("--cookie-entries", type=int, default=2500)
    ap.add_argument("--keep-daemon", action="store_true")
    args = ap.parse_args()

    if not OPS_MIN <= args.ops <= OPS_MAX:
        print(f"--ops must be in {OPS_MIN}..{OPS_MAX}", file=sys.stderr)
        return 2

    here = Path(__file__).resolve().parent.parent
    binaries = here / "bench" / "out" / "ready-21" / "target" / "release"
    for b in ("cowfs-daemon", "cowfs"):
        if not (binaries / b).is_file():
            print(f"missing {binaries / b}; build it first", file=sys.stderr)
            return 2

    stamp = time.strftime("%Y%m%dT%H%M%S")
    root = Path(args.out) if args.out else here / "bench" / "out" / "ready-21"
    attempt = root / f"{stamp}-attempt"
    attempt.mkdir(parents=True, exist_ok=True)
    run = Run(attempt / "log.jsonl")
    summary: dict = {"attempt": str(attempt), "started": time.time()}

    shared_before = shared_snapshot()
    run.rec("preflight", shared_daemon=shared_before,
            git_version=proc(["git", "--version"])["stdout"].strip(),
            python=sys.version.split()[0], platform=platform.platform(),
            binaries={b: sha256_file(binaries / b) for b in ("cowfs-daemon", "cowfs")},
            git_config_list=proc(["git", "config", "--list", "--show-origin"])["stdout"][-4000:])

    win = OpWindow(args.ops)
    summary["declared_window"] = {"size": len(win.ops), "ops": win.names()}
    run.rec("window.declared", size=len(win.ops), ops=win.names())

    # ---- fixture
    seed_dir = attempt / "seed"
    seed = make_seed(seed_dir)
    run.rec("fixture", **{"seed": seed_dir, **seed})
    summary["seed"] = seed

    summary["shared_daemon"] = {"before": shared_before}

    # ---- private daemon + import
    # Everything from here to the end runs under `finally`, so an exception can never leave a
    # private daemon serving and its NFS mount hanging every later `ls` on this machine.
    leak = Leaked(run, keep=args.keep_daemon)
    try:
        return body(args, here, binaries, attempt, run, summary, win, seed, leak)
    finally:
        swept = leak.sweep()
        summary["teardown"] = swept
        try:
            (attempt / "summary.json").write_text(json.dumps(summary, indent=2, default=str))
        except OSError:
            pass
        if not run.fh.closed:
            run.rec("end", swept=swept)
            run.close()


def body(args, here, binaries, attempt, run, summary, win, seed, leak) -> int:
    # ---- private daemon + import
    daemon = leak.add(PrivateDaemon(binaries, attempt, run, "a"))
    started = daemon.start()
    run.rec("daemon.ready", **started)
    summary["daemon"] = started
    if not started.get("ok"):
        summary["verdict"] = "DAEMON DID NOT START"
        summary["shared_daemon_after"] = shared_snapshot()
        print(json.dumps(summary["verdict"]))
        return 1

    seed_dir = attempt / "seed"
    imp = daemon.ctl("import", str(seed_dir), "--name", "seed")
    run.rec("import", rc=imp["rc"], stdout=imp["stdout"][-2000:], stderr=imp["stderr"][-2000:])
    summary["import"] = {"rc": imp["rc"], "stdout": imp["stdout"][-2000:]}
    if imp["rc"] != 0:
        summary["verdict"] = "IMPORT FAILED"
        return 1

    snapshot_dir = daemon.mount / "seed"

    # ---- arms
    arms = {}
    arms["native"] = clone_arm(seed_dir, attempt / "arm-native", "native")
    arms["mount"] = clone_arm(snapshot_dir, snapshot_dir / "work", "mount")
    run.rec("arms.cloned", **{k: {kk: vv for kk, vv in v.items() if kk != "clone"}
                               for k, v in arms.items()})
    summary["arms"] = {k: {kk: vv for kk, vv in v.items() if kk != "clone"}
                       for k, v in arms.items()}
    if not arms["native"]["ok"] or not arms["mount"]["ok"]:
        summary["verdict"] = "CLONE FAILED"
        summary["clone_detail"] = {k: v.get("clone", v) for k, v in arms.items()}
        return 1

    repo = {"native": attempt / "arm-native", "mount": snapshot_dir / "work"}

    # ---- the declared window, both arms
    summary["window"] = {}
    for label in ("native", "mount"):
        w = run_window(win, repo[label], label, run)
        summary["window"][label] = w
        if w["skipped"]:
            print(f"window {label}: {w['skipped']} skipped ops", file=sys.stderr)

    # ---- semantic checks, both arms
    summary["checks"] = {label: semantic_checks(repo[label], label, seed)
                         for label in ("native", "mount")}

    # matched readback: the window rewrote the same paths on both arms, so the two worktrees must
    # agree byte for byte, which covers the rewritten paths the seed comparison skips
    n_wt = summary["checks"]["native"]["worktree_map"]
    m_wt = summary["checks"]["mount"]["worktree_map"]
    wt_cmp = {
        "only_native": sorted(set(n_wt) - set(m_wt)),
        "only_mount": sorted(set(m_wt) - set(n_wt)),
        "compared": [{"path": k, "equal": n_wt[k] == m_wt[k],
                      "native_sha256": n_wt[k], "mount_sha256": m_wt[k]}
                     for k in sorted(set(n_wt) & set(m_wt))],
    }
    wt_cmp["pass"] = (not wt_cmp["only_native"] and not wt_cmp["only_mount"]
                      and bool(wt_cmp["compared"])
                      and all(c["equal"] for c in wt_cmp["compared"]))
    summary["worktree_compare"] = wt_cmp
    run.rec("worktree.compare", **wt_cmp)
    for label in ("native", "mount"):
        run.rec("checks", arm=label,
                **{k: v for k, v in summary["checks"][label].items()
                   if k not in ("repo", "arm", "checks", "pack_zero_report", "idx_by_name",
                                "commits")})
        run.rec("pack_zero_report", arm=label,
                report=summary["checks"][label]["pack_zero_report"])
        run.rec("idx_by_name", arm=label, idx=summary["checks"][label]["idx_by_name"])

    # matched answer: the two arms ran the same ops on the same seed, so identical packs must be
    # byte identical, and the history must match exactly
    n_idx = summary["checks"]["native"]["idx_by_name"]
    m_idx = summary["checks"]["mount"]["idx_by_name"]
    shared = sorted(set(n_idx) & set(m_idx))
    pack_cmp = {
        "native_idx": sorted(n_idx),
        "mount_idx": sorted(m_idx),
        "only_native": sorted(set(n_idx) - set(m_idx)),
        "only_mount": sorted(set(m_idx) - set(n_idx)),
        "compared": [
            {"idx": k, "native_sha256": n_idx[k], "mount_sha256": m_idx[k],
             "equal": n_idx[k] == m_idx[k]}
            for k in shared
        ],
    }
    pack_cmp["pass"] = bool(shared) and all(c["equal"] for c in pack_cmp["compared"]) \
        and not pack_cmp["only_native"] and not pack_cmp["only_mount"]
    run.rec("pack.compare", **pack_cmp)
    summary["pack_compare"] = pack_cmp

    hist_cmp = {
        "native_head": summary["checks"]["native"]["head"],
        "mount_head": summary["checks"]["mount"]["head"],
        "head_equal": summary["checks"]["native"]["head"] == summary["checks"]["mount"]["head"],
        "native_commits": len(summary["checks"]["native"]["commits"]),
        "mount_commits": len(summary["checks"]["mount"]["commits"]),
        "commit_lists_equal":
            summary["checks"]["native"]["commits"] == summary["checks"]["mount"]["commits"],
    }
    hist_cmp["pass"] = hist_cmp["head_equal"] and hist_cmp["commit_lists_equal"]
    run.rec("history.compare", **hist_cmp)
    summary["history_compare"] = hist_cmp

    # ---- readdir cookie probe, reported for the #19 slot-1 owner.
    # Run before the reopen phase, which stops this daemon.
    cookie = readdir_cookie_probe(attempt, snapshot_dir, args.cookie_entries)
    run.rec("readdir.cookie", **cookie)
    summary["readdir_cookie"] = cookie
    cookie_sweep = cookie_sweep_probe(attempt, snapshot_dir, args.cookie_entries)
    run.rec("readdir.cookie_sweep", **cookie_sweep)
    summary["readdir_cookie_sweep"] = cookie_sweep

    # ---- store reopen: stop this daemon, start a second on the same store, re-check
    stopped = daemon.stop()
    run.rec("daemon.stopped", **stopped)
    leak.daemons.remove(daemon)  # already stopped and swept; do not signal it twice
    daemon2 = leak.add(PrivateDaemon(binaries, attempt, run, "b"))
    # same store, new mount and socket, so the data is re-read from the packs rather than reused
    daemon2.store = daemon.store
    started2 = daemon2.start()
    run.rec("daemon.reopen", **started2)
    summary["reopen"] = {"stop": stopped, "start": started2}
    if started2.get("ok"):
        snap2 = daemon2.mount / "seed"
        fsck = daemon2.ctl("fsck")
        reread = {}
        reread["files_present"] = sorted(
            p.name for p in (snap2 / "work").iterdir()
        ) if (snap2 / "work").is_dir() else []
        # The oracle is the mount arm's own post-window state, read back through a fresh daemon on
        # the same store. Comparing against the seed would flag the paths the window rewrote.
        want_map = summary["checks"]["mount"]["worktree_map"]
        hashes, bad = {}, []
        for rel, want in want_map.items():
            p = snap2 / "work" / rel
            if p.is_file():
                got = sha256_file(p)
                hashes[rel] = got
                if got != want:
                    bad.append({"path": rel, "want": want, "got": got})
            else:
                bad.append({"path": rel, "want": want, "got": None})
        reread["source_hash"] = {
            "checked": len(hashes), "bad": bad,
            "compared_against": "mount arm post-window state, re-read after a fresh daemon",
            "pass": not bad and len(hashes) == len(want_map) and bool(want_map),
        }
        reread["cowfs_fsck"] = {"rc": fsck["rc"], "stdout": fsck["stdout"][-3000:]}
        reread["cowfs_fsck_pass"] = fsck["rc"] == 0
        reread["git_fsck"] = None
        if (snap2 / "work" / ".git").is_dir():
            g = proc(["git", "fsck", "--full", "--no-progress"], cwd=snap2 / "work")
            reread["git_fsck"] = {"rc": g["rc"], "stdout": g["stdout"][-3000:],
                                  "pass": g["rc"] == 0}
            for kind in ("show-index", "verify-pack"):
                rows = run_idx_check(snap2 / "work", kind)
                reread[f"idx_{kind}"] = {"count": len(rows),
                                         "pass": bool(rows) and all(x["rc"] == 0 for x in rows),
                                         "idx": [x["idx"] for x in rows],
                                         "sha256": {x["idx"]: x["idx_sha256"] for x in rows}}
            reread["status"] = proc(["git", "status", "--porcelain=v1"], cwd=snap2 / "work")
        reread["pack_zero_report"] = pack_zero_report(snap2 / "work") \
            if (snap2 / "work" / ".git").is_dir() else []
        reread["pass"] = bool(
            reread["cowfs_fsck_pass"]
            and reread["source_hash"]["pass"]
            and (reread.get("git_fsck") or {}).get("pass", False)
            and (reread.get("idx_show-index") or {}).get("pass", False)
            and (reread.get("idx_verify-pack") or {}).get("pass", False)
        )
        run.rec("reopen.checks", **{k: v for k, v in reread.items()
                                    if k != "pack_zero_report"})
        run.rec("reopen.pack_zero_report", report=reread["pack_zero_report"])
        summary["reopen"]["checks"] = reread
        summary["reopen"]["stop"] = daemon2.stop()
    else:
        summary["reopen"]["checks"] = {"pass": False, "why": started2}

    # ---- isolation
    shared_after = shared_snapshot()
    summary["shared_daemon"]["after"] = shared_after
    summary["shared_daemon"]["untouched"] = summary["shared_daemon"]["before"] == shared_after
    run.rec("isolation", **summary["shared_daemon"])

    # ---- verdict
    parts = {
        "window_executed": all(summary["window"][a]["executed"] == len(win.ops)
                               for a in ("native", "mount")),
        "window_no_failed_op": all(not summary["window"][a]["failed_ops"]
                                   for a in ("native", "mount")),
        "checks_native": summary["checks"]["native"]["pass"],
        "checks_mount": summary["checks"]["mount"]["pass"],
        "pack_compare": pack_cmp["pass"],
        "history_compare": hist_cmp["pass"],
        "worktree_compare": wt_cmp["pass"],
        "reopen": summary["reopen"].get("checks", {}).get("pass", False),
        "shared_daemon_untouched": summary["shared_daemon"]["untouched"],
    }
    summary["verdict_parts"] = parts
    integrity_clean = all(v for k, v in parts.items()
                          if k not in ("shared_daemon_untouched",))
    summary["integrity_clean"] = integrity_clean
    summary["verdict"] = (
        "NOT REPRODUCED: no git index or pack-index corruption in the declared window on either arm"
        if integrity_clean else
        "REPRODUCED: at least one integrity check failed, see the failing check and preserved fixture"
    )
    # the cookie probe is deliberately outside the verdict: slot 1 owns it and it is not an
    # index-integrity result
    summary["ended"] = time.time()
    (attempt / "summary.json").write_text(json.dumps(summary, indent=2, default=str))
    run.rec("verdict", verdict=summary["verdict"], **parts)
    if args.keep_daemon:
        print(json.dumps({"kept_daemon": daemon2.pid, "attempt": str(attempt)}))
    print(json.dumps({"verdict": summary["verdict"], "attempt": str(attempt),
                      "declared_ops": len(win.ops), **parts}, indent=2))
    return 0 if integrity_clean else 1


if __name__ == "__main__":
    sys.exit(main())
