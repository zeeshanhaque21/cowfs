#!/usr/bin/env python3
"""Matched pjdfstest acceptance for a private real-Core cowfs mount against a native baseline.

Gate g3 of docs/ready-wave-dispatch.md. One pinned pjdfstest build runs the same case list on two
arms of the same host: a native directory and a mount served by `cowfs-daemon --backend core`.

    bench/pjdfstest.py --out bench/out/ready-g3/run
    bench/pjdfstest.py --reconcile bench/out/ready-g3/run/<stamp>

## What a verdict is allowed to rest on

An assertion is identified by the operation its script performs, never by its position in the
stream and never by its error text.
The pinned script is the authority: an identity counts as established only when the script
proves the same operation, which is why a repeated operation needs a literal `for` loop in the
script to prove its iteration order, and why an assertion with no operation text (the suite's
`test_check`) can never be identified and is reported as unpairable.

A run that cannot be measured is UNMEASURABLE and a run whose records are malformed is INVALID.
Neither is ever a pass, and neither is ever reported as a filesystem failure.

An ordinal-worse position (native passes, cowfs fails at the same stream position) is a FAIL
unless bench/pjdfstest-accepted-divergences.json lists it by (test, n) with an issue and a reason.
Listed entries that are no longer worse are reported, so the list cannot rot.
Established regressions are scored separately and are never waived by that list.

Exit status: 0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
import re
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

PASS, FAIL, UNMEASURABLE, INVALID = "PASS", "FAIL", "UNMEASURABLE", "INVALID"
EXIT_STATUS = {PASS: 0, FAIL: 1, UNMEASURABLE: 2, INVALID: 3}
# Why a verdict is what it is, kept apart from the wording of the message.
#   INTEGRITY   the run's own records or provenance cannot be trusted: INVALID, exit 3
#   CAPABILITY  the tool, a prerequisite or a capability is absent: UNMEASURABLE, exit 2
#   COVERAGE    a limit on what the transcript can conclude, disclosed, not an exit on its own
#   DIVERGENCE  an established assertion passes on one arm and fails on the other: FAIL, exit 1
INTEGRITY, CAPABILITY, COVERAGE, DIVERGENCE = "INTEGRITY", "CAPABILITY", "COVERAGE", "DIVERGENCE"


def reason(kind: str, message: str) -> dict:
    return {"kind": kind, "message": message}


def state_from(reasons: list[dict]) -> str:
    """Integrity outranks everything, then a real divergence, then an absent capability."""
    kinds = {r["kind"] for r in reasons}
    if INTEGRITY in kinds:
        return INVALID
    if DIVERGENCE in kinds:
        return FAIL
    if CAPABILITY in kinds:
        return UNMEASURABLE
    return PASS
MOUNTED, NOT_MOUNTED, UNKNOWN = "MOUNTED", "NOT_MOUNTED", "UNKNOWN"
# How a pair of arms was matched. Only ESTABLISHED can carry a defect; the rest are reported as
# scope this transcript cannot adjudicate.
ESTABLISHED, CANDIDATE, UNPAIRABLE = "ESTABLISHED", "CANDIDATE", "UNPAIRABLE"

# (config.h macro, probe source). A macro is defined only when its probe compiles and links, which
# is what autoconf's AC_CHECK_FUNCS does.
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
TRIED_RE = re.compile(r"tried '([^']*)'")
# misc.sh generates every file name with namegen into ${n0}, ${n1}, ... and prints it as
# pjdfstest_<hex>. Both spellings are a generated name, and its position in the argument list is
# what identifies the operation. This is operation text, never outcome text.
GENERATED_RE = re.compile(r"pjdfstest_[0-9a-f]{6,}")
NAME_TOKEN_RE = re.compile(r"pjdfstest_[0-9a-f]{6,}|\$\{(?:n[0-9x]*|name)\}|\$n[0-9]+")
ROOT_REQUIRED_RE = re.compile(r"(?:^|\s)-(?:u|g)\s|\bmknod\s|not root")
# A condition whose result depends on a previous command's outcome makes the assertion sequence
# diverge between arms, so the script cannot prove which assertion a later one is.
RESULT_DEPENDENT_RE = re.compile(r"\$\?|\[\[|\$\(\(|-\s(eq|ne|lt|gt|true|false)\b|&&|\|\||;\s*then|;\s*fi|\bcase\b|\bif\b")
LITERAL_FOR_RE = re.compile(r"^\s*for\s+(\w+)\s+in\s+([^\s;]+(\s+[^\s;]+)*)\s*;", re.MULTILINE)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with Path(path).open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def log(msg: str) -> None:
    print(msg, flush=True)


# ---------------------------------------------------------------- mount table


def _unescape(field: str) -> str:
    """Decode the octal escapes /sbin/mount prints inside a path."""
    return re.sub(r"\\(\d{3})", lambda m: chr(int(m.group(1), 8)), field)


def mount_table(timeout: int = 15) -> tuple[str | None, str]:
    """The mount table, or (None, reason). A table that could not be read whole is not evidence."""
    try:
        done = subprocess.run(
            ["/sbin/mount"], capture_output=True, text=True, timeout=timeout, check=False
        )
    except subprocess.TimeoutExpired:
        return None, f"/sbin/mount did not finish within {timeout}s"
    except OSError as exc:
        return None, f"/sbin/mount could not run: {exc}"
    if done.returncode != 0:
        return None, f"/sbin/mount exited {done.returncode}"
    if not done.stdout.strip():
        return None, "/sbin/mount printed nothing"
    if not done.stdout.endswith("\n"):
        return None, "/sbin/mount output is truncated (no trailing newline)"
    return done.stdout, ""


def mount_entries(table: str) -> list[tuple[str, str, str]]:
    """(source, decoded mount point, options) per line. macOS escapes a space in a path."""
    entries = []
    for line in table.splitlines():
        if " on " not in line:
            continue
        source, rest = line.split(" on ", 1)
        if " (" not in rest:
            continue
        mountpoint, options = rest.rsplit(" (", 1)
        entries.append((source.strip(), _unescape(mountpoint.strip()), options.rstrip(")")))
    return entries


def mount_state(path: Path, timeout: int = 15) -> tuple[str, str]:
    """MOUNTED on an exact decoded match, NOT_MOUNTED only from a complete table, else UNKNOWN.

    A caller that gets UNKNOWN must not unmount, walk or delete anything: absence was not observed.
    """
    table, why = mount_table(timeout)
    if table is None:
        return UNKNOWN, why
    target = str(path)
    for source, mountpoint, options in mount_entries(table):
        if mountpoint == target:
            return MOUNTED, f"{source} on {mountpoint} ({options})"
    return NOT_MOUNTED, f"{target} is absent from a complete mount table of {len(table.splitlines())} lines"


def validate_runtime_identity(identity: dict | None,
                              expected_cowfs_mount: str | None = None) -> list[dict]:
    """Refuse to score unless both arms are on a filesystem we actually identified.

    An absent device, an absent type, a stat that failed or a missing mount point is not a
    measurement, and two arms that report the same device are one arm.

    This runs before any **case** child process exists and before the identity receipt is trusted.
    It cannot run before the daemon does: the cowfs arm's filesystem only exists once the mount is
    up, so the daemon and its snapshot are already serving when this is called. What it guarantees
    is that no case is run, and no receipt is accepted, on an unplaceable arm.
    """
    problems: list[dict] = []
    if not identity:
        return [reason(INTEGRITY, "no runtime identity was supplied, so no arm can be placed")]
    for arm in ("native", "cowfs"):
        record = identity.get(arm) or {}
        where = record.get("path") or arm
        if record.get("problem"):
            problems.append(reason(INTEGRITY, f"{arm} arm identity is unusable: {record['problem']}"))
        device = record.get("st_dev")
        if device is None:
            problems.append(reason(INTEGRITY, f"{arm} arm reported no st_dev for {where}"))
        elif not isinstance(device, int) or isinstance(device, bool) or device <= 0:
            problems.append(reason(INTEGRITY, f"{arm} arm st_dev {device!r} is not a device number"))
        fstype = record.get("fstype")
        if not fstype or not isinstance(fstype, str):
            problems.append(reason(INTEGRITY, f"{arm} arm reported no filesystem type for {where}"))
        if not record.get("mountpoint"):
            problems.append(reason(INTEGRITY, f"{arm} arm has no mount point in the mount table"))
    if expected_cowfs_mount:
        cowfs_mount = (identity.get("cowfs") or {}).get("mountpoint")
        if cowfs_mount and cowfs_mount != str(expected_cowfs_mount):
            problems.append(reason(INTEGRITY, f"cowfs arm is mounted at {cowfs_mount}, not at the "
                                              f"path this run asked the daemon for, "
                                              f"{expected_cowfs_mount}"))
    native, cowfs = identity.get("native") or {}, identity.get("cowfs") or {}
    if isinstance(native.get("st_dev"), int) and native.get("st_dev") == cowfs.get("st_dev"):
        problems.append(reason(INTEGRITY, f"both arms report st_dev {native['st_dev']}, so they are "
                                          "not two filesystems"))
    return problems


def fs_identity(path: Path, timeout: int = 15) -> dict:
    """Which filesystem a path is on, from the mount table and st_dev. Never inferred from the OS."""
    resolved = Path(path).resolve()
    identity = {
        "path": str(resolved),
        "st_dev": None,
        "mountpoint": None,
        "source": None,
        "fstype": None,
        "table_line": None,
        "problem": None,
    }
    try:
        identity["st_dev"] = os.stat(resolved).st_dev
    except OSError as exc:
        identity["problem"] = f"stat failed: {exc}"
        return identity
    table, why = mount_table(timeout)
    if table is None:
        identity["problem"] = why
        return identity
    best = ""
    for source, mountpoint, options in mount_entries(table):
        if (resolved == Path(mountpoint) or str(resolved).startswith(mountpoint.rstrip("/") + "/")) and \
                len(mountpoint) >= len(best):
            best, identity["mountpoint"] = mountpoint, mountpoint
            identity["source"], identity["fstype"] = source, options.split(",")[0].split("(")[0]
    if identity["mountpoint"] is None:
        identity["problem"] = f"no mount table entry covers {resolved}"
    return identity


# ---------------------------------------------------------------- tool


def probe(work: Path, name: str, source: str) -> bool:
    c, out = work / f"probe-{name}.c", work / f"probe-{name}.bin"
    c.write_text(source)
    try:
        rc = subprocess.run([CC, str(c), "-o", str(out)], capture_output=True, timeout=120,
                            check=False).returncode
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
        source = f"{BASE_INCLUDES}\nint main(void){{void *p=(void*)&{fn};return p!=0;}}\n"
        if probe(work, fn, source):
            found.append(f"HAVE_{fn.upper()}")
    for fn in ACL_FUNCS:
        source = f"{BASE_INCLUDES}\n#include <sys/acl.h>\nint main(void){{void *p=(void*)&{fn};return p!=0;}}\n"
        if "HAVE_SYS_ACL_H" in found and probe(work, fn, source):
            found.append(f"HAVE_{fn.upper()}")
    if "HAVE_SYS_ACL_H" in found and probe(
        work, "acl_type_nfs4", "#include <sys/acl.h>\nint main(void){return ACL_TYPE_NFS4;}\n"
    ):
        found.append("HAS_NFSV4_ACL_SUPPORT")
    for member in MEMBERS:
        source = (f"#include <sys/stat.h>\n"
                  f"int main(void){{struct stat s; s.{member} = s.{member}; return 0;}}\n")
        if probe(work, member, source):
            found.append(f"HAVE_STRUCT_STAT_{member.upper()}")
    return sorted(set(found))


def git_out(src: Path, *argv: str) -> str:
    done = subprocess.run(["git", "-C", str(src), *argv], capture_output=True, text=True,
                          timeout=120, check=False)
    if done.returncode != 0:
        raise RuntimeError(f"git {' '.join(argv)} failed {done.returncode}: {done.stderr.strip()}")
    return done.stdout


def pinned_blob_sha(src: Path, relpath: str) -> str:
    """The sha256 of a file as it exists in the pinned commit, never a hash the caller supplied."""
    blob = subprocess.run(["git", "-C", str(src), "show", f"{PINNED_COMMIT}:{relpath}"],
                          capture_output=True, timeout=120, check=False)
    if blob.returncode != 0:
        raise RuntimeError(f"{relpath} is not in the pinned commit {PINNED_COMMIT}")
    return sha256_text(blob.stdout.decode("utf-8", "surrogateescape"))


# The scripts a curated closure may carry, pinned here by the sha256 each file has in
# PINNED_COMMIT. A closure is checked against these values and refused otherwise, so the fallback
# is a fixed auditable set rather than a way to approve whatever source a caller supplies.
CURATED_CASES = {
    "tests/mkdir/00.t": "bd017018a17cbaed6d0197ec9ee072a5a23f20edc0938f37d410b233e912ba35",
    "tests/mkfifo/00.t": "f631099ba6afbf0f23ee03759278166b332127d649ed99aaffed2c9cf7a6f866",
    "tests/open/17.t": "b2aa69d1662b85b4bb473c0831097a6d83a4b2087eff3d4a6ada2ace1639bab8",
    "tests/rmdir/12.t": "0078ce2fb06a08d51895a15d126da79319194e8447cb9610466d27cd6a235e03",
    "tests/unlink/14.t": "ce168a45c3fa61352f9f26f81dcc2fe17f77a460328c59fc9822a34bc39ac007",
}


# Metadata a curated closure may carry beside its pinned cases. COPYING is the upstream notice and
# is pinned exactly like a case; README.md is documentation and is allowed by name only. Every other
# file is refused, so a closure cannot carry a script or a payload the pin does not cover.
CURATED_METADATA = {"COPYING": "e12b8e42b14e014b3e02f19a6b49de44dfb5f16dec55db1ace0f110be2d71330"}
CURATED_METADATA_NAMES = {"COPYING", "README.md"}


def verify_curated_closure(src: Path) -> dict:
    """Verify a curated script directory against the closure pinned in this file.

    This is what a clean archive has, where no pinned checkout exists. It can only ever prove the
    cases it pins, so a run reaching beyond them is refused later by the classification
    prerequisite instead of being scored from guesswork.
    """
    problems: list[str] = []
    cases: dict[str, str] = {}
    present = {str(p.relative_to(src)) for p in sorted((src / "tests").glob("*/*.t"))} \
        if (src / "tests").is_dir() else set()
    if not present:
        problems.append(f"{src} has no tests directory")
    for rel, want in CURATED_CASES.items():
        path = src / rel
        if not path.is_file():
            problems.append(f"{rel} is missing from the curated closure")
            continue
        actual = sha256(path)
        if actual != want:
            problems.append(f"{rel} sha256 {actual} is not the pinned blob {want}")
        cases[rel] = actual
    extra = present - set(CURATED_CASES)
    if extra:
        problems.append(f"a curated closure carries only the pinned cases, not {sorted(extra)[:3]}")
    for item in sorted(src.rglob("*")):
        if not item.is_file() or item.is_symlink():
            continue
        rel = item.relative_to(src).as_posix()
        if rel.startswith("tests/"):
            continue
        if rel not in CURATED_METADATA_NAMES:
            problems.append(f"a curated closure carries no {rel}; only {sorted(CURATED_METADATA_NAMES)}")
            continue
        want = CURATED_METADATA.get(rel)
        if want and sha256(item) != want:
            problems.append(f"{rel} sha256 {sha256(item)} is not the pinned blob {want}")
    return {"problems": problems, "source_sha256": None, "cases": cases,
            "metadata": {rel: sha256(src / rel) for rel in sorted(CURATED_METADATA_NAMES)
                         if (src / rel).is_file()}}


def verify_tool(src: Path) -> dict:
    """Verify a tool root: the pinned checkout when it is one, else a curated closure."""
    if (src / ".git").is_dir():
        return verify_tool_source(src)
    return verify_curated_closure(src)


def verify_tool_source(src: Path) -> dict:
    """Refuse a checkout that is not exactly the pinned commit's bytes, before any case runs."""
    dirty = git_out(src, "status", "--porcelain").strip()
    head = git_out(src, "rev-parse", "HEAD").strip()
    problems = []
    if head != PINNED_COMMIT:
        problems.append(f"HEAD is {head}, not the pinned {PINNED_COMMIT}")
    # config.h and the binary are produced by this harness, so only upstream files are compared.
    generated = {"config.h", "pjdfstest"}
    modified = [ln for ln in dirty.splitlines() if ln[3:] not in generated]
    if modified:
        problems.append(f"checkout is modified: {modified[:5]}")
    source_sha = sha256(src / "pjdfstest.c")
    expected = pinned_blob_sha(src, "pjdfstest.c")
    if source_sha != expected:
        problems.append(f"pjdfstest.c sha256 {source_sha} is not the pinned blob {expected}")
    cases = {}
    for case in sorted(p for p in (src / "tests").glob("*/*.t")):
        rel = str(case.relative_to(src))
        actual, want = sha256(case), pinned_blob_sha(src, rel)
        if actual != want:
            problems.append(f"{rel} sha256 {actual} is not the pinned blob {want}")
        cases[rel] = actual
    return {"problems": problems, "source_sha256": source_sha, "cases": cases}


