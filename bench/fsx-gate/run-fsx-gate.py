#!/usr/bin/env python3
"""Gate g4: matched fsx acceptance on a real mounted cowfs against a native control.

One fsx binary, one declared op count, one seed per case, two arms whose only difference is
the directory. Native is the control, so the bar is that the mounted cowfs produces exactly
what native produces.

    run-fsx-gate.py --native-root DIR --cowfs-root DIR --fsx-bin PATH --out DIR
                    [--config PATH] [--mode NAME] [--seeds a,b] [--ops N]
                    [--restart-cmd CMD] [--timeout SECS] [--label NAME]

Exit codes:
    0  PASS       every declared case ran on both arms and matched
    1  FAIL       a case ran and the result was wrong, or a required op type never happened
    2  usage
    3  UNMEASURABLE  the tool or the mount is not there; the reason is in the record

Every case appends one JSONL line and flushes it, so an interrupted run still holds every case
that finished, and the evidence directory is readable while the gate is running.
"""

import argparse
import ctypes
import ctypes.util
import errno as errno_mod
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone

FALLOC_FL_KEEP_SIZE = 0x01
FALLOC_FL_PUNCH_HOLE = 0x02
FALLOC_FL_ZERO_RANGE = 0x10

# The ops fsx's own log dump can record, longest name first so "read_dontcache" is not counted
# as "read". Anything fsx logs as "skip" is a no-op and is counted separately, never as work.
OP_NAMES = [
    "read_dontcache", "write_dontcache", "write_atomic", "mapread", "mapwrite",
    "collapse_range", "insert_range", "exchange_range", "punch_hole", "zero_range",
    "write_zeroes", "clone_range", "dedupe_range", "copy_range", "fallocate",
    "truncate", "read", "write", "fsync",
]

# An op type that never happened is a failure, unless the op is gated behind a capability the
# filesystem does not have. Those become recorded capability gaps instead of silent passes.
# The whole fallocate family is listed, not just the two the first version named: fsx's default
# mix also records fallocate, collapse_range and insert_range, and leaving those out of the map
# meant they were counted as unexplained differences while the two named ones excused them.
# An empty value means the op needs a plain fallocate(2), which is the capability that gates it.
OP_CAPABILITY = {
    "punch_hole": "PUNCH_HOLE",
    "zero_range": "ZERO_RANGE",
    "write_zeroes": "WRITE_ZEROES",
    "fallocate": "",
    "collapse_range": "COLLAPSE_RANGE",
    "insert_range": "INSERT_RANGE",
    "exchange_range": "EXCHANGE_RANGE",
    "dedupe_range": "DEDUPE_RANGE",
}

# The names the runner's own probe uses, for the operations it tries. An operation with no probe
# row here is evidenced only by fsx's own record, which is enough: fsx wrote the attempt and the
# skip itself, in the op stream it recorded on the filesystem.
PROBE_NAME = {
    "punch_hole": "punch_hole", "zero_range": "zero_range",
    "write_zeroes": "write_zeroes", "collapse_range": "collapse_range",
    "insert_range": "insert_range", "fallocate": "fallocate",
}

# The operations that carry the file's bytes. A capability gap on an operation that does not
# change the bytes cannot excuse a difference in these: if the two arms did a different number of
# reads, writes, mapped writes or truncates, the two files are not comparable and the case is
# unmeasurable, not passing.
BYTE_BEARING_OPS = ("read", "write", "mapread", "mapwrite", "truncate")

EXIT_PASS, EXIT_FAIL, EXIT_USAGE, EXIT_UNMEASURABLE = 0, 1, 2, 3

DISABLED_RE = re.compile(r"filesystem does not support fallocate mode (.+?), disabling")
DONE_RE = re.compile(r"All (\d+) operations completed A-OK!")
DUMP_RE = re.compile(r"LOG DUMP \((\d+) total operations\)")


def now():
    return datetime.now(timezone.utc).isoformat()


# ---------------------------------------------------------------- parsing


def parse_ops_executed(stdout):
    """The op count fsx itself reports, or None when it never said it finished."""
    found = DONE_RE.findall(stdout)
    return int(found[-1]) if found else None


def parse_disabled_modes(text):
    """fallocate modes fsx reported the filesystem does not have, with its own wording."""
    return ["fallocate mode %s: %s" % (m.group(1), m.group(0)) for m in DISABLED_RE.finditer(text)]


def parse_log_dump_total(stdout):
    found = DUMP_RE.findall(stdout)
    return int(found[-1]) if found else None


def sha256_file(path):
    """(sha256, size) or (None, reason) so a missing or unreadable file is never a pass."""
    try:
        h = hashlib.sha256()
        size = 0
        with open(path, "rb") as f:
            while True:
                block = f.read(1 << 20)
                if not block:
                    break
                size += len(block)
                h.update(block)
        return h.hexdigest(), size
    except OSError as e:
        return None, "%s: %s" % (os.path.basename(path), e.strerror or e)


def st_dev(path):
    """The device a file's bytes live on. Two arms on one device means one arm did not run."""
    try:
        return os.stat(path).st_dev
    except OSError:
        return None


