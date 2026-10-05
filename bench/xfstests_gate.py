#!/usr/bin/env python3
"""cowfs success criterion 2, gate g5: xfstests-generic, matched arms, fail closed.

Usage:
  xfstests_gate.py preflight --xfstests DIR --out DIR
  xfstests_gate.py classify  --xfstests DIR [--out DIR] [--verbose]
  xfstests_gate.py run       --xfstests DIR --native-root DIR --cowfs-root DIR
                             --out DIR [--cases IDS] [--timeout SECS] [--require-full]
  xfstests_gate.py report    --run DIR

The gate answers one question: for the generic cases it is allowed to run, is
cowfs no worse than a native directory?  Everything else refuses.

Five properties make a run a measurement rather than a claim.  Each one exists
because a simpler version of this gate was wrong.

1.  Arms run where they were told to run.  Each arm's per-case directory is
    created inside its own `--native-root` or `--cowfs-root`, and the case itself
    reports the directory it was given, the device that directory is on and the
    filesystem type.  Roots that are the same directory, one inside the other, a
    symlink to each other, or indistinguishable by device and mount are refused.
    The two arms are never the same filesystem, and the cowfs arm must be a FUSE
    mount whose fstype is not the native arm's.  Logs and meta stay under --out.

2.  The suite's own gate decides.  `common/config` calls `_fatal` for a missing
    mkfs, mount, umount, perl, awk, sed, df, xfs_io, ltp/fsstress or ltp/fsx, so
    one absent helper means zero cases run rather than a partial pass.
    `preflight` checks each by name and then runs a real case.  A missing
    prerequisite prints UNMEASURABLE and exits 2.  There is no code path from a
    missing prerequisite to PASS.

3.  Success needs positive evidence.  Exit 0 is necessary and not sufficient.
    `direct` can never produce a PASS at all, because without the suite's runner
    there is no supported success witness.  Under `check` a PASS needs a receipt
    that verified, each probe measured: the runner exists, is executable and its
    bytes are the ones the reviewed pin names; the tree is the reviewed tree, at
    the reviewed sha, clean; the runner named exactly the requested case, with no
    id missing, extra or duplicated; the suite's own last summary line reports a
    pass whose count equals the ids named, with nothing not run or ignored; and
    the suite's own group.list selects the case.  The suite streams the case's
    output too, so its summary lines are taken by position and by count: a
    second `Ran:` line, or a banner a case printed, cannot become the witness.
    A case whose outcome is a skip or a refusal is classified before any
    comparison happens, and neither arm's skip is ever ignored.  A case that
    exits nonzero is judged FAILED before any observer bookkeeping, so a real
    filesystem failure is never explained away by an arm that could not be
    measured.

4.  Source is pinned, not named.  The allowlist carries the tree sha, the case
    sha of every allowlisted case, the sha of every `common/*` file those cases
    reach by reading their sources, and the sha of the suite's own runner.
    Both shell spellings are read, `. ./common/rc` and `. common/config`, and a
    source line this scan cannot resolve is a refusal rather than a clean
    result.  The closure is walked at one depth, CLOSURE_DEPTH, both when a pin
    is generated and when it is verified, so the two cannot claim different
    coverage.  Every attempt re-reads the tree and re-hashes those files and
    refuses before the first subprocess when any of them moved, when the worktree
    is dirty, or when the reviewed set drifted.  A per-case source hash is
    recorded next to every executed case.

5.  The reviewed suite is the one this gate ships a pin for.  That pin lives
    beside this file and is not selectable from the command line, so pointing the
    gate at a tree this lane built, with a pin matching that tree, produces a
    receipt labelled harness proof and never xfstests acceptance evidence.

6.  Only what was measured is recorded.  Capability notes are probe results,
    never literals, and each entry records how it was measured; the twenty-third
    entry says in its own `how` that it is stated rather than probed.  A report
    on a FAIL or INVALID run exits nonzero.

Exit codes, the same set `bench/compare.py` uses:
  0  PASS          every measured case passed on both arms
  1  FAIL          cowfs failed a case native passed
  2  UNMEASURABLE  nothing was measured, or a prerequisite was absent
  3  INVALID       the request, the source pin or the harness is wrong

Environment:
  COWFS_XFSTESTS_SRC   default: the xfstests tree to measure
  COWFS_XFSTESTS_OUT   default: bench/out/ready-g5
"""

import argparse
import hashlib
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
CASE_ID_RE = re.compile(r"[0-9]+")
DEFAULT_TIMEOUT = 300
DEFAULT_SPACE_CAP = 1 << 30
NR_OPS_MIN = 100_000

# --- what a case may not do -------------------------------------------------