def fetch_tool(root: Path) -> Path:
    src = root / "tool" / "pjdfstest"
    if (src / ".git").is_dir():
        return src
    src.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["git", "clone", "--quiet", PINNED_URL, str(src)], check=True)
    subprocess.run(["git", "-C", str(src), "checkout", "--quiet", PINNED_COMMIT], check=True)
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
    binary = src / "pjdfstest"
    subprocess.run(
        [CC, "-O2", "-Wall", "-Werror", "-I", str(src), "-o", str(binary), str(src / "pjdfstest.c")],
        check=True,
    )
    return binary, features


def tool_identity(src: Path, binary: Path, features: list[str], source: dict) -> dict:
    return {
        "url": PINNED_URL,
        "commit": PINNED_COMMIT,
        "pjdfstest_c_sha256": source["source_sha256"],
        "config_h_sha256": sha256(src / "config.h"),
        "binary_sha256": sha256(binary),
        "compiler": subprocess.run([CC, "--version"], capture_output=True, text=True,
                                   check=False).stdout.splitlines()[0],
        "features": features,
        "cases_sha256": source["cases"],
    }


# ---------------------------------------------------------------- suite input

# The macOS NFS client answers EINVAL for pathconf(_PC_PATH_MAX) without asking the server (issue
# 218, docs/verification/evidence/nfs108-pathconf.md), so `dirgen_max` in misc.sh builds an empty
# path on the cowfs arm and 13 ENAMETOOLONG cases cannot compare anything. The suite reads
# PATH_MAX in exactly this one place (and never reads _PC_PIPE_BUF), so the run supplies the value
# the native arm reports, and only where an arm's own answer is not a positive number.
PATH_MAX_READ = "\tpath_max=`${fstest} pathconf . _PC_PATH_MAX`\n"
PATH_MAX_SUPPLY = '\t[ "${path_max}" -gt 0 ] 2>/dev/null || path_max=%d\n'


def pathconf_answer(path: Path, name: str) -> dict:
    try:
        return {"value": os.pathconf(path, name)}
    except OSError as exc:
        return {"value": None, "errno": exc.errno, "error": os.strerror(exc.errno or 0)}


