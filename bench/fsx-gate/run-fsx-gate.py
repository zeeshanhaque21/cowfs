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
OP_CAPABILITY = {
    "punch_hole": "PUNCH_HOLE",
    "zero_range": "ZERO_RANGE",
    "write_zeroes": "WRITE_ZEROES",
}

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
    except OSError as e:
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


def run_case(binary, arm, root, mode, seed, ops, caps, out_dir, timeout):
    """One fsx run on one arm. The case directory is inside the arm root, so the work happens on
    that filesystem and not on whatever holds the evidence directory."""
    case_dir = os.path.join(root, ".fsx-gate", "%s-seed%s" % (mode["name"], seed))
    os.makedirs(case_dir, exist_ok=True)
    data = os.path.join(case_dir, "fsx.dat")
    argv = fsx_argv(binary, mode["flags"], seed, ops, caps, case_dir, "fsx.dat")
    started = time.monotonic()
    timed_out = False
    try:
        proc = subprocess.run(argv, cwd=root, capture_output=True, text=True, timeout=timeout)
        code, out, err = proc.returncode, proc.stdout, proc.stderr
    except subprocess.TimeoutExpired as e:
        timed_out = True
        code = None
        out = (e.stdout or b"").decode(errors="replace") if isinstance(e.stdout, bytes) else (e.stdout or "")
        err = (e.stderr or b"").decode(errors="replace") if isinstance(e.stderr, bytes) else (e.stderr or "")
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
    return {
        "kind": "case", "time": now(), "mode": mode["name"], "seed": seed, "arm": arm,
        "argv": argv, "cwd": root, "exit": code, "timed_out": timed_out, "seconds": round(seconds, 3),
        "ops_declared": ops, "ops_executed": parse_ops_executed(out),
        "log_dump_total": parse_log_dump_total(out),
        "case_dir": case_dir, "data_path": data, "data_sha256": data_sha, "data_size": data_size,
        "data_st_dev": st_dev(data), "root_st_dev": st_dev(root),
        "ops_file": opsfile, "ops_sha256": ops_sha, "op_counts": counts,
        "fsx_reported_unsupported": parse_disabled_modes(out + err),
        "evidence": evidence, "evidence_copy": copied, "log": log,
    }


def capability_evidence(case, probe_by_arm):
    """Which of the hole operations this filesystem is known not to have, with the source."""
    names = set(OP_CAPABILITY)
    have = {op for op in names if probe_by_arm.get(case["arm"], {}).get(op, True)}
    disabled = " ".join(case["fsx_reported_unsupported"])
    for op in list(have):
        if OP_CAPABILITY[op] in disabled:
            have.discard(op)
    return have