NEEDS_DEVICE = (
    (r"\bmkfs\b", "formats a filesystem"),
    (r"\blosetup\b", "attaches a loop device"),
    (r"\bblkid\b", "probes a block device"),
    (r"\bmodprobe\b", "loads a kernel module"),
    (r"\bswapon\b|\bmkswap\b", "swaps"),
    (r"\bdmsetup\b", "creates a device mapper target"),
    # `mount` without a leading word boundary on purpose: the suite calls it
    # through wrappers like `_test_cycle_mount` and `remount`.
    (r"(?<![a-z])mount|remount|umount", "mounts, remounts or unmounts"),
    # `/dev/null` and friends appear in nearly every case, so only a device
    # node that is not one of the standard character devices counts.
    (r"/dev/(?!null\b|zero\b|full\b|random\b|urandom\b|tty\b)", "writes to a device node"),
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
    (r"_require_mknod\b", "requires mknod, which needs root"),
    (r"\bchown\b", "changes file ownership, which needs a second user or root"),
    (r"\bchgrp\b", "changes a group, which needs a second group or root"),
    (r"\buseradd\b|\bgroupadd\b", "creates a user or group"),
    (r"\bchroot\b", "chroots"),
    (r"/proc/sys/", "writes to /proc/sys"),
)
NEEDS_SCRATCH = (
    (r"\b_require_scratch\b", "requires a scratch device"),
    (r"\bSCRATCH_DEV\b", "uses SCRATCH_DEV"),
    (r"\bSCRATCH_MNT\b", "uses SCRATCH_MNT"),
    (r"\b_require_scratch_nocheck\b", "requires a scratch device"),
)
NEEDS_LONG = (
    (r"_begin_fstest[^\n]*\b(soak|long)\b", "the suite groups it as soak or long-running"),
    (r"\bSOAK_DURATION\b", "honours a soak duration"),
)
NR_OPS_RE = re.compile(r"nr_ops=\$\(\(?([0-9]+)")
HELPER = (
    # The attr/acl surface. Kept explicit rather than folded into the catch-all,
    # because these names appear as bare words and a case can reach all of them.
    (r"\bgetfacl\b|\bsetfacl\b", "uses getfacl/setfacl"),
    (r"\bchacl\b|\bgetfacl\b", "uses the acl tools"),
    (r"\bfs_id\b|\b_t\b\s+-c\b", "uses filesystem id helpers"),
    (r"\b_nfacl\b|\bnfacl\b", "uses nfacl"),
    (r"\bSYSACL\b|\bsysctl\b", "reads or sets a sysctl"),
    (r"\$SRC_DIR|\$\{SRC_DIR\}", "reaches the helper tree by SRC_DIR"),
    (r"\$here\b|\$\{here\}", "reaches the helper tree by $here"),
    (r"\brun_[a-z0-9_]+\b", "runs a helper through a common/rc wrapper"),
    (r"_run_[a-z0-9_]+", "runs a helper through a common/rc wrapper"),
    # Every helper the suite exposes is a `*_PROG` variable, so this catches a
    # case that names one without saying which.
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
DESTRUCTIVE = re.compile(r"\b(rm|rmdir|unlink|shred|mv|cp|dd|truncate|chmod|chown|mkfifo|mknod)\b[^|;&]*")
ABS_PATH = re.compile(r"(?<![\w$.])/(?:[A-Za-z0-9_.+-]+/)*[A-Za-z0-9_.+-]+")
SAFE_ABS_PREFIX = ("/bin/", "/usr/bin/", "/usr/sbin/", "/sbin/", "/usr/local/bin/",
                   "/proc/", "/dev/null", "/dev/zero", "/dev/urandom")
ALLOWED_SOURCES = {
    "preamble", "rc", "filter", "list", "config", "promotion",
    "ftruncate.inc", "util", "attr", "pwrite-buffers", "rc.local",
}
# Shell sourcing, as the suite actually writes it. The pinned tree uses
# `. common/config`, `. common/exit`, `. common/test_names` as well as
# `. ./common/rc`, so an optional `./` is required or the closure silently misses
# files. `.` and `source` are both accepted because both appear.
SOURCE_RE = re.compile(r"^\s*(?:\.|source)\s+\.?/?([a-z]+)/([A-Za-z0-9_.-]+)\s*$", re.M)
SIZE_RE = re.compile(r"\btruncate\s+-s\s+([0-9]+)([KMGT])?")
SIZE_UNIT = {"K": 1 << 10, "M": 1 << 20, "G": 1 << 30, "T": 1 << 40}

# --- the suite's own startup gate, in the order common/config checks it -----

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
STARTUP_GATE_FILES = [
    ("ltp/fsstress", "common/config:123 fsstress not found or executable"),
    ("ltp/fsx", "common/config:126 fsx not found or executable"),
]
# Files the suite build generates that are read as data, not executed. Separate
# from STARTUP_GATE_FILES because the executable bit is the wrong test for them.
STARTUP_GATE_DATA = [
    # `check` resolves a testlist entry by grepping it against the group's
    # group.list. Without it check answers "unknown test, ignored", runs nothing,
    # and its summary line then describes zero executed cases.
    ("tests/generic/group.list", "check:370 cannot resolve a testlist entry without group.list"),
]
# Which PATH an operator needs. Recorded, not asserted: the value below is only
# a hint, and `preflight` reports what PATH it actually used.
SBIN_PATH_HINT = "/usr/sbin:/sbin"

VERDICT_OK = "PASS"
VERDICT_FAIL = "FAIL"
VERDICT_UNMEASURABLE = "UNMEASURABLE"
VERDICT_INVALID = "INVALID"

# Case outcome, classified before any comparison between the two arms.
OUTCOME_PASSED = "PASSED"
OUTCOME_FAILED = "FAILED"
OUTCOME_SKIPPED = "SKIPPED"
OUTCOME_REFUSED = "REFUSED"


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sha_of(items):
    return hashlib.sha256("\n".join(items).encode()).hexdigest()


def tests_dir(tree):
    """`tree` is the xfstests tree root, the directory that holds tests/."""
    return Path(tree).resolve() / "tests"


def which_all(names, path=None):
    return {n: shutil.which(n, path=path) for n in names}


def arm_env(base, tmpdir, result_dir, tree_root):
    """The environment a case runs under. Identical for both arms except base."""
    return {
        "TEST_DIR": base,
        "TEST_DEV": base,
        "SCRATCH_DEV": "",
        "SCRATCH_MNT": "",
        # FSTYP stays empty on both arms on purpose: every fstype-specific block
        # in common/config wants mkfs.<fstype>, and this gate has no block
        # capability to spend on one. Empty is also the only value that keeps
        # the two arms comparable.
        "FSTYP": "",
        "TMPDIR": tmpdir,
        "XFSTESTS_TEST_TMPDIR": tmpdir,
        "RESULT_DIR": result_dir,
        "here": str(tree_root),
        "MSGVERB": "text:action",
        "QA_CHECK_FS": "true",
        "DIFF_LENGTH": "10",
    }


def git_provenance(root):
    rec = {"path": str(root), "git": False}
    if not shutil.which("git"):
        rec["detail"] = "no git"
        return rec
    try:
        sha = subprocess.run(["git", "-C", str(root), "rev-parse", "HEAD"],
                             capture_output=True, text=True, timeout=60)
        rec["git"] = sha.returncode == 0
        if not rec["git"]:
            rec["detail"] = (sha.stderr or sha.stdout).strip()[:200]
            return rec
        rec["sha"] = sha.stdout.strip()
        when = subprocess.run(["git", "-C", str(root), "log", "-1", "--format=%cI"],
                              capture_output=True, text=True, timeout=60)
        rec["committed"] = when.stdout.strip()
        # -uall so an untracked directory is listed as its files, not as
        # `?? dir/`, which would hide the very path that matters here.
        dirty = subprocess.run(["git", "-C", str(root), "status", "--porcelain", "-uall"],
                               capture_output=True, text=True, timeout=180)
        # Every modified path is kept for the refusal decision. A cap on the
        # recorded copy is safe; a cap on the list that decides is not, because a
        # modified pinned file past position 20 would be invisible to it.
        rec["dirty_paths_all"] = [l for l in dirty.stdout.splitlines() if l.strip()]
        rec["dirty_paths"] = rec["dirty_paths_all"][:20]
        rec["dirty_path_count"] = len(rec["dirty_paths_all"])
        rec["dirty"] = bool(rec["dirty_paths_all"])
    except (OSError, subprocess.SubprocessError) as exc:
        rec["detail"] = f"{type(exc).__name__}: {exc}"
    return rec


def tool_version(path):
    if not path:
        return None
    try:
        out = subprocess.run([path, "--version"], capture_output=True, text=True, timeout=30)
        text = (out.stdout or out.stderr).strip()
        return text.splitlines()[0] if text else ""
    except (OSError, subprocess.SubprocessError):
        return "version probe failed"


def probe_capabilities(path, tree_root):
    """Capability notes are probe results. Nothing here is a literal claim.

    Every entry records how it was measured, so a reader can tell a measurement
    from a stated value without trusting this docstring. 22 entries are measured
    by a lookup, a stat or a getuid; one is derived from two lookups, and one is
    recorded absent by construction and says so in its own `how`.
    """
    caps = {}
    for name in ("autoconf", "automake", "libtool", "libtoolize", "m4", "aclocal",
                 "autoheader", "autoreconf", "getfattr", "setfattr", "attr",
                 "mkfs.ext4", "mkfs.xfs", "xfs_io", "mkfs"):
        found = shutil.which(name, path=path)
        caps[name] = {"present": bool(found), "path": found,
                      "how": f"PATH lookup of {name!r} with PATH={path}"}
    for rel, key in (("include/builddefs", "include/builddefs"),
                     ("include/config.h", "include/config.h"),
                     ("ltp/fsstress", "ltp/fsstress"),
                     ("ltp/fsx", "ltp/fsx"),
                     ("src/mkfile", "src/mkfile")):
        target = Path(tree_root) / rel
        caps[key] = {"present": target.is_file(),
                     "executable": bool(target.is_file() and os.access(target, os.X_OK)),
                     "how": f"stat of {target}"}
    caps["sbin_on_path"] = {"present": any(caps[n]["present"] for n in ("mkfs", "xfs_io")),
                            "mkfs": caps["mkfs"]["present"],
                            "xfs_io": caps["xfs_io"]["present"],
                            "how": "derived from the two PATH lookups above"}
    # A loop or scratch device would let ./check format a real test device.
    # Recorded as absent by construction: this gate never asks for one, so there
    # is no probe behind this value and the entry must not claim one.
    caps["block_scratch_device"] = {
        "present": False,
        "how": "stated, not probed",
        "detail": "not requested and not probed: this gate runs cases against "
                  "directory-backed arms with SCRATCH_DEV empty, and formatting a "
                  "device is out of scope for it",
    }
    caps["uid_is_root"] = {"present": os.getuid() == 0, "how": "os.getuid() == 0"}
    # 23 entries: 15 PATH lookups and 6 stat-or-getuid measurements, one derived
    # from two of those lookups, and one stated rather than probed.
    caps["_counts"] = {"measured": 21, "derived": 1, "stated": 1, "total": 23}
    return caps


# --- arm identity -----------------------------------------------------------

def fstype_of(path):
    """The filesystem type backing a path.

    Three probes, in order, because no single one is portable: `findmnt` is the
    best source and only exists on Linux, `stat -c %T` is GNU, and macOS `df`
    has no `-T` at all. Whichever answered is recorded by name, and an unreadable
    fstype stays None so the arm rules refuse rather than assume.
    """
    rec = {"fstype": None, "source": None, "mount_target": None,
           "maj_min": None, "fstype_from": None}
    findmnt = shutil.which("findmnt")
    if findmnt:
        out = subprocess.run([findmnt, "-n", "-o", "TARGET,FSTYPE,SOURCE,MAJ:MIN", "--target", str(path)],
                             capture_output=True, text=True, timeout=60)
        line = out.stdout.strip().splitlines()
        if line:
            parts = line[0].split(None, 3)
            rec["mount_target"] = parts[0] if parts else None
            rec["fstype"] = parts[1] if len(parts) > 1 else None
            rec["source"] = parts[2] if len(parts) > 2 else None
            rec["maj_min"] = parts[3] if len(parts) > 3 else None
            if rec["fstype"]:
                rec["fstype_from"] = "findmnt"
    if not rec["fstype"]:
        # GNU stat only. BSD `stat -f %T` reports the file type character, not a
        # filesystem type, so it is deliberately not used here.
        res = subprocess.run(["stat", "-c", "%T", str(path)], capture_output=True, text=True, timeout=30)
        value = res.stdout.strip()
        if res.returncode == 0 and value and value != "%T" and len(value) > 1:
            rec["fstype"] = value
            rec["fstype_from"] = "stat -c %T"
    if not rec["fstype"]:
        # Linux `df -PT` has the type in field 2. macOS has no -T.
        res = subprocess.run(["df", "-PT", str(path)], capture_output=True, text=True, timeout=60)
        lines = res.stdout.splitlines()
        if len(lines) > 1:
            fields = lines[-1].split()
            if len(fields) >= 2:
                rec["fstype"] = fields[1]
                rec["source"] = fields[0]
                rec["fstype_from"] = "df -PT"
    if not rec["fstype"]:
        # The mount table, longest matching mount point wins. This is the only
        # source that works on macOS, where `df` has no -T and `stat -f %T` is
        # the file type.
        res = subprocess.run(["mount"], capture_output=True, text=True, timeout=60)
        try:
            real = os.path.realpath(str(path))
        except OSError:
            real = str(path)
        best = None
        for line in res.stdout.splitlines():
            parts = line.split()
            # `<device> on <target> (<fstype>, <opts>)`, and some lines put the
            # type first, so the `on` keyword is what anchors the target.
            if "on" not in parts:
                continue
            idx = parts.index("on")
            if len(parts) <= idx + 2:
                continue
            device, target = parts[0], parts[idx + 1]
            fstype = None
            if len(parts) > idx + 2 and parts[idx + 2].startswith("("):
                fstype = parts[idx + 2].lstrip("(").split(",")[0]
            if not fstype:
                continue
            if real == target or real.startswith(target.rstrip("/") + "/"):
                if best is None or len(target) > len(best[0]):
                    best = (target, fstype, device)
        if best:
            rec["mount_target"], rec["fstype"], rec["source"] = best
            rec["fstype_from"] = "mount"
    return rec


def mount_identity(path):
    """What the kernel says about a path. Read-only, never walks the tree."""
    stat_res = os.stat(path)
    if not os.path.isdir(path):
        raise ValueError(f"{path} is not a directory")
    rec = {"path": str(path)}
    rec.update(fstype_of(path))
    rec["device_id"] = stat_res.st_dev
    rec["inode"] = stat_res.st_ino
    rec["is_dir"] = True
    return rec


def inside(child, root):
    """Containment by resolved path. Symlinks are resolved on both sides."""
    try:
        child_r = Path(child).resolve()
        root_r = Path(root).resolve()
    except (OSError, RuntimeError) as exc:
        raise ValueError(f"cannot resolve {child} or {root}: {exc}")
    return child_r == root_r or root_r in child_r.parents


def validate_arms(native_root, cowfs_root, require_distinct_fstype=True):
    """Refuse arm pairs that cannot support a comparison. Returns both records."""
    problems = []
    native_r = Path(native_root)
    cowfs_r = Path(cowfs_root)
    for label, root in (("native", native_r), ("cowfs", cowfs_r)):
        if not root.exists():
            problems.append(f"{label} root {root} does not exist")
            continue
        if root.is_symlink():
            problems.append(f"{label} root {root} is a symlink; give the real path")
            continue
        try:
            mount_identity(root)
        except (OSError, ValueError) as exc:
            problems.append(f"{label} root: {exc}")
    if problems:
        raise ValueError("; ".join(problems))
    native_id = mount_identity(native_r)
    cowfs_id = mount_identity(cowfs_r)
    if native_r.resolve() == cowfs_r.resolve():
        problems.append("native and cowfs roots are the same directory")
    elif inside(native_r, cowfs_r) or inside(cowfs_r, native_r):
        problems.append("native and cowfs roots overlap, one is inside the other")
    if native_id["device_id"] == cowfs_id["device_id"] and \
            native_id.get("maj_min") == cowfs_id.get("maj_min"):
        problems.append(
            f"both arms are on device {native_id['device_id']} "
            f"({native_id.get('maj_min')}); the arms would not be distinguishable")
    # An arm whose filesystem type could not be read is not a usable arm, and a
    # missing fstype must never pass a `.startswith` test by accident. Every
    # rule below is evaluated so the operator sees all of them at once.
    for label, rec in (("native", native_id), ("cowfs", cowfs_id)):
        if not rec.get("fstype"):
            problems.append(f"{label} root filesystem type could not be read for {rec['path']}")
        elif label == "cowfs" and not rec["fstype"].startswith("fuse"):
            problems.append(f"cowfs root fstype is {rec['fstype']!r}, not a FUSE mount; "
                            "a native directory is not a cowfs arm")
        elif label == "native" and rec["fstype"].startswith("fuse"):
            problems.append(f"native root fstype is {rec['fstype']!r}, a FUSE mount; "
                            "the native arm must be a native filesystem")
    if native_id.get("fstype") and cowfs_id.get("fstype") and \
            require_distinct_fstype and native_id["fstype"] == cowfs_id["fstype"]:
        problems.append("both arms report the same fstype")
    if problems:
        raise ValueError("; ".join(problems))
    return {"native": native_id, "cowfs": cowfs_id}


# --- the child-side observer ------------------------------------------------

# Printed by the case itself after it exits, so it is the child's own account of
# what it did and where. The harness parses these lines; a case that produces no
# complete block is INVALID, never PASS.
OBSERVER_BEGIN = "##G5-OBSERVER-BEGIN##"
OBSERVER_END = "##G5-OBSERVER-END##"
OBSERVER_WRAPPER = r"""#!/bin/sh
# Harness wrapper. Runs the case with cwd at the tree root, which is the only
# cwd where its `. ./common/preamble` resolves and where `$here` is the tree
# root. Then reports what the child actually saw, so a pass cannot be claimed
# from an exit code alone.
#
# argv: <test_dir> <case relative to tree root> <expected case id> <log path>
__g5_dir="$1"
__g5_case="$2"
__g5_expect="$3"
__g5_log="$4"
__g5_rc=0
"./$__g5_case" || __g5_rc=$?
# GNU stat first, BSD second, so the same script works on the Pi and the Mac.
__g5_dev=$(stat -c %d "$__g5_dir" 2>/dev/null) || __g5_dev=$(stat -f %d "$__g5_dir" 2>/dev/null)
# xfstests' own _fs_type is `df -PT`. GNU stat is the fallback. BSD
# `stat -f %T` is deliberately absent: it prints the file type character, not a
# filesystem type, and reporting that as an fstype would be a false identity.
__g5_fstype=$(df -PT "$__g5_dir" 2>/dev/null | awk 'NR==2 {print $2}')
[ -n "$__g5_fstype" ] || __g5_fstype=$(stat -c %T "$__g5_dir" 2>/dev/null)
__g5_mnt=""
if command -v findmnt >/dev/null 2>&1; then
    __g5_mnt=$(findmnt -n -o TARGET --target "$__g5_dir" 2>/dev/null | head -1)
fi
# Real I/O inside the directory the case was given, so "it passed" cannot mean
# "it did nothing". Read-only test dirs make this SKIPPED, which is not a pass.
__g5_probe="$__g5_dir/.g5-observer-probe"
__g5_io=SKIPPED
if mkdir -p "$__g5_probe" 2>/dev/null && printf 'witness\n' > "$__g5_probe/f" 2>/dev/null; then
    if [ "$(cat "$__g5_probe/f" 2>/dev/null)" = "witness" ]; then
        __g5_io=OK
    fi
    rm -f "$__g5_probe/f" 2>/dev/null
    rmdir "$__g5_probe" 2>/dev/null
fi
# What the case left behind in its own directory. A case that asserts and then
# cleans up leaves nothing, so this is recorded and compared between arms rather
# than required to be nonzero.
__g5_residue=$(ls -A "$__g5_dir" 2>/dev/null | grep -v '^\.g5-observer-probe$' | tr '\n' ' ')
__g5_bytes=$(wc -c < "$__g5_log" 2>/dev/null) || __g5_bytes=0
printf '%s\n' "$OBS_BEGIN"
printf 'CASE=[%s]\n' "$__g5_case"
printf 'EXPECT=[%s]\n' "$__g5_expect"
printf 'CASE_RC=[%s]\n' "$__g5_rc"
printf 'TEST_DIR=[%s]\n' "$__g5_dir"
printf 'DEVICE_ID=[%s]\n' "$__g5_dev"
printf 'FSTYPE=[%s]\n' "$__g5_fstype"
printf 'MOUNT=[%s]\n' "$__g5_mnt"
printf 'IO=[%s]\n' "$__g5_io"
printf 'RESIDUE=[%s]\n' "$__g5_residue"
printf 'LOG_BYTES=[%s]\n' "$__g5_bytes"
printf '%s\n' "$OBS_END"
exit $__g5_rc
"""


def write_observer(tmpdir):
    """The wrapper script, written once per run into the run's private tmp."""
    path = Path(tmpdir) / "observer.sh"
    body = (OBSERVER_WRAPPER
            .replace("$OBS_BEGIN", OBSERVER_BEGIN)
            .replace("$OBS_END", OBSERVER_END))
    path.write_text("#!/bin/sh\n" + body.split("\n", 1)[1])
    path.chmod(path.stat().st_mode | 0o755)
    return path


def parse_observer(log_path):
    """Read the child's own account. Returns a record; `complete` says whether
    every field the gate needs is present."""
    text = Path(log_path).read_text(errors="replace") if Path(log_path).exists() else ""
    rec = {"complete": False, "raw_bytes": len(text)}
    if OBSERVER_BEGIN not in text or OBSERVER_END not in text:
        rec["why"] = "no observer block: the case did not run through the harness wrapper"
        return rec
    block = text.split(OBSERVER_BEGIN, 1)[1].split(OBSERVER_END, 1)[0]
    fields = {}
    for line in block.splitlines():
        if "=[" in line:
            k, _, v = line.partition("=")
            fields[k.strip()] = v.strip().strip("[]")
    rec.update(fields)
    missing = [k for k in ("CASE", "EXPECT", "CASE_RC", "TEST_DIR", "IO", "RESIDUE", "LOG_BYTES")
               if k not in fields]
    if missing:
        rec["why"] = f"observer block incomplete, missing {missing}"
        return rec
    rec["complete"] = True
    return rec


# --- skip and no-op detection ----------------------------------------------

# The suite's own refusal grammar, plus the malformed-comparison signature that
# this gate was written after: a case that could not find a helper compared
# nothing and still exited 0.
SKIP_SIGNATURES = (
    (re.compile(r"command not found"), "a command was missing"),
    (re.compile(r"(No such file or directory.*(/src/|/ltp/))|((/src/|/ltp/)\S*.*No such file or directory)"),
     "a suite helper binary was missing"),
    (re.compile(r"unary operator expected|bad substitution"),
     "a shell comparison was malformed, so its assertion did not run"),
    (re.compile(r"_notrun\b|\bnotrun\b"), "the case refused to run"),
    (re.compile(r"Test not run|\bSkipped\b|\bskipped\b"), "the case skipped work"),
    (re.compile(r"\bFAIL\b|\bfail:\s"), "the log reports a failure"),
    (re.compile(r"\bnot supported\b|\bunsupported\b", re.I), "the case reports the feature unsupported"),
    (re.compile(r"\bERROR\b"), "the log reports an error"),
)


def scan_log(text):
    """What the log says about the case, independent of the exit code."""
    out = {"bytes": len(text), "skips": [], "empty": not text.strip()}
    for pattern, why in SKIP_SIGNATURES:
        if pattern.search(text):
            out["skips"].append(why)
    return out


# --- classification ---------------------------------------------------------

def classify_case(path, space_cap=DEFAULT_SPACE_CAP):
    """Classify one generic case from its own source. A hypothesis about safety,
    never a proof: the reviewed set in the allowlist is the authority."""
    src = path.read_text(errors="replace")
    reasons = []
    verdict = "SAFE"
    for group, name in ((NEEDS_DEVICE, "NEEDS_DEVICE"), (NEEDS_ROOT, "NEEDS_ROOT"),
                        (NEEDS_SCRATCH, "NEEDS_SCRATCH")):
        for pattern, why in group:
            hit = re.search(pattern, src, re.I if name != "NEEDS_SCRATCH" else 0)
            if hit:
                verdict = name
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
    for size in SIZE_RE.finditer(src):
        value = int(size.group(1)) * SIZE_UNIT.get(size.group(2) or "K", 1)
        if value > space_cap:
            verdict = "NEEDS_BIG_SPACE"
            reasons.append(f"line {src[:size.start()].count(chr(10)) + 1}: allocates "
                           f"{value >> 20} MiB, over the {space_cap >> 20} MiB cap")
    for cmd in DESTRUCTIVE.finditer(src):
        for ap in ABS_PATH.finditer(cmd.group(0)):
            path_text = ap.group(0)
            if path_text.startswith(SAFE_ABS_PREFIX) or path_text in ("/", "//"):
                continue
            verdict = "UNSAFE"
            reasons.append(f"line {src[:cmd.start()].count(chr(10)) + 1}: {cmd.group(1)} "
                           f"names absolute {path_text!r}")
    external = sorted({f"{d}/{f}" for d, f in SOURCE_RE.findall(src) if f not in ALLOWED_SOURCES})
    if external:
        if verdict == "SAFE":
            verdict = "UNREAD_SOURCE"
        reasons.append("sources unreviewed code: " + ", ".join(external))
    helpers = sorted({why for pattern, why in HELPER if re.search(pattern, src)})
    if helpers and verdict == "SAFE":
        verdict = "NEEDS_HELPER"
    reasons.extend(f"needs a helper: {h}" for h in helpers)
    return {"id": path.name, "verdict": verdict, "bytes": len(src), "reasons": reasons}


def classify_group(tests_root):
    group = Path(tests_root) / "generic"
    if not group.is_dir():
        raise FileNotFoundError(f"{group} is not a directory; --xfstests must name the tree root")
    out = []
    for case in sorted(group.iterdir(), key=lambda p: p.name):
        # Suffixed entries (`069_o_tmpfile`) and subdirectories (`307_recovery`)
        # are cases too, and a coverage denominator that skips them is wrong.
        if case.is_dir():
            continue
        if not re.fullmatch(r"[0-9]+[A-Za-z0-9_.-]*", case.name):
            continue
        if case.name.endswith((".cfg", ".out", ".default", ".nfs")) or ".out" in case.name:
            continue
        out.append(classify_case(case))
    return out


# The reviewed suite's identity. `--allowlist` chooses which reviewed cases run;
# it does not get to say which suite is the suite. A caller that points this gate
# at a tree it built itself, and writes a pin matching that tree, gets a receipt
# labelled harness proof and never xfstests acceptance.
REVIEWED_PIN_FILE = Path(__file__).resolve().with_name("xfstests-allowlist.txt")


def reviewed_pin():
    """The pin that ships with the gate, read fresh so a stale copy cannot leak."""
    pin, err = parse_allowlist(REVIEWED_PIN_FILE)
    if err or not pin:
        return None, f"the reviewed pin {REVIEWED_PIN_FILE} is unreadable: {err}"
    return pin, None


# --- the reviewed pin -------------------------------------------------------

def parse_allowlist(path=None):
    """The allowlist file is machine-readable pin data, not a comment block.
    Format: `key value` lines, `#` comments. Keys: tree_sha, case_count,
    case <id> <sha256>, common <relpath> <sha256>, runner <relpath> <sha256>,
    review <id> <note>."""
    path = Path(path or ALLOWLIST_FILE)
    if not path.exists():
        return None, f"{path} is missing"
    pin = {"tree_sha": None, "case_count": None, "cases": {}, "common": {},
           "runner": {}, "reviews": {}}
    for raw in path.read_text().splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        key = parts[0]
        if key == "tree_sha" and len(parts) == 2:
            pin["tree_sha"] = parts[1]
        elif key == "case_count" and len(parts) == 2:
            pin["case_count"] = int(parts[1])
        elif key == "case" and len(parts) == 3:
            pin["cases"][parts[1]] = parts[2]
        elif key == "common" and len(parts) == 3:
            pin["common"][parts[1]] = parts[2]
        elif key == "runner" and len(parts) == 3:
            pin["runner"][parts[1]] = parts[2]
        elif key == "review" and len(parts) >= 3:
            pin["reviews"][parts[1]] = " ".join(parts[2:])
        elif CASE_ID_RE.fullmatch(key):
            pin.setdefault("ids", []).append(key)
    return pin, None


# How far the source scan follows `.` lines. The pin is generated and verified at
# this depth, so the two can never claim different coverage; the reviewed cases
# settle at seven levels, and eight leaves the deepest one a margin without
# pretending to follow an unbounded chain.
CLOSURE_DEPTH = 8


def common_closure(case_path, tree_root, depth=None):
    """Every file an allowlisted case can execute, by reading its sources.

    `depth` defaults to CLOSURE_DEPTH, so generating a pin and verifying it walk
    the same graph and the two cannot claim different coverage.

    The case itself is pinned as a case, so only the files it pulls in are
    returned here. A `.` line inside a reviewed common file pulls in more, so
    this walks the whole chain rather than trusting one level. A file reached but
    not pinned is the caller's problem to hear about.
    """
    depth = CLOSURE_DEPTH if depth is None else depth
    tree_root = Path(tree_root).resolve()
    case_path = Path(case_path).resolve()
    try:
        case_rel = case_path.relative_to(tree_root).as_posix()
    except ValueError:
        case_rel = None
    seen = {}
    unresolved = []
    frontier = [(case_path, True)]
    for _ in range(depth):
        nxt = []
        for item, is_case in frontier:
            if not item.is_file():
                continue
            try:
                text = item.read_text(errors="replace")
            except OSError:
                continue
            if not is_case:
                try:
                    rel = item.relative_to(tree_root).as_posix()
                except ValueError:
                    rel = None
                if rel and rel != case_rel:
                    seen[rel] = sha256_file(item)
            for d, name in SOURCE_RE.findall(text):
                # Resolve against every directory the suite can source from, so a
                # file this scan cannot name is refused later rather than missed
                # here. `closure_unresolved` in the record is what makes that
                # visible.
                for base in (tree_root / d, tree_root / "common", tree_root / "generic",
                             tree_root / "xfs", tree_root / "tests" / "generic"):
                    cand = (base / name).resolve()
                    if cand.is_file() and cand != case_path:
                        nxt.append((cand, False))
                    elif base == tree_root / d:
                        unresolved.append(f"{d}/{name}")
        if not nxt:
            break
        frontier = nxt
    if unresolved:
        seen["__unresolved__"] = sorted(set(unresolved))
    return seen


def verify_source_pin(tree_root, records=None, allowlist=None):
    """Re-read the tree and compare every pinned byte before anything runs.
    Returns (ok, [problems], detail). Refuses on drift, dirt, or an unexpected
    commit; a built artifact in a pinned directory is allowed only when it is
    exactly the pinned path."""
    tree_root = Path(tree_root).resolve()
    problems = []
    detail = {}
    pin, err = parse_allowlist(allowlist)
    if err:
        return False, [err], detail
    prov = git_provenance(tree_root)
    detail["provenance"] = prov
    if not prov.get("git"):
        return False, [f"the tree at {tree_root} is not a git checkout: {prov.get('detail')}"], detail
    if pin["tree_sha"] and prov["sha"] != pin["tree_sha"]:
        problems.append(f"tree sha {prov['sha']} does not match the reviewed {pin['tree_sha']}")
    # A dirty worktree is refused. Building the suite leaves object files and
    # binaries in its own directories; that is build output, not a changed case,
    # so those are allowed by extension. Everything else modified is a refusal.
    pinned_paths = set(pin["cases"]) | set(pin["common"])
    build_suffixes = (".o", ".a", ".la", ".lo", ".log", ".so", ".d")
    dirty = list(prov.get("dirty_paths_all") or prov.get("dirty_paths") or [])
    unexpected_dirty, ignored_dirty = [], []
    for line in dirty:
        m = re.match(r"^..\s+(.*)$", line)
        path = (m.group(1) if m else line).strip().strip('"')
        rel = str(Path(path).relative_to(tree_root)) if str(path).startswith(str(tree_root)) else path
        if rel in pinned_paths:
            # Modified but hash-identical to the reviewed bytes is fine; a
            # different hash was already reported above.
            ignored_dirty.append(line)
            continue
        if rel.startswith(("src/", "ltp/", "lib/", "include/", "tests/common/")) and \
                (rel.endswith(build_suffixes) or "/" not in rel.split("/", 1)[1]):
            ignored_dirty.append(line)
            continue
        unexpected_dirty.append(line)
    detail["dirty_ignored"] = ignored_dirty
    detail["dirty_refused"] = unexpected_dirty
    if unexpected_dirty:
        problems.append("worktree is dirty in paths outside the pinned set: "
                        + "; ".join(unexpected_dirty[:5])
                        + (f" (and {len(unexpected_dirty) - 5} more)" if len(unexpected_dirty) > 5
                           else ""))
    # Every allowlisted case: source hash.
    cases_dir = tree_root / "tests" / "generic"
    for cid, want in sorted(pin["cases"].items()):
        path = cases_dir / cid
        if not path.is_file():
            problems.append(f"reviewed case {cid} is missing from the tree")
            continue
        got = sha256_file(path)
        detail.setdefault("case_sha", {})[cid] = got
        if got != want:
            problems.append(f"case {cid} sha256 {got[:12]} does not match the reviewed {want[:12]}")
    # Every transitive common file, hashed from the real tree.
    for cid in sorted(pin["cases"]):
        path = cases_dir / cid
        if not path.is_file():
            continue
        closure = common_closure(path, tree_root)
        detail.setdefault("closure", {})[cid] = closure
        for rel, got in sorted(closure.items()):
            if rel == "__unresolved__":
                # A source line this scan could not resolve to a file. That is a
                # gap in what was read, not a clean result, so it refuses.
                problems.append(f"case {cid} sources files this scan could not resolve: {got}")
                continue
            want = pin["common"].get(rel)
            if want is None:
                problems.append(f"case {cid} pulls in unpinned {rel}")
            elif got != want:
                problems.append(f"{rel} sha256 {got[:12]} does not match the reviewed {want[:12]}")
    # The reviewed set must still be the set the classifier calls safe. Drift is
    # a refusal, not a warning.
    if records is None:
        try:
            records = classify_group(tree_root / "tests")
        except FileNotFoundError as exc:
            return False, [str(exc)], detail
    computed = sorted(r["id"] for r in records if r["verdict"] == "SAFE")
    reviewed = sorted(pin["cases"])
    if computed != reviewed:
        added = sorted(set(computed) - set(reviewed))
        removed = sorted(set(reviewed) - set(computed))
        problems.append(f"allowlist drift: classifier-safe added {added}, removed {removed}")
    if pin["case_count"] is not None and len(computed) != pin["case_count"]:
        problems.append(f"classifier-safe count {len(computed)} does not match the pinned "
                        f"case_count {pin['case_count']}")
    return (not problems), problems, detail


# --- evidence ---------------------------------------------------------------

def for_json(rec):
    """The record without the raw log body.

    `log_text` is kept on the in-memory record so the outcome classifier can read
    the suite's own grammar out of it, and dropped here so a results file stays
    readable. The log itself is on disk at the recorded path.
    """
    return {k: v for k, v in rec.items() if k != "log_text"}


def append_jsonl(path, rec):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a") as fh:
        fh.write(json.dumps(rec, sort_keys=True) + "\n")
        fh.flush()
        os.fsync(fh.fileno())


def write_jsonl(path, records):
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w") as fh:
        for rec in records:
            fh.write(json.dumps(rec, sort_keys=True) + "\n")
        fh.flush()
        os.fsync(fh.fileno())


def prepare_dir(path):
    """A per-case directory that must be new and empty. Never repairs, reuses."""
    path = Path(path)
    if path.exists():
        raise FileExistsError(f"{path} already exists; a case directory is immutable per attempt")
    path.mkdir(parents=True)
    if any(path.iterdir()):
        raise RuntimeError(f"{path} is not empty")
    return path


# The suite's own verdict grammar, read from `check` itself rather than guessed.
# These are the exact lines check prints for a case that passed, was not run, or
# failed, plus the exit code it returns when any case failed.
CHECK_PASS_ALL = re.compile(r"^Passed all (\d+) tests$", re.M)
CHECK_FAILED_N = re.compile(r"^Failed (\d+) of (\d+) tests$", re.M)
CHECK_NOT_RUN = re.compile(r"^Not run: (.+)$", re.M)
CHECK_TEST_LINE = re.compile(r"^Ran: (.+)$", re.M)
CHECK_IGNORED = re.compile(r"^(.+) - unknown test, ignored$", re.M)


def suite_witness(text):
    """The suite's own verdict lines, taken by position and by how many there are.

    The runner streams the case's output too, so a case can print a line shaped
    exactly like the suite's summary. Two rules make those lines inert rather than
    a defence against them:

      * the runner names its testlist once, so a second `Ran:` line means the
        stream is not the runner's alone and the run is refused
      * the pass/fail summary is the runner's last line, so a summary printed by a
        case earlier in the stream is not read as the verdict

    Returns (ids, run_lines, summary_matches, problem).
    """
    run_lines = CHECK_TEST_LINE.findall(text)
    problem = None
    if len(run_lines) > 1:
        problem = (f"the runner's output has {len(run_lines)} `Ran:` lines, so the "
                   "stream is not the suite runner's alone")
    ids = []
    if run_lines:
        ids = [t.strip() for t in run_lines[0].split(":")[-1].split() if t.strip()]
    # The summary is the last one in the stream, never the first. Both forms are
    # collected with their position so stream order is kept rather than grouped.
    summaries = sorted(
        [(m.start(), m.group(0)) for m in
         list(CHECK_PASS_ALL.finditer(text)) + list(CHECK_FAILED_N.finditer(text))])
    return ids, run_lines, [line for _, line in summaries], problem


def parse_check_output(text, rc):
    """What the suite's own output says, and whether that is a pass.

    Only the suite's own grammar is read. A case's output is in the same stream,
    so the lines are taken by position and counted, not by pattern match alone.
    """
    ids, run_lines, summaries, problem = suite_witness(text)
    out = {"rc": rc, "pass": False, "passed": None, "failed": None, "total": None,
           "ran": ids, "not_run": CHECK_NOT_RUN.findall(text),
           "ignored": CHECK_IGNORED.findall(text), "why": None, "problem": problem}
    if problem:
        out["why"] = problem
        return out
    if not summaries:
        out["why"] = "check printed neither a pass nor a failure summary"
        return out
    last = summaries[-1]
    if re.fullmatch(r"Passed all (\d+) tests", last):
        out["passed"] = int(re.fullmatch(r"Passed all (\d+) tests", last).group(1))
        out["total"] = out["passed"]
    else:
        m = re.fullmatch(r"Failed (\d+) of (\d+) tests", last)
        out["failed"], out["total"] = int(m.group(1)), int(m.group(2))
        out["why"] = f"the suite reported {last}"
        return out
    if len(ids) != out["total"]:
        out["why"] = (f"the suite named {len(ids)} tests but counted {out['total']}; "
                      "the two must agree")
        return out
    if rc != 0:
        out["why"] = f"the suite reported a pass but exited {rc}"
        return out
    if out["not_run"]:
        out["why"] = f"the suite did not run {out['not_run']}"
        return out
    if out["ignored"]:
        out["why"] = f"the suite ignored {out['ignored']}"
        return out
    out["pass"] = True
    out["why"] = (f"the suite ran {len(ids)} tests and reported {last}, on the runner's "
                  "own last summary line")
    return out



def check_argv(case_rel):
    """How the suite's runner is asked to run one case.

    `check` takes a testlist of `<group>/<id>` and resolves it against the tree,
    so the id is passed as the harness knows it rather than as a filesystem path
    it would have to guess. Nothing else reaches check's argv.
    """
    return ["./check", "-d", case_rel]


# --- owned child registry ---------------------------------------------------
#
# A case is signalled only when the harness can still prove, at the moment of the
# signal, that the pid it holds is the process it spawned. Four things have to
# line up, and any of them failing means no signal at all:
#
#   1. the pid is in the spawn registry, so the harness spawned it
#   2. the child handle has not been reaped, so the pid was never recycled
#   3. the kernel's own record for that pid still matches what was recorded at
#      spawn: start time, process group and session
#   4. the process group and session are the harness's own spawn session, never
#      the harness's own group
#
# `start_new_session=True` makes the child's session and process group id equal
# its pid, so check 4 is what distinguishes a case from the harness itself.
#
# The signal is sent to the single pid the harness holds a handle for, not to a
# process group. That is the standing instruction, and it also means a
# descendant is not contained by the signal; that is recorded in the case record
# as `descendants_contained: false` rather than papered over.

SPAWNED = {}
SIGNAL_LOG = []


def proc_identity(pid):
    """What the kernel says about a pid right now, or None if it is gone.

    Reads /proc, so it is a measurement rather than an assumption. On a host with
    no /proc it returns None, and every caller then refuses to signal.
    """
    try:
        with open(f"/proc/{pid}/stat", "rb") as fh:
            raw = fh.read().decode("utf-8", "replace")
        with open(f"/proc/{pid}/cmdline", "rb") as fh:
            cmdline = fh.read().decode("utf-8", "replace").split("\0")
    except (OSError, ValueError):
        return None
    # comm can contain spaces and parentheses, so parse after the last ')'.
    close = raw.rfind(")")
    if close < 0:
        return None
    fields = raw[close + 2:].split()
    try:
        pgrp = int(fields[2])
        sid = int(fields[3])
        starttime = int(fields[19])
    except (IndexError, ValueError):
        return None
    return {"pid": pid, "pgrp": pgrp, "sid": sid, "starttime": starttime,
            "argv": [a for a in cmdline if a]}


def register_child(proc, argv, cwd):
    """Record what was spawned, before anything can go wrong."""
    ident = proc_identity(proc.pid)
    entry = {
        "pid": proc.pid,
        "proc": proc,
        "argv": list(argv),
        "cwd": str(cwd),
        "spawn_identity": ident,
        "reaped": False,
    }
    SPAWNED[proc.pid] = entry
    return entry


def forget_child(pid):
    entry = SPAWNED.pop(pid, None)
    if entry is not None:
        entry["reaped"] = True
    return entry


def child_owns_pid(entry):
    """Can this harness still prove the pid is the process it spawned?

    Returns (ok, reason). Every refusal reason is recorded, so a quarantine is
    auditable rather than silent.
    """
    if entry is None:
        return False, "no spawn registry entry for this pid"
    if entry.get("reaped"):
        return False, "the child handle was already reaped"
    proc = entry.get("proc")
    if proc is None:
        return False, "no child handle is held for this pid"
    if proc.poll() is not None:
        return False, "the child already exited"
    spawn = entry.get("spawn_identity")
    if not spawn:
        return False, "no spawn-time identity was recorded, so identity cannot be proven"
    now = proc_identity(entry["pid"])
    if now is None:
        return False, "the pid is gone from the kernel's process table"
    if now["starttime"] != spawn["starttime"]:
        return False, (f"start time differs: recorded {spawn['starttime']}, "
                       f"now {now['starttime']}, so this pid was reused")
    if now["pgrp"] != spawn["pgrp"] or now["sid"] != spawn["sid"]:
        return False, (f"process group or session changed: recorded "
                       f"pgrp={spawn['pgrp']} sid={spawn['sid']}, now "
                       f"pgrp={now['pgrp']} sid={now['sid']}")
    # start_new_session=True puts the child in its own session, so its sid is its
    # pid and its pgrp is its pid. A child that reports the harness's own group
    # or session has been reparented or joined to us, and is not ours to signal.
    harness_pgid = os.getpgrp()
    harness_sid = os.getsid(0)
    if now["sid"] == entry["pid"] and now["pgrp"] == entry["pid"]:
        if now["pgrp"] == harness_pgid or now["sid"] == harness_sid:
            return False, "the child is in the harness's own process group or session"
        return True, "owned spawn session, pid still ours"
    return False, (f"the child is not in its own spawn session "
                   f"(pgrp={now['pgrp']} sid={now['sid']})")


def signal_owned_child(entry, sig, grace=30):
    """Signal the single pid we hold a handle for, after proving ownership.

    Never raises. Returns a record of what happened, including a refusal.
    """
    ok, reason = child_owns_pid(entry)
    if not ok:
        rec = {"signalled": False, "signal": sig, "why": f"quarantined: {reason}"}
        SIGNAL_LOG.append(rec)
        return rec
    pid = entry["pid"]
    try:
        os.kill(pid, sig)
    except ProcessLookupError:
        rec = {"signalled": False, "signal": sig, "why": "ProcessLookupError: the pid vanished"
                                                            " between the check and the signal"}
        SIGNAL_LOG.append(rec)
        return rec
    except PermissionError:
        rec = {"signalled": False, "signal": sig, "why": "PermissionError"}
        SIGNAL_LOG.append(rec)
        return rec
    rec = {"signalled": True, "signal": sig, "pid": pid, "why": reason}
    SIGNAL_LOG.append(rec)
    return rec


def stop_child(entry, grace=30):
    """Terminate an owned case: SIGTERM, then SIGKILL, re-proving ownership each
    time. Logs are flushed by the caller before this runs."""
    out = {"term": None, "kill": None, "quarantined": None}
    term = signal_owned_child(entry, 15, grace)
    out["term"] = term
    if not term["signalled"]:
        out["quarantined"] = term["why"]
        return out
    try:
        entry["proc"].wait(timeout=grace)
        return out
    except subprocess.TimeoutExpired:
        pass
    kill = signal_owned_child(entry, 9, grace)
    out["kill"] = kill
    if not kill["signalled"]:
        out["quarantined"] = kill["why"]
        return out
    try:
        entry["proc"].wait(timeout=grace)
    except subprocess.TimeoutExpired:
        out["quarantined"] = "the child did not exit after SIGKILL"
    return out

def read_fd_all(fd):
    """Read a stream's bytes from offset 0 without moving its offset.

    The child wrote through the same open file description, so the offset sits at
    the end; a plain read would start there and find nothing.
    """
    try:
        size = os.fstat(fd).st_size
    except OSError:
        return ""
    if size <= 0:
        return ""
    try:
        return os.pread(fd, size, 0).decode("utf-8", "replace")
    except OSError:
        return ""


def measure_io(test_dir):
    """Do real I/O in a directory, on the filesystem that directory is on.

    Used by the check path, where the case is wrapped by the suite's runner and
    cannot print its own observer block. The probe directory is left behind and
    named so it is obvious in the evidence what created it.
    """
    probe = Path(test_dir) / ".g5-io-probe"
    try:
        probe.mkdir(parents=True, exist_ok=False)
        (probe / "f").write_text("witness\n")
        ok = (probe / "f").read_text() == "witness\n"
        (probe / "f").unlink()
        probe.rmdir()
        return "OK" if ok else "MISMATCH"
    except OSError:
        return "SKIPPED"


def check_receipt(check_out, check_err, tree_root, witness_text, case_rel, rc):
    """The receipt an accepted run must carry.

    A pass is admissible only when all of these hold, and each is measured rather
    than asserted:
      * `check` is a real file in this tree and is executable
      * the tree sha equals the reviewed one, from the tree's own git
      * the case the runner named is the case that was requested
      * the runner reported a positive count, and it is at least the number asked
      * the ids it named are exactly the ids requested: none missing, none extra,
        none duplicated
      * nothing was reported as not run or ignored
      * the runner's exit code is 0

    The receipt records which of these failed, so a refusal is legible.
    """
    out = {"ok": False, "probes": [], "tree_sha": None, "check": None,
           "ran_ids": [], "requested_ids": [case_rel], "problems": []}

    def probe(name, ok, detail):
        out["probes"].append({"probe": name, "ok": bool(ok), "detail": str(detail)})
        if not ok:
            out["problems"].append(f"{name}: {detail}")
        return ok

    runner = Path(tree_root) / "check"
    probe("check_present", runner.is_file(), str(runner))
    probe("check_executable", runner.is_file() and os.access(runner, os.X_OK), str(runner))
    if runner.is_file():
        out["check"] = {"path": str(runner), "sha256": sha256_file(runner),
                        "size": runner.stat().st_size}
    prov = git_provenance(tree_root)
    out["tree_sha"] = prov.get("sha")
    out["tree_clean"] = not prov.get("dirty")
    pin, perr = reviewed_pin()
    if perr or not pin:
        probe("reviewed_pin_readable", False, perr or "no reviewed pin")
        pin = {"tree_sha": None, "runner": {}}
    else:
        probe("reviewed_pin_readable", True, REVIEWED_PIN_FILE)
    # The tree must be the reviewed suite by the gate's own pin, so a caller that
    # supplies its own matching pin still gets harness proof and not acceptance.
    probe("tree_sha_is_reviewed", bool(pin["tree_sha"]) and prov.get("sha") == pin["tree_sha"],
          f"tree={prov.get('sha')} reviewed={pin['tree_sha']}")
    probe("tree_clean", not prov.get("dirty"), prov.get("dirty_paths"))

    # The executor is pinned by the reviewed pin, not by anything the run passes in.
    want_check = (pin.get("runner") or {}).get("check")
    if runner.is_file():
        got = out["check"]["sha256"]
        probe("check_sha_is_pinned", bool(want_check) and got == want_check,
              f"got={got} pinned={want_check} (pinned by {REVIEWED_PIN_FILE.name})")
    group = Path(tree_root) / "tests" / "generic" / "group.list"
    out["group_list"] = {"path": str(group), "present": group.is_file()}
    if not group.is_file():
        probe("group_list_selects_case", False,
              f"{group} is missing, so the suite selected nothing")
    else:
        out["group_list"]["sha256"] = sha256_file(group)
        ids = []
        for line in group.read_text(errors="replace").splitlines():
            parts = line.split()
            if len(parts) >= 2 and parts[0] in ("auto", "long", "medium", "short"):
                ids.append(parts[1])
        out["group_list"]["selected"] = ids
        bare = Path(case_rel).name
        probe("group_list_selects_case", bare in ids,
              f"{bare} in the suite's own group.list selection {ids[:12]}")

    ran = CHECK_TEST_LINE.search(witness_text)
    listed = []
    if ran:
        listed = [t.strip() for t in ran.group(1).split(":")[-1].split() if t.strip()]
    out["ran_ids"] = listed
    requested = [case_rel]
    missing = sorted(set(requested) - set(listed))
    extra = sorted(set(listed) - set(requested))
    dupes = sorted({i for i in listed if listed.count(i) > 1})
    probe("case_named", listed[:1] == requested, f"runner named {listed} wanted {requested}")
    probe("no_missing_ids", not missing, f"missing={missing}")
    probe("no_extra_ids", not extra, f"extra={extra}")
    probe("no_duplicate_ids", not dupes, f"duplicated={dupes}")
    probe("runner_exit_zero", rc == 0, f"rc={rc}")

    verdict = parse_check_output(witness_text, rc)
    out["suite_verdict"] = verdict
    probe("suite_reported_pass", verdict["pass"], verdict.get("why"))
    probe("suite_count_matches_request",
          verdict.get("passed") == len(requested) == len(listed),
          f"passed={verdict.get('passed')} requested={len(requested)} named={len(listed)}")
    probe("one_testlist_line", verdict.get("problem") is None, verdict.get("problem"))
    probe("nothing_not_run", not verdict.get("not_run"), verdict.get("not_run"))
    probe("nothing_ignored", not verdict.get("ignored"), verdict.get("ignored"))

    out["case_id"] = listed[0] if listed else None
    out["ok"] = not out["problems"]
    out["synthetic"] = is_synthetic_tree(tree_root)
    if out["synthetic"]:
        # A receipt from a tree this gate did not review describes the runner and
        # the harness, not xfstests. It is kept, labelled, and never acceptance.
        out["acceptance"] = False
        out["label"] = ("harness proof only: this tree is not the reviewed suite, "
                        "so this receipt is not xfstests acceptance evidence")
    elif not out["ok"]:
        out["acceptance"] = False
        out["label"] = "refused"
    else:
        # Source and build provenance for the case, read here rather than trusted
        # from the caller: the suite's own selection file and the executor's bytes.
        out["acceptance"] = True
        out["label"] = ("xfstests acceptance evidence: reviewed tree sha, pinned check "
                        "executor, case id from the suite's own runner, and the suite's "
                        "own group.list selection")
    case_path = Path(tree_root) / "tests" / case_rel
    out["case_source"] = {
        "case": sha256_file(case_path) if case_path.is_file() else None,
        "case_path": str(case_path),
        "group_list": (out.get("group_list") or {}).get("sha256"),
        "check": out.get("check"),
    }
    return out


def is_synthetic_tree(tree_root):
    """Whether a tree is the reviewed suite or something this lane built.

    Read from the reviewed pin, not from `--allowlist` and not from a path, so
    moving, renaming, or re-pinning a tree cannot turn a stand-in into the suite.
    """
    pin, err = reviewed_pin()
    if err or not pin or not pin["tree_sha"]:
        return True
    prov = git_provenance(tree_root)
    return prov.get("sha") != pin["tree_sha"]


def run_case_check(observer, case_rel, test_dir, tmpdir, result_dir, tree_root, timeout,
                   log=None):
    """Run one case through the suite's own runner, `check`.

    `check` is the supported path and the only one whose verdict this gate will
    accept as a pass. It needs TEST_DEV, and it will try to mount whatever
    TEST_DEV names; that is why TEST_DEV is the arm's own directory, so
    `_fs_type` finds it already a filesystem and `init_rc` skips the mount."""
    env = dict(os.environ)
    env.update(arm_env(str(test_dir), str(tmpdir), str(result_dir), str(tree_root)))
    # check needs its own result base, kept with the log under --out.
    result_base = Path(log).parent / "check-results" if log else Path(tmpdir) / "check-results"
    result_base.mkdir(parents=True, exist_ok=True)
    env["RESULT_BASE"] = str(result_base)
    log = Path(log) if log else Path(tmpdir) / "case.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    argv = check_argv(case_rel)
    started = time.time()
    with open(log, "ab", buffering=0) as fh:
        fh.write(f"# argv={argv} cwd={tree_root} test_dir={test_dir}\n".encode())
        fh.flush()
        # check's own stdout and stderr go to their own files, not into the case
        # log. That separation is what makes its summary lines unforgeable by the
        # case, and it keeps the evidence on disk for the receipt.
        check_dir = Path(log).parent / "check-streams"
        check_dir.mkdir(parents=True, exist_ok=True)
        check_out = Path(check_dir / f"{case_rel.replace('/', '_')}.out").open("w+b", buffering=0)
        check_err = Path(check_dir / f"{case_rel.replace('/', '_')}.err").open("w+b", buffering=0)
        proc = subprocess.Popen(argv, cwd=str(tree_root), env=env,
                                stdout=check_out, stderr=check_err,
                                stdin=subprocess.DEVNULL, start_new_session=True)
        # Registered at spawn, before anything can go wrong, so the identity that
        # a later signal is checked against was captured while it was still true.
        entry = register_child(proc, argv, tree_root)
        timed_out = False
        stop = None
        try:
            rc = proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            # The log is flushed and fsynced before any signal, so a failure row
            # keeps the output the case produced up to that point.
            fh.flush()
            os.fsync(fh.fileno())
            stop = stop_child(entry)
            rc = entry["proc"].returncode if entry["proc"].returncode is not None else -15
        finally:
            forget_child(proc.pid)
    killed = None
    if stop is not None:
        killed = ("signalled" if (stop["term"] or {}).get("signalled")
                  else f"quarantined, no signal: {stop.get('quarantined')}")
    text = log.read_text(errors="replace") if log.exists() else ""
    # The witness is read from check's own streams, never from the case's stdout.
    # A case can print anything, including a line shaped exactly like check's
    # summary, and the two are separated here so a forged banner is inert.
    witness_text = "".join(read_fd_all(f.fileno()) for f in (check_out, check_err)
                           if f is not None)
    rec = check_receipt(check_out, check_err, tree_root, witness_text, case_rel, rc)
    if rec is None:
        raise RuntimeError("the suite runner's own streams are unreadable")
    case_id = rec.pop("case_id")
    verdict = rec.pop("suite_verdict")
    # `Ran:` holds check's own testlist, which is what it resolved and ran.
    ran = CHECK_TEST_LINE.search(witness_text)
    case_id = None
    if ran:
        listed = [t.strip() for t in ran.group(1).split(":")[-1].split() if t.strip()]
        case_id = listed[0] if listed else None
    rec = {
        "kind": "case",
        "case": case_rel,
        "runner": "check",
        "invoked_as": case_rel,
        "rc": rc,
        "timed_out": timed_out,
        "kill": killed,
        "stop": stop,
        "descendants_contained": False,
        "wall_s": round(time.time() - started, 3),
        "test_dir": str(test_dir),
        "log": str(log),
        "suite_verdict": verdict,
        "receipt": rec,
        "scan": scan_log(text),
        "log_text": text,
        "observer": {
            "complete": case_id is not None,
            "CASE": case_id,
            "EXPECT": case_rel,
            "CASE_RC": str(rc),
            "TEST_DIR": str(test_dir),
            "IO": measure_io(test_dir),
            "RESIDUE": " ".join(sorted(
                p.name for p in Path(test_dir).iterdir())) if Path(test_dir).is_dir() else "",
            "LOG_BYTES": str(len(text)),
            "runner": "check",
        },
    }
    if case_id is not None and case_id != case_rel:
        rec["observer"]["complete"] = False
        rec["observer"]["CASE"] = f"check ran {case_id!r}"
    rec["outcome"], rec["outcome_why"] = classify_outcome(rec)
    return rec


def run_case(observer, case_rel, test_dir, tmpdir, result_dir, tree_root, timeout, log=None):
    """Run one case through the observer wrapper. The exit code is the case's own.

    `log` is passed in rather than derived from the case directory, so the case
    directory can live inside an arm root while the log stays under --out. That
    split is what makes the arm's own filesystem observable without putting the
    evidence on it. Returns a record; never raises for a case that merely failed.
    """
    env = dict(os.environ)
    env.update(arm_env(str(test_dir), str(tmpdir), str(result_dir), str(tree_root)))
    log = Path(log) if log else Path(tmpdir) / "case.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    # The case is named relative to the tree root, which is the observer's cwd.
    # `generic/005` would resolve to tests/generic/005 only from tests/, and the
    # suite's own `. ./common/preamble` needs the tree root.
    tree_rel = "tests/" + case_rel
    argv = [str(observer), str(test_dir), tree_rel, case_rel, str(log)]
    started = time.time()
    with open(log, "ab", buffering=0) as fh:
        fh.write(f"# argv={argv} cwd={tree_root} test_dir={test_dir}\n".encode())
        fh.flush()
        # The direct runner has no suite runner to separate from, so the case's
        # own output is the case log.
        proc = subprocess.Popen(argv, cwd=str(tree_root), env=env, stdout=fh,
                                stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                start_new_session=True)
        # Registered at spawn, before anything can go wrong, so the identity that
        # a later signal is checked against was captured while it was still true.
        entry = register_child(proc, argv, tree_root)
        timed_out = False
        stop = None
        try:
            rc = proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            # The log is flushed and fsynced before any signal, so a failure row
            # keeps the output the case produced up to that point.
            fh.flush()
            os.fsync(fh.fileno())
            stop = stop_child(entry)
            rc = entry["proc"].returncode if entry["proc"].returncode is not None else -15
        finally:
            forget_child(proc.pid)
    killed = None
    if stop is not None:
        killed = ("signalled" if (stop["term"] or {}).get("signalled")
                  else f"quarantined, no signal: {stop.get('quarantined')}")
    text = log.read_text(errors="replace") if log.exists() else ""
    obs = parse_observer(log)
    # The observer writes into the same stream, so the log holds its own bytes.
    obs["raw_tail"] = [l for l in text.splitlines() if l.strip()][-8:]
    scan = scan_log(text)
    rec = {
        "kind": "case",
        "case": case_rel,
        "runner": "direct",
        "invoked_as": tree_rel,
        "rc": rc,
        "timed_out": timed_out,
        "kill": killed,
        "stop": stop,
        "descendants_contained": False,
        "wall_s": round(time.time() - started, 3),
        "test_dir": str(test_dir),
        "log": str(log),
        "log_text": text,
        "observer": obs,
        "scan": scan,
    }
    rec["outcome"], rec["outcome_why"] = classify_outcome(rec)
    return rec


# The suite's own startup gate, read from `common/config`. `_fatal` prints one of
# these and exits, so its presence in a log means no case asserted anything.
SUITE_FATAL_RE = re.compile(
    r"(mkfs|mount|umount|perl|awk|sed|df|xfs_io|fsstress|fsx)\s+not found"
    r"|\$_?[A-Z_]*TEST_(DEV|SCRATCH_DEV|MNT)\b is not set"
    r"|common/(rc|config): Error", re.I)


def classify_outcome(rec):
    """A case's own outcome, from the child's account, before any comparison.

    Two runners, and the difference decides whether a pass is admissible at all.

    `check` is the suite's own runner. Its verdict grammar is the only one xfstests
    supports: it writes a result directory for the case and its own pass/fail
    report. A PASS requires that witness. Exit 0 on its own is not a verdict,
    because a case that could not find a helper compares nothing and still
    exits 0.

    `direct` invokes the case script without the suite's runner. It is here for
    diagnosis and it can never produce a PASS: there is no supported success
    witness, so the ceiling is UNMEASURABLE. Recording it as a pass is exactly
    the failure this gate was written after.
    """
    rc = rec["rc"]
    obs = rec["observer"]
    scan = rec["scan"]
    if rec["timed_out"]:
        return OUTCOME_REFUSED, f"timed out: {rec['kill']}"
    # Identity first: which case ran, against the one asked for. The two runners
    # report it differently, so each compares in its own terms and neither accepts
    # a case that is not the one requested.
    expect = str(obs.get("EXPECT"))
    ran_case = str(obs.get("CASE"))
    if rec.get("runner") == "check":
        identity_ok = ran_case == expect
        ran_desc = f"check ran {ran_case!r}, not the case asked for {expect!r}"
    else:
        # A direct invocation reports the path it ran, relative to the tree root.
        identity_ok = ran_case in (expect, "tests/" + expect)
        ran_desc = (f"the case that ran was {ran_case!r}, "
                    f"not the one asked for {expect!r}")
    # A hard failure the suite's own runner attributed to the requested case is a
    # measured failure of that case. Reporting it as a harness comparison problem
    # instead would let a real filesystem failure be explained away by an arm that
    # could not be measured, so it is judged before the observer's own bookkeeping.
    if rc != 0 and identity_ok:
        return OUTCOME_FAILED, (f"the case exited {rc}" +
                                (" and left no observer block" if not obs.get("complete") else ""))
    if not obs.get("complete"):
        return OUTCOME_SKIPPED, obs.get("why", "no complete observer block")
    if not identity_ok:
        return OUTCOME_SKIPPED, ran_desc
    if scan["skips"]:
        return OUTCOME_SKIPPED, "log says the case did not assert: " + ", ".join(scan["skips"])
    if rc != 0:
        return OUTCOME_FAILED, f"the case exited {rc}"
    # The suite's own refusal grammar, checked before any success claim. A log
    # carrying it means `common/config` exited and nothing asserted, whatever the
    # exit code was.
    if SUITE_FATAL_RE.search(rec.get("log_text") or ""):
        return OUTCOME_SKIPPED, "the suite's own startup gate refused: " + \
            SUITE_FATAL_RE.search(rec["log_text"]).group(0)
    if obs.get("IO") != "OK":
        return OUTCOME_SKIPPED, f"the case did no I/O in its own test directory (IO={obs.get('IO')})"
    if int(obs.get("LOG_BYTES") or 0) and scan["bytes"] < int(obs["LOG_BYTES"]) - 1:
        return OUTCOME_SKIPPED, "the log lost bytes the observer measured"
    if not obs.get("LOG_BYTES") and scan["empty"]:
        return OUTCOME_SKIPPED, "empty log and the observer measured no bytes"
    if rec.get("runner") == "check":
        receipt = rec.get("receipt") or {}
        if not receipt.get("ok"):
            # Every probe the receipt made, so a refusal says which one failed
            # rather than only that it did.
            failed = [f"{q['probe']}={q['detail']}" for q in receipt.get("probes", [])
                      if not q.get("ok")]
            return OUTCOME_SKIPPED, ("the suite receipt did not verify: "
                                     + "; ".join(failed[:6]))
        if not receipt.get("acceptance"):
            return OUTCOME_SKIPPED, ("receipt verified the runner but not the suite: "
                                     + str(receipt.get("label")))
        return OUTCOME_PASSED, ("the suite receipt verified: pinned executor, reviewed tree "
                                "sha, the requested case id, and the suite's own pass count")
    return OUTCOME_SKIPPED, ("direct invocation: the case exited 0 with a clean log, but "
                             "without the suite's runner there is no supported success "
                             "witness, so this cannot be a pass")


def preflight(tree, out_dir, timeout=DEFAULT_TIMEOUT, pin_check=True):
    """Check every prerequisite, then prove the answer by running one case.
    The suite's own exit code is the answer."""
    tests_root = tests_dir(tree)
    out_dir = Path(out_dir).resolve()
    tree_root = tests_root.parent
    src_root = tree_root
    env_path = os.environ.get("PATH", "")
    run_id = time.strftime("%Y%m%d-%H%M%S")
    rec = {
        "kind": "preflight",
        "run_id": run_id,
        "host": {
            "uname": subprocess.run(["uname", "-srm"], capture_output=True, text=True).stdout.strip(),
            "uid": os.getuid(),
            "is_root": os.getuid() == 0,
        },
        "path": env_path,
        "source": git_provenance(src_root),
        "capabilities": probe_capabilities(env_path, src_root),
        "tools": {},
        "startup_gate": [],
        "blocking": [],
    }
    if rec["host"]["is_root"]:
        rec["blocking"].append({"key": "uid", "detail": "refusing to run as root"})
    if not (tests_root / "generic").is_dir():
        rec["blocking"].append({"key": "tests_dir", "detail": f"{tests_root}/generic is not a directory"})
    # Pin check happens before any subprocess runs a case.
    if pin_check:
        ok, problems, pin_detail = verify_source_pin(src_root)
        rec["pin"] = {"ok": ok, "problems": problems, "detail": pin_detail}
        rec["pin_read"] = pin_detail.get("pin")
        if not ok:
            # Source drift is a wrong-input condition, not a missing capability.
            # The two must not share an exit code, or a CI step cannot tell a
            # moved tree from an unbuilt one.
            rec["invalid"] = problems
    tools = which_all([name for name, _ in STARTUP_GATE] + ["bash", "sh", "git"], env_path)
    for name, _ in STARTUP_GATE:
        rec["tools"][name] = {"path": tools.get(name), "version": tool_version(tools.get(name))}
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
                                    "fatal": fatal if not ok else None, "path": str(target)})
        if not ok:
            rec["blocking"].append({"key": rel, "detail": fatal})
    for rel, fatal in STARTUP_GATE_DATA:
        target = src_root / rel
        ok = target.is_file()
        rec["startup_gate"].append({"key": rel, "status": "present" if ok else "absent",
                                    "fatal": fatal if not ok else None, "path": str(target)})
        if not ok:
            rec["blocking"].append({"key": rel, "detail": fatal})
    # The answer that matters is the suite's own, so ask the suite. A missing
    # probe case is UNMEASURABLE with evidence, never a traceback and never FAIL.
    probe_case = "generic/010"
    probe_path = tests_root / probe_case
    probe = {"case": probe_case, "rc": None, "log": None, "why": None, "timeout_s": timeout}
    if not probe_path.is_file():
        probe["why"] = f"probe case {probe_case} is missing from the tree"
    else:
        tmp = out_dir / f"preflight-{run_id}"
        tmp.mkdir(parents=True, exist_ok=True)
        observer = write_observer(tmp)
        test_dir = prepare_dir(tmp / "testdir")
        result_dir = tmp / "results"
        result_dir.mkdir()
        try:
            # `check` first: it is the runner a real run would use, so its answer
            # is the one that matters. The direct wrapper is the fallback when the
            # tree has no runner.
            runner_path = src_root / "check"
            if runner_path.is_file() and os.access(runner_path, os.X_OK):
                probe_rec = run_case_check(observer, probe_case, test_dir, tmp, result_dir,
                                           src_root, timeout)
            else:
                probe_rec = run_case(observer, probe_case, test_dir, tmp, result_dir,
                                     src_root, timeout)
            probe = {"case": probe_case, "rc": probe_rec["rc"], "log": probe_rec["log"],
                     "outcome": probe_rec["outcome"], "why": probe_rec["outcome_why"],
                     "runner": probe_rec["runner"],
                     "suite_verdict": probe_rec.get("suite_verdict"),
                     "log_text": probe_rec.get("log_text", "")[-4000:],
                     "observer": probe_rec["observer"], "timeout_s": timeout}
        except (OSError, ValueError) as exc:
            probe = {"case": probe_case, "rc": None, "log": None,
                     "why": f"probe could not run: {type(exc).__name__}: {exc}"}
    rec["suite_probe"] = probe
    if rec.get("invalid"):
        rec["verdict"] = VERDICT_INVALID
        rec["reason"] = "; ".join(rec["invalid"][:5])
    elif probe["rc"] is not None and probe["rc"] == 0 and not rec["blocking"]:
        rec["verdict"] = VERDICT_OK
    else:
        rec["verdict"] = VERDICT_UNMEASURABLE
        if rec["blocking"]:
            rec["reason"] = "; ".join(f"{b['key']}: {b['detail']}" for b in rec["blocking"])
        elif probe["rc"] is None:
            rec["reason"] = probe["why"]
        else:
            tail = (probe.get("log_text") or probe.get("observer", {}).get("raw_tail") or [])
            ignored = (probe.get("suite_verdict") or {}).get("ignored") or []
            if ignored:
                rec["reason"] = (f"the suite did not run the probe case {ignored[0]!r}; "
                                 "check could not resolve it, which needs the group.list "
                                 "the suite build generates")
            else:
                first = next((l for l in tail if "_fatal" in l or "not found" in l), "")
                rec["reason"] = f"the suite refused a probe case with exit {probe['rc']}: {first}"
    # Evidence is written whatever happened, including a missing probe case.
    append_jsonl(out_dir / f"preflight-{run_id}.jsonl", rec)
    rec["evidence"] = str(out_dir / f"preflight-{run_id}.jsonl")
    return rec