def build_suite_overlay(tests_root: Path, dest: Path, binary: Path, path_max: int) -> tuple[Path, dict]:
    """A per-run copy of the pinned suite whose only difference is one line in misc.sh.

    Every file but misc.sh is a symlink to the pinned file, so the bytes that run are the pinned
    bytes. The pinned checkout itself is never touched, which keeps verify_tool_source meaningful.
    The returned receipt names what was injected and the sha256 of both misc.sh files.
    """
    pinned = (tests_root / "misc.sh").read_text()
    if pinned.count(PATH_MAX_READ) != 1:
        raise RuntimeError("misc.sh does not contain the one PATH_MAX read this harness patches")
    patched = pinned.replace(PATH_MAX_READ, PATH_MAX_READ + PATH_MAX_SUPPLY % path_max)
    tests = dest / "tests"
    tests.mkdir(parents=True)
    for entry in sorted(tests_root.iterdir()):
        if entry.name == "misc.sh":
            continue
        if entry.is_dir():
            # A real directory of symlinks: sh resolves `<dir>/../misc.sh` physically, so a
            # symlinked group directory would reach the pinned misc.sh instead of the overlay's.
            (tests / entry.name).mkdir()
            for case in sorted(entry.iterdir()):
                (tests / entry.name / case.name).symlink_to(case.resolve())
        else:
            (tests / entry.name).symlink_to(entry.resolve())
    (tests / "misc.sh").write_text(patched)
    (dest / "pjdfstest").symlink_to(binary.resolve())
    return tests, {
        "mechanism": "one line inserted after the _PC_PATH_MAX read in misc.sh dirgen_max, in a "
                     "per-run overlay; every other suite file is a symlink to the pinned file",
        "injected_line": (PATH_MAX_SUPPLY % path_max).strip(),
        "applies_to": "any arm whose pathconf answer is not a positive number (both arms run the "
                      "same overlay, so only an arm that cannot answer is changed)",
        "path_max": path_max,
        "misc_sh_pinned_sha256": sha256_text(pinned),
        "misc_sh_overlay_sha256": sha256_text(patched),
        "overlay": str(tests),
    }


def suite_input_receipt(native_root: Path, cowfs_root: Path, tests_root: Path, run_dir: Path,
                        binary: Path) -> tuple[Path, dict]:
    """What each arm's filesystem answers for PATH_MAX, and what the run supplied because of it."""
    answers = {arm: {name: pathconf_answer(root, name) for name in ("PC_PATH_MAX", "PC_NAME_MAX")}
               for arm, root in (("native", native_root), ("cowfs", cowfs_root))}
    native_max = answers["native"]["PC_PATH_MAX"]["value"]
    receipt = {"pathconf": answers}
    if not isinstance(native_max, int) or native_max <= 0:
        receipt["injection"] = None
        receipt["note"] = "the native arm gave no PATH_MAX, so none was supplied and the pinned suite ran as is"
        return tests_root, receipt
    tests, receipt["injection"] = build_suite_overlay(tests_root, run_dir / "suite", binary, native_max)
    return tests, receipt


# ---------------------------------------------------------------- process registry


class Registry:
    """Every child this harness starts, with the facts needed to signal only that child later.

    An entry is registered before the harness waits on it, so a mount wait or a case that hangs
    still has a pid on record. Nothing here signals a process group and nothing walks a mount.
    """

    def __init__(self) -> None:
        self.entries: list[dict] = []

    def add(self, role: str, proc: subprocess.Popen, argv: list[str], **facts: str) -> dict:
        entry = {
            "role": role, "pid": proc.pid, "argv": list(argv), "proc": proc,
            "registered_at": time.strftime("%Y-%m-%dT%H:%M:%S%z", time.localtime()),
            "ps_at_start": self._ps(proc.pid), **facts,
        }
        self.entries.append(entry)
        return entry

    @staticmethod
    def _ps(pid: int) -> str:
        done = subprocess.run(["ps", "-o", "lstart=,command=", "-p", str(pid)],
                              capture_output=True, text=True, timeout=15, check=False)
        return done.stdout.strip() if done.returncode == 0 else ""

    def verify(self, entry: dict) -> tuple[bool, str]:
        """The pid is still the process we started, by start time and by argv."""
        now = self._ps(entry["pid"])
        if not now:
            return False, f"pid {entry['pid']} is not running"
        if entry["ps_at_start"] and now.split(None, 5)[5:] != entry["ps_at_start"].split(None, 5)[5:]:
            return False, f"pid {entry['pid']} now reads {now!r}, not the argv we started"
        if not any(fact in now for fact in (entry["store"], entry["socket"]) if fact):
            return False, f"pid {entry['pid']} does not carry our store or socket: {now!r}"
        return True, now

    def signal(self, entry: dict, sig: int) -> str:
        ok, detail = self.verify(entry)
        if not ok and entry["proc"].poll() is None:
            return f"refused to signal {entry['role']} pid {entry['pid']}: {detail}"
        entry["proc"].send_signal(sig)
        return f"sent {signal.Signals(sig).name} to {entry['role']} pid {entry['pid']}"

    def children_of(self, pid: int) -> list[int]:
        done = subprocess.run(["ps", "-o", "pid=,ppid=", "-A"], capture_output=True, text=True,
                              timeout=30, check=False)
        if done.returncode != 0:
            return []
        return [int(line.split()[0]) for line in done.stdout.splitlines()
                if len(line.split()) == 2 and int(line.split()[1]) == pid]


# ---------------------------------------------------------------- daemon


def start_daemon(registry: Registry, daemon: Path, store: Path, mount: Path, sock: Path,
                 log_path: Path) -> dict:
    """Start the private daemon and return its identity. Registered before anything waits on it."""
    for directory in (store, mount, sock.parent):
        directory.mkdir(parents=True, exist_ok=True)
    sock.parent.chmod(0o700)
    handle = log_path.open("ab")
    argv = [str(daemon), "--store", str(store), "--mount", str(mount), "--socket", str(sock),
            "--backend", "core"]
    proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=handle, stderr=handle)
    entry = registry.add("daemon", proc, argv, store=str(store), socket=str(sock))
    identity = {**{k: v for k, v in entry.items() if k != "proc"}, "mount_path": str(mount),
                "log": str(log_path), "entry": entry}
    deadline = time.monotonic() + 180
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"daemon exited {proc.returncode} before serving; see {log_path}")
        state, detail = mount_state(mount)
        if state == MOUNTED:
            identity["mount_table_line"] = detail
            return identity
        if state == UNKNOWN:
            raise RuntimeError(f"cannot confirm the mount: {detail}")
        time.sleep(0.5)
    raise RuntimeError(f"daemon did not serve within 180s; see {log_path}")


def stop_daemon(registry: Registry, identity: dict, keep_mount: bool = False) -> str:
    """Signal the one recorded pid after checking it is still ours, then prove the mount is gone."""
    entry = identity["entry"]
    proc: subprocess.Popen = entry["proc"]
    mount = Path(identity["mount_path"])
    if proc.poll() is not None:
        return f"daemon pid {entry['pid']} already exited {proc.returncode}"
    sent = registry.signal(entry, signal.SIGTERM)
    try:
        code = proc.wait(timeout=120)
    except subprocess.TimeoutExpired:
        return f"{sent}, but pid {entry['pid']} did not exit on SIGTERM"
    deadline = time.monotonic() + 60
    state, detail = mount_state(mount)
    while state == MOUNTED and time.monotonic() < deadline:
        time.sleep(1)
        state, detail = mount_state(mount)
    if state == UNKNOWN:
        return f"{sent}, exited {code}; mount state of {mount} is UNKNOWN ({detail}), not unmounted"
    if state == MOUNTED:
        if keep_mount:
            return f"{sent}, exited {code}; mount {mount} still listed (kept)"
        raise RuntimeError(f"daemon {entry['pid']} exited {code} but {mount} is still mounted")
    return f"{sent}, exited {code}; {detail}"


# ---------------------------------------------------------------- cases


def parse_tap(text: str) -> dict:
    plan, cases, todo, skips, bail = None, [], 0, 0, 0
    malformed = []
    seen_plan = False
    for number, line in enumerate(text.splitlines(), start=1):
        if not seen_plan:
            match = PLAN_RE.match(line)
            if match:
                plan, seen_plan = int(match.group(1)), True
            elif line.strip():
                malformed.append(f"line {number} before any plan: {line[:60]!r}")
            continue
        if line.startswith("Bail out!"):
            bail += 1
            continue
        match = RESULT_RE.match(line)
        if not match:
            if line.strip():
                malformed.append(f"line {number} is not a TAP result: {line[:60]!r}")
            continue
        ok, index, tag, detail = match.group(1) == "ok", int(match.group(2)), match.group(3), match.group(4)
        todo += tag == "TODO"
        skips += tag == "skip"
        cases.append({"n": index, "ok": ok, "todo": tag == "TODO", "detail": detail.strip()})
    return {
        "plan": plan, "cases": cases, "todo": todo, "skip": skips, "bail_out": bail,
        "malformed": malformed,
        "ok": sum(1 for c in cases if c["ok"]), "not_ok": sum(1 for c in cases if not c["ok"]),
    }


