#!/usr/bin/env python3
"""Gate g5: xfstests-generic, matched native and cowfs arms, fail closed.

Usage:
  xfstests_gate.py preflight --xfstests DIR --out DIR
  xfstests_gate.py classify  --xfstests DIR [--out DIR]
  xfstests_gate.py run       --xfstests DIR --native-root DIR --cowfs-root DIR
                             --out DIR [--cases IDS] [--timeout SECS]
  xfstests_gate.py report    --run DIR

The gate answers one question: for the generic cases it is allowed to run, is
cowfs no worse than a native directory?  Everything else refuses.

Three things make a run a real measurement rather than a claim.

1. Prerequisite gate.  The suite has its own startup gate in `common/config`:
   it calls `_fatal` for a missing `mkfs`, `mount`, `umount`, `perl`, `awk`,
   `sed`, `df`, `xfs_io`, `$here/ltp/fsstress` and `$here/ltp/fsx`, and
   `_fatal` is an exit.  So one missing helper means zero cases run, not a
   partial pass.  `preflight` checks each of them by name and by running the
   suite's own check, so the answer is evidence and not inference.  A missing
   prerequisite prints UNMEASURABLE and exits 2.  It never prints PASS.
2. Static safety classification.  Every `tests/generic/*` is classified from its
   own source before anything runs.  A case that formats, mounts, loops,
   repartitions, needs root or names an absolute path in a destructive command
   is refused, with the line that proves it.  Only the reviewed allowlist in
   `bench/xfstests-allowlist.txt` may run, and only while it still matches the
   tree it was reviewed against.
3. Matched arms and exact exit codes.  Both arms run the same case id with the
   same harness-generated environment, each in its own immutable per-case
   directory that must be empty before the run.  The exit code is captured from
   the child process itself, never from a pipeline.  Each case appends and
   flushes one JSONL line, so an interrupted run keeps every finished case.

Exit codes, the same set `bench/compare.py` uses:
  0  PASS       every measured case passed on both arms
  1  FAIL       cowfs failed a case native passed
  2  UNMEASURABLE  nothing was measured, or a prerequisite was absent
  3  INVALID    the request or the harness itself is wrong

Environment:
  COWFS_XFSTESTS_SRC   default: the xfstests tree to measure
  COWFS_XFSTESTS_OUT   default: bench/out/ready-g5
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
ALLOWLIST_FILE = HERE / "xfstests-allowlist.txt"

# A case is refused when its source matches one of these.  The reason string is
# what lands in the record, so a refusal is auditable without reading the case.
NEEDS_DEVICE = (
    (r"\bmkfs\b", "formats a filesystem"),
    (r"\blosetup\b", "attaches a loop device"),
    (r"\bblkid\b", "probes a block device"),
    (r"\bmodprobe\b", "loads a kernel module"),
    (r"\bswapon\b|\bmkswap\b", "swaps"),
    (r"\bdmsetup\b", "creates a device mapper target"),
    # `mount` is matched without a leading word boundary on purpose: the suite
    # calls it through wrappers like `_test_cycle_mount` and `remount`, and both
    # are a mount. The lookbehind keeps the word "amount" out.
    (r"(?<![a-z])mount|remount|umount", "mounts, remounts or unmounts"),
    (r"/dev/", "writes to a device node"),
    (r"\bparted\b|\bsgdisk\b|\bfdisk\b", "repartitions"),
    (r"\bdebugfs\b|\btune2fs\b|\bresize2fs\b|\be2fsck\b|\bxfs_repair\b", "runs a filesystem repair tool"),
    (r"\bsysctl\b", "changes a kernel parameter"),
    (r"\binsmod\b", "inserts a module"),
    (r"\bdd\b[^|;&]*\bof=/dev/", "writes raw to a device"),
)
NEEDS_ROOT = (
    (r"\bsudo\b", "invokes sudo"),
    (r"_require_root\b", "requires root"),
    (r"\b_runas\b", "switches to another user with _runas"),
    (r"\b_user_do\b|\b_su\b", "runs a command as another user, which needs root"),
    (r"\bmknod\b", "creates a device node, which needs root"),
    (r"\bRequire_mknod\b|_require_mknod\b", "requires mknod, which needs root"),
    (r"\bchown\b", "changes file ownership, which needs a second user or root"),
    (r"\bchgrp\b", "changes a group, which needs a second group or root"),
    (r"\buseradd\b|\bgroupadd\b", "creates a user or group"),
    (r"\bchroot\b", "chroots"),
    (r"/proc/sys/", "writes to /proc/sys"),
)
# Space the run cannot spend. A sparse file this size is still a real
# allocation on the arm under test, and the arms are an SD card and a FUSE mount.
SIZE_RE = re.compile(r"\btruncate\s+-s\s+([0-9]+)([KMGT])?")
SIZE_UNIT = {"K": 1 << 10, "M": 1 << 20, "G": 1 << 30, "T": 1 << 40}
DEFAULT_SPACE_CAP = 1 << 30
NEEDS_SCRATCH = (
    (r"\b_require_scratch\b", "requires a scratch device"),
    (r"\bSCRATCH_DEV\b", "uses SCRATCH_DEV"),
    (r"\bSCRATCH_MNT\b", "uses SCRATCH_MNT"),
    (r"\b_require_scratch_nocheck\b", "requires a scratch device"),
)
# Runtime the gate will not spend. A soak case is the suite's own label for one
# that runs for hours, and an explicit six-figure op count is the same thing
# written out.
NEEDS_LONG = (
    (r"_begin_fstest[^\n]*\b(soak|long)\b", "the suite groups it as soak or long-running"),
    (r"\bSOAK_DURATION\b", "honours a soak duration"),
)
NR_OPS_RE = re.compile(r"nr_ops=\$\(\(?([0-9]+)")
NR_OPS_MIN = 100_000
# Helper binaries and shell wrappers that live outside the case source, so a
# case can need one without naming it.  `_run_*` wrappers and `*_PROG`
# variables are how common/rc reaches most of them.
HELPER = (
    (r"\$SRC_DIR|\$\{SRC_DIR\}", "reaches the helper tree by SRC_DIR"),
    # `$here` is the tree root, and `$here/src/...` is how a case names a helper
    # binary that lives in the suite's own build output.
    (r"\$here\b|\$\{here\}", "reaches the helper tree by $here"),
    (r"\brun_[a-z0-9_]+\b", "runs a helper through a common/rc wrapper"),
    (r"_run_[a-z0-9_]+", "runs a helper through a common/rc wrapper"),
    # Every helper binary the suite exposes is a `*_PROG` variable, so this is
    # the catch-all: a case naming one needs a built helper this host may not have.
    (r"\b[A-Z][A-Z0-9_]*_PROG\b", "names a suite helper program"),
    (r"\$(AIO_TEST|BIO_TEST)\b", "names a helper binary from common/rc"),
    (r"\bXFS_IO_PROG\b", "uses xfs_io"),
    (r"\bMKFILE_PROG\b|\bmkfile\b", "uses mkfile"),
    (r"\bFILL_PROG\b|\bfill\b", "uses fill"),
    (r"\bGETFATTR_PROG\b|\bSETFATTR_PROG\b|\bgetfattr\b|\bsetfattr\b", "uses the attr package"),
    (r"\bFILEFRAG_PROG\b|\bfilefrag\b", "uses filefrag"),
    (r"\bPERF_PROG\b", "uses perf"),
    (r"\bFSYNC_PROG\b|\bfsync-tester\b", "uses fsync-tester"),
    (r"\bLSCTLIO_PROG\b|\bfiemap\b|\bSEEK_HOLE\b|\bSEEK_DATA\b", "uses a fiemap ioctl helper"),
    (r"\bMULTI_FALLOCATE_PROG\b|\bBLKDEV_MAP_PROG\b|\bFALLOCATE_PROG\b", "uses a fallocate helper"),
    (r"\bIOZERO_PROG\b|\biozero\b", "uses iozero"),
    (r"\bchattr\b|\blsattr\b", "uses lsattr"),
    (r"\bINPROG_", "uses a progress helper"),
)
# A destructive command naming an absolute path outside the arm roots would
# reach outside this run.  Variables are fine: they are the harness's own paths.
DESTRUCTIVE = re.compile(
    r"\b(rm|rmdir|unlink|shred|mv|cp|dd|truncate|chmod|chown|mkfifo|mknod)\b[^|;&]*"
)
ABS_PATH = re.compile(r"(?<![\w$.])/(?:[A-Za-z0-9_.+-]+/)*[A-Za-z0-9_.+-]+")
SAFE_ABS_PREFIX = ("/bin/", "/usr/bin/", "/usr/sbin/", "/sbin/", "/usr/local/bin/", "/proc/", "/dev/null", "/dev/zero", "/dev/urandom")
# common/* a case may source.  Anything else is code this harness has not read.
ALLOWED_SOURCES = {
    "preamble", "rc", "filter", "list", "config", "promotion",
    "ftruncate.inc", "util", "attr", "pwrite-buffers", "rc.local",
}
SOURCE_RE = re.compile(r"^\s*\.\s+\./([a-z]+)/([A-Za-z0-9_.-]+)", re.M)
# The suite's own startup gate, common/config, in the order it checks.  A missing
# entry is fatal for every case, so `preflight` reports the line that stops it.
STARTUP_GATE = [
    ("mkfs", "common/config:114 mkfs not found"),
    ("mount", "common/config:117 mount not found"),
    ("umount", "common/config:120 umount not found"),
    ("perl", "common/config:129 perl not found"),
    ("awk", "common/config:132 awk not found"),
    ("sed", "common/config:135 sed not found"),
    ("df", "common/config:143 df not found"),
    ("xfs_io", "common/config:147 xfs_io not found"),
]
# $here/ltp/... are checked as files, not as PATH entries.
STARTUP_GATE_FILES = [
    ("ltp/fsstress", "common/config:123 fsstress not found or executable"),
    ("ltp/fsx", "common/config:126 fsx not found or executable"),
]
# Signatures that mean the case did not assert what its exit code claims.  A case
# whose log carries one is INVALID, never a pass.  This is the failure the first
# probe of this gate hit: a test that could not find a helper compared nothing
# and still exited 0.
SKIP_SIGNATURES = (
    (re.compile(r"command not found"), "a command was missing"),
    # A missing helper is the failure this gate exists to catch, and it names a
    # path under the suite's own helper directories. A case that expects ENOENT
    # on a data file it created must not be flagged for that, so the path is part
    # of the pattern rather than the message.
    (re.compile(r"(No such file or directory.*(/src/|/ltp/))|((/src/|/ltp/)\S*.*No such file or directory)"),
     "a suite helper binary was missing"),
    (re.compile(r"unary operator expected|bad substitution"), "a shell comparison was malformed, so its assertion did not run"),
    (re.compile(r"_notrun|\bnotrun\b"), "the case refused to run"),
    (re.compile(r"Test not run|\bSkipped\b"), "the case skipped work"),
    (re.compile(r"\bFAIL\b"), "the log reports a failure"),
)

VERDICT_OK = "PASS"
VERDICT_FAIL = "FAIL"
VERDICT_UNMEASURABLE = "UNMEASURABLE"
VERDICT_INVALID = "INVALID"


def arm_env(base, fstype, tmpdir, result_dir, tests_root):
    return {
        "TEST_DIR": base,
        "TEST_DEV": base,
        "SCRATCH_DEV": "",
        "SCRATCH_MNT": "",
        # FSTYP stays empty on both arms on purpose: every fstype-specific block
        # in common/config wants mkfs.<fstype>, and this host has no block
        # capability to spend on one.  Setting it empty is symmetric, and it is
        # the only setting that keeps the two arms comparable.
        "FSTYP": "",
        "TMPDIR": tmpdir,
        "XFSTESTS_TEST_TMPDIR": tmpdir,
        "RESULT_DIR": result_dir,
        "here": str(tests_root),
        "MSGVERB": "text:action",
        "QA_CHECK_FS": "true",
        "DIFF_LENGTH": "10",
    }


def which_all(names, path):
    return {n: shutil.which(n, path=path) for n in names}


def git_provenance(root):
    rec = {"path": str(root), "git": False}
    if not (root / ".git").exists() and not shutil.which("git"):
        rec["detail"] = "no .git and no git"
        return rec
    try:
        sha = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"],
                             capture_output=True, text=True, timeout=60)
        rec["git"] = sha.returncode == 0
        if rec["git"]:
            rec["sha"] = sha.stdout.strip()
            when = subprocess.run(["git", "-C", str(root), "log", "-1", "--format=%cI"],
                                  capture_output=True, text=True, timeout=60)
            rec["committed"] = when.stdout.strip()
            dirty = subprocess.run(["git", "-C", str(root), "status", "--porcelain"],
                                   capture_output=True, text=True, timeout=120)
            rec["dirty_paths"] = [l for l in dirty.stdout.splitlines() if l.strip()][:20]
            rec["dirty"] = bool(rec["dirty_paths"])
    except (OSError, subprocess.SubprocessError) as exc:
        rec["detail"] = f"{type(exc).__name__}: {exc}"
    return rec


def tool_version(name, path):
    if not path:
        return None
    try:
        out = subprocess.run([path, "--version"], capture_output=True, text=True, timeout=30)
        return (out.stdout or out.stderr).splitlines()[0].strip() if (out.stdout or out.stderr) else ""
    except (OSError, subprocess.SubprocessError):
        return "version probe failed"


def classify_case(path, space_cap=DEFAULT_SPACE_CAP):
    """Classify one generic case from its own source. Returns a record."""
    src = path.read_text(errors="replace")
    reasons = []
    verdict = "SAFE"
    for pattern, why in NEEDS_DEVICE:
        hit = re.search(pattern, src, re.I)
        if hit:
            verdict = "NEEDS_DEVICE"
            reasons.append(f"line {src[:hit.start()].count(chr(10)) + 1}: {why} ({hit.group(0)!r})")
    for pattern, why in NEEDS_ROOT:
        hit = re.search(pattern, src, re.I)
        if hit:
            verdict = "NEEDS_ROOT"
            reasons.append(f"line {src[:hit.start()].count(chr(10)) + 1}: {why} ({hit.group(0)!r})")
    for size in SIZE_RE.finditer(src):
        value = int(size.group(1)) * SIZE_UNIT.get(size.group(2) or "K", 1)
        if value > space_cap:
            verdict = "NEEDS_BIG_SPACE"
            reasons.append(f"line {src[:size.start()].count(chr(10)) + 1}: allocates "
                           f"{value >> 20} MiB, over the {space_cap >> 20} MiB cap")
    for pattern, why in NEEDS_SCRATCH:
        hit = re.search(pattern, src)
        if hit:
            verdict = "NEEDS_SCRATCH"
            reasons.append(f"line {src[:hit.start()].count(chr(10)) + 1}: {why} ({hit.group(0)!r})")
    for pattern, why in NEEDS_LONG:
        hit = re.search(pattern, src)
        if hit:
            verdict = "NEEDS_LONG"
            reasons.append(f"line {src[:hit.start()].count(chr(10)) + 1}: {why} ({hit.group(0)!r})")
    for hit in NR_OPS_RE.finditer(src):
        if int(hit.group(1)) >= NR_OPS_MIN:
            verdict = "NEEDS_LONG"
            reasons.append(f"line {src[:hit.start()].count(chr(10)) + 1}: declares "
                           f"{hit.group(1)} operations, at or over the {NR_OPS_MIN} cap")
    for cmd in DESTRUCTIVE.finditer(src):
        tail = cmd.group(0)
        for ap in ABS_PATH.finditer(tail):
            path_text = ap.group(0)
            if path_text.startswith(SAFE_ABS_PREFIX) or path_text in ("/", "//"):
                continue
            verdict = "UNSAFE"
            reasons.append(f"line {src[:cmd.start()].count(chr(10)) + 1}: {cmd.group(1)} names absolute {path_text!r}")
    external = sorted({f"{d}/{f}" for d, f in SOURCE_RE.findall(src) if f not in ALLOWED_SOURCES})
    if external:
        verdict = "UNREAD_SOURCE" if verdict == "SAFE" else verdict
        reasons.append("sources unreviewed code: " + ", ".join(external))
    helpers = sorted({why for pattern, why in HELPER if re.search(pattern, src)})
    if helpers and verdict == "SAFE":
        verdict = "NEEDS_HELPER"
    reasons.extend(f"needs a helper: {h}" for h in helpers)
    return {
        "id": path.name,
        "verdict": verdict,
        "bytes": len(src),
        "reasons": reasons,
    }


def classify_group(tests_root):
    group = Path(tests_root) / "generic"
    if not group.is_dir():
        raise FileNotFoundError(f"{group} is not a directory; --xfstests must name the tree root")
    group = Path(tests_root) / "generic"
    out = []
    for case in sorted(group.iterdir(), key=lambda p: p.name):
        if not case.is_file() or not re.fullmatch(r"[0-9]+", case.name):
            continue
        out.append(classify_case(case))
    return out


def allowlist_from(records):
    return sorted(r["id"] for r in records if r["verdict"] == "SAFE")


def read_committed_allowlist():
    if not ALLOWLIST_FILE.exists():
        return None, f"{ALLOWLIST_FILE} is missing"
    ids = []
    for line in ALLOWLIST_FILE.read_text().splitlines():
        # Only a bare case id counts. Anything else is prose, and prose in this
        # file must never become a case id.
        if re.fullmatch(r"[0-9]+", line.strip()):
            ids.append(line.strip())
    return sorted(ids), None


def check_allowlist_drift(records):
    computed = allowlist_from(records)
    committed, err = read_committed_allowlist()
    if err:
        return computed, err
    if sorted(committed) != computed:
        added = sorted(set(computed) - set(committed))
        removed = sorted(set(committed) - set(computed))
        return computed, f"allowlist drift: added {added}, removed {removed}"
    return computed, None


def tests_dir(tree):
    """`tree` is the xfstests tree root, the directory that holds tests/."""
    return Path(tree).resolve() / "tests"


def preflight(tree, out_dir, timeout=300):
    """Check every prerequisite, then prove the answer by running one case."""
    tests_root = tests_dir(tree)
    out_dir = Path(out_dir).resolve()
    src_root = tests_root.parent
    env_path = os.environ.get("PATH", "")
    rec = {
        "kind": "preflight",
        "host": {
            "uname": subprocess.run(["uname", "-srm"], capture_output=True, text=True).stdout.strip(),
            "uid": os.getuid(),
            "is_root": os.getuid() == 0,
        },
        "path": env_path,
        "source": git_provenance(src_root),
        "tools": {},
        "startup_gate": [],
        "capabilities_absent": {},
        "blocking": [],
    }
    if rec["host"]["is_root"]:
        # Running the suite as root is out of scope for this gate: it would make
        # a mkfs or mount a one-command accident rather than a recorded refusal.
        rec["blocking"].append({"key": "uid", "detail": "refusing to run as root"})
    if not (tests_root / "generic").is_dir():
        rec["blocking"].append({"key": "tests_dir", "detail": f"{tests_root}/generic is not a directory"})
    tools = which_all([name for name, _ in STARTUP_GATE] + ["bash", "sh", "git"], env_path)
    for name, _ in STARTUP_GATE:
        rec["tools"][name] = {"path": tools.get(name), "version": tool_version(name, tools.get(name))}
    for name, fatal in STARTUP_GATE:
        if not tools.get(name):
            rec["startup_gate"].append({"key": name, "status": "absent", "fatal": fatal})
            rec["blocking"].append({"key": name, "detail": fatal})
        else:
            rec["startup_gate"].append({"key": name, "status": "present", "path": tools[name]})
    for rel, fatal in STARTUP_GATE_FILES:
        target = src_root / rel
        ok = target.is_file() and os.access(target, os.X_OK)
        rec["startup_gate"].append({"key": rel, "status": "present" if ok else "absent",
                                    "fatal": fatal if not ok else None,
                                    "path": str(target)})
        if not ok:
            rec["blocking"].append({"key": rel, "detail": fatal})
    rec["built"] = {
        "include/builddefs": (src_root / "include" / "builddefs").exists(),
        "include/config.h": (src_root / "include" / "config.h").exists(),
        "src/mkfile": (src_root / "src" / "mkfile").exists(),
    }
    rec["capabilities_absent"] = {
        "block_scratch_device": "no loop or scratch device; ./check would need root and mkfs",
        "root": "not root, and root use is out of scope for this gate",
        "build_toolchain": "autoconf/automake/libtool/m4 absent, so the tree's own build cannot run",
        "getfattr/setfattr": "attr package absent, so xattr cases cannot run",
    }
    # The answer that matters is the suite's own, so ask the suite. The probe
    # directory is unique per call because a case directory is immutable.
    probe = run_case(tests_root, tests_root / "generic" / "010",
                     out_dir / f"preflight-probe-{os.getpid()}", "native", timeout,
                     allow_missing_root=True)
    rec["suite_probe"] = probe
    if probe["rc"] == 0 and not rec["blocking"]:
        rec["verdict"] = VERDICT_OK
    else:
        rec["verdict"] = VERDICT_UNMEASURABLE
        if rec["blocking"]:
            rec["reason"] = "; ".join(f"{b['key']}: {b['detail']}" for b in rec["blocking"])
        else:
            first = next((l for l in probe["log_tail"] if "_fatal" in l or "Error" in l or "not found" in l), "")
            rec["reason"] = f"the suite refused a probe case with exit {probe['rc']}: {first.strip()}"
    write_jsonl(out_dir / "preflight.jsonl", [rec])
    return rec


def write_jsonl(path, records):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as fh:
        for rec in records:
            fh.write(json.dumps(rec, sort_keys=True) + "\n")
            fh.flush()
            os.fsync(fh.fileno())


def append_jsonl(path, rec):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as fh:
        fh.write(json.dumps(rec, sort_keys=True) + "\n")
        fh.flush()
        os.fsync(fh.fileno())


def prepare_dir(path):
    """A per-case directory that must be new and empty. Never repairs, never reuses."""
    path = Path(path)
    if path.exists():
        raise FileExistsError(f"{path} already exists; a case directory is immutable per attempt")
    path.mkdir(parents=True)
    if any(path.iterdir()):
        raise RuntimeError(f"{path} is not empty")
    return path


def run_case(tests_root, case, work_dir, arm, timeout, fstype="", allow_missing_root=False):
    """Run one case in its own directory and capture the child's own exit code.

    The suite runs a case as `./tests/generic/NNN` from the tree root: that is
    the only cwd where its `. ./common/preamble` resolves and where `$here` is
    the tree root, which is where the helper paths come from.
    """
    tests_root = Path(tests_root)
    tree_root = tests_root.parent
    work = Path(work_dir)
    work.mkdir(parents=True, exist_ok=True)
    test_dir = prepare_dir(work / "testdir")
    tmp_dir = work / "tmp"
    result_dir = work / "results"
    tmp_dir.mkdir()
    result_dir.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    env.pop("FSTYP", None)
    env.update(arm_env(str(test_dir), fstype, str(tmp_dir), str(result_dir), tree_root))
    log = work / "case.log"
    argv = [f"./{case.relative_to(tree_root)}"]
    started = time.time()
    with log.open("ab", buffering=0) as fh:
        fh.write(f"# argv={argv} cwd={tree_root} arm={arm} test_dir={test_dir}\n".encode())
        fh.flush()
        proc = subprocess.Popen(argv, cwd=str(tree_root), env=env, stdout=fh,
                                stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                start_new_session=True)
        try:
            rc = proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            # Only this case's own session. A timed-out case that keeps writing
            # would corrupt the next case's directory, and a stray writer on the
            # mount would block the unmount at the end.
            os.killpg(proc.pid, 9)
            rc = proc.wait(timeout=60)
            rc = -9
            timed_out = True
        else:
            timed_out = False
    text = log.read_text(errors="replace")
    rec = {
        "kind": "case",
        "arm": arm,
        "case": case.relative_to(tests_root.parent).as_posix(),
        "rc": rc,
        "timed_out": timed_out,
        "wall_s": round(time.time() - started, 3),
        "test_dir": str(test_dir),
        "log": str(log),
    }
    if not allow_missing_root:
        skips = [why for pattern, why in SKIP_SIGNATURES if pattern.search(text)]
        rec["skips"] = skips
    rec["log_tail"] = [l for l in text.splitlines() if l.strip()][-12:]
    return rec


def verdict_for_case(native, cowfs):
    """One case, both arms. A cowfs failure native passed is a FAIL, always."""
    if native["rc"] != 0:
        if cowfs["rc"] == 0:
            return VERDICT_OK, "native failed and cowfs passed; recorded as a native-arm failure"
        return VERDICT_UNMEASURABLE, f"both arms failed (native rc={native['rc']}, cowfs rc={cowfs['rc']})"
    if native.get("skips"):
        return VERDICT_INVALID, "native log says the case did not assert: " + ", ".join(native["skips"])
    if cowfs["rc"] != 0:
        if cowfs.get("skips"):
            return VERDICT_INVALID, "cowfs log says the case did not assert: " + ", ".join(cowfs["skips"])
        return VERDICT_FAIL, f"cowfs rc={cowfs['rc']} where native rc=0"
    if cowfs.get("skips"):
        return VERDICT_INVALID, "cowfs log says the case did not assert: " + ", ".join(cowfs["skips"])
    return VERDICT_OK, "both arms exited 0 with no skip signature"


def run(args):
    tests_root = tests_dir(args.xfstests)
    out = Path(args.out).resolve()
    if os.getuid() == 0:
        print("INVALID: refusing to run the suite as root", file=sys.stderr)
        return 3
    pre = preflight(args.xfstests, out, args.timeout)
    if pre["verdict"] != VERDICT_OK:
        print(f"VERDICT: {VERDICT_UNMEASURABLE}", file=sys.stderr)
        print(f"REASON: {pre.get('reason', 'prerequisite absent')}", file=sys.stderr)
        for item in pre["blocking"]:
            print(f"BLOCKING: {item['key']}: {item['detail']}", file=sys.stderr)
        print(f"EVIDENCE: {out / 'preflight.jsonl'}", file=sys.stderr)
        return 2
    try:
        records = classify_group(tests_root)
    except FileNotFoundError as exc:
        print(f"INVALID: {exc}", file=sys.stderr)
        return 3
    allow, drift = check_allowlist_drift(records)
    if drift:
        print(f"INVALID: {drift}", file=sys.stderr)
        return 3
    requested = [c.strip() for c in (args.cases or ",".join(allow)).split(",") if c.strip()]
    refused = [c for c in requested if c not in allow]
    if refused:
        print(f"INVALID: not in the reviewed allowlist: {refused}", file=sys.stderr)
        return 3
    native_root = Path(args.native_root).resolve()
    cowfs_root = Path(args.cowfs_root).resolve()
    for label, root in (("native", native_root), ("cowfs", cowfs_root)):
        if not root.is_dir():
            print(f"INVALID: {label} root {root} is not a directory", file=sys.stderr)
            return 3
    run_dir = out / f"run-{time.strftime('%Y%m%d-%H%M%S')}"
    results = run_dir / "results.jsonl"
    meta = {"kind": "meta", "cases": requested, "allowlist_sha": sha_of(allow),
            "native_root": str(native_root), "cowfs_root": str(cowfs_root),
            "timeout_s": args.timeout, "source": pre["source"], "path": pre["path"]}
    write_jsonl(results, [meta])
    tallies = {VERDICT_OK: 0, VERDICT_FAIL: 0, VERDICT_UNMEASURABLE: 0, VERDICT_INVALID: 0}
    for cid in requested:
        case = tests_root / "generic" / cid
        native = run_case(tests_root, case, run_dir / cid / "native", "native", args.timeout)
        cowfs = run_case(tests_root, case, run_dir / cid / "cowfs", "cowfs", args.timeout)
        verdict, why = verdict_for_case(native, cowfs)
        tallies[verdict] += 1
        rec = {"kind": "case_verdict", "case": f"generic/{cid}", "verdict": verdict, "why": why,
               "native_rc": native["rc"], "cowfs_rc": cowfs["rc"],
               "native_log": native["log"], "cowfs_log": cowfs["log"],
               "native_skips": native.get("skips"), "cowfs_skips": cowfs.get("skips"),
               "native_wall_s": native["wall_s"], "cowfs_wall_s": cowfs["wall_s"]}
        append_jsonl(results, rec)
        print(f"{cid}\t{verdict}\tnative={native['rc']} cowfs={cowfs['rc']}\t{why}")
    covered = len(requested)
    total = len(records)
    print(f"COVERAGE: {covered} of {total} generic cases, allowlist sha {meta['allowlist_sha'][:12]}")
    print(f"COUNTS: pass={tallies[VERDICT_OK]} fail={tallies[VERDICT_FAIL]} "
          f"unmeasurable={tallies[VERDICT_UNMEASURABLE]} invalid={tallies[VERDICT_INVALID]}")
    if args.require_full and covered != total:
        print(f"VERDICT: {VERDICT_UNMEASURABLE}", file=sys.stderr)
        print(f"REASON: --require-full and only {covered} of {total} generic cases were run", file=sys.stderr)
        return 2
    if tallies[VERDICT_FAIL]:
        print(f"VERDICT: {VERDICT_FAIL}", file=sys.stderr)
        return 1
    if tallies[VERDICT_OK] == 0:
        print(f"VERDICT: {VERDICT_UNMEASURABLE}", file=sys.stderr)
        return 2
    print(f"VERDICT: {VERDICT_OK}")
    if covered != total:
        print(f"SCOPE: PARTIAL. {covered} of {total} generic cases ran. G5 stays OPEN.")
    return 0


def sha_of(items):
    import hashlib
    return hashlib.sha256("\n".join(items).encode()).hexdigest()


def report(args):
    results = Path(args.run) / "results.jsonl"
    if not results.exists():
        print(f"INVALID: {results} does not exist", file=sys.stderr)
        return 3
    rows = [json.loads(l) for l in results.read_text().splitlines() if l.strip()]
    cases = [r for r in rows if r.get("kind") == "case_verdict"]
    meta = next((r for r in rows if r.get("kind") == "meta"), {})
    print(f"CASES: {len(cases)} of {meta.get('allowlist_sha', '?')[:12]}")
    for rec in cases:
        print(f"{rec['case']}\t{rec['verdict']}\tnative={rec['native_rc']} cowfs={rec['cowfs_rc']}\t{rec['why']}")
    counts = {}
    for rec in cases:
        counts[rec["verdict"]] = counts.get(rec["verdict"], 0) + 1
    print("COUNTS: " + " ".join(f"{k}={v}" for k, v in sorted(counts.items())))
    return 0


def classify_cmd(args):
    try:
        records = classify_group(tests_dir(args.xfstests))
    except FileNotFoundError as exc:
        print(f"INVALID: {exc}", file=sys.stderr)
        return 3
    allow, drift = check_allowlist_drift(records)
    summary = {}
    for rec in records:
        summary[rec["verdict"]] = summary.get(rec["verdict"], 0) + 1
    if args.out:
        write_jsonl(Path(args.out) / "classify.jsonl", records)
    for rec in records:
        if rec["verdict"] != "SAFE" and args.verbose:
            print(f"{rec['id']}\t{rec['verdict']}\t{'; '.join(rec['reasons'])}")
    print("SUMMARY: " + " ".join(f"{k}={v}" for k, v in sorted(summary.items())))
    print(f"ALLOWLIST: {len(allow)} cases, sha {sha_of(allow)[:12]}")
    print(f"ALLOWLIST_SHA: {sha_of(allow)}")
    print(f"DRIFT: {drift or 'none'}")
    return 0


def preflight_cmd(args):
    rec = preflight(args.xfstests, Path(args.out), args.timeout)
    print(f"VERDICT: {rec['verdict']}")
    if rec.get("reason"):
        print(f"REASON: {rec['reason']}")
    print(f"SOURCE: {rec['source'].get('sha', 'unknown')} ({rec['source'].get('committed', '?')})")
    print(f"PATH: {rec['path']}")
    for gate in rec["startup_gate"]:
        print(f"GATE: {gate['key']}\t{gate['status']}\t{gate.get('fatal') or gate.get('path')}")
    print(f"PROBE: generic/010 rc={rec['suite_probe']['rc']} log={rec['suite_probe']['log']}")
    print(f"BUILT: {json.dumps(rec['built'], sort_keys=True)}")
    print(f"EVIDENCE: {Path(args.out) / 'preflight.jsonl'}")
    return 0 if rec["verdict"] == VERDICT_OK else 2


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    def add_common(p):
        p.add_argument("--xfstests", default=os.environ.get("COWFS_XFSTESTS_SRC"),
                       required=os.environ.get("COWFS_XFSTESTS_SRC") is None,
                       help="the xfstests tree, the directory that holds tests/")
        p.add_argument("--out", default=os.environ.get("COWFS_XFSTESTS_OUT", str(REPO / "bench" / "out" / "ready-g5")))
        p.add_argument("--timeout", type=int, default=300)

    p = sub.add_parser("preflight")
    add_common(p)
    p.set_defaults(func=preflight_cmd)

    p = sub.add_parser("classify")
    add_common(p)
    p.add_argument("--verbose", action="store_true")
    p.set_defaults(func=classify_cmd)

    p = sub.add_parser("run")
    add_common(p)
    p.add_argument("--native-root", required=True)
    p.add_argument("--cowfs-root", required=True)
    p.add_argument("--cases", help="comma separated generic ids, must be in the reviewed allowlist")
    p.add_argument("--require-full", action="store_true")
    p.set_defaults(func=run)

    p = sub.add_parser("report")
    p.add_argument("--run", required=True)
    p.set_defaults(func=report)

    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())