def verdict_for_case(native, cowfs):
    """One case, both arms, each classified on its own account first.

    Never PASS on a missing assertion. Never PASS on a broken native arm. And
    never PASS when the two arms did demonstrably different work, because "both
    exited 0" over different activity is not evidence that cowfs matches native.
    """
    for label, rec in (("native", native), ("cowfs", cowfs)):
        if rec["outcome"] == OUTCOME_SKIPPED:
            return VERDICT_INVALID, (f"{label} arm did not assert: {rec['outcome_why']}")
        if rec["outcome"] == OUTCOME_REFUSED:
            return VERDICT_UNMEASURABLE, f"{label} arm refused: {rec['outcome_why']}"
    n_obs, c_obs = native.get("observer", {}), cowfs.get("observer", {})
    # Both arms really ran. Now the comparison is meaningful.
    if native["outcome"] == OUTCOME_FAILED:
        return VERDICT_UNMEASURABLE, (f"the native arm failed ({native['outcome_why']}); "
                                       "that is not a cowfs verdict")
    if cowfs["outcome"] == OUTCOME_FAILED and native["outcome"] == OUTCOME_PASSED:
        return VERDICT_FAIL, f"cowfs {cowfs['outcome_why']} where the native arm passed"
    if native["outcome"] == OUTCOME_PASSED and cowfs["outcome"] == OUTCOME_PASSED:
        n_res = (n_obs.get("RESIDUE") or "").split()
        c_res = (c_obs.get("RESIDUE") or "").split()
        if sorted(n_res) != sorted(c_res):
            return VERDICT_UNMEASURABLE, (
                f"the two arms left different residue: native {sorted(n_res)}, "
                f"cowfs {sorted(c_res)}; both exiting 0 over different work is not "
                "evidence that cowfs matches native")
        return VERDICT_OK, ("both arms recorded a pass witness with a clean log, and "
                             "they left the same residue behind")
    return VERDICT_UNMEASURABLE, f"native={native['outcome']} cowfs={cowfs['outcome']}"