def classification_prerequisite(tool: dict | None, tests_root: Path | None, compared: list[str],
                                profiles: dict) -> list[str]:
    """Why this run cannot be classified, if it cannot be. Empty means the gate can run.

    Pairing is proved from the upstream case scripts. Without them every assertion falls through to
    the text route, which needs operation text the suite does not print on a pass, so a run with no
    scripts pairs almost nothing and can find no regression. Reading that as PASS would report a
    gate that never ran, so the missing prerequisite is an integrity failure, not coverage, and it
    is discovered before anything is classified rather than after the damage is counted.
    """
    if tool is None:
        return ["the pinned pjdfstest source was not verified, so no assertion can be classified"]
    if tool.get("problems"):
        return [f"the pjdfstest source is not the pinned {PINNED_COMMIT}: {tool['problems'][0]}"]
    if tests_root is None:
        return ["no pinned test directory was read, so no assertion can be classified"]
    missing = sorted(test for test in compared if test not in profiles)
    if missing:
        return [(f"{len(missing)} compared case(s) have no script profile, first {missing[0]}: "
                 "a missing script cannot prove an assertion's position")]
    return []


def stream_path(run_dir: Path, raw: str) -> Path:
    """Where a record's raw stream is, refusing a relative path that climbs out of the run.

    A relative path is anchored on the run directory, not on the working directory, so the same
    record set scores identically from any directory. An absolute path is taken as written, because
    the historical record sets name their own run directory and are read-only evidence.
    """
    path = Path(raw)
    if path.is_absolute():
        return path
    root, resolved = Path(run_dir).resolve(), (Path(run_dir) / path).resolve()
    if resolved != root and root not in resolved.parents:
        raise ValueError(f"{raw!r} resolves to {resolved}, which is outside the run directory {root}")
    return resolved


def guard_case(record: dict, expected_test: str, raw_text: str | None, legacy: bool = False) -> list[str]:
    """Every reason this case's numbers cannot be scored. Called by verdict, not only by the runner."""
    problems = []
    if record.get("synthetic"):
        problems.append("synthetic fixture: a unit-test record is not conformance evidence")
    if record.get("test") != expected_test:
        problems.append(f"record says {record.get('test')!r}, the run listed it as {expected_test!r}")
    if record.get("timed_out"):
        problems.append("the case timed out, so its stream is partial")
    if record.get("rc") not in (0,):
        problems.append(f"child exited {record.get('rc')}, not 0")
    tap = parse_tap(raw_text) if raw_text is not None else record
    if raw_text is None and not record.get("raw") and not legacy:
        problems.append("no raw stream on disk: the parsed record cannot be re-checked")
    if tap.get("plan") is None:
        problems.append("no plan line, so the stream cannot be scored")
    if tap.get("bail_out"):
        problems.append(f"{tap['bail_out']} Bail out! line(s)")
    ids = [c["n"] for c in tap["cases"]]
    if len(ids) != len(set(ids)):
        problems.append("duplicate assertion ids")
    if ids and sorted(ids) != list(range(1, len(ids) + 1)):
        problems.append("assertion ids are not contiguous from 1")
    if tap.get("plan") is not None and tap["plan"] != len(ids):
        problems.append(f"plan {tap['plan']} does not match the {len(ids)} assertions emitted")
    problems += list(tap.get("malformed", [])[:3])
    return problems


def run_case(registry: Registry, tests_root: Path, test: str, case_dir: Path, raw_path: Path,
             timeout: int) -> dict:
    """One case, in its own directory, with the raw stream kept next to the record."""
    case_dir.mkdir(parents=True, exist_ok=True)
    raw_path.parent.mkdir(parents=True, exist_ok=True)
    started = time.time()
    proc = subprocess.Popen(["sh", str(tests_root / test)], cwd=case_dir, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True,
                            env={**os.environ, "TZ": os.environ.get("TZ", "UTC")})
    entry = registry.add("case", proc, ["sh", str(tests_root / test)], store=str(case_dir))
    timed_out = False
    try:
        out, err = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        out, err = proc.communicate(timeout=5)
        if proc.poll() is None:
            # A hung case is signalled one pid at a time, and only after each pid's own argv shows
            # it is inside this case's directory. No process group, no pattern kill.
            for child in registry.children_of(proc.pid):
                seen = Registry._ps(child)
                if str(case_dir) in seen:
                    try:
                        os.kill(child, signal.SIGKILL)
                    except OSError as exc:
                        err += f"\nchild {child} could not be signalled: {exc}"
                else:
                    err += f"\nchild {child} left alone, it is not ours: {seen!r}"
            registry.signal(entry, signal.SIGKILL)
            out, err = proc.communicate(timeout=30)
    with raw_path.open("w") as f:
        f.write(out)
        f.write("\n--- stderr ---\n")
        f.write(err)
        f.flush()
    tap = parse_tap(out)
    source = (tests_root / test).read_text()
    whole_case_root = bool(re.search(r"^\s*requires_root\s*$", source, flags=re.MULTILINE))
    for case in tap["cases"]:
        case["root_required"] = whole_case_root or bool(ROOT_REQUIRED_RE.search(case["detail"]))
    return {
        "test": test, "rc": proc.returncode, "timed_out": timed_out,
        "seconds": round(time.time() - started, 1), "case_dir": str(case_dir),
        "raw": str(raw_path), "raw_sha256": sha256(raw_path), "raw_bytes": raw_path.stat().st_size,
        "stderr_tail": err[-2000:], **tap,
    }


# ---------------------------------------------------------------- identity


def normalize_operation(args: str) -> str:
    """The operation's arguments with generated names replaced by their position.

    The same canonical form is used for the pinned script, where a generated name is a `${n0}`
    style variable, and for the transcript, where it is the name `misc.sh` printed. Position is
    what survives, so `create <gen1> 0644` identifies the same operation in both.
    """
    index = 0

    def slot(match: re.Match) -> str:
        nonlocal index
        index += 1
        return f"<gen{index}>"

    return NAME_TOKEN_RE.sub(slot, args.strip())


def operation_key(detail: str) -> tuple[str, str] | None:
    """(kind, operation) for an assertion, or None when the stream carries no operation text.

    The suite's `test_check` prints no text, so those assertions cannot be identified and are
    never paired.
    """
    match = TRIED_RE.search(detail)
    if not match:
        return None
    return "expect", normalize_operation(match.group(1))


def normalize_text(detail: str) -> str:
    """Comparison text for the ordinal diagnostic only. Never used to build an identity."""
    return GENERATED_RE.sub("<name>", detail).strip()


def literal_pinned(operation: str) -> bool:
    """True when something in the operation other than a generated name pins it.

    `create <gen1> 0644` is pinned by its mode. `link <gen1> <gen2>` is not: both names are
    generated, so `link a b` and `link b a` are the same string after canonicalisation and the
    stream cannot say which order it performed. Such a pair is a candidate, never a defect.
    """
    tokens = operation.split()
    return any(not re.fullmatch(r"<gen\d+>", token) for token in tokens[1:])


def script_profile(text: str) -> dict:
    """What the pinned script can prove about its own assertion sequence.

    A site is one assertion the script makes, in order, with the operation it performs and the
    expectation dropped, because that is what the transcript prints back. A `test_check` site has
    no operation at all: it is a shell expression, and the transcript prints nothing for it, which
    is exactly why such an assertion cannot be identified from the stream alone.

    `slot_identity_provable` is the load-bearing part. When the script makes no assertion whose
    position depends on an earlier outcome, no helper call that injects a variable number of
    assertions, and no jail case, then the k-th assertion in the transcript is the k-th site, and
    pairing by position is proved rather than assumed.
    """
    body = re.sub(r"#[^\n]*", "", text)
    sites: list[dict] = []
    continued = False
    for line in body.splitlines():
        stripped = line.strip()
        if stripped.endswith("\\"):
            continued = True
            continue
        if continued:
            continued = False
        if stripped.startswith("expect "):
            sites.append({"helper": "expect", "operation": normalize_operation(" ".join(stripped.split()[2:]))})
        elif stripped.startswith("test_check"):
            sites.append({"helper": "test_check", "operation": None})
        elif stripped.startswith("jexpect "):
            sites.append({"helper": "jexpect", "operation": None})
    literal_loops = [m.group(0).strip() for m in LITERAL_FOR_RE.finditer(body)]
    blockers = []
    if RESULT_DEPENDENT_RE.search(body):
        blockers.append("the script branches on an earlier result")
    if "create_file" in body:
        blockers.append("create_file injects a variable number of assertions")
    if any(site["helper"] == "jexpect" for site in sites):
        blockers.append("jexpect runs the case in a jail")
    if continued:
        blockers.append("an assertion call is split across lines")
    return {
        "sites": sites,
        "literal_for_loops": literal_loops,
        "result_dependent_control_flow": bool(RESULT_DEPENDENT_RE.search(body)),
        "helper_expands_assertions": "create_file" in body,
        "slot_identity_blockers": blockers,
    }


