#!/usr/bin/env python3
"""Matched pjdfstest acceptance for a private real-Core cowfs mount against a native baseline.

Gate g3 of docs/ready-wave-dispatch.md. One pinned pjdfstest build runs the same test list on
two arms of the same host: a native directory (APFS on macOS, ext4 on Linux) and a mount served
by `cowfs-daemon --backend core`. Nothing inside a test knows which arm it is on.

    bench/pjdfstest.py --out bench/out/ready-g3/run

Every case appends one JSON line and flushes before the next starts, so an interrupted run keeps
the cases that finished. Exit 0 only when the run is complete on both arms and no assertion the
native arm passes fails on the cowfs arm.

The gate needs a tool that is not packaged on the hosts it runs on and an autoconf build that is
not available either, so `config.h` is produced by compiling a probe per feature and the suite is
built with the system compiler. The pinned source SHA, the binary SHA and the detected features
go into the summary: a verdict without them is not reproducible.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

PINNED_COMMIT = "85a8aea9e685999ef0540392fd80535f873d7ff7"
PINNED_URL = "https://github.com/pjd/pjdfstest.git"
CC = os.environ.get("CC", "cc")
SNAPSHOT = "pjd"
CASE_TIMEOUT = 600

# (config.h macro, probe source). A macro is defined only when its probe compiles and links.
HEADERS = {
    "HAVE_SYS_ACL_H": "#include <sys/acl.h>",
    "HAVE_SYS_MKDEV_H": "#include <sys/mkdev.h>",
    "HAVE_SYS_SYSMACROS_H": "#include <sys/sysmacros.h>",
}
BASE_INCLUDES = """#include <sys/types.h>
#include <sys/param.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/mman.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdarg.h>
#include <time.h>
#include <unistd.h>
#include <fcntl.h>
"""
FUNCS = [
    "bindat", "chflags", "chflagsat", "connectat", "faccessat", "fchflags", "fchmodat",
    "fchownat", "fstatat", "lchflags", "lchmod", "linkat", "lpathconf", "mkdirat", "mkfifoat",
    "mknodat", "openat", "posix_fallocate", "readlinkat", "renameat", "symlinkat", "unlinkat",
    "utimensat",
]
ACL_FUNCS = ["acl_create_entry_np", "acl_from_text", "acl_get_entry", "acl_get_file", "acl_set_file"]
MEMBERS = [
    "st_atim", "st_atimespec", "st_birthtim", "st_birthtime", "st_birthtimespec",
    "st_ctim", "st_ctimespec", "st_mtim", "st_mtimespec",
]

PLAN_RE = re.compile(r"^1\.\.(\d+)")
RESULT_RE = re.compile(r"^(ok|not ok)\s+(\d+)(?:\s*#\s*(TODO|skip))?\s*-?\s*(.*)$")
# An assertion that needs privilege this run does not have. Matched on the attempt text the
# suite prints, so it is a property of the pinned suite and not of either filesystem.
ROOT_REQUIRED_RE = re.compile(r"(?:^|\s)-(?:u|g)\s|\bmknod\s|not root")


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def log(msg: str) -> None:
    print(msg, flush=True)


# ---------------------------------------------------------------- tool


def probe(work: Path, name: str, source: str) -> bool:
    c = work / f"probe-{name}.c"
    out = work / f"probe-{name}.bin"
    c.write_text(source)
    try:
        rc = subprocess.run([CC, str(c), "-o", str(out)], capture_output=True, timeout=120).returncode
    except subprocess.TimeoutExpired:
        rc = 1
    c.unlink(missing_ok=True)
    out.unlink(missing_ok=True)
    return rc == 0


def detect_features(work: Path) -> list[str]:
    found = []
    for macro, header in HEADERS.items():
        if probe(work, macro, f"{header}\nint main(void){{return 0;}}\n"):
            found.append(macro)
    for fn in FUNCS:
        if probe(work, fn, f"{BASE_INCLUDES}\nint main(void){{void *p=(void*)&{fn};return p!=0;}}\n"):
            found.append(f"HAVE_{fn.upper()}")
    for fn in ACL_FUNCS:
        src = f"{BASE_INCLUDES}\n#include <sys/acl.h>\nint main(void){{void *p=(void*)&{fn};return p!=0;}}\n"
        if "HAVE_SYS_ACL_H" in found and probe(work, fn, src):
            found.append(f"HAVE_{fn.upper()}")
    if "HAVE_SYS_ACL_H" in found and probe(
        work, "acl_type_nfs4", "#include <sys/acl.h>\nint main(void){return ACL_TYPE_NFS4;}\n"
    ):
        found.append("HAS_NFSV4_ACL_SUPPORT")
    for m in MEMBERS:
        src = f"#include <sys/stat.h>\nint main(void){{struct stat s; s.{m} = s.{m}; return 0;}}\n"
        if probe(work, m, src):
            found.append(f"HAVE_STRUCT_STAT_{m.upper()}")
    return sorted(set(found))


def fetch_tool(root: Path) -> Path:
    """Clone the pinned suite into the ignored output tree, or reuse an exact checkout."""
    src = root / "tool" / "pjdfstest"
    if (src / ".git").is_dir():
        head = subprocess.run(
            ["git", "-C", str(src), "rev-parse", "HEAD"], capture_output=True, text=True
        ).stdout.strip()
        if head == PINNED_COMMIT:
            return src
        log(f"tool at {src} is {head}, not the pinned {PINNED_COMMIT}")
        raise SystemExit(f"refusing to reuse a checkout at {head}; move {src} aside")
    src.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "clone", "--quiet", PINNED_URL, str(src)], check=True)
    subprocess.run(["git", "-C", str(src), "checkout", "--quiet", PINNED_COMMIT], check=True)
    head = subprocess.run(
        ["git", "-C", str(src), "rev-parse", "HEAD"], capture_output=True, text=True
    ).stdout.strip()
    if head != PINNED_COMMIT:
        raise SystemExit(f"checkout is {head}, not the pinned commit")
    return src


def build_tool(src: Path) -> tuple[Path, list[str]]:
    build = src.parent / "build"
    build.mkdir(parents=True, exist_ok=True)
    features = detect_features(build)
    (src / "config.h").write_text(
        "/* Generated by bench/pjdfstest.py, standing in for the autoconf config.h. */\n"
        "#ifndef PJDFSTEST_CONFIG_H\n#define PJDFSTEST_CONFIG_H\n"
        + "".join(f"#define {m} 1\n" for m in features)
        + "#endif\n"
    )
    # Upstream AM_CFLAGS, and the suite's own layout: misc.sh finds ./pjdfstest by walking up.
    binary = src / "pjdfstest"
    subprocess.run(
        [CC, "-O2", "-Wall", "-Werror", "-I", str(src), "-o", str(binary), str(src / "pjdfstest.c")],
        check=True,
    )
    return binary, features


def tool_identity(src: Path, binary: Path, features: list[str]) -> dict:
    return {
        "url": PINNED_URL,
        "commit": PINNED_COMMIT,
        "pjdfstest_c_sha256": sha256(src / "pjdfstest.c"),
        "config_h_sha256": sha256(src / "config.h"),
        "binary_sha256": sha256(binary),
        "compiler": subprocess.run([CC, "--version"], capture_output=True, text=True).stdout.splitlines()[0],
        "features": features,
    }


# ---------------------------------------------------------------- daemon


def ensure_binaries(repo: Path) -> tuple[Path, Path]:
    daemon, cli = repo / "target" / "release" / "cowfs-daemon", repo / "target" / "release" / "cowfs"
    if not daemon.is_file() or not cli.is_file():
        log("building cowfs-daemon and cowfs-cli (release)")
        subprocess.run(
            ["cargo", "build", "--release", "-p", "cowfs-daemon", "-p", "cowfs-cli"],
            cwd=repo,
            check=True,
        )
    for b in (daemon, cli):
        if not b.is_file():
            raise SystemExit(f"{b} is missing after the build")
    return daemon, cli


def mount_listed(mount: Path) -> bool:
    return f" on {mount} " in subprocess.run(["/sbin/mount"], capture_output=True, text=True).stdout


def started(argv_pid_line: str, binary: Path) -> bool:
    """True only when the pid we recorded is still the binary we started."""
    return str(binary) in argv_pid_line


def start_daemon(daemon: Path, store: Path, mount: Path, sock: Path, log_path: Path) -> dict:
    """Start the private daemon and return its identity. Raises unless it serves."""
    for d in (store, mount, sock.parent):
        d.mkdir(parents=True, exist_ok=True)
    sock.parent.chmod(0o700)
    logf = log_path.open("ab")
    argv = [
        str(daemon), "--store", str(store), "--mount", str(mount), "--socket", str(sock),
        "--backend", "core",
    ]
    proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=logf, stderr=logf)
    identity = {
        "pid": proc.pid,
        "argv": argv,
        "start_time": time.strftime("%Y-%m-%dT%H:%M:%S%z", time.localtime()),
        "store": str(store),
        "mount": str(mount),
        "socket": str(sock),
        "log": str(log_path),
        "pop": proc,
    }
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise SystemExit(f"daemon exited {proc.returncode} before serving; see {log_path}")
        if sock.is_socket() and mount_listed(mount):
            identity["mount_table"] = [
                ln for ln in subprocess.run(
                    ["/sbin/mount"], capture_output=True, text=True
                ).stdout.splitlines() if f" on {mount} " in ln
            ]
            return identity
        time.sleep(0.5)
    raise SystemExit(f"daemon did not serve within 180s; see {log_path}")


def create_snapshot(cli: Path, sock: Path, name: str, mount: Path) -> None:
    out = subprocess.run(
        [str(cli), "--socket", str(sock), "--json", "snapshot", "create", name],
        capture_output=True, text=True,
    )
    if out.returncode != 0:
        raise SystemExit(f"snapshot create failed {out.returncode}: {out.stderr.strip()}")
    log(f"snapshot {name}: {out.stdout.strip()}")
    # The client caches names, so a snapshot created behind the mount shows up later.
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        if (mount / name).is_dir():
            return
        time.sleep(1)
    raise SystemExit(f"snapshot {name} never appeared under {mount}")


def stop_daemon(identity: dict, keep_mount: bool = False) -> str:
    """SIGTERM the exact pid we started, verify the argv first, then verify the mount is gone."""
    proc: subprocess.Popen = identity.pop("pop")
    pid, binary = proc.pid, identity["argv"][0]
    if proc.poll() is not None:
        return f"daemon {pid} already exited {proc.returncode}"
    ps = subprocess.run(
        ["ps", "-o", "command=", "-p", str(pid)], capture_output=True, text=True
    ).stdout.strip()
    if not started(ps, Path(binary)):
        raise SystemExit(f"refusing to signal pid {pid}: its argv is {ps!r}, not {binary}")
    proc.send_signal(signal.SIGTERM)
    try:
        code = proc.wait(timeout=120)
    except subprocess.TimeoutExpired:
        raise SystemExit(f"daemon {pid} did not exit on SIGTERM")
    mount = Path(identity["mount"])
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if not mount_listed(mount):
            return f"daemon {pid} exited {code}, mount {mount} gone"
        time.sleep(1)
    if keep_mount:
        return f"daemon {pid} exited {code}, mount {mount} STILL LISTED (kept)"
    raise SystemExit(f"daemon {pid} exited {code} but {mount} is still mounted")


# ---------------------------------------------------------------- cases


def test_list(tests_root: Path, groups: list[str] | None, only: list[str] | None) -> list[str]:
    found = sorted(
        str(p.relative_to(tests_root)) for p in tests_root.glob("*/*.t")
    )
    if groups:
        found = [t for t in found if t.split("/")[0] in groups]
    if only:
        found = [t for t in found if t in only]
    return found


def parse_tap(text: str) -> dict:
    plan, cases, todo = None, [], 0
    for line in text.splitlines():
        if plan is None:
            m = PLAN_RE.match(line)
            if m:
                plan = int(m.group(1))
            continue
        m = RESULT_RE.match(line)
        if not m:
            continue
        ok, idx, tag, detail = m.group(1) == "ok", int(m.group(2)), m.group(3), m.group(4)
        if tag == "TODO":
            todo += 1
        cases.append({"n": idx, "ok": ok, "todo": tag == "TODO", "detail": detail.strip()})
    return {"plan": plan, "cases": cases, "todo": todo,
            "ok": sum(1 for c in cases if c["ok"]), "not_ok": sum(1 for c in cases if not c["ok"])}


def run_case(tests_root: Path, test: str, case_dir: Path, binary: Path, timeout: int) -> dict:
    case_dir.mkdir(parents=True, exist_ok=True)
    started_at = time.time()
    try:
        proc = subprocess.run(
            ["sh", str(tests_root / test)], cwd=case_dir, capture_output=True, text=True,
            timeout=timeout, env={**os.environ, "TZ": os.environ.get("TZ", "UTC")},
        )
        rc, out, err, timed_out = proc.returncode, proc.stdout, proc.stderr, False
    except subprocess.TimeoutExpired as e:
        rc, out, err, timed_out = None, e.stdout or "", e.stderr or "", True
        if isinstance(out, bytes):
            out = out.decode("utf-8", "replace")
        if isinstance(err, bytes):
            err = err.decode("utf-8", "replace")
    tap = parse_tap(out)
    source = (tests_root / test).read_text()
    root_required = bool(re.search(r"^\s*requires_root\s*$", source, re.M))
    for c in tap["cases"]:
        c["root_required"] = root_required or bool(ROOT_REQUIRED_RE.search(c["detail"]))
    return {
        "test": test, "rc": rc, "timed_out": timed_out, "seconds": round(time.time() - started_at, 1),
        "case_dir": str(case_dir), "stderr": err[-2000:], **tap,
    }


def append_case(path: Path, record: dict) -> None:
    with path.open("a") as f:
        f.write(json.dumps(record, sort_keys=True) + "\n")
        f.flush()


def run_arm(arm: str, tests: list[str], tests_root: Path, root: Path, binary: Path,
            jsonl: Path, timeout: int) -> dict:
    """One arm over every case, each in its own immutable directory, each flushed as it lands."""
    totals = {"cases": 0, "executed": 0, "skipped": 0, "assertions": 0, "ok": 0, "not_ok": 0,
              "root_required_assertions": 0, "timeouts": 0, "nonzero_rc": 0, "empty_output": 0}
    for test in tests:
        case_dir = root / arm / test.replace("/", "_")
        record = run_case(tests_root, test, case_dir, binary, timeout)
        record["arm"] = arm
        append_case(jsonl, record)
        totals["cases"] += 1
        totals["assertions"] += len(record["cases"])
        totals["ok"] += record["ok"]
        totals["not_ok"] += record["not_ok"]
        totals["root_required_assertions"] += sum(1 for c in record["cases"] if c["root_required"])
        if record["timed_out"]:
            totals["timeouts"] += 1
        if record["rc"] not in (0, None):
            totals["nonzero_rc"] += 1
        if not record["cases"]:
            totals["empty_output"] += 1
            totals["skipped"] += 1
            continue
        if record["plan"] == 1 and record["not_ok"] == 0 and len(record["cases"]) == 1:
            totals["skipped"] += 1  # quick_exit: the suite declined to run this case here
        else:
            totals["executed"] += 1
        log(f"  {arm:6} {test:24} rc={record['rc']} {record['seconds']:>6}s "
            f"plan={record['plan']} ok={record['ok']} not_ok={record['not_ok']} "
            f"root={sum(1 for c in record['cases'] if c['root_required'])}")
    return totals


# ---------------------------------------------------------------- verdict


def load(jsonl: Path) -> dict:
    arms: dict = {}
    with jsonl.open() as f:
        for line in f:
            rec = json.loads(line)
            arms.setdefault(rec["arm"], {})[rec["test"]] = rec
    return arms


def compare(native: dict, cowfs: dict) -> dict:
    """A regression is one assertion the native arm passes and the cowfs arm fails."""
    regressions, looser, unpaired = [], [], []
    for test in sorted(set(native) & set(cowfs)):
        n = {c["n"]: c for c in native[test]["cases"]}
        c = {c["n"]: c for c in cowfs[test]["cases"]}
        for idx in sorted(set(n) & set(c)):
            if n[idx]["ok"] and not c[idx]["ok"]:
                regressions.append({"test": test, "n": idx, "detail": c[idx]["detail"],
                                    "root_required": c[idx]["root_required"]})
            elif not n[idx]["ok"] and c[idx]["ok"]:
                looser.append({"test": test, "n": idx, "detail": c[idx]["detail"],
                               "root_required": c[idx]["root_required"]})
    for test in sorted(set(native) ^ set(cowfs)):
        unpaired.append(test)
    return {
        "regressions": regressions,
        "cowfs_looser_than_native": looser,
        "unpaired_cases": unpaired,
        "regressions_outside_root_required": [r for r in regressions if not r["root_required"]],
    }


def verdict(totals: dict, diff: dict, tests: list[str], features: list[str]) -> dict:
    """PASS needs a complete matched run with nothing the native arm passes failing on cowfs.

    A run that could not be measured is UNMEASURABLE, never FAIL: a missing case says nothing
    about the filesystem, and calling it worse would be a guess. FAIL is reserved for a real
    difference between two complete arms.
    """
    unmeasurable = []
    if totals["native"]["executed"] == 0 or totals["cowfs"]["executed"] == 0:
        unmeasurable.append("an arm executed no assertion, so there is nothing to compare")
    if totals["native"]["empty_output"] or totals["cowfs"]["empty_output"]:
        unmeasurable.append("an arm produced a case with no TAP output")
    if totals["native"]["timeouts"] or totals["cowfs"]["timeouts"]:
        unmeasurable.append("a case timed out")
    if diff["unpaired_cases"]:
        unmeasurable.append(f"cases only on one arm: {diff['unpaired_cases']}")
    failed = []
    outside = diff["regressions_outside_root_required"]
    if outside:
        failed.append(f"{len(outside)} assertion(s) pass on native and fail on the cowfs mount")
    reasons = unmeasurable + failed
    state = "UNMEASURABLE" if unmeasurable else ("FAIL" if failed else "PASS")
    return {"state": state, "reasons": reasons, "tests": len(tests), "features": features,
            "totals": totals, "comparison": diff}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    ap.add_argument("--out", type=Path, help="run directory (default <repo>/bench/out/ready-g3/run)")
    ap.add_argument("--tests", help="comma-separated test paths under the suite's tests/ dir")
    ap.add_argument("--groups", help="comma-separated group dirs to include")
    ap.add_argument("--case-timeout", type=int, default=CASE_TIMEOUT)
    ap.add_argument("--keep-mount", action="store_true", help="leave the mount up after the run")
    args = ap.parse_args()

    repo = args.repo.resolve()
    out = (args.out or (repo / "bench" / "out" / "ready-g3" / "run")).resolve()
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    run_dir = out / stamp
    run_dir.mkdir(parents=True, exist_ok=True)
    jsonl = run_dir / "cases.jsonl"
    summary_path = run_dir / "summary.json"
    jsonl.touch()

    src = fetch_tool(repo / "bench" / "out" / "ready-g3")
    binary, features = build_tool(src)
    tool_id = tool_identity(src, binary, features)
    log(f"tool {PINNED_COMMIT} binary {tool_id['binary_sha256'][:16]} features {len(features)}")

    tests_root = src / "tests"
    tests = test_list(
        tests_root,
        args.groups.split(",") if args.groups else None,
        args.tests.split(",") if args.tests else None,
    )
    if not tests:
        raise SystemExit("no tests selected: nothing to measure")
    log(f"{len(tests)} cases, one build, two arms")

    daemon_bin, cli = ensure_binaries(repo)
    store, mount = run_dir / "store", run_dir / "mnt"
    # sun_path is 104 bytes and a lease path can be longer than that on its own, so the socket
    # lives at the repo root under the shortest name that fits.
    sock = repo / "rt" / "c.sock"
    if len(str(sock).encode()) > 103:
        raise SystemExit(f"control socket {sock} is {len(str(sock).encode())} bytes; sun_path holds 103")
    identity_path = run_dir / "daemon.json"
    identity = start_daemon(daemon_bin, store, mount, sock, run_dir / "daemon.log")
    identity_path.write_text(json.dumps({k: v for k, v in identity.items() if k != "pop"}, indent=2))
    log(f"daemon pid {identity['pid']} core on {mount}")

    summary = {}
    try:
        create_snapshot(cli, sock, SNAPSHOT, mount)
        native_root, cowfs_root = run_dir / "native", mount / SNAPSHOT
        log("native arm")
        native = run_arm("native", tests, tests_root, native_root, binary, jsonl, args.case_timeout)
        log("cowfs arm")
        cowfs = run_arm("cowfs", tests, tests_root, cowfs_root, binary, jsonl, args.case_timeout)
        arms = load(jsonl)
        diff = compare(arms.get("native", {}), arms.get("cowfs", {}))
        summary = verdict({"native": native, "cowfs": cowfs}, diff, tests, features)
        summary["tool"] = tool_id
        summary["host"] = {
            "uname": subprocess.run(["uname", "-srm"], capture_output=True, text=True).stdout.strip(),
            "native_fs": subprocess.run(["df", "-T", str(native_root)], capture_output=True,
                                        text=True).stdout.strip().splitlines()[-1:],
            "cowfs_mount": identity.get("mount_table", []),
            "daemon": {k: identity[k] for k in ("pid", "argv", "start_time", "store", "socket")},
        }
        summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True, default=str))
        log(f"\nverdict {summary['state']}")
        for r in summary["reasons"]:
            log(f"  - {r}")
        log(f"regressions {len(diff['regressions'])} "
            f"(outside root-required {len(diff['regressions_outside_root_required'])}), "
            f"cowfs looser {len(diff['cowfs_looser_than_native'])}")
        for r in diff["regressions_outside_root_required"][:20]:
            log(f"  REGRESSION {r['test']} #{r['n']}: {r['detail'][:160]}")
    finally:
        log(stop_daemon(identity, keep_mount=args.keep_mount))
        if not args.keep_mount:
            shutil.rmtree(repo / "rt", ignore_errors=True)
    log(f"evidence {run_dir}")
    return 0 if summary.get("state") == "PASS" else 1


if __name__ == "__main__":
    sys.exit(main())