def run(args):
    tree_root = tests_dir(args.xfstests).parent
    out = Path(args.out).resolve()
    if os.getuid() == 0:
        print("INVALID: refusing to run the suite as root", file=sys.stderr)
        return 3
    # Arms first: refuse a pair that cannot support a comparison before any case.
    try:
        arms = validate_arms(args.native_root, args.cowfs_root)
    except ValueError as exc:
        print(f"INVALID: {exc}", file=sys.stderr)
        return 3
    pre = preflight(args.xfstests, out, args.timeout)
    if pre["verdict"] != VERDICT_OK:
        # A source-pin failure is INVALID (3) and a missing prerequisite is
        # UNMEASURABLE (2). They are different problems and a CI step must be
        # able to tell them apart.
        code = {"INVALID": 3}.get(pre["verdict"], 2)
        print(f"VERDICT: {pre['verdict']}", file=sys.stderr)
        print(f"REASON: {pre.get('reason', 'prerequisite absent')}", file=sys.stderr)
        for item in pre.get("invalid") or []:
            print(f"SOURCE: {item}", file=sys.stderr)
        for item in pre["blocking"]:
            print(f"BLOCKING: {item['key']}: {item['detail']}", file=sys.stderr)
        print(f"EVIDENCE: {pre.get('evidence')}", file=sys.stderr)
        return code
    records = classify_group(tree_root / "tests")
    allow, drift = check_allowlist_drift(records)
    if drift:
        print(f"INVALID: {drift}", file=sys.stderr)
        return 3
    requested = [c.strip() for c in (args.cases or ",".join(allow)).split(",") if c.strip()]
    refused = [c for c in requested if c not in allow]
    if refused:
        print(f"INVALID: not in the reviewed allowlist: {refused}", file=sys.stderr)
        return 3
    run_dir = out / f"run-{time.strftime('%Y%m%d-%H%M%S')}"
    results = run_dir / "results.jsonl"
    pin_detail = pre.get("pin", {}).get("detail", {})
    meta = {"kind": "meta", "run_id": run_dir.name, "cases": requested,
            "tree_sha": pre["source"].get("sha"), "path": pre["path"],
            "arms": arms, "timeout_s": args.timeout, "runner": args.runner,
            "expected_cases": len(records),
            "pin": {"tree_sha": pin_detail.get("provenance", {}).get("sha"),
                    "case_sha": pin_detail.get("case_sha"),
                    "closure": pin_detail.get("closure")}}
    write_jsonl(results, [meta])
    tallies = {VERDICT_OK: 0, VERDICT_FAIL: 0, VERDICT_UNMEASURABLE: 0, VERDICT_INVALID: 0}
    observer_dir = run_dir / "observer"
    observer_dir.mkdir(parents=True, exist_ok=True)
    observer = write_observer(observer_dir)
    for cid in requested:
        case = f"generic/{cid}"
        # Each arm's per-case directory lives inside its own arm root.
        case_work = {}
        arm_recs = {}
        for arm in ("native", "cowfs"):
            arm_root = Path(arms[arm]["path"])
            work = arm_root / run_dir.name / f"{cid}-{arm}"
            test_dir = prepare_dir(work / "testdir")
            tmpdir = prepare_dir(work / "tmp")
            result_dir = prepare_dir(work / "results")
            case_work[arm] = {"work": work, "test_dir": test_dir,
                              "tmpdir": tmpdir, "result_dir": result_dir}
            # The case directory is inside the arm root, so it measures the arm's
            # filesystem. The log is under --out, so the evidence does not land on
            # the filesystem under test.
            log = run_dir / "logs" / f"{cid}-{arm}.log"
            if args.runner == "check":
                arm_recs[arm] = run_case_check(observer, case, test_dir, tmpdir,
                                               result_dir, tree_root, args.timeout,
                                               log=log)
            else:
                arm_recs[arm] = run_case(observer, case, test_dir, tmpdir,
                                         result_dir, tree_root, args.timeout, log=log)
        verdict, why = verdict_for_case(arm_recs["native"], arm_recs["cowfs"])
        tallies[verdict] += 1
        rec = {"kind": "case_verdict", "case": case, "verdict": verdict, "why": why,
               "native_rc": arm_recs["native"]["rc"], "cowfs_rc": arm_recs["cowfs"]["rc"],
               "native_outcome": arm_recs["native"]["outcome"], "cowfs_outcome": arm_recs["cowfs"]["outcome"],
               "native_log": arm_recs["native"]["log"], "cowfs_log": arm_recs["cowfs"]["log"],
               "native_skips": arm_recs["native"]["scan"]["skips"],
               "cowfs_skips": arm_recs["cowfs"]["scan"]["skips"],
               "native_test_dir": str(case_work["native"]["test_dir"]),
               "cowfs_test_dir": str(case_work["cowfs"]["test_dir"]),
               "native_observer": for_json(arm_recs["native"]["observer"]),
               "cowfs_observer": for_json(arm_recs["cowfs"]["observer"]),
               "native_suite_verdict": arm_recs["native"].get("suite_verdict"),
               "cowfs_suite_verdict": arm_recs["cowfs"].get("suite_verdict"),
               "native_receipt": for_json(arm_recs["native"].get("receipt") or {}),
               "cowfs_receipt": for_json(arm_recs["cowfs"].get("receipt") or {}),
               "native_source_sha": pin_detail.get("case_sha", {}).get(cid),
               "cowfs_source_sha": pin_detail.get("case_sha", {}).get(cid),
               "runner": args.runner,
               "native_wall_s": arm_recs["native"]["wall_s"],
               "cowfs_wall_s": arm_recs["cowfs"]["wall_s"]}
        append_jsonl(results, for_json(rec))
        print(f"{cid}\t{verdict}\tnative={arm_recs['native']['rc']} cowfs={arm_recs['cowfs']['rc']}\t{why}")
    covered = len(requested)
    total = len(records)
    print(f"COVERAGE: {covered} of {total} generic cases, reviewed set {sha_of(sorted(allow))[:12]}")
    print(f"COUNTS: pass={tallies[VERDICT_OK]} fail={tallies[VERDICT_FAIL]} "
          f"unmeasurable={tallies[VERDICT_UNMEASURABLE]} invalid={tallies[VERDICT_INVALID]}")
    pinned_total = pin_case_count(pre.get("pin_read"))
    if args.require_full and covered != pinned_total:
        print(f"VERDICT: {VERDICT_UNMEASURABLE}", file=sys.stderr)
        print(f"REASON: --require-full and only {covered} of {pinned_total} reviewed cases ran",
              file=sys.stderr)
        return 2
    if tallies[VERDICT_FAIL]:
        print(f"VERDICT: {VERDICT_FAIL}", file=sys.stderr)
        return 1
    if tallies[VERDICT_OK] == 0:
        print(f"VERDICT: {VERDICT_UNMEASURABLE}", file=sys.stderr)
        return 2
    print(f"VERDICT: {VERDICT_OK}")
    if covered != len(records):
        print(f"SCOPE: PARTIAL. {covered} of {len(records)} generic cases ran. G5 stays OPEN.")
    return 0