def pair_case(native: dict, cowfs: dict, profile: dict) -> dict:
    """Pair one case's assertions, and say how solid each pairing is.

    Two routes, in order of strength. When the script proves that the k-th assertion it makes is
    the k-th assertion in the stream, position is identity and the pair is established. Otherwise
    the operation text decides, and a pair needs equal text on both arms plus a literal that pins
    it; everything else is unpairable rather than guessed.
    """
    sites = profile.get("sites", [])
    blockers = profile.get("slot_identity_blockers", ["no script profile"])
    if len(sites) == len(native["cases"]) == len(cowfs["cases"]) and not blockers:
        pairs, unpairable = [], []
        for position, (n_case, c_case) in enumerate(zip(native["cases"], cowfs["cases"])):
            site = sites[position]
            text_key = operation_key(c_case["detail"])
            if text_key is not None and text_key[1] != site["operation"]:
                unpairable.append({
                    "reason": f"the stream says {text_key[1]!r} where the script's slot {position} "
                              f"is {site['operation']!r}", "n": n_case["n"]})
                continue
            pairs.append({
                "confidence": ESTABLISHED,
                "identity": f"script slot {position} ({site['helper']} "
                            f"{site['operation'] or '(no operation text)'})",
                "operation": site["operation"] or f"({site['helper']})",
                "occurrence": position, "n": n_case["n"],
                "native_ok": n_case["ok"], "cowfs_ok": c_case["ok"],
                "native_detail": n_case["detail"], "cowfs_detail": c_case["detail"],
                "root_required": bool(c_case["root_required"] or n_case["root_required"]),
            })
        return {"pairs": pairs, "unpairable": unpairable, "route": "script slot"}

    def buckets(record: dict) -> dict:
        out: dict = {}
        for position, case in enumerate(record["cases"]):
            key = operation_key(case["detail"])
            out.setdefault(key, []).append((position, case))
        return out

    native_buckets, cowfs_buckets = buckets(native), buckets(cowfs)
    pairs, unpairable = [], []
    script_sites = {site["operation"] for site in sites if site["operation"]}
    loop_provable = bool(profile.get("literal_for_loops"))
    for key in sorted(set(native_buckets) | set(cowfs_buckets), key=lambda k: (k is None, str(k))):
        left, right = native_buckets.get(key, []), cowfs_buckets.get(key, [])
        if key is None:
            unpairable += [{"reason": "no operation text in the stream (test_check)", "n": c["n"]}
                           for _, c in left] + [{"reason": "no operation text (test_check)", "n": c["n"]}
                                                 for _, c in right]
            continue
        if len(left) != len(right):
            unpairable += [{"reason": f"{key[1]!r} appears {len(left)} times natively and "
                                    f"{len(right)} times on the mount", "n": c["n"]}
                           for _, c in left + right]
            continue
        if len(left) > 1 and not loop_provable:
            # Identical operations with no literal loop to order them: a text duplicate. Choosing
            # which one pairs with which is a guess, so all of them are ambiguous.
            unpairable += [{"reason": f"{key[1]!r} repeats with no literal for loop to order it",
                            "n": c["n"]} for _, c in left + right]
            continue
        for occurrence, ((_, n_case), (_, c_case)) in enumerate(zip(left, right)):
            in_script = key[1] in script_sites
            if not (in_script or loop_provable):
                unpairable.append({"reason": f"{key[1]!r} is not an operation the pinned script makes",
                                   "n": n_case["n"]})
                continue
            proven = literal_pinned(key[1])
            pairs.append({
                "confidence": ESTABLISHED if proven else CANDIDATE,
                "identity": f"{key[1]} occurrence {occurrence}",
                "operation": key[1], "occurrence": occurrence, "n": n_case["n"],
                "native_ok": n_case["ok"], "cowfs_ok": c_case["ok"],
                "native_detail": n_case["detail"], "cowfs_detail": c_case["detail"],
                "root_required": bool(c_case["root_required"] or n_case["root_required"]),
            })
    return {"pairs": pairs, "unpairable": unpairable, "route": "operation text"}


def outcome_text(detail: str) -> str:
    """The outcome the assertion reported, which is never part of an identity."""
    match = re.search(r"got (.+)$", detail)
    return (match.group(1).strip() if match else ("(empty)" if "expected" in detail else "(textless)"))


def first_call(detail: str) -> str:
    """The syscall an assertion's argument string starts with, skipping pjdfstest's own flags."""
    match = TRIED_RE.search(detail)
    if not match:
        return "(textless)"
    tokens = match.group(1).split()
    index = 0
    while index < len(tokens) and (tokens[index].startswith("-") or
                                  re.fullmatch(r"0?[0-7]{3,4}", tokens[index])):
        index += 1
    return tokens[index] if index < len(tokens) else "(no syscall)"


def ordinal_rows(arms: dict) -> tuple[list[dict], list[dict]]:
    """Positions where native passes and cowfs fails (worse), and the reverse (better)."""
    regressions, looser = [], []
    for test in sorted(set(arms.get("native", {})) & set(arms.get("cowfs", {}))):
        native = {c["n"]: c for c in arms["native"][test]["cases"]}
        cowfs = {c["n"]: c for c in arms["cowfs"][test]["cases"]}
        for index in sorted(set(native) & set(cowfs)):
            left, right = native[index], cowfs[index]
            row = {"test": test, "n": index, "native_detail": left["detail"],
                   "cowfs_detail": right["detail"], "root_required": bool(right["root_required"]),
                   "cowfs_stderr": arms["cowfs"][test].get("stderr")
                   or arms["cowfs"][test].get("stderr_tail") or ""}
            if left["ok"] and not right["ok"]:
                regressions.append(row)
            elif not left["ok"] and right["ok"]:
                looser.append(row)
    return regressions, looser


def ordinal_diagnostic(arms: dict, profiles: dict) -> dict:
    """The old ordinal-position accounting, kept as a labelled diagnostic.

    Every number here is "N positions in the stream where native passed and cowfs failed". That is
    a differential, not a count of defects: pairing by position cannot tell a real divergence from
    a case that simply took a different path after an earlier failure. It is reported because it
    bounds what the historical record can say, not because it counts anything.
    """
    regressions, looser = ordinal_rows(arms)
    outside_gate = [r for r in regressions if not r["root_required"]]
    same_text = [r for r in outside_gate
                 if normalize_text(r["native_detail"]) == normalize_text(r["cowfs_detail"])]
    pathconf_cases = {t for records in arms.values() for t, r in records.items()
                      if "pathconf returned -1" in (r.get("stderr") or r.get("stderr_tail") or "")}
    # An exclusive partition, first match wins, so the buckets sum to the differential.
    eio = [r for r in outside_gate if outcome_text(r["cowfs_detail"]) == "EIO"]
    rest = [r for r in outside_gate if r not in eio]
    pathconf = [r for r in rest if r["test"] in pathconf_cases]
    rest = [r for r in rest if r not in pathconf]
    ename = [r for r in rest if outcome_text(r["cowfs_detail"]) == "ENOENT"]
    rest = [r for r in rest if r not in ename]
    textless = [r for r in rest if outcome_text(r["cowfs_detail"]) == "(textless)"]
    other = [r for r in rest if r not in textless]
    owner_calls = collections.Counter(first_call(r["native_detail"]) for r in looser)
    non_owner = [r for r in looser if first_call(r["native_detail"]) not in ("chown", "lchown")]
    return {
        "label": "ordinal-position differential, not a defect count",
        "ordinal_regressions_total": len(regressions),
        "ordinal_regressions_outside_privilege_gate": len(outside_gate),
        "pairs_with_matching_text": len(same_text),
        "of_those_textless_on_both_sides": sum(
            1 for r in same_text
            if outcome_text(r["native_detail"]) == outcome_text(r["cowfs_detail"]) == "(textless)"),
        "pairs_structurally_different": len(outside_gate) - len(same_text),
        "ordinal_looser_total": len(looser),
        "looser_with_matching_text": sum(
            1 for r in looser
            if normalize_text(r["native_detail"]) == normalize_text(r["cowfs_detail"])),
        "partition": {
            "A_direct_eio_rows": len(eio),
            "A_create_calls": dict(collections.Counter(first_call(r["cowfs_detail"]) for r in eio)),
            "B_pathconf_case_rows": len(pathconf),
            "C_enoent_cascade_rows": len(ename),
            "D_textless_rows": len(textless),
            "E_other_errno_rows": len(other),
            "sum": len(eio) + len(pathconf) + len(ename) + len(textless) + len(other),
            "E_rows": [{"test": r["test"], "n": r["n"], "cowfs_detail": r["cowfs_detail"][:120]}
                       for r in other],
        },
        "looser_calls": dict(owner_calls),
        "looser_owner_call_rows": owner_calls["chown"] + owner_calls["lchown"],
        "looser_non_owner_rows": len(non_owner),
        "looser_non_owner_rows_by_case": dict(collections.Counter(r["test"] for r in non_owner)),
    }


ACCEPTED_DIVERGENCES = Path(__file__).resolve().parent / "pjdfstest-accepted-divergences.json"


def load_accepted(path: Path = ACCEPTED_DIVERGENCES) -> list[dict]:
    """The checked-in list of ordinal-worse positions that are documented, accepted divergences.

    Every entry names (test, n, issue) and carries a reason, so an accepted position is a decision
    on record and not a silent skip. `match` is a regex searched in the failing assertion's cowfs
    detail followed by the case's stderr, so a waiver only covers the failure it was written for:
    a different failure at a listed position is not waived. A malformed list raises: a gate must not guess its own waivers.
    An absent file waives nothing, which can only make the verdict stricter.
    """
    if not Path(path).is_file():
        return []
    document = json.loads(Path(path).read_text())
    entries = document["entries"]
    for entry in entries:
        missing = [k for k in ("test", "n", "issue", "reason", "match") if not entry.get(k)]
        if missing:
            raise ValueError(f"{path}: accepted divergence {entry!r} lacks {missing}")
        re.compile(entry["match"])
    return entries