def compare_case(mode, seed, ops, native, cowfs, fresh_open, probe_by_arm=None):
    """The per-seed verdict for one seed on both arms.

    The matched part of the gate is the tool, the declared flags, the seed and the op count. The
    op stream is fsx's own output, and fsx picks operations after probing what the filesystem
    supports, so it can legitimately differ between arms. Two rules follow from that:

    * a mode declared to run the same mix on both arms must produce the same stream, and then
      the bytes must match exactly;
    * a mode that lets fsx use everything the filesystem offers is judged on fsx's own exit
      status, and every stream difference must trace to a capability one arm is recorded as
      lacking. A difference with no such capability behind it is a failure, never an excuse.
    """
    probe_by_arm = probe_by_arm or {}
    require_same_stream = bool(mode.get("require_identical_op_stream"))
    problems = []
    if native["timed_out"] or cowfs["timed_out"]:
        problems.append("fsx timed out after %ss" % native["seconds"])
    for arm, case in (("native", native), ("cowfs", cowfs)):
        if case["exit"] != 0:
            problems.append("%s fsx exited %s" % (arm, case["exit"]))
        if case["ops_executed"] is None:
            problems.append("%s fsx never reported its op count" % arm)
        elif case["ops_executed"] != ops:
            problems.append("%s fsx executed %d ops, %d declared" % (arm, case["ops_executed"], ops))
        if case["data_sha256"] is None:
            problems.append("%s data file unreadable: %s" % (arm, case["data_size"]))
        elif case["data_size"] == 0:
            problems.append("%s data file is empty" % arm)

    devices = {arm: case.get("data_st_dev") for arm, case in (("native", native), ("cowfs", cowfs))}
    if devices["native"] is not None and devices["native"] == devices["cowfs"]:
        problems.append("both arms ran on the same filesystem (st_dev %s), so the cowfs arm did not "
                        "touch the mount" % devices["native"])

    native_counts = native["op_counts"] if isinstance(native["op_counts"], dict) else {}
    cowfs_counts = cowfs["op_counts"] if isinstance(cowfs["op_counts"], dict) else {}
    deltas = {}
    for op in sorted(set(native_counts) | set(cowfs_counts)):
        left, right = native_counts.get(op, 0), cowfs_counts.get(op, 0)
        if left != right:
            deltas[op] = {"native": left, "cowfs": right}
    skip_delta = deltas.pop("skip", {"native": 0, "cowfs": 0})
    native_gaps = capability_evidence(native, probe_by_arm)
    cowfs_gaps = capability_evidence(cowfs, probe_by_arm)
    gaps = {op for op in deltas if op in OP_CAPABILITY and (op in native_gaps or op in cowfs_gaps)}
    consequent = sorted(op for op in deltas if op not in gaps)
    stream_match = native["ops_sha256"] is not None and native["ops_sha256"] == cowfs["ops_sha256"]
    if require_same_stream and not stream_match:
        problems.append("op stream differs although %s declares one operation mix for both arms "
                        "(differs in %s)" % (mode["name"], ", ".join(sorted(deltas)) or "unknown"))
    elif not gaps and consequent:
        problems.append("op stream differs in %s with no capability gap on either arm"
                        % ", ".join(consequent))

    hashes_compared = False
    if stream_match:
        hashes_compared = True
        if native["data_sha256"] is not None and cowfs["data_sha256"] is not None:
            if native["data_sha256"] != cowfs["data_sha256"]:
                problems.append("identical op streams produced different bytes: native %s, cowfs %s"
                                % (native["data_sha256"], cowfs["data_sha256"]))
    fresh = fresh_open.get("cowfs")
    if fresh and cowfs["data_sha256"] and fresh != cowfs["data_sha256"]:
        problems.append("fresh open read %s, fsx left %s" % (fresh, cowfs["data_sha256"]))
    return {
        "kind": "compare", "time": now(), "mode": mode["name"], "seed": seed,
        "require_identical_op_stream": require_same_stream,
        "ops_declared": ops, "ops_executed": {"native": native["ops_executed"], "cowfs": cowfs["ops_executed"]},
        "data_sha256": {"native": native["data_sha256"], "cowfs": cowfs["data_sha256"]},
        "data_size": {"native": native["data_size"], "cowfs": cowfs["data_size"]},
        "data_st_dev": devices,
        "ops_stream_sha256": {"native": native["ops_sha256"], "cowfs": cowfs["ops_sha256"]},
        "ops_stream_match": stream_match, "hashes_compared": hashes_compared,
        "hash_comparison": ("compared, streams identical" if hashes_compared else
                            "not applicable: the two arms ran different operations"),
        "op_count_deltas": deltas, "skip_delta": skip_delta,
        "explained_by_capability": sorted(gaps), "consequent_deltas": consequent,
        "cowfs_capability_gaps": sorted(cowfs_gaps),
        "native_capability_gaps": sorted(native_gaps),
        "fresh_open_sha256": fresh_open,
        "problems": problems,
    }


def verdict(cases, compares, restarts, required_ops, probe_rows):
    """The gate's decision, from the records only.

    A case that ran and produced the wrong bytes is FAIL, whatever the reason. A capability the
    filesystem does not have is recorded as unsupported and does not become a pass by itself:
    the case still has to match native on everything else.
    """
    failures = []
    ran = 0
    for case in cases:
        ran += 1
    if ran == 0:
        failures.append("no case ran")
    for compare in compares:
        failures.extend("%s seed %s: %s" % (compare["mode"], compare["seed"], p) for p in compare["problems"])

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
            capability = OP_CAPABILITY.get(op)
            # Quote the message that names this operation's capability. Every punch_hole message
            # also contains KEEP_SIZE, so matching on the first disabled mode would quote the
            # wrong one.
            named = [d for d in disabled_modes if capability and capability in d]
            if capability and (op in unsupported_ops or named):
                # The filesystem has no such capability. Recorded as a gap with its evidence,
                # not counted as work done and not counted as a pass on its own.
                gaps.append("%s, unsupported on this filesystem%s"
                            % (entry, " (fsx reported: %s)" % named[0] if named else
                               " (the runner's own %s probe reported it unsupported)" % op))
            else:
                missing.append(entry)
    failures.extend(missing)
    for restart in restarts:
        if restart.get("exit") != 0:
            failures.append("daemon restart leg exited %s: %s" % (restart.get("exit"), restart.get("error")))
        failures.extend("after restart: %s" % p for p in restart.get("problems", []))
    status = "PASS" if not failures else "FAIL"
    return {"kind": "verdict", "time": now(), "status": status, "cases": ran,
            "failures": failures, "unsupported": unsupported,
            "required_ops_missing": missing, "capability_gaps": gaps}