def pin_case_count(pin=None):
    """The pinned denominator, from the same read the run already verified.

    Re-reading the file here would let the count come from a pin that changed
    after the cases ran, which is exactly the drift the first read refused.
    """
    if pin is None:
        pin, _ = parse_allowlist()
    return (pin or {}).get("case_count") or 0


def check_allowlist_drift(records, allowlist=None):
    """Drift is a refusal everywhere, including this report path."""
    pin, err = parse_allowlist(allowlist)
    if err:
        return [], err
    computed = sorted(r["id"] for r in records if r["verdict"] == "SAFE")
    reviewed = sorted(pin["cases"])
    if computed != reviewed:
        added = sorted(set(computed) - set(reviewed))
        removed = sorted(set(reviewed) - set(computed))
        return computed, f"allowlist drift: classifier-safe added {added}, removed {removed}"
    return computed, None


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
    if args.verbose:
        for rec in records:
            if rec["verdict"] != "SAFE":
                print(f"{rec['id']}\t{rec['verdict']}\t{'; '.join(rec['reasons'])}")
    print("SUMMARY: " + " ".join(f"{k}={v}" for k, v in sorted(summary.items())))
    print(f"REVIEWED: {len(allow)} cases, sha {sha_of(allow)[:12]}")
    print(f"DRIFT: {drift or 'none'}")
    if drift:
        print(f"INVALID: {drift}", file=sys.stderr)
        return 3
    return 0