def accepted_reasons(arms: dict, accepted: list[dict]) -> list[dict]:
    """Ordinal-worse positions not covered by the accepted list FAIL; list entries that rot are named.

    This sits beside the established-regression rule and never relaxes it: an established
    regression stays a DIVERGENCE whether or not a position is listed here.
    """
    worse, _ = ordinal_rows(arms)
    listed = {(e["test"], e["n"]): e for e in accepted}
    def waived(row: dict) -> bool:
        entry = listed.get((row["test"], row["n"]))
        return bool(entry) and bool(re.search(entry["match"], row["cowfs_detail"] + "\n" + row["cowfs_stderr"]))

    uncovered = [r for r in worse if not waived(r)]
    unmatched = [r for r in uncovered if (r["test"], r["n"]) in listed]
    out = []
    if uncovered:
        shown = ", ".join(f"{r['test']} #{r['n']}" for r in uncovered[:20])
        more = f" (+{len(uncovered) - 20} more)" if len(uncovered) > 20 else ""
        out.append(reason(DIVERGENCE, f"{len(uncovered)} ordinal-worse position(s) are not covered by "
                                      f"an accepted divergence: {shown}{more}"
                                      + (f"; {len(unmatched)} of them are listed but the failure no "
                                         "longer matches the entry's signature" if unmatched else "")))
    worse_keys = {(r["test"], r["n"]) for r in worse}
    compared = set(arms.get("native", {})) & set(arms.get("cowfs", {}))
    stale = [e for e in accepted if e["test"] in compared and (e["test"], e["n"]) not in worse_keys]
    if stale:
        shown = ", ".join(f"{e['test']} #{e['n']} ({e['issue']})" for e in stale[:20])
        out.append(reason(COVERAGE, f"{len(stale)} accepted divergence(s) are listed but not worse in "
                                    f"this run, remove them if fixed: {shown}"))
    covered = len(worse) - len(uncovered)
    if covered:
        issues = collections.Counter(listed[(r["test"], r["n"])]["issue"] for r in worse if waived(r))
        out.append(reason(COVERAGE, f"{covered} ordinal-worse position(s) are accepted divergences: "
                                    f"{dict(issues)}"))
    return out


def compare(arms: dict, profiles: dict) -> dict:
    regressions, looser, unpairable, ambiguous_cases = [], [], [], []
    for test in sorted(set(arms.get("native", {})) & set(arms.get("cowfs", {}))):
        result = pair_case(arms["native"][test], arms["cowfs"][test], profiles.get(test, {}))
        for pair in result["pairs"]:
            if pair["native_ok"] and not pair["cowfs_ok"]:
                (regressions if pair["confidence"] == ESTABLISHED else ambiguous_cases).append(
                    {"test": test, **pair})
            elif not pair["native_ok"] and pair["cowfs_ok"]:
                looser.append({"test": test, **pair})
        unpairable += [{"test": test, **u} for u in result["unpairable"]]
    return {
        "established_regressions": regressions,
        "candidate_regressions": ambiguous_cases,
        "unpairable": unpairable,
        "looser_not_a_pass": looser,
        "cases": sorted(set(arms.get("native", {})) & set(arms.get("cowfs", {}))),
    }


# ---------------------------------------------------------------- verdict


def arm_totals(records: list[dict]) -> dict:
    totals = {"cases": 0, "executed": 0, "declined": 0, "assertions": 0, "ok": 0, "not_ok": 0,
              "root_required_assertions": 0, "root_required_passed": 0, "non_root_failures": 0,
              "timeouts": 0, "nonzero_rc": 0, "invalid": 0, "bail_out": 0}
    for record in records:
        totals["cases"] += 1
        totals["assertions"] += len(record.get("cases", []))
        totals["ok"] += record.get("ok", 0)
        totals["not_ok"] += record.get("not_ok", 0)
        gated = [c for c in record.get("cases", []) if c.get("root_required")]
        totals["root_required_assertions"] += len(gated)
        totals["root_required_passed"] += sum(1 for c in gated if c["ok"])
        totals["non_root_failures"] += sum(1 for c in record.get("cases", [])
                                           if not c["ok"] and not c.get("root_required"))
        totals["timeouts"] += bool(record.get("timed_out"))
        totals["nonzero_rc"] += record.get("rc") != 0
        totals["bail_out"] += record.get("bail_out", 0)
        if record.get("plan") == 1 and record.get("not_ok") == 0 and len(record.get("cases", [])) == 1:
            totals["declined"] += 1
        elif record.get("cases"):
            totals["executed"] += 1
        if record.get("_guard_problems"):
            totals["invalid"] += 1
    return totals


def verdict(run_dir: Path, tool: dict | None = None, tests_root: Path | None = None,
            identity: dict | None = None, accepted: list[dict] | None = None) -> dict:
    """Re-read the run's own records, re-parse every raw stream, and refuse anything malformed."""
    jsonl = Path(run_dir) / "cases.jsonl"
    if not jsonl.is_file():
        return {"state": INVALID, "exit_status": EXIT_STATUS[INVALID],
                "reasons": [reason(INTEGRITY, f"{jsonl} does not exist")]}
    arms: dict = {}
    problems: list[str] = []
    parsed: list[dict] = []
    for number, line in enumerate(jsonl.read_text().splitlines(), start=1):
        try:
            record = json.loads(line)
        except json.JSONDecodeError as exc:
            problems.append(f"{jsonl.name} line {number} is not JSON: {exc}")
            continue
        parsed.append(record)
    # A record set either keeps every raw stream or none. Mixing the two would let a run hide a
    # case behind a format change, so a mixture is refused rather than scored.
    with_raw = [bool(r.get("raw")) for r in parsed]
    legacy = bool(parsed) and not any(with_raw)
    if any(with_raw) and not all(with_raw):
        problems.append("the record set mixes cases with a raw stream and cases without one")
        legacy = False
    for record in parsed:
        raw_text = None
        stream = None
        if record.get("raw"):
            try:
                stream = stream_path(run_dir, record["raw"])
            except ValueError as exc:
                problems.append(f"{record.get('test')} on {record.get('arm')}: {exc}")
        if stream is not None and stream.is_file():
            if sha256(stream) != record.get("raw_sha256"):
                problems.append(f"{record.get('test')} on {record.get('arm')}: raw stream hash moved")
            raw_text = stream.read_text().split("\n--- stderr ---\n")[0]
        elif record.get("raw"):
            problems.append(f"{record.get('test')} on {record.get('arm')}: raw stream is missing")
        complaints = guard_case(record, record.get("test", ""), raw_text, legacy=legacy)
        record["_guard_problems"] = complaints
        problems += [f"{record.get('arm')}/{record.get('test')}: {c}" for c in complaints]
        arms.setdefault(record.get("arm"), {})[record["test"]] = record
    if tool is not None and tool.get("problems"):
        problems += [f"tool source: {p}" for p in tool["problems"]]
    totals = {arm: arm_totals(list(records.values())) for arm, records in arms.items()}
    # Identity is proved from the pinned script, so load it rather than trusting a record's path.
    profiles = {}
    for arm_records in arms.values():
        for test in arm_records:
            if tests_root is not None and (Path(tests_root) / test).is_file():
                profiles[test] = script_profile((Path(tests_root) / test).read_text())
            elif "script_text" in arm_records[test]:
                profiles[test] = script_profile(arm_records[test]["script_text"])
    diff = compare(arms, profiles)
    executed = {arm: t["executed"] for arm, t in totals.items()}
    reasons: list[dict] = []
    # A record set that cannot be trusted is INVALID, not unmeasurable: the difference matters to
    # whoever reads the exit status, so the two are never merged into one bucket.
    reasons += [reason(INTEGRITY, message) for message in problems]
    compared = sorted(set(arms.get("native", {})) & set(arms.get("cowfs", {})))
    reasons += [reason(INTEGRITY, message) for message in
                classification_prerequisite(tool, tests_root, compared, profiles)]
    reasons += validate_runtime_identity(identity)
    scope = (identity or {}).get("declared_scope") if isinstance(identity, dict) else None
    if scope:
        reasons.append(reason(COVERAGE, f"the identity receipt declares scope {scope!r}, so this is "
                                        "a reading of the records it ships with and attests nothing "
                                        "about a live mount"))
    if not arms or set(arms) != {"native", "cowfs"}:
        reasons.append(reason(CAPABILITY,
                              f"the run has arms {sorted(arms)}, not a matched native and cowfs pair"))
    if any(count == 0 for count in executed.values()):
        reasons.append(reason(CAPABILITY, f"an arm executed no assertion: {executed}"))
    if diff["unpairable"]:
        reasons.append(reason(COVERAGE, f"{len(diff['unpairable'])} assertion(s) cannot be paired "
                                        "across the arms"))
    established = diff["established_regressions"]
    outside_gate = [r for r in established if not r["root_required"]]
    textless_unpairable = sum(1 for u in diff["unpairable"] if "no operation text" in u["reason"])
    if textless_unpairable:
        reasons.append(reason(COVERAGE,
                             f"identity is unrecoverable for {textless_unpairable} assertion(s) with "
                             "no operation text in a case whose script cannot prove a slot order: the "
                             "suite prints no operation text on a pass, so those pairs are unknown "
                             "rather than matched"))
    if diff["candidate_regressions"]:
        reasons.append(reason(COVERAGE, f"{len(diff['candidate_regressions'])} paired assertion(s) "
                                        "are candidates, not established"))
    if outside_gate:
        reasons.append(reason(DIVERGENCE, f"{len(outside_gate)} established assertion(s) pass "
                                          "natively and fail on the mount"))
    reasons += accepted_reasons(arms, load_accepted() if accepted is None else accepted)
    # An established divergence is a result and an unpairable scope is a limit on what can be
    # concluded, so coverage never changes the exit on its own.
    state = state_from(reasons)
    return {
        "state": state, "exit_status": EXIT_STATUS[state], "reasons": reasons,
        "totals": totals, "comparison": diff, "guard_problems": problems,
        "record_format": "legacy: parsed records only, no raw streams on disk" if legacy
                         else "raw-attested: every case keeps its stream and its hash",
        "historical_ordinal_diagnostic": ordinal_diagnostic(arms, profiles),
        "runtime_identity": identity,
        "provenance": {"jsonl": str(jsonl), "jsonl_sha256": sha256(jsonl),
                       "identity_receipt": str(Path(run_dir) / "identity.json")
                       if (Path(run_dir) / "identity.json").is_file() else None},
    }


# ---------------------------------------------------------------- run


def test_list(tests_root: Path, groups: list[str] | None, only: list[str] | None) -> list[str]:
    found = sorted(str(p.relative_to(tests_root)) for p in tests_root.glob("*/*.t"))
    if groups:
        found = [t for t in found if t.split("/")[0] in groups]
    if only:
        found = [t for t in found if t in only]
    return found