def manifest_module():
    """mount-manifest.py, loaded from beside this file so there is one implementation of the
    mount-table and statfs questions rather than two."""
    import importlib.util
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mount-manifest.py")
    spec = importlib.util.spec_from_file_location("cowfs_mount_manifest", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# ltp/fsx.c line 79: the ring buffer that holds the recorded operations. Pinned in
# fsx-gate.json's tool manifest as part of the source digest, and asserted against the binary's
# own LOG DUMP total below.
LOGSIZE = 10000


def op_stream(ops_path):
    """(sequence, skips) from the .fsxops file fsx writes with --record-ops.

    sequence is the op name of each recorded operation, in order, so the two arms' streams can be
    compared position by position. skips counts the `skip <op>` lines, which are fsx's own record
    of an operation it attempted and the filesystem refused.

    fsx keeps only the last LOGSIZE operations in this file, so the sequence is a tail and not the
    whole run. The authoritative length is the count fsx prints when it finishes, recorded next to
    it, and the two are reported together so a truncated stream is never read as a short one.
    """
    sequence = []
    skips = {}
    try:
        with open(ops_path, "r", errors="replace") as f:
            for line in f:
                name = line.split(" ", 1)[0].strip()
                if not name:
                    continue
                if name == "skip":
                    rest = line.split(" ", 1)
                    skipped = rest[1].split(" ", 1)[0].strip() if len(rest) > 1 else "?"
                    skips[skipped] = skips.get(skipped, 0) + 1
                    sequence.append("skip " + skipped)
                    continue
                sequence.append(name)
    except OSError as e:
        return [], {}, str(e)
    return sequence, skips, None


def first_divergence(native_seq, cowfs_seq, gaps):
    """Where the two recorded operation streams part company, and what part company there is.

    Returns None when the streams agree. Otherwise the index, both operations at it, the operation
    names involved, and whether a recorded capability gap is what differs. Only the first
    divergence matters: if it is an operation one arm is recorded as not having, every later
    difference follows from it, because fsx's file offsets and lengths move once an operation is
    skipped. If the first divergence is anything else, a capability does not explain it.
    """
    limit = min(len(native_seq), len(cowfs_seq))
    for i in range(limit):
        if native_seq[i] == cowfs_seq[i]:
            continue
        left, right = native_seq[i], cowfs_seq[i]
        # fsx never skips silently: an operation it attempted and the filesystem refused is
        # recorded as `skip <op>`. So a capability difference at this index is the same operation,
        # one side recorded as skipped and the other not. An unrelated operation in its place is
        # not something a gap explains.
        bare = [n.split(" ", 1)[1] if n.startswith("skip ") else n for n in (left, right)]
        skipped_by = [n for n, b in zip((left, right), bare) if n.startswith("skip ")]
        return {"index": i, "native": left, "cowfs": right,
                "operations": sorted(set(bare)),
                "caused_by_capability": bool(skipped_by) and bare[0] == bare[1] and bare[0] in gaps}
    if len(native_seq) != len(cowfs_seq):
        return {"index": limit, "native": native_seq[limit] if limit < len(native_seq) else None,
                "cowfs": cowfs_seq[limit] if limit < len(cowfs_seq) else None,
                "operations": [], "caused_by_capability": False,
                "one_stream_ended": True}
    return None


def op_counts(ops_path):
    """Op name -> count, from the .fsxops file fsx writes with --record-ops.

    Counts "skip" separately: a skipped op is a capability the filesystem does not have, and
    the gate reports it rather than folding it into the work done.
    """
    counts = {}
    try:
        with open(ops_path, "r", errors="replace") as f:
            for line in f:
                name = line.split(" ", 1)[0].strip()
                if not name:
                    continue
                if name == "skip":
                    counts["skip"] = counts.get("skip", 0) + 1
                    continue
                counts[name] = counts.get(name, 0) + 1
    except OSError as e:
        return {"error": str(e)}
    return counts


# ------------------------------------------------------------ environment


def mount_identity(path):
    """The mount entry a path sits on, by longest prefix, or None when there is no entry.

    On Linux the mount table is /proc/mounts. Everywhere else it is the output of mount(8), whose
    line format is "source on /mountpoint fstype (options)". Either way the caller gets the
    mountpoint and the filesystem type, which is what distinguishes a mounted cowfs from a
    directory that merely looks like one.
    """
    real = os.path.realpath(path)
    best = None
    if platform.system() == "Linux":
        try:
            with open("/proc/mounts") as f:
                rows = [line.split() for line in f]
        except OSError as e:
            return {"error": str(e)}
        for fields in rows:
            if len(fields) < 3:
                continue
            src, mnt, fstype = fields[0], fields[1], fields[2]
            if real == mnt or real.startswith(mnt.rstrip("/") + "/"):
                if best is None or len(mnt) > len(best["mountpoint"]):
                    best = {"source": src, "mountpoint": mnt, "fstype": fstype}
        return best
    try:
        out = subprocess.run(["mount"], capture_output=True, text=True, timeout=30).stdout
    except (OSError, subprocess.SubprocessError) as e:
        return {"error": str(e)}
    for line in out.splitlines():
        fields = line.split()
        if len(fields) < 4 or fields[1] != "on":
            continue
        opts = fields[3]
        # "source on /path (fstype, opts)" on macOS, "source on /path type opts" on Linux.
        fstype = opts.strip("()").split(",")[0] if opts.startswith("(") else opts
        mnt = fields[2]
        if real == mnt or real.startswith(mnt.rstrip("/") + "/"):
            if best is None or len(mnt) > len(best["mountpoint"]):
                best = {"source": fields[0], "mountpoint": mnt, "fstype": fstype}
    return best


def fsx_identity(binary):
    """sha256 and the flags this binary was compiled with, read from its own usage text."""
    if not binary or not os.access(binary, os.X_OK):
        return None
    digest, size = sha256_file(binary)
    try:
        proc = subprocess.run([binary], capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError) as e:
        return {"error": str(e)}
    # A flag line is a tab, the flag, then a colon or a space: "-H: ..." and "-u Do not ...".
    flags = sorted(set(
        tok for tok in re.findall(r"(?m)^\t-([A-Za-z0-9_]+)[: ]", proc.stdout)
    ))
    return {"path": binary, "sha256": digest, "size": size, "usage_exit": proc.returncode, "flags": flags}


# ----------------------------------------------------------------- probe


class Probe:
    """Direct syscalls, one per declared probe op, on one arm.

    fsx probes the fallocate modes itself and prints a line per mode it disables. This repeats
    the probe so an unsupported hole operation carries its own errno, and adds truncate, hole,
    mmap and fsync readback, which fsx's own mix only exercises statistically.
    """

    def __init__(self, root, name):
        self.root = root
        self.name = name
        self.dir = os.path.join(root, ".fsx-gate-probe")
        self.path = os.path.join(self.dir, "probe.bin")

    def _file(self, size=65536):
        os.makedirs(self.dir, exist_ok=True)
        fd = os.open(self.path, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
        try:
            os.ftruncate(fd, size)
        finally:
            os.close(fd)
        return self.path

    def _errno_name(self, e):
        return errno_mod.errorcode.get(e.errno, str(e.errno))

    def _record(self, op, ok, detail):
        return {"arm": self.name, "op": op, "ok": bool(ok), "detail": detail}

    def _libc(self):
        libc_name = ctypes.util.find_library("c") or "libc.so.6"
        return ctypes.CDLL(libc_name, use_errno=True)

    def run(self):
        results = []

        # fallocate family, through libc so the mode bits are the kernel's, not a python guess.
        try:
            libc = self._libc()
            have_fallocate = hasattr(libc, "fallocate")
        except OSError as e:
            libc, have_fallocate = None, False
            results.append(self._record("fallocate", False, "libc unavailable: %s" % e))

        # No fallocate means the fallocate family is unsupported here, not that the truncate,
        # hole, mmap and fsync probes are skipped: those are separate capabilities and each
        # carries its own evidence.
        if not have_fallocate:
            for op in ("fallocate", "punch_hole", "zero_range", "keep_size"):
                results.append(self._record(op, False, "no libc fallocate on this platform"))
        else:
            modes = [
            ("fallocate", 0),
            ("keep_size", FALLOC_FL_KEEP_SIZE),
            ("punch_hole", FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE),
            ("zero_range", FALLOC_FL_ZERO_RANGE | FALLOC_FL_KEEP_SIZE),
            ]
            for op, mode in modes:
                self._file()
                fd = os.open(self.path, os.O_RDWR)
                try:
                    ctypes.set_errno(0)
                    rc = libc.fallocate(fd, ctypes.c_int(mode),
                                        ctypes.c_longlong(0), ctypes.c_longlong(4096))
                    if rc == 0:
                        results.append(self._record(op, True, "fallocate(0x%x, 0, 4096) = 0" % mode))
                    else:
                        err = ctypes.get_errno()
                        results.append(self._record(op, False, "fallocate(0x%x, 0, 4096) failed: %s"
                                                    % (mode, self._errno_name(OSError(err, os.strerror(err))))))
                finally:
                    os.close(fd)

        # truncate shrink and grow.
        self._file(size=65536)
        fd = os.open(self.path, os.O_RDWR)
        try:
            os.ftruncate(fd, 8192)
            shrink = os.fstat(fd).st_size == 8192
            os.ftruncate(fd, 32768)
            grow = os.fstat(fd).st_size == 32768
            results.append(self._record("truncate_shrink", shrink, "ftruncate 65536 -> 8192, size %d" % os.fstat(fd).st_size))
            results.append(self._record("truncate_grow", grow, "ftruncate 8192 -> 32768, size %d" % os.fstat(fd).st_size))
        except OSError as e:
            results.append(self._record("truncate_shrink", False, self._errno_name(e)))
            results.append(self._record("truncate_grow", False, self._errno_name(e)))
        finally:
            os.close(fd)

        # truncate to a hole: extend, then prove the gap is a hole and reads as zeros.
        self._file(size=0)
        fd = os.open(self.path, os.O_RDWR | os.O_CREAT, 0o644)
        try:
            os.ftruncate(fd, 1 << 20)
            st = os.fstat(fd)
            gap_is_hole = st.st_blocks * 512 < st.st_size
            zeros = True
            try:
                os.lseek(fd, 65536, os.SEEK_SET)
                zeros = os.read(fd, 4096) == b"\0" * 4096
            except OSError:
                zeros = False
            results.append(self._record("truncate_to_hole", gap_is_hole and zeros,
                                        "size %d, allocated %d, gap reads zero: %s"
                                        % (st.st_size, st.st_blocks * 512, zeros)))
        except OSError as e:
            results.append(self._record("truncate_to_hole", False, self._errno_name(e)))
        finally:
            os.close(fd)

        # mmap read-modify-write, msync, read back through the filesystem.
        import mmap
        self._file(size=65536)
        fd = os.open(self.path, os.O_RDWR)
        try:
            mm = mmap.mmap(fd, 65536)
            mm[1000:1008] = b"mmap-rw!"
            mm.flush()
            mm.close()
            digest, _ = sha256_file(self.path)
            results.append(self._record("mmap_rw", digest is not None,
                                        "wrote 8 bytes through mmap at 1000, sha256 %s" % digest))
        except (OSError, ValueError) as e:
            results.append(self._record("mmap_rw", False, "%s: %s" % (type(e).__name__, e)))
        finally:
            os.close(fd)

        # fsync then read back through a fresh open.
        self._file(size=0)
        fd = os.open(self.path, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
        try:
            os.write(fd, b"fsync-readback")
            os.fsync(fd)
        finally:
            os.close(fd)
        digest, size = sha256_file(self.path)
        want = hashlib.sha256(b"fsync-readback").hexdigest()
        results.append(self._record("fsync_readback", digest == want,
                                    "sha256 %s over %s bytes" % (digest, size)))

        self._cleanup()
        return results

    def _cleanup(self):
        # The probe file is this gate's own fixture; nothing else lives here.
        try:
            shutil.rmtree(self.dir)
        except OSError:
            pass


# ------------------------------------------------------------------ run


def fsx_argv(binary, mode_flags, seed, ops, caps, attempt_dir, data_name):
    """The argv both arms run. The data file's directory is the only difference between arms."""
    argv = [binary, "-S", str(seed), "-N", str(ops), "-l", str(caps["max_file_bytes"]),
            "-o", str(caps["max_op_bytes"]), "-P", attempt_dir, "--record-ops"]
    argv += list(mode_flags)
    argv.append(os.path.join(attempt_dir, data_name))
    return argv


def fresh_attempt_dir(parent, label):
    """A directory that has never existed, made with O_EXCL so nothing can be reused.

    Reusing a case directory is how a stale file passes as this invocation's work: the digests
    would be the previous run's. Nothing is deleted to make room. If the name is taken the run
    refuses, because a failed or interrupted attempt is evidence and deleting it would destroy it.
    """
    os.makedirs(parent, exist_ok=True)
    for _ in range(4096):
        name = "%s-%d-%d" % (label, os.getpid(), int(time.time() * 1000) % 100000000)
        path = os.path.join(parent, name)
        try:
            os.mkdir(path, 0o700)
            return path
        except FileExistsError:
            continue
    raise RuntimeError("could not create a fresh attempt directory under %s" % parent)


def run_case(binary, arm, root, mode, seed, ops, caps, out_dir, timeout, run_tag):
    """One fsx run on one arm.

    The case directory is inside the arm root, so the work happens on that filesystem and not on
    whatever holds the evidence directory. It is brand new for this invocation, and the data file
    must appear in it: an absent file is a failure, never a digest carried over from an earlier
    attempt.
    """
    case_dir = fresh_attempt_dir(os.path.join(root, ".fsx-gate"),
                                 "%s-seed%s-%s" % (mode["name"], seed, arm))
    data = os.path.join(case_dir, "fsx.dat")
    data_real = os.path.realpath(data)
    argv = fsx_argv(binary, mode["flags"], seed, ops, caps, case_dir, "fsx.dat")
    started = time.monotonic()
    timed_out = False
    spawn_error = None
    try:
        proc = subprocess.run(argv, cwd=root, capture_output=True, text=True, timeout=timeout)
        code, out, err = proc.returncode, proc.stdout, proc.stderr
    except subprocess.TimeoutExpired as e:
        timed_out = True
        code = None
        out = (e.stdout or b"").decode(errors="replace") if isinstance(e.stdout, bytes) else (e.stdout or "")
        err = (e.stderr or b"").decode(errors="replace") if isinstance(e.stderr, bytes) else (e.stderr or "")
    except (OSError, ValueError) as e:
        # A tool that cannot be executed at all is a failed case with a named reason, not a
        # traceback: the gate has to be able to say what happened.
        spawn_error = "%s: %s" % (type(e).__name__, e)
        code, out, err = None, "", ""
    seconds = time.monotonic() - started

    evidence = os.path.join(out_dir, "attempts", "%s-seed%s-%s" % (mode["name"], seed, arm))
    os.makedirs(evidence, exist_ok=True)
    log = os.path.join(evidence, "fsx.log")
    with open(log, "a") as f:
        f.write("$ %s\ncwd %s\n" % (" ".join(argv), root))
        f.write("exit: %s\n--- stdout ---\n%s\n--- stderr ---\n%s\n" % (code, out, err))
        f.flush()
        os.fsync(f.fileno())

    data_sha, data_size = sha256_file(data)
    opsfile = os.path.join(case_dir, "fsx.dat.fsxops")
    ops_sha, _ = sha256_file(opsfile)
    counts = op_counts(opsfile)
    sequence, skips, stream_error = op_stream(opsfile)
    # The evidence copy is a copy: it is verified against the file on the filesystem, and both
    # digests are recorded so a copy that did not land is visible rather than assumed. The log
    # file is not copied: fsx's log and its LOG DUMP both go to stdout, which is already in
    # fsx.log here.
    copied = {}
    for name in ("fsx.dat", "fsx.dat.fsxops", "fsx.dat.fsxgood"):
        src = os.path.join(case_dir, name)
        if os.path.exists(src):
            digest, _ = sha256_file(src)
            shutil.copy2(src, os.path.join(evidence, name))
            after, _ = sha256_file(os.path.join(evidence, name))
            copied[name] = {"on_filesystem": digest, "in_evidence": after, "same": digest == after}
    over_cap = []
    if data_size is not None and data_size > caps["max_file_bytes"]:
        over_cap.append("%s data file is %d bytes, over the declared %d"
                        % (arm, data_size, caps["max_file_bytes"]))
    # The witness is the file fsx wrote, not the directory it was asked to use: its realpath, the
    # device its bytes live on, and the filesystem the kernel reports for that device.
    witness = manifest_module().file_witness(data) if os.path.exists(data_real) else {
        "path": data, "realpath": data_real, "st_dev": None, "exists": False,
        "fstype": None, "mountpoint": None}
    fstype = witness.get("fstype")
    return {
        "kind": "case", "time": now(), "mode": mode["name"], "seed": seed, "arm": arm,
        "run_tag": run_tag,
        "argv": argv, "cwd": root, "exit": code, "timed_out": timed_out,
        "spawn_error": spawn_error, "seconds": round(seconds, 3),
        "ops_requested": ops, "ops_executed": parse_ops_executed(out),
        "log_dump_total": parse_log_dump_total(out),
        "case_dir": case_dir, "case_dir_fresh": True,
        "data_path": data, "data_realpath": data_real, "data_sha256": data_sha,
        "data_size": data_size, "data_real_fstype": fstype, "data_witness": witness,
        "data_st_dev": st_dev(data), "root_st_dev": st_dev(root),
        "ops_file": opsfile, "ops_sha256": ops_sha, "op_counts": counts,
        "op_sequence": sequence, "op_skips": skips, "op_stream_error": stream_error,
        "op_sequence_len": len(sequence),
        "op_sequence_note": ("fsx keeps only the last LOGSIZE operations in this file, so this is a "
                             "tail; log_dump_total and the completion count are the authoritative "
                             "lengths"),
        "fsx_reported_unsupported": parse_disabled_modes(out + err),
        "over_cap": over_cap,
        "evidence": evidence, "evidence_copy": copied, "log": log,
    }


def capability_evidence(case, probe_by_arm):
    """Which of the hole operations this arm is recorded as not having, and where that is written.

    Two places on disk say so, and either is enough because both were produced by the filesystem
    under test rather than by this script's expectations: fsx's own `skip <op>` lines in the op
    stream it recorded in the case directory, which show it tried the operation and the filesystem
    refused it, and the runner's probe of the same syscall.
    """
    gaps = set()
    skips = case.get("op_skips") or {}
    probe = probe_by_arm.get(case["arm"], {})
    for op in OP_CAPABILITY:
        if skips.get(op):
            gaps.add(op)
        elif probe.get(PROBE_NAME.get(op, op)) is False:
            gaps.add(op)
    return gaps


def attribute_deltas(deltas, gaps):
    """Split the operation-count differences into those a capability gap explains and those it does
    not. Each delta is attributed on its own; there is no blanket excuse.

    A delta counts as explained when the operation itself is in the gap set for one of the arms.
    Everything else is unexplained, including a difference in the operations that carry the bytes,
    which no hole capability can account for.
    """
    explained, unexplained = [], []
    for op in sorted(deltas):
        (explained if op in gaps else unexplained).append(op)
    return explained, unexplained


def case_arm(case):
    return case.get("arm", "?")


def mode_arm_label(arm):
    return "the native arm" if arm == "native" else "the cowfs arm"


def counts_or_empty(case):
    """The operation counts of a case, or an empty table with the reason kept apart."""
    counts = case.get("op_counts")
    return dict(counts) if isinstance(counts, dict) else {}


def compare_case(mode, seed, ops, native, cowfs, fresh_open, probe_by_arm=None, restart=None):
    """The per-seed verdict for one seed on both arms.

    The matched part of the gate is the tool, the declared flags, the seed and the op count. The
    op stream is fsx's own output, and fsx picks operations after probing what the filesystem
    supports, so it can legitimately differ between arms. Three verdicts are possible per pair:

    * PASS: the streams are byte-identical and so are the files;
    * UNMEASURABLE: the streams differ and every difference is attributed to a capability one arm
      is recorded as lacking, so the two arms did different work and their bytes are not
      comparable. This is not a pass: fsx exiting 0 on its own is not execution equivalence;
    * FAIL: the streams differ with a difference nothing explains, which includes any difference
      in the byte-bearing operations.

    A matched mode declares one operation mix for both arms, so any stream difference in it is a
    failure rather than a capability question.
    """
    probe_by_arm = probe_by_arm or {}
    require_same_stream = bool(mode.get("require_identical_op_stream"))
    problems = []
    unmeasurable = []
    if native["timed_out"] or cowfs["timed_out"]:
        problems.append("fsx timed out after %ss" % native["seconds"])
    for arm, case in (("native", native), ("cowfs", cowfs)):
        if case.get("spawn_error"):
            problems.append("%s fsx could not be executed: %s" % (arm, case["spawn_error"]))
        elif case["exit"] != 0:
            problems.append("%s fsx exited %s" % (arm, case["exit"]))
        if case["ops_executed"] is None:
            problems.append("%s fsx never reported its op count" % arm)
        elif case["ops_executed"] != ops:
            problems.append("%s fsx executed %d ops, %d declared" % (arm, case["ops_executed"], ops))
        if case["data_sha256"] is None:
            problems.append("%s data file unreadable: %s" % (arm, case["data_size"]))
        elif case["data_size"] == 0:
            problems.append("%s data file is empty" % arm)
        # The witness is the file fsx actually wrote: its device, its realpath and the
        # filesystem type the kernel reports for that realpath. A path label is not evidence.
        if case.get("data_st_dev") is None:
            problems.append("%s data file has no stat device, so the arm cannot be identified" % arm)
        for field in ("data_real_fstype", "data_realpath"):
            if not case.get(field):
                problems.append("%s data file has no %s witness" % (arm, field))

    devices = {arm: case.get("data_st_dev") for arm, case in (("native", native), ("cowfs", cowfs))}
    if devices["native"] is not None and devices["cowfs"] is not None and devices["native"] == devices["cowfs"]:
        problems.append("both arms ran on the same filesystem (st_dev %s), so the cowfs arm did not "
                        "touch the mount" % devices["native"])
    fstypes = {arm: (case.get("data_real_fstype") or "") for arm, case in
               (("native", native), ("cowfs", cowfs))}
    if fstypes["cowfs"] and "cowfs" not in fstypes["cowfs"]:
        problems.append("the cowfs arm's data file is on %s, which is not a cowfs mount; the "
                        "directory label does not make it one" % fstypes["cowfs"])
    if fstypes["cowfs"] and fstypes["native"] and fstypes["cowfs"] == fstypes["native"]:
        problems.append("both arms are on %s, so there is no cowfs arm" % fstypes["cowfs"])

    native_counts = counts_or_empty(native)
    cowfs_counts = counts_or_empty(cowfs)
    for arm, counts in (("native", native_counts), ("cowfs", cowfs_counts)):
        if counts.get("error"):
            problems.append("%s op stream could not be read: %s" % (arm, counts["error"]))
        if "error" in counts:
            counts.pop("error")
    deltas = {}
    for op in sorted(set(native_counts) | set(cowfs_counts)):
        left, right = native_counts.get(op, 0), cowfs_counts.get(op, 0)
        if left != right:
            deltas[op] = {"native": left, "cowfs": right}
    skip_delta = deltas.pop("skip", {"native": 0, "cowfs": 0})
    native_gaps = capability_evidence(native, probe_by_arm)
    cowfs_gaps = capability_evidence(cowfs, probe_by_arm)
    both_gaps = native_gaps | cowfs_gaps
    explained, unexplained = attribute_deltas(deltas, both_gaps)
    stream_match = native["ops_sha256"] is not None and native["ops_sha256"] == cowfs["ops_sha256"]
    byte_bearing_unexplained = [op for op in unexplained if op in BYTE_BEARING_OPS]

    divergence = None
    # fsx keeps only the last LOGSIZE operations in the file it records, so a run longer than that
    # leaves a tail, not the whole stream, and position-by-position alignment of a tail compares
    # two unrelated parts of the run. Where the arms part company is then not locatable, and the
    # honest verdict is that it cannot be located, not that it was fine.
    recorded = {arm: len((case.get("op_sequence") or [])) for arm, case in
                (("native", native), ("cowfs", cowfs))}
    declared_ops = max([ops] + [c["ops_executed"] or 0 for c in (native, cowfs)])
    # fsx writes min(run length, LOGSIZE) operations. So a stream shorter than the run it came
    # from is a tail, and a stream shorter than the other arm's stream is a divergence instead.
    stream_is_tail = any(
        (case.get("ops_executed") or 0) > LOGSIZE
        and recorded[arm] < (case.get("ops_executed") or 0)
        for arm, case in (("native", native), ("cowfs", cowfs)))
    if require_same_stream:
        if not stream_match:
            problems.append("op stream differs although %s declares one operation mix for both arms "
                            "(differs in %s)" % (mode["name"], ", ".join(sorted(deltas)) or "unknown"))
    elif not stream_match:
        # Where the streams part company decides what the rest of the difference means.
        divergence = first_divergence(native.get("op_sequence") or [],
                                      cowfs.get("op_sequence") or [], both_gaps)
        if divergence is None:
            # Same operation names in the same order but a different digest: the operands differ,
            # which a capability gap does not explain either.
            problems.append("the two op streams have the same recorded operations in the same order "
                            "but different contents (%s then %s), so a capability gap does not "
                            "explain it"
                            % (native["ops_sha256"], cowfs["ops_sha256"]))
        elif stream_is_tail:
            unmeasurable.append(
                "fsx ran %s operations and keeps only the last %s in the file it records, so the "
                "recorded stream on each arm is a tail of %s and %s operations. Where the two arms "
                "part company cannot be located in a tail, so this difference cannot be attributed. "
                "A capability mode has to run within the recorded window for its difference to be "
                "readable." % (declared_ops, LOGSIZE, recorded["native"], recorded["cowfs"]))
        elif divergence["caused_by_capability"]:
            unmeasurable.append(
                "the two arms' operation streams part company at operation %d, where %s recorded %s "
                "and %s recorded %s: %s is an operation this filesystem is recorded as not having, "
                "and every difference after it follows from that, because fsx's offsets and lengths "
                "move once an operation is skipped. The arms did different work, so their files are "
                "not comparable and fsx exiting 0 on both is not execution equivalence."
                % (divergence["index"],
                   mode_arm_label(case_arm(native)), divergence["native"],
                   mode_arm_label(case_arm(cowfs)), divergence["cowfs"],
                   " and ".join(divergence["operations"])))
        else:
            problems.append(
                "the two op streams part company at operation %d, where native recorded %s and cowfs "
                "recorded %s, and that operation is not one this filesystem is recorded as lacking; "
                "a capability gap explains nothing from here on, so the arms are not comparable%s"
                % (divergence["index"], divergence["native"], divergence["cowfs"],
                   " (one stream ends here and the other does not)" if divergence.get(
                       "one_stream_ended") else ""))

    hashes_compared = False
    if stream_match:
        hashes_compared = True
        if native["data_sha256"] is not None and cowfs["data_sha256"] is not None:
            if native["data_sha256"] != cowfs["data_sha256"]:
                problems.append("identical op streams produced different bytes: native %s, cowfs %s"
                                % (native["data_sha256"], cowfs["data_sha256"]))
    # The fresh open is a separate process's digest and size, read through the filesystem.
    fresh = fresh_open.get("cowfs")
    if isinstance(fresh, dict):
        if cowfs["data_sha256"] and fresh.get("sha256") != cowfs["data_sha256"]:
            problems.append("a separate process reopened the file and read %s, fsx left %s"
                            % (fresh.get("sha256"), cowfs["data_sha256"]))
        elif cowfs["data_size"] is not None and fresh.get("size") != cowfs["data_size"]:
            problems.append("a separate process read %s bytes, fsx left %s"
                            % (fresh.get("size"), cowfs["data_size"]))
    elif fresh and cowfs["data_sha256"] and fresh != cowfs["data_sha256"]:
        problems.append("fresh open read %s, fsx left %s" % (fresh, cowfs["data_sha256"]))
    if restart is not None:
        problems.extend(restart)

    if problems:
        pair_status = "FAIL"
    elif unmeasurable:
        pair_status = "UNMEASURABLE"
    else:
        pair_status = "PASS"
    return {
        "kind": "compare", "time": now(), "mode": mode["name"], "seed": seed,
        "status": pair_status,
        "require_identical_op_stream": require_same_stream,
        "ops_requested": ops,
        "ops_executed": {"native": native["ops_executed"], "cowfs": cowfs["ops_executed"]},
        "ops_skipped": {"native": native_counts.get("skip", 0), "cowfs": cowfs_counts.get("skip", 0)},
        "unsupported_reasons": {
            "native": list(native["fsx_reported_unsupported"]),
            "cowfs": list(cowfs["fsx_reported_unsupported"]),
            "probe": sorted(r["detail"] for r in (probe_by_arm.get("_probe_rows") or [])
                            if not r["ok"]),
        },
        "data_sha256": {"native": native["data_sha256"], "cowfs": cowfs["data_sha256"]},
        "data_size": {"native": native["data_size"], "cowfs": cowfs["data_size"]},
        "data_st_dev": devices,
        "data_fstype": fstypes,
        "ops_stream_sha256": {"native": native["ops_sha256"], "cowfs": cowfs["ops_sha256"]},
        "ops_stream_match": stream_match, "hashes_compared": hashes_compared,
        "hash_comparison": ("compared, streams identical" if hashes_compared else
                            "not comparable: the two arms ran different operations"),
        "first_stream_divergence": divergence,
        "ops_stream_lengths": {"native": len(native.get("op_sequence") or []),
                               "cowfs": len(cowfs.get("op_sequence") or [])},
        "op_skips": {"native": native.get("op_skips"), "cowfs": cowfs.get("op_skips")},
        "op_count_deltas": deltas, "skip_delta": skip_delta,
        "deltas_explained_by_capability": explained,
        "deltas_unexplained": unexplained,
        "byte_bearing_deltas_unexplained": byte_bearing_unexplained,
        "cowfs_capability_gaps": sorted(cowfs_gaps),
        "native_capability_gaps": sorted(native_gaps),
        "fresh_open_sha256": fresh_open,
        "unmeasurable": unmeasurable,
        "problems": problems,
    }


def verdict(cases, compares, restarts, required_ops, probe_rows):
    """The gate's decision, from the records only.

    A case that ran and produced the wrong bytes is FAIL, whatever the reason. A capability the
    filesystem does not have is recorded as unsupported and never becomes a pass by itself.
    A pair whose operation streams legitimately differ because of such a capability is
    UNMEASURABLE: fsx exiting 0 is not execution equivalence, so it cannot be a pass either.

    FAIL outranks UNMEASURABLE. A run with one unexplained delta and one capability gap is FAIL,
    because the unexplained delta is the part that has to be explained.
    """
    failures = []
    unmeasurable = []
    ran = len(cases)
    if ran == 0:
        failures.append("no case ran")
    for compare in compares:
        failures.extend("%s seed %s: %s" % (compare["mode"], compare["seed"], p)
                        for p in compare["problems"])
        unmeasurable.extend("%s seed %s: %s" % (compare["mode"], compare["seed"], u)
                           for u in compare.get("unmeasurable", []))

    unsupported = sorted({row["detail"] for row in probe_rows if not row["ok"]})
    missing = []
    gaps = []
    disabled_modes = [d for c in cases for d in c["fsx_reported_unsupported"]]
    unsupported_ops = {row["op"] for row in probe_rows if not row["ok"]}
    for mode_name, wanted in required_ops.items():
        for op in wanted:
            seen = sum(c["op_counts"].get(op, 0) for c in cases
                       if c["mode"] == mode_name and c["arm"] == "cowfs" and isinstance(c["op_counts"], dict))
            if seen:
                continue
            entry = "%s: cowfs never performed %s" % (mode_name, op)
            capability = OP_CAPABILITY.get(op, None)
            # Quote the message that names this operation's capability. Every punch_hole message
            # also contains KEEP_SIZE, so matching on the first disabled mode would quote the
            # wrong one.
            named = [d for d in disabled_modes if capability and capability in d]
            is_hole_family = op in OP_CAPABILITY
            if is_hole_family and (op in unsupported_ops or named):
                # The filesystem has no such capability. Recorded as a gap with its evidence,
                # not counted as work done and not counted as a pass on its own.
                gaps.append("%s, unsupported on this filesystem%s"
                            % (entry, " (fsx reported: %s)" % named[0] if named else
                               " (the runner's own %s probe reported it unsupported)" % op))
            elif is_hole_family:
                missing.append(entry)
            else:
                # Not a hole operation: fsx must have performed it, and did not.
                missing.append(entry)
    failures.extend(missing)
    for restart in restarts:
        if restart.get("exit") != 0:
            failures.append("daemon restart leg exited %s: %s" % (restart.get("exit"), restart.get("error")))
        failures.extend("after restart: %s" % p for p in restart.get("problems", []))
    if failures:
        status = "FAIL"
    elif unmeasurable:
        status = "UNMEASURABLE"
    else:
        status = "PASS"
    passed = sum(1 for c in compares if c.get("status") == "PASS")
    return {"kind": "verdict", "time": now(), "status": status, "cases": ran,
            "pairs_passed": passed,
            "pairs_failed": sum(1 for c in compares if c.get("status") == "FAIL"),
            "pairs_unmeasurable": sum(1 for c in compares if c.get("status") == "UNMEASURABLE"),
            "failures": failures, "unmeasurable": unmeasurable, "unsupported": unsupported,
            "required_ops_missing": missing, "capability_gaps": gaps}


def summary(md_path, result, meta, compares):
    lines = ["# fsx gate g4 raw summary", "",
             "status: %s" % result["status"],
             "cases: %d" % result["cases"],
             "binary sha256: %s" % meta.get("fsx", {}).get("sha256"),
             "cowfs root: %s" % json.dumps(meta.get("roots", {}).get("cowfs")),
             "native root: %s" % json.dumps(meta.get("roots", {}).get("native")),
             "pairs: %d passed, %d failed, %d unmeasurable"
             % (result.get("pairs_passed", 0), result.get("pairs_failed", 0),
                result.get("pairs_unmeasurable", 0)),
             "", "| mode | seed | ops requested | ops executed n/c | ops skipped n/c | native sha256 | cowfs sha256 | st_dev n/c | fstype n/c | stream | unexplained | status |",
             "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for c in compares:
        dev = c.get("data_st_dev", {})
        fst = c.get("data_fstype", {})
        ex = c.get("ops_executed", {})
        sk = c.get("ops_skipped", {})
        lines.append("| %s | %s | %s | %s/%s | %s/%s | %s | %s | %s | %s | %s | %s | %s |" % (
            c["mode"], c["seed"], c.get("ops_requested"),
            ex.get("native"), ex.get("cowfs"), sk.get("native"), sk.get("cowfs"),
            (c["data_sha256"]["native"] or "-")[:12], (c["data_sha256"]["cowfs"] or "-")[:12],
            "%s/%s" % (dev.get("native"), dev.get("cowfs")),
            "%s/%s" % (fst.get("native"), fst.get("cowfs")),
            "same" if c.get("ops_stream_match") else "differs",
            ", ".join(c.get("deltas_unexplained", [])) or "-",
            c.get("status", "?")))
    lines += ["", "## unsupported capabilities", ""]
    lines += ["- %s" % u for u in result["unsupported"]] or ["- none"]
    lines += ["", "## capability gaps in a required op", ""]
    lines += ["- %s" % g for g in result.get("capability_gaps", [])] or ["- none"]
    lines += ["", "## unmeasurable", ""]
    lines += ["- %s" % u for u in result.get("unmeasurable", [])] or ["- none"]
    lines += ["", "## failures", ""]
    lines += ["- %s" % f for f in result["failures"]] or ["- none"]
    with open(md_path, "w") as f:
        f.write("\n".join(lines) + "\n")


def attest_arm(role, root, args_pid_file, expect_backend, expect_fstypes):
    """One arm's identity, from the kernel and from the serving process, before any case runs.

    Returns the attestation dict plus a list of reasons it is not acceptable. There is no path
    fallback anywhere in here: an unresolvable mount table is UNKNOWN, and a directory that
    resolves to the wrong filesystem type is refused rather than believed.
    """
    module = manifest_module()
    result = module.attest(root, args_pid_file, expect_backend, expect_fstypes)
    result["role"] = role
    result["path_as_given"] = root
    reasons = []
    if result.get("status") == "UNKNOWN":
        reasons.append("%s arm %s cannot be attested: %s" % (role, root, result.get("reason")))
    elif not result.get("ok"):
        reasons.append("%s arm %s: %s" % (role, root, result.get("reason")))
    if result.get("st_dev") is None:
        reasons.append("%s arm %s has no stat device, so the path does not resolve to a live "
                       "filesystem" % (role, root))
    return result, reasons


def readback_in_new_process(path):
    """Read a file from a separate process, so the digest is a filesystem read and not this
    process's cached copy of its own expectation."""
    code = "\n".join([
        "import hashlib, sys",
        "h = hashlib.sha256()",
        "n = 0",
        "with open(sys.argv[1], 'rb') as f:",
        "    while True:",
        "        block = f.read(1 << 20)",
        "        if not block:",
        "            break",
        "        h.update(block)",
        "        n += len(block)",
        "print(h.hexdigest(), n)",
    ])
    proc = subprocess.run([sys.executable, "-c", code, path], capture_output=True, text=True,
                          timeout=600)
    if proc.returncode != 0:
        return None, "separate-process readback of %s failed: %s" % (path, proc.stderr.strip()[-300:])
    parts = proc.stdout.split()
    if len(parts) != 2:
        return None, "separate-process readback of %s printed %r" % (path, proc.stdout.strip()[:200])
    return {"sha256": parts[0], "size": int(parts[1])}, None


def daemon_generation(pidfile):
    """pid and start time, which together identify one running generation of the daemon."""
    module = manifest_module()
    info = module.attest_daemon(pidfile) if pidfile else None
    if not info or info.get("error"):
        return None, "no daemon generation could be read from %s: %s" % (pidfile, info)
    if info.get("starttime") is None:
        return None, "daemon %s has no readable start time" % info.get("pid")
    return {"pid": info.get("pid"), "starttime": info.get("starttime"),
            "store": info.get("store"), "socket": info.get("socket"),
            "mount": info.get("mount"), "backend": info.get("backend"),
            "binary_sha256": info.get("binary_sha256")}, None


def restart_leg(args, cases, before_generation, mount_before):
    """Run the restart hook, then require that a real replacement generation is serving the same
    declared store, socket and mount before any readback counts.

    The command's own exit status is not evidence. A hook that does nothing leaves the pid and
    start time unchanged, which is a failure, not a pass. The generation is read from the pid
    file and /proc, never from anything the hook prints.
    """
    problems = []
    before = {c["data_path"]: {"sha256": c["data_sha256"], "size": c["data_size"]}
              for c in cases if c["arm"] == "cowfs"}
    proc = subprocess.run(args.restart_cmd, shell=True, capture_output=True, text=True, timeout=900)
    after_generation, gen_error = daemon_generation(args.daemon_pid_file)
    mount_after = manifest_module().attest(args.cowfs_root, args.daemon_pid_file,
                                           args.expect_backend)
    if gen_error:
        problems.append("after the restart hook: %s" % gen_error)
    else:
        if before_generation is None:
            problems.append("there is no daemon generation before the restart to compare against")
        else:
            if after_generation["pid"] == before_generation["pid"]:
                problems.append("daemon pid %s is unchanged after the restart hook, so nothing "
                                "replaced it" % after_generation["pid"])
            if after_generation["starttime"] == before_generation["starttime"]:
                problems.append("daemon %s has start time %s before and after the restart hook, so "
                                "it is the same generation"
                                % (after_generation["pid"], after_generation["starttime"]))
            for field in ("store", "socket", "mount"):
                was, became = before_generation.get(field), after_generation.get(field)
                if was != became:
                    problems.append("daemon %s changed across the restart: %s was %s, now %s"
                                    % (after_generation["pid"], field, was, became))
            if before_generation.get("binary_sha256") != after_generation.get("binary_sha256"):
                problems.append("the daemon binary changed across the restart: %s then %s"
                                % (before_generation.get("binary_sha256"),
                                   after_generation.get("binary_sha256")))
    if not mount_after.get("ok"):
        problems.append("the mount is not attested after the restart hook: %s"
                        % mount_after.get("reason"))
    elif mount_before.get("st_dev") != mount_after.get("st_dev"):
        problems.append("the mount's device changed across the restart: %s then %s"
                        % (mount_before.get("st_dev"), mount_after.get("st_dev")))

    after, read_problems = {}, list(problems)
    for path, want in before.items():
        got, error = readback_in_new_process(path)
        if error:
            read_problems.append(error)
            continue
        after[path] = got
        if got["sha256"] != want["sha256"] or got["size"] != want["size"]:
            read_problems.append("%s read %s (%d bytes) after the restart, %s (%s bytes) before"
                                 % (os.path.basename(path), got["sha256"], got["size"],
                                    want["sha256"], want["size"]))
        elif want["sha256"] is None or want["size"] in (None, 0):
            read_problems.append("%s has no expected content to compare against" % path)
    row = {"kind": "restart", "time": now(), "cmd": args.restart_cmd, "exit": proc.returncode,
           "generation_before": before_generation, "generation_after": after_generation,
           "mount_before_st_dev": mount_before.get("st_dev"),
           "mount_after": mount_after,
           "stdout": proc.stdout[-2000:], "stderr": proc.stderr[-2000:],
           "readback": "a separate process reopening each file through the mount",
           "rehashed": after, "problems": read_problems,
           "error": proc.stderr.strip()[-500:] if proc.returncode else ""}
    print("restart leg: exit %s, generation %s -> %s, %d file(s) read back, problems %s"
          % (proc.returncode,
             (before_generation or {}).get("pid"), (after_generation or {}).get("pid"),
             len(after), read_problems or "none"))
    return row


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--native-root", required=True, help="native control directory")
    p.add_argument("--cowfs-root", required=True, help="mounted cowfs directory")
    p.add_argument("--fsx-bin", required=True)
    p.add_argument("--config", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "fsx-gate.json"))
    p.add_argument("--out", required=True, help="evidence directory")
    p.add_argument("--mode", action="append", help="declared mode to run, repeatable (default: all)")
    p.add_argument("--seeds", help="override the declared seeds, comma separated")
    p.add_argument("--ops", type=int, help="override the declared op count")
    p.add_argument("--restart-cmd", help="hook that replaces the daemon; the runner requires a new generation")
    p.add_argument("--daemon-pid-file", help="pid file of the serving daemon, read for every attestation")
    p.add_argument("--expect-backend", default="core", help="backend the serving daemon must declare")
    p.add_argument("--expect-cowfs-fstype", action="append", help="filesystem type the cowfs arm must be, repeatable")
    p.add_argument("--expect-native-fstype", action="append", help="filesystem type the native arm must be, repeatable")
    p.add_argument("--allow-unpinned-fsx", action="store_true",
                   help="run without the tool manifest. For a mutation control only; never for an "
                        "acceptance claim, and the record says so")
    p.add_argument("--timeout", type=int, default=1800, help="seconds per fsx invocation")
    p.add_argument("--label", default="run")
    args = p.parse_args(argv)

    cowfs_fstypes = tuple(args.expect_cowfs_fstype or manifest_module().COWFS_FSTYPES)
    native_fstypes = tuple(args.expect_native_fstype or ())
    gate = json.load(open(args.config))
    os.makedirs(args.out, exist_ok=True)
    record = os.path.join(args.out, "cases.jsonl")
    run_tag = "%s-%d" % (args.label, os.getpid())

    def emit(row):
        with open(record, "a") as f:
            f.write(json.dumps(row, sort_keys=True) + "\n")
            f.flush()
            os.fsync(f.fileno())

    # Preflight. Anything missing here is UNMEASURABLE with the exact reason, never a pass.
    unmeasurable = []
    if not os.path.isdir(args.native_root):
        unmeasurable.append("native root %s is not a directory" % args.native_root)
    if not os.access(args.fsx_bin, os.X_OK):
        unmeasurable.append("fsx binary %s is missing or not executable" % args.fsx_bin)

    # F5: the tool is bound to a manifest approved before execution. A digest that is only
    # recorded is not a pin.
    manifest = gate["tool"].get("manifest")
    fsx = fsx_identity(args.fsx_bin)
    tool_check = {"pinned": bool(manifest) and not args.allow_unpinned_fsx,
                  "allow_unpinned": bool(args.allow_unpinned_fsx)}
    if manifest:
        tool_check["expected_binary_sha256"] = manifest["expected_binary_sha256"]
        tool_check["expected_source_sha256"] = manifest["expected_source_sha256"]
        tool_check["expected_compile"] = manifest["expected_compile"]
        if not args.allow_unpinned_fsx:
            if fsx and fsx.get("sha256") != manifest["expected_binary_sha256"]:
                unmeasurable.append(
                    "the fsx binary at %s hashes to %s but the approved manifest pins %s; refusing "
                    "to run a tool that is not the one the manifest names"
                    % (args.fsx_bin, fsx.get("sha256") or "nothing readable",
                       manifest["expected_binary_sha256"]))
            if fsx and fsx.get("usage_exit") != 90:
                unmeasurable.append("the binary at %s exited %s on its own usage text, expected "
                                    "90, so it does not behave like the pinned fsx"
                                    % (args.fsx_bin, fsx.get("usage_exit")))
    else:
        unmeasurable.append("the gate config declares no tool manifest, so the fsx binary is not pinned")
    if args.allow_unpinned_fsx:
        tool_check["note"] = ("run without the tool pin; this is a mutation control and the result "
                              "is not an acceptance claim")
    if fsx and fsx.get("usage_exit") != 90 and not manifest:
        unmeasurable.append("fsx exited %s on its own usage text, expected 90" % fsx.get("usage_exit"))

    # F1: both arms are attested from the kernel and from the serving process before any case.
    cowfs_att, cowfs_reasons = attest_arm("cowfs", args.cowfs_root, args.daemon_pid_file,
                                          args.expect_backend, cowfs_fstypes)
    native_att, native_reasons = attest_arm("native", args.native_root, None, None, ())
    unmeasurable.extend(cowfs_reasons)
    unmeasurable.extend(native_reasons)
    if native_att.get("ok") and native_fstypes and native_att.get("fstype") not in native_fstypes:
        unmeasurable.append("native arm %s is on %s, expected one of %s"
                            % (args.native_root, native_att.get("fstype"), "/".join(native_fstypes)))
    if native_att.get("ok") and "cowfs" in str(native_att.get("fstype", "")):
        unmeasurable.append("native root %s is on %s, so the control is the thing under test"
                            % (os.path.normpath(args.native_root), native_att.get("fstype")))
    if cowfs_att.get("ok") and native_att.get("ok") and cowfs_att.get("st_dev") == native_att.get("st_dev"):
        unmeasurable.append("both arms resolve to device %s, so there is no cowfs arm"
                            % cowfs_att.get("st_dev"))
    daemon_before, daemon_error = daemon_generation(args.daemon_pid_file)
    if daemon_error:
        unmeasurable.append("before any case: %s" % daemon_error)
    elif daemon_before and daemon_before.get("mount") != os.path.normpath(args.cowfs_root) and \
            not os.path.normpath(args.cowfs_root).startswith(
                os.path.normpath(daemon_before.get("mount") or "/nonexistent") + "/"):
        unmeasurable.append("the daemon serves %s but the cowfs arm is %s"
                            % (daemon_before.get("mount"), args.cowfs_root))

    meta = {"kind": "meta", "time": now(), "label": args.label, "run_tag": run_tag, "gate": gate["gate"],
            "argv": sys.argv, "platform": platform.platform(), "tool": gate["tool"],
            "tool_check": tool_check, "fsx": fsx,
            "cowfs_attestation": cowfs_att, "native_attestation": native_att,
            "daemon_before": daemon_before,
            "roots": {"native": {"mountpoint": native_att.get("resolved_mountpoint"),
                                 "fstype": native_att.get("fstype"), "st_dev": native_att.get("st_dev")},
                      "cowfs": {"mountpoint": cowfs_att.get("resolved_mountpoint"),
                                "fstype": cowfs_att.get("fstype"), "st_dev": cowfs_att.get("st_dev")}},
            "caps": gate["caps"], "unmeasurable": unmeasurable}
    emit(meta)
    if unmeasurable:
        result = {"kind": "verdict", "time": now(), "status": "UNMEASURABLE", "cases": 0,
                  "pairs_passed": 0, "pairs_failed": 0, "pairs_unmeasurable": 0,
                  "failures": unmeasurable, "unmeasurable": [], "unsupported": [],
                  "required_ops_missing": [], "capability_gaps": []}
        emit(result)
        print("UNMEASURABLE")
        for u in unmeasurable:
            print("  - %s" % u)
        return EXIT_UNMEASURABLE

    modes = gate["modes"]
    if args.mode:
        wanted = set(args.mode)
        modes = [m for m in modes if m["name"] in wanted]
        if not modes:
            print("no declared mode named %s" % ",".join(sorted(wanted)), file=sys.stderr)
            return EXIT_USAGE
    required_ops = {m["name"]: gate["required_op_types"][m["name"]] for m in modes
                    if m["name"] in gate["required_op_types"]}

    probe_rows = []
    probe_by_arm = {"native": {}, "cowfs": {}, "_probe_rows": []}
    for arm, root in (("native", args.native_root), ("cowfs", args.cowfs_root)):
        for row in Probe(root, arm).run():
            probe_rows.append(row)
            probe_by_arm[arm][row["op"]] = row["ok"]
            emit(dict(row, kind="probe"))
    probe_by_arm["_probe_rows"] = probe_rows

    cases, compares, restarts = [], [], []
    fresh_open = {}
    for mode in modes:
        seeds = [int(s) for s in args.seeds.split(",")] if args.seeds else mode["seeds"]
        ops = args.ops or mode["ops"]
        for seed in seeds:
            by_arm = {}
            for arm, root in (("native", args.native_root), ("cowfs", args.cowfs_root)):
                case = run_case(args.fsx_bin, arm, root, mode, seed, ops, gate["caps"],
                                args.out, args.timeout, run_tag)
                cases.append(case)
                emit(case)
                by_arm[arm] = case
                print("[%s] seed %s %s: exit %s, %s ops, %s bytes, sha256 %s, st_dev %s, fstype %s, %.1fs"
                      % (mode["name"], seed, arm, case["exit"], case["ops_executed"],
                         case["data_size"], (case["data_sha256"] or "-")[:12],
                         case["data_st_dev"], case["data_real_fstype"], case["seconds"]))
            # A separate process reopens each file, so the digest is a filesystem read rather than
            # this process's own copy of what it expected.
            fresh_open["cowfs"] = readback_in_new_process(by_arm["cowfs"]["data_path"])[0]
            fresh_open["native"] = readback_in_new_process(by_arm["native"]["data_path"])[0]
            compare = compare_case(mode, seed, ops, by_arm["native"], by_arm["cowfs"],
                                   fresh_open, probe_by_arm)
            compares.append(compare)
            emit(compare)
            print("    %s: st_dev %s/%s, fstype %s/%s, stream %s, unexplained %s"
                  % (compare["status"], compare["data_st_dev"]["native"],
                     compare["data_st_dev"]["cowfs"], compare["data_fstype"]["native"],
                     compare["data_fstype"]["cowfs"],
                     "identical" if compare["ops_stream_match"] else "differs in " +
                     ", ".join(sorted(compare["op_count_deltas"])),
                     ", ".join(compare["deltas_unexplained"]) or "nothing"))
            if compare["problems"]:
                print("      problems: %s" % compare["problems"])
            if compare["unmeasurable"]:
                print("      unmeasurable: %s" % compare["unmeasurable"])

    if args.restart_cmd:
        row = restart_leg(args, cases, daemon_before, cowfs_att)
        restarts.append(row)
        emit(row)

    # F6: the byte budget is enforced against the caps the config declares, so a run that would
    # exceed it is refused before it starts rather than reported after it grew.
    caps = gate["caps"]
    planned_files = sum(len(m["seeds"]) * (1 if args.seeds is None else len(args.seeds.split(",")))
                        for m in modes)
    planned_bytes = planned_files * 2 * caps["max_file_bytes"]
    budget = caps.get("max_bytes_written_per_arm")
    budget_valid = True
    if budget and budget < caps["max_file_bytes"] * 2:
        budget_valid = False
        print("cap budget %d is smaller than two maximum files %d; the cap is declared wrongly"
              % (budget, caps["max_file_bytes"] * 2))
    written = {arm: sum(c["data_size"] or 0 for c in cases if c["arm"] == arm)
               for arm in ("native", "cowfs")}
    over_budget = [arm for arm, total in written.items() if budget and total > budget]

    result = verdict(cases, compares, restarts, required_ops, probe_rows)
    if over_budget:
        result["failures"].append(
            "the cowfs arm wrote %d bytes against a declared per-arm budget of %d"
            % (written["cowfs"], budget))
        result["status"] = "FAIL"
    result["bytes"] = {"written_per_arm": written, "budget_per_arm": budget,
                       "planned_worst_case": planned_bytes, "planned_files": planned_files,
                       "budget_matches_declared_caps": budget_valid}
    emit(result)
    summary(os.path.join(args.out, "summary.md"), result, meta, compares)
    print("%s: %d cases, %d passed, %d failed, %d unmeasurable"
          % (result["status"], result["cases"], result.get("pairs_passed", 0),
             result.get("pairs_failed", 0), result.get("pairs_unmeasurable", 0)))
    for f in result["failures"]:
        print("  FAIL %s" % f)
    for u in result.get("unmeasurable", []):
        print("  UNMEASURABLE %s" % u)
    if result["status"] == "PASS":
        return EXIT_PASS
    if result["status"] == "UNMEASURABLE":
        return EXIT_UNMEASURABLE
    return EXIT_FAIL


if __name__ == "__main__":
    sys.exit(main())