def preflight_cmd(args):
    rec = preflight(args.xfstests, Path(args.out), args.timeout, pin_check=not args.no_pin_check)
    if rec["verdict"] == VERDICT_INVALID:
        for problem in rec.get("invalid") or []:
            print(f"SOURCE: {problem}")
    print(f"VERDICT: {rec['verdict']}")
    if rec.get("reason"):
        print(f"REASON: {rec['reason']}")
    print(f"SOURCE: {rec['source'].get('sha', 'unknown')} ({rec['source'].get('committed', '?')})")
    print(f"PIN: {rec.get('pin', {}).get('ok')} {rec.get('pin', {}).get('problems')}")
    print(f"PATH: {rec['path']}")
    for gate in rec["startup_gate"]:
        print(f"GATE: {gate['key']}\t{gate['status']}\t{gate.get('fatal') or gate.get('path')}")
    caps = rec["capabilities"]
    print(f"CAPS: measured={caps['_counts']['measured']} "
          f"derived={caps['_counts']['derived']} stated={caps['_counts']['stated']} "
          f"total={caps['_counts']['total']} "
          f"uid_root={caps['uid_is_root']['present']} "
          f"autoconf={caps['autoconf']['present']} automake={caps['automake']['present']} "
          f"libtool={caps['libtool']['present']} m4={caps['m4']['present']} "
          f"getfattr={caps['getfattr']['present']} fsstress={caps['ltp/fsstress']['present']}")
    print(f"PROBE: {rec['suite_probe'].get('case')} rc={rec['suite_probe'].get('rc')} "
          f"outcome={rec['suite_probe'].get('outcome')} log={rec['suite_probe'].get('log')}")
    if rec["suite_probe"].get("log") and Path(rec["suite_probe"]["log"]).exists():
        for line in Path(rec["suite_probe"]["log"]).read_text(errors="replace").splitlines()[-4:]:
            print(f"  | {line[:150]}")
    print(f"EVIDENCE: {rec.get('evidence')}")
    if rec["verdict"] == VERDICT_OK:
        return 0
    return {"INVALID": 3}.get(rec["verdict"], 2)