def ensure_binaries(repo: Path) -> tuple[Path, Path, dict]:
    daemon = repo / "target" / "release" / "cowfs-daemon"
    cli = repo / "target" / "release" / "cowfs"
    if not daemon.is_file() or not cli.is_file():
        log("building cowfs-daemon and cowfs-cli (release)")
        subprocess.run(["cargo", "build", "--release", "-p", "cowfs-daemon", "-p", "cowfs-cli"],
                       cwd=repo, check=True)
    head = subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"], capture_output=True,
                          text=True, timeout=60, check=False).stdout.strip()
    return daemon, cli, {
        "cowfs_head": head,
        "cowfs_daemon_sha256": sha256(daemon),
        "cowfs_cli_sha256": sha256(cli),
    }


def create_snapshot(cli: Path, sock: Path, name: str, mount: Path) -> None:
    out = subprocess.run([str(cli), "--socket", str(sock), "--json", "snapshot", "create", name],
                         capture_output=True, text=True, timeout=120, check=False)
    if out.returncode != 0:
        raise RuntimeError(f"snapshot create failed {out.returncode}: {out.stderr.strip()}")
    log(f"snapshot {name}: {out.stdout.strip()}")
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        if (mount / name).is_dir():
            return
        time.sleep(1)
    raise RuntimeError(f"snapshot {name} never appeared under {mount}")


def run_arm(registry: Registry, arm: str, tests: list[str], tests_root: Path, root: Path,
            raw_root: Path, jsonl: Path, timeout: int) -> dict:
    totals = {"cases": 0, "executed": 0, "declined": 0, "assertions": 0, "ok": 0, "not_ok": 0,
              "root_required_assertions": 0, "timeouts": 0, "nonzero_rc": 0, "invalid": 0}
    for test in tests:
        flat = test.replace("/", "_")
        record = run_case(registry, tests_root, test, root / flat, raw_root / f"{arm}_{flat}.tap",
                          timeout)
        record["arm"] = arm
        record["script_path"] = str(tests_root / test)
        with jsonl.open("a") as f:
            f.write(json.dumps(record, sort_keys=True) + "\n")
            f.flush()
        totals["cases"] += 1
        totals["assertions"] += len(record["cases"])
        totals["ok"] += record["ok"]
        totals["not_ok"] += record["not_ok"]
        totals["root_required_assertions"] += sum(1 for c in record["cases"] if c["root_required"])
        totals["timeouts"] += bool(record["timed_out"])
        totals["nonzero_rc"] += record["rc"] != 0
        record["_guard_problems"] = guard_case(record, test, None)
        totals["invalid"] += bool(record["_guard_problems"])
        if record["plan"] == 1 and record["not_ok"] == 0 and len(record["cases"]) == 1:
            totals["declined"] += 1
        elif record["cases"]:
            totals["executed"] += 1
        log(f"  {arm:6} {test:22} rc={record['rc']} {record['seconds']:>6}s plan={record['plan']} "
            f"ok={record['ok']} not_ok={record['not_ok']}")
    return totals


def reseal(report: dict) -> dict:
    """Recompute the state and exit status after a refusal added a reason to a finished verdict."""
    report["state"] = state_from(report["reasons"])
    report["exit_status"] = EXIT_STATUS[report["state"]]
    return report


def resolve_output(raw: str | Path) -> Path:
    """Resolve a destination's parent but never its last component.

    Resolving the whole path would follow a symlink at the destination and write somewhere else
    entirely, so a link named as the output could redirect an analysis onto a foreign path. A
    dangling link is refused the same way a real file is, because link replaces neither.
    """
    path = Path(raw).expanduser()
    parent = path.parent if str(path.parent) else Path(".")
    return parent.resolve() / path.name


def _discard_staged(staged: Path) -> None:
    """Remove our own staging file, whose name carries our pid. Failing to do so is not fatal."""
    try:
        staged.unlink()
    except OSError:
        pass


def write_exclusive(path: Path, payload: str) -> tuple[bool, str]:
    """Create `path` with the bytes `payload`, or change nothing at all.

    The write is staged in the target directory and then linked into place, because `link` is the
    one creation call that refuses an existing name atomically. A separate existence check followed
    by a write would leave a window in which a concurrent writer, including another lane's
    reconcile of the same run, could be overwritten. A staged file that cannot be linked is kept as
    evidence of the failure rather than deleted, so nothing this run produced disappears silently.
    """
    staged = path.with_name(f"{path.name}.staged-{os.getpid()}")
    try:
        with staged.open("x") as f:
            f.write(payload)
            f.flush()
            os.fsync(f.fileno())
    except FileExistsError:
        return False, f"the staging path {staged} already exists, so nothing was written"
    except OSError as exc:
        return False, f"{path} could not be staged: {exc}; nothing was written"
    try:
        os.link(staged, path)
    except FileExistsError:
        _discard_staged(staged)
        return False, f"{path} already exists, so nothing was written"
    except OSError as exc:
        return False, f"{path} could not be created: {exc}; the staged copy is kept at {staged}"
    _discard_staged(staged)
    return True, str(path)


def git_optional(repo: Path, *argv: str) -> str:
    """git's answer, or an empty string. Absence is a fact here, not an error."""
    done = subprocess.run(["git", "-C", str(repo), *argv], capture_output=True, text=True,
                          timeout=60, check=False)
    return "" if done.returncode != 0 else done.stdout.strip()


def script_revision(repo: Path, script: Path) -> dict:
    """The revision of this harness, but only where the bytes can prove it.

    A checkout's HEAD says nothing about which bytes ran, so it is recorded as ambient context and
    never as the origin of an analysis. The revision is believed only when the blob HEAD records for
    this path is the same object as the script actually on disk. No repository, an unrelated
    checkout, or a modified script all give UNKNOWN, because a guess would misattribute the result
    to code that did not produce it.
    """
    ambient = {"top_level": None, "head": None,
               "relevance": "the checkout this ran in, not the origin of this analysis"}
    try:
        ambient["top_level"] = git_optional(repo, "rev-parse", "--show-toplevel") or None
        ambient["head"] = git_optional(repo, "rev-parse", "HEAD") or None
    except (OSError, subprocess.SubprocessError):
        ambient["top_level"] = ambient["head"] = None
    if not ambient["head"] or not ambient["top_level"]:
        return {"analyser_revision": "UNKNOWN", "ambient_checkout": ambient,
                "analyser_revision_evidence": "no git checkout, so no revision can be proved"}
    try:
        rel = str(script.resolve().relative_to(Path(ambient["top_level"]).resolve()))
    except ValueError:
        return {"analyser_revision": "UNKNOWN", "ambient_checkout": ambient,
                "analyser_revision_evidence": "the script is outside the checkout this ran in"}
    blob = git_optional(repo, "rev-parse", f"HEAD:{rel}")
    actual = git_optional(repo, "hash-object", str(script.resolve()))
    if blob and actual and blob == actual:
        return {"analyser_revision": ambient["head"], "ambient_checkout": ambient,
                "analyser_revision_evidence": f"the blob HEAD records for {rel} is the script on disk",
                "tracked_path": rel}
    return {"analyser_revision": "UNKNOWN", "ambient_checkout": ambient,
            "analyser_revision_evidence": f"the script on disk is not the blob HEAD records for {rel}"}


def analysis_provenance(repo: Path, run_dir: Path, tests_root: Path | None, tool: dict | None) -> dict:
    """What this analysis is, and which bytes it read.

    The result belongs to this output, not to the runtime that captured the run: a reconciliation
    is a fresh reading of preserved records, so it carries its own revision and its own input
    hashes, and a reader can tell which reading it is looking at.
    """
    inputs = {}
    for name in ("cases.jsonl", "identity.json", "summary.json", "daemon.json"):
        candidate = run_dir / name
        if candidate.is_file():
            inputs[name] = sha256(candidate)
    raw = run_dir / "raw"
    if raw.is_dir():
        inputs["raw/"] = {p.name: sha256(p) for p in sorted(raw.iterdir()) if p.is_file()}
    provenance = {
        "analyser": "bench/pjdfstest.py",
        "analyser_sha256": sha256(Path(__file__).resolve()),
        "pinned_tool_commit": PINNED_COMMIT,
        "tool_source_problems": (tool or {}).get("problems", []),
        "tool_metadata": (tool or {}).get("metadata", {}),
        "tests_root": str(tests_root) if tests_root else None,
        "inputs": inputs,
        "note": "This file is a fresh reading of the inputs above. It says nothing about the runtime "
                "that captured them, and it does not replace any receipt in the run directory.",
    }
    provenance.update(script_revision(repo, Path(__file__).resolve()))
    return provenance