def summary(md_path, result, meta, compares):
    lines = ["# fsx gate g4 raw summary", "",
             "status: %s" % result["status"],
             "cases: %d" % result["cases"],
             "binary sha256: %s" % meta.get("fsx", {}).get("sha256"),
             "cowfs root: %s" % json.dumps(meta.get("roots", {}).get("cowfs")),
             "native root: %s" % json.dumps(meta.get("roots", {}).get("native")),
             "", "| mode | seed | ops | native sha256 | cowfs sha256 | st_dev native/cowfs | op stream | explained gaps | problems |",
             "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for c in compares:
        dev = c.get("data_st_dev", {})
        lines.append("| %s | %s | %s | %s | %s | %s | %s | %s | %s |" % (
            c["mode"], c["seed"], c["ops_declared"],
            (c["data_sha256"]["native"] or "-")[:12], (c["data_sha256"]["cowfs"] or "-")[:12],
            "%s/%s" % (dev.get("native"), dev.get("cowfs")),
            "same" if c.get("ops_stream_match") else "differs",
            ", ".join(c.get("explained_by_capability", [])) or "-",
            "; ".join(c["problems"]) or "none"))
    lines += ["", "## unsupported capabilities", ""]
    lines += ["- %s" % u for u in result["unsupported"]] or ["- none"]
    lines += ["", "## capability gaps in a required op", ""]
    lines += ["- %s" % g for g in result.get("capability_gaps", [])] or ["- none"]
    lines += ["", "## failures", ""]
    lines += ["- %s" % f for f in result["failures"]] or ["- none"]
    with open(md_path, "w") as f:
        f.write("\n".join(lines) + "\n")


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
    p.add_argument("--restart-cmd", help="run once after the cases; the runner re-hashes the cowfs files after it returns")
    p.add_argument("--timeout", type=int, default=1800, help="seconds per fsx invocation")
    p.add_argument("--label", default="run")
    args = p.parse_args(argv)

    gate = json.load(open(args.config))
    os.makedirs(args.out, exist_ok=True)
    record = os.path.join(args.out, "cases.jsonl")

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
    roots = {"native": mount_identity(args.native_root), "cowfs": mount_identity(args.cowfs_root)}
    # The native control is a directory on whatever local filesystem holds it. It must not be a
    # cowfs mount: a control that is the thing under test measures nothing.
    if roots["native"] and "fuse" in str(roots["native"].get("fstype", "")):
        unmeasurable.append("native root %s is on %s, so the control is the thing under test"
                            % (os.path.realpath(args.native_root), roots["native"].get("fstype")))
    # The cowfs arm must be a mount of its own, not a plain directory under whatever holds it.
    if roots["cowfs"] is None:
        unmeasurable.append("cowfs root %s is not on any mount, so this would measure a directory "
                            "rather than a mounted cowfs" % os.path.realpath(args.cowfs_root))
    elif roots["cowfs"].get("mountpoint") == "/":
        unmeasurable.append("cowfs root %s resolves to the root filesystem, not to a cowfs mount"
                            % os.path.realpath(args.cowfs_root))
    elif roots["native"] and roots["cowfs"]["mountpoint"] == roots["native"]["mountpoint"]:
        unmeasurable.append("both arms are on the same mount %s, so there is no cowfs arm"
                            % roots["cowfs"]["mountpoint"])
    fsx = fsx_identity(args.fsx_bin)
    if fsx and fsx.get("usage_exit") != 90:
        unmeasurable.append("fsx exited %s on its own usage text, expected 90" % fsx.get("usage_exit"))

    meta = {"kind": "meta", "time": now(), "label": args.label, "gate": gate["gate"],
            "argv": sys.argv, "platform": platform.platform(), "tool": gate["tool"],
            "fsx": fsx, "roots": roots, "caps": gate["caps"],
            "unmeasurable": unmeasurable}
    emit(meta)
    if unmeasurable:
        result = {"kind": "verdict", "time": now(), "status": "UNMEASURABLE", "cases": 0,
                  "failures": unmeasurable, "unsupported": [], "required_ops_missing": []}
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
    probe_by_arm = {"native": {}, "cowfs": {}}
    for arm, root in (("native", args.native_root), ("cowfs", args.cowfs_root)):
        for row in Probe(root, arm).run():
            probe_rows.append(row)
            probe_by_arm[arm][row["op"]] = row["ok"]
            emit(dict(row, kind="probe"))

    cases, compares, restarts = [], [], []
    fresh_open = {}
    for mode in modes:
        seeds = [int(s) for s in args.seeds.split(",")] if args.seeds else mode["seeds"]
        ops = args.ops or mode["ops"]
        for seed in seeds:
            by_arm = {}
            for arm, root in (("native", args.native_root), ("cowfs", args.cowfs_root)):
                case = run_case(args.fsx_bin, arm, root, mode, seed, ops, gate["caps"],
                                args.out, args.timeout)
                cases.append(case)
                emit(case)
                by_arm[arm] = case
                print("[%s] seed %s %s: exit %s, %s ops, %s bytes, sha256 %s, st_dev %s, %.1fs"
                      % (mode["name"], seed, arm, case["exit"], case["ops_executed"],
                         case["data_size"], (case["data_sha256"] or "-")[:12],
                         case["data_st_dev"], case["seconds"]))
            # The runner is a different process from fsx, so its open is a fresh open.
            digest, size = sha256_file(by_arm["cowfs"]["data_path"])
            fresh_open["cowfs"] = digest
            native_digest, _ = sha256_file(by_arm["native"]["data_path"])
            fresh_open["native"] = native_digest
            fresh_open["cowfs_size"] = size
            compare = compare_case(mode, seed, ops, by_arm["native"], by_arm["cowfs"],
                                   fresh_open, probe_by_arm)
            compares.append(compare)
            emit(compare)
            stream = ("identical" if compare["ops_stream_match"]
                      else "differs in " + ", ".join(sorted(compare["op_count_deltas"])))
            print("    compare: st_dev %s/%s, op stream %s, explained %s, %s"
                  % (compare["data_st_dev"]["native"], compare["data_st_dev"]["cowfs"], stream,
                     ", ".join(compare["explained_by_capability"]) or "nothing",
                     "ok" if not compare["problems"] else compare["problems"]))

    if args.restart_cmd:
        before = {c["data_path"]: c["data_sha256"] for c in cases if c["arm"] == "cowfs"}
        proc = subprocess.run(args.restart_cmd, shell=True, capture_output=True, text=True, timeout=900)
        after, problems = {}, []
        for path, want in before.items():
            got, size = sha256_file(path)
            after[path] = {"sha256": got, "size": size}
            if got != want:
                problems.append("%s read %s after the restart, %s before" % (os.path.basename(path), got, want))
        row = {"kind": "restart", "time": now(), "cmd": args.restart_cmd, "exit": proc.returncode,
               "stdout": proc.stdout[-2000:], "stderr": proc.stderr[-2000:],
               "rehashed": after, "problems": problems,
               "error": proc.stderr.strip()[-500:] if proc.returncode else ""}
        restarts.append(row)
        emit(row)
        print("restart leg: exit %s, %d file(s) rehashed, problems %s"
              % (proc.returncode, len(after), problems or "none"))

    result = verdict(cases, compares, restarts, required_ops, probe_rows)
    emit(result)
    summary(os.path.join(args.out, "summary.md"), result, meta, compares)
    print("%s: %d cases, %d failures" % (result["status"], result["cases"], len(result["failures"])))
    for f in result["failures"]:
        print("  FAIL %s" % f)
    return EXIT_PASS if result["status"] == "PASS" else EXIT_FAIL


if __name__ == "__main__":
    sys.exit(main())