def report(args):
    results = Path(args.run) / "results.jsonl"
    if not results.exists():
        print(f"INVALID: {results} does not exist", file=sys.stderr)
        return 3
    rows = [json.loads(l) for l in results.read_text().splitlines() if l.strip()]
    cases = [r for r in rows if r.get("kind") == "case_verdict"]
    meta = next((r for r in rows if r.get("kind") == "meta"), {})
    counts = {}
    for rec in cases:
        counts[rec["verdict"]] = counts.get(rec["verdict"], 0) + 1
    print(f"RUN: {meta.get('run_id')} tree={str(meta.get('tree_sha'))[:12]} runner={meta.get('runner')}")
    for arm in ("native", "cowfs"):
        arm_id = (meta.get("arms") or {}).get(arm, {})
        print(f"ARM {arm}: fstype={arm_id.get('fstype')} dev={arm_id.get('device_id')} "
              f"mount={arm_id.get('mount_target')} source={arm_id.get('source')}")
    for rec in cases:
        print(f"{rec['case']}\t{rec['verdict']}\tnative={rec['native_rc']}({rec.get('native_outcome')}) "
              f"cowfs={rec['cowfs_rc']}({rec.get('cowfs_outcome')})\t{rec['why']}")
    print("COUNTS: " + (" ".join(f"{k}={v}" for k, v in sorted(counts.items())) or "no cases"))
    print(f"COVERAGE: {len(cases)} of {meta.get('expected_cases', '?')} reviewed cases")
    # A report is a printer, but a CI step wired to it must not go green on a
    # failing or invalid run.
    if counts.get(VERDICT_FAIL):
        return 1
    if counts.get(VERDICT_INVALID):
        return 3
    if counts.get(VERDICT_UNMEASURABLE) or not cases:
        return 2
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    def add_common(p):
        p.add_argument("--xfstests", default=os.environ.get("COWFS_XFSTESTS_SRC"),
                       required=os.environ.get("COWFS_XFSTESTS_SRC") is None,
                       help="the xfstests tree root, the directory that holds tests/")
        p.add_argument("--out", default=os.environ.get("COWFS_XFSTESTS_OUT", str(REPO / "bench" / "out" / "ready-g5")))
        p.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT)

    p = sub.add_parser("preflight")
    add_common(p)
    p.add_argument("--no-pin-check", action="store_true",
                   help="diagnose the suite gate without the source pin (control use only)")
    p.set_defaults(func=preflight_cmd)

    p = sub.add_parser("classify")
    add_common(p)
    p.add_argument("--verbose", action="store_true")
    p.set_defaults(func=classify_cmd)

    p = sub.add_parser("run")
    add_common(p)
    p.add_argument("--native-root", required=True)
    p.add_argument("--cowfs-root", required=True)
    p.add_argument("--cases", help="comma separated generic ids, must be in the reviewed set")
    p.add_argument("--require-full", action="store_true")
    p.add_argument("--runner", choices=("check", "direct"), default="check",
                   help="check is the suite's own runner and the only one whose "
                        "verdict can be a pass; direct invokes the case script for "
                        "diagnosis and can never record a pass")
    p.set_defaults(func=run)

    p = sub.add_parser("report")
    p.add_argument("--run", required=True)
    p.set_defaults(func=report)

    args = ap.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())