def cmd_reconcile(args: argparse.Namespace) -> int:
    """Re-derive a verdict from a run directory's own records. No mount, no daemon, no build."""
    run_dir = Path(args.reconcile).resolve()
    repo = Path(args.repo).resolve()
    # Pairing is proved from the pinned case scripts, so they are a prerequisite and are verified
    # before anything is classified. Silence here would let a run with no scripts pair nothing,
    # find no divergence and report PASS for a gate that never ran.
    src = Path(args.tool) if args.tool else repo / "bench" / "out" / "ready-g3" / "tool" / "pjdfstest"
    tool, tests_root, tool_problem = None, None, None
    if not (src / "tests").is_dir():
        tool_problem = (f"{src} is not a pjdfstest checkout. A reconciliation pairs assertions by "
                        f"reading the pinned case scripts, so pass --tool with a checkout of "
                        f"{PINNED_COMMIT} or with the curated closure.")
    else:
        tool, tests_root = verify_tool(src), src / "tests"
        if tool["problems"]:
            tool_problem = (f"the pjdfstest source at {src} is not the pinned {PINNED_COMMIT}: "
                            f"{tool['problems'][0]}")
    if tool_problem:
        refused = {"state": INVALID, "exit_status": EXIT_STATUS[INVALID],
                   "reasons": [reason(INTEGRITY, tool_problem)],
                   "analysis": analysis_provenance(repo, run_dir, None, tool)}
        for item in refused["reasons"]:
            log(f"  - [{item['kind']}] {item['message']}")
        log(f"state {refused['state']} exit {refused['exit_status']}")
        log("refused: nothing was classified and nothing was written")
        return refused["exit_status"]
    # The identity comes from the run's own receipt, written while its mount was up. A run without
    # one cannot be placed on a filesystem, so it is INVALID rather than quietly unpairable.
    receipt = run_dir / "identity.json"
    document = json.loads(receipt.read_text()) if receipt.is_file() else {}
    identity = document.get("runtime_identity")
    report = verdict(run_dir, tool, tests_root, identity)
    report["analysis"] = analysis_provenance(repo, run_dir, tests_root, tool)
    # A receipt that declares itself sanitised says so in the analysis too, so the reader sees the
    # scope next to the verdict instead of having to remember it.
    report["analysis"]["identity_receipt"] = {
        "path": str(receipt),
        "sha256": sha256(receipt) if receipt.is_file() else None,
        "declared_scope": (identity or {}).get("declared_scope"),
        "sanitisation": (identity or {}).get("sanitisation"),
    }

    def emit() -> None:
        log(f"state {report['state']} exit {report['exit_status']}")
        for item in report["reasons"]:
            log(f"  - [{item['kind']}] {item['message']}")

    # A run directory this harness never created has nowhere to write its receipt, so the refusal
    # is printed and the exit status is the answer.
    if not run_dir.is_dir():
        emit()
        return report["exit_status"]

    # An analysis never writes into the evidence it reads, so an explicit destination has to sit
    # outside the run directory. The default sits beside the records for convenience and is
    # refused the moment anything is there.
    out = resolve_output(args.output) if args.output else run_dir / "reconciliation.json"
    if args.output and (out == run_dir or run_dir in out.parents):
        report["reasons"].insert(0, reason(
            INTEGRITY, f"{out} is inside the run directory it reads, so an analysis would land among "
                       f"the evidence it derives from; pass --output with a path outside {run_dir}"))
        reseal(report)
        emit()
        return report["exit_status"]
    written, message = write_exclusive(out, json.dumps(report, indent=2, sort_keys=True, default=str))
    if not written:
        report["reasons"].insert(0, reason(INTEGRITY, message))
        reseal(report)
        emit()
        log(f"refused: {message}")
        return report["exit_status"]
    emit()
    diff = report.get("comparison")
    if diff:
        log(f"established regressions {len(diff['established_regressions'])}, "
            f"candidates {len(diff['candidate_regressions'])}, unpairable {len(diff['unpairable'])}, "
            f"looser {len(diff['looser_not_a_pass'])}")
    log(f"wrote {out}")
    return report["exit_status"]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    ap.add_argument("--out", type=Path)
    ap.add_argument("--tests", help="comma-separated case paths under the suite's tests/")
    ap.add_argument("--groups", help="comma-separated group dirs")
    ap.add_argument("--case-timeout", type=int, default=CASE_TIMEOUT)
    ap.add_argument("--keep-mount", action="store_true")
    ap.add_argument("--reconcile", help="re-derive a verdict from an existing run directory")
    ap.add_argument("--tool", help="pjdfstest checkout to verify the pinned source against")
    ap.add_argument("--output", help="where --reconcile writes; must be outside the run it reads, "
                                     "and must not already exist")
    args = ap.parse_args()

    if args.reconcile:
        return cmd_reconcile(args)

    repo = args.repo.resolve()
    out = (args.out or (repo / "bench" / "out" / "ready-g3" / "run")).resolve()
    run_dir = out / time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    run_dir.mkdir(parents=True, exist_ok=True)
    jsonl = run_dir / "cases.jsonl"
    jsonl.touch()
    registry = Registry()

    src = fetch_tool(repo / "bench" / "out" / "ready-g3")
    source = verify_tool_source(src)
    if source["problems"]:
        for problem in source["problems"]:
            log(f"tool source: {problem}")
        return EXIT_STATUS[INVALID]
    binary, features = build_tool(src)
    tool = tool_identity(src, binary, features, source)
    log(f"tool {PINNED_COMMIT} binary {tool['binary_sha256'][:16]} features {len(features)}")

    tests_root = src / "tests"
    tests = test_list(tests_root,
                      args.groups.split(",") if args.groups else None,
                      args.tests.split(",") if args.tests else None)
    if not tests:
        log("no cases selected: nothing to measure")
        return EXIT_STATUS[INVALID]

    daemon_bin, cli, cowfs_build = ensure_binaries(repo)
    store, mount = run_dir / "store", run_dir / "mnt"
    # sun_path holds 103 bytes and a lease path alone is 88 of them.
    sock = repo / "rt" / "c.sock"
    if len(str(sock).encode()) > 103:
        log(f"control socket {sock} is {len(str(sock).encode())} bytes; sun_path holds 103")
        return EXIT_STATUS[INVALID]
    identity, arm_fs = None, {}
    try:
        identity = start_daemon(registry, daemon_bin, store, mount, sock, run_dir / "daemon.log")
        (run_dir / "daemon.json").write_text(
            json.dumps({**{k: v for k, v in identity.items() if k not in ("entry",)},
                        "cowfs_build": cowfs_build}, indent=2, sort_keys=True, default=str))
        log(f"daemon pid {identity['pid']} core on {mount}")
        create_snapshot(cli, sock, SNAPSHOT, mount)
        native_root, cowfs_root = run_dir / "native", mount / SNAPSHOT
        # The native root has to exist before it can be identified, or the separation check below
        # has nothing to compare and silently proves nothing.
        native_root.mkdir(parents=True, exist_ok=True)
        native_fs, cowfs_fs = fs_identity(native_root), fs_identity(cowfs_root)
        arm_fs = {"native": native_fs, "cowfs": cowfs_fs}
        log(f"native arm on {native_fs['fstype']} {native_fs['mountpoint']} dev={native_fs['st_dev']}")
        log(f"cowfs arm on {cowfs_fs['fstype']} {cowfs_fs['mountpoint']} dev={cowfs_fs['st_dev']}")
        # Fail closed before a single case runs: an arm we could not place, or two arms on one
        # device, is not a measurement. The receipt is written here, while the mount is up, so the
        # identity survives teardown.
        identity_problems = validate_runtime_identity(arm_fs, expected_cowfs_mount=mount)
        run_tests_root, suite_input = suite_input_receipt(native_root, cowfs_root, tests_root, run_dir,
                                                          binary)
        log(f"suite input {json.dumps(suite_input['pathconf'], sort_keys=True)} "
            f"injection {(suite_input['injection'] or {}).get('injected_line')}")
        (run_dir / "identity.json").write_text(json.dumps({
            "runtime_identity": arm_fs,
            "validated": not identity_problems,
            "problems": identity_problems,
            "suite_input": suite_input,
            "mount_table_line": identity["mount_table_line"],
            "daemon": {k: identity[k] for k in ("pid", "argv", "registered_at", "store", "socket")},
            "cowfs_build": cowfs_build,
            "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%S%z", time.localtime()),
        }, indent=2, sort_keys=True, default=str))
        if identity_problems:
            for problem in identity_problems:
                log(f"runtime identity: {problem['message']}")
            log(f"refusing to score: no case ran. receipt {run_dir / 'identity.json'}")
            return EXIT_STATUS[INVALID]
        log("native arm")
        run_arm(registry, "native", tests, run_tests_root, native_root, run_dir / "raw", jsonl,
                args.case_timeout)
        log("cowfs arm")
        run_arm(registry, "cowfs", tests, run_tests_root, cowfs_root, run_dir / "raw", jsonl,
                args.case_timeout)
    finally:
        if identity is not None:
            log(stop_daemon(registry, identity, keep_mount=args.keep_mount))
            # The socket directory belongs to this run and to nothing else, so it goes with it.
            # Only names this harness creates, and only once the socket itself is gone.
            leftovers = {sock.name, f"{sock.name}.lock"}
            if not sock.is_socket():
                for entry in sock.parent.glob("*") if sock.parent.is_dir() else []:
                    if entry.name in leftovers:
                        entry.unlink()
                if sock.parent.is_dir() and not any(sock.parent.iterdir()):
                    sock.parent.rmdir()

    summary = verdict(run_dir, verify_tool_source(src), tests_root, arm_fs or None)
    summary["host"] = {
        "uname": subprocess.run(["uname", "-srm"], capture_output=True, text=True,
                                check=False).stdout.strip(),
        # The identity each arm was measured on, read while the mount was up. Reading it again
        # after teardown would report a filesystem that is no longer there.
        "native_fs": arm_fs.get("native"), "cowfs_fs": arm_fs.get("cowfs"),
        "mount_table_line": identity["mount_table_line"] if identity else None,
        "daemon": {k: identity[k] for k in ("pid", "argv", "registered_at", "store", "socket")}
        if identity else None,
        "cowfs_build": cowfs_build,
    }
    summary["tool"] = tool
    (run_dir / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True, default=str))
    log(f"state {summary['state']} exit {summary['exit_status']}")
    for item in summary["reasons"]:
        log(f"  - [{item['kind']}] {item['message']}")
    log(f"identity receipt {run_dir / 'identity.json'}")
    log(f"evidence {run_dir}")
    return summary["exit_status"]


if __name__ == "__main__":
    sys.exit(main())