#!/usr/bin/env python3
"""Full-stack daemon crash recovery acceptance for cowfs (issue #88).

Drives the real `cowfs-daemon --backend core`, the real `cowfs` control CLI and a
real macOS NFSv3 loopback mount over a private store, kills the daemon with SIGKILL
at declared public boundaries, then reopens the same store with a fresh daemon and
compares every promised byte with a source hash taken before the crash.

The receipt model, read from the source
--------------------------------------
An NFS WRITE ack is NOT durable. `Vfs::write` (`cowfs_core::io::op_write`) writes
into the node's in-memory state and returns. The control API's mutating calls ack
`Ack::Applied` (`cowfs_meta::db::Ack`); nothing in the tree sets `Ack::Durable`, so
a `snapshot create` that returned 0 is applied, not durable. Only these are
durability receipts:

  * `os.fsync(fd)` on a file with dirty pages in the mount, which the NFS adapter
    maps to `fsync(ino, false)` (`crates/cowfs-nfs/src/lib.rs`), reaching
    `op_fsync` -> `flush_snapshot` (blocks into the pack, then the metadata commit)
    and `meta.sync()`, whose `before_sync` hook is `cowfs_core::store_sync_hook` ->
    `Store::sync()` -> fsync of the pack, then `watermark.advance`.
  * `cowfs shutdown`, which is `Core::close`.

Receipts are append-only. A receipt is never reclassified after the fact: a promise
is either kept or reported as a failure. `receipt.demoted` does not exist because
demotion is not an operation this harness performs.

Three kinds, enforced differently after the reopen:

  * ``durable``  the caller's sync returned. Must be present and byte-identical.
                 A miss is a FAILURE. This is what makes the rename boundary
                 falsifiable instead of unfalsifiable.
  * ``applied``  genuinely un-fsynced work. May be present or absent; the design
                 accepts bounded loss of recent writes. Absence is recorded, never
                 failed.
  * ``removed``  a name this harness deleted on purpose. Must be absent.

SIGKILL scope
-------------
SIGKILL kills a process, not the kernel. Bytes the daemon already `write(2)`ed into
a pack are in the host page cache and survive any process death. So this harness
samples process-crash recovery, not power loss, and does not claim otherwise.

Wire observation
----------------
The `Probe` class in this file reads the kernel NFSv3 client RPC counters with
`nfsstat`, which needs no root, and reports the `Commit` delta around a step. It is
a host-wide counter, so every measurement is bracketed by an idle baseline and
reported as a delta, never as an absolute. When `nfsstat` is missing the probe
records itself unavailable and this harness makes no claim about COMMIT.

Exit contract
-------------
  0  at least one case executed in this invocation, every executed case passed
  1  at least one case executed, at least one failed
  2  zero cases executed: cached-only. The JSON and the human summary both say
     `fresh_acceptance: false`. Never reported as a pass on its own.
  3  harness error: bad arguments or missing binaries

Usage
-----
    scripts/verify-daemon-crash.py --stage sample
    scripts/verify-daemon-crash.py --stage all --reps 2
    scripts/verify-daemon-crash.py --only rename_posix_durability
    python3 -m unittest discover -s bench        # the fail-closed controls

Everything lives under `bench/out/crash88*/` (gitignored). No shared daemon, store,
mount, socket, lease or runner is touched: every signal goes to a pid this process
spawned, and only after its command line is confirmed to carry this run's own store
and socket.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import mmap
import os
import shutil
import signal
import subprocess
import sys
import time

# --------------------------------------------------------------------------
# budgets, declared before any run
# --------------------------------------------------------------------------

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HERE = os.path.dirname(os.path.abspath(__file__))
DAEMON = os.path.join(REPO, "target/debug/cowfs-daemon")
COWFS = os.path.join(REPO, "target/debug/cowfs")
OUT_ROOT = os.path.join(REPO, "bench/out/crash88")

# A Unix socket path must fit sun_path (104 bytes on macOS), and its parent must be
# a private 0700 directory, which /private/tmp is not. Each run therefore makes its
# own short-lived directory there and removes it at teardown.
SOCK_ROOT = "/private/tmp"
SOCK_DIR = None

MAX_OP_BYTES = 64 * 1024
MAX_OPS_PER_CASE = 16
CASE_DEADLINE_SECS = 180
NO_PROGRESS_SECS = 90
DAEMON_READY_SECS = 60
CLI_TIMEOUT_SECS = 60

# Only the gc case writes more than MAX_OP_BYTES, and only enough to pass the
# collector's own default floor (cowfs_gc::Options::min_dead_bytes = 8 MiB).
GC_CASE_BYTES = 12 * 1024 * 1024

SAMPLE_FILES = 4
SAMPLE_BYTES = 4096

SCHEMA_VERSION = 3


def sock_path(name):
    return os.path.join(SOCK_DIR, name + ".sock")


def log(msg):
    sys.stderr.write("%s\n" % msg)
    sys.stderr.flush()


class Budget(Exception):
    """A declared budget was exceeded. Stops the run instead of growing it."""


class ForeignProcess(RuntimeError):
    """The pid's command line does not carry this fixture's exact paths."""


class CaseFailure(Exception):
    """A case's own assertion failed. Distinct from a harness error."""


class Proc:
    class Result:
        def __init__(self, returncode, stdout, stderr):
            self.returncode = returncode
            self.stdout = stdout
            self.stderr = stderr

    def __init__(self, argv, timeout=CLI_TIMEOUT_SECS, cwd=None):
        self.argv = argv
        try:
            self.p = subprocess.run(
                argv,
                capture_output=True,
                text=True,
                timeout=timeout,
                cwd=cwd,
                stdin=subprocess.DEVNULL,
            )
        except subprocess.TimeoutExpired as e:
            raise RuntimeError(
                "command timed out after %ds: %s" % (timeout, " ".join(argv))
            ) from e

    @property
    def returncode(self):
        return self.p.returncode

    @property
    def stdout(self):
        return self.p.stdout

    @property
    def stderr(self):
        return self.p.stderr


# --------------------------------------------------------------------------
# digests and stable identity
# --------------------------------------------------------------------------


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(1 << 20)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


def sha256_bytes(b):
    return hashlib.sha256(b).hexdigest()


def canonical(obj):
    return json.dumps(obj, sort_keys=True, separators=(",", ":"))


def digest_of(obj):
    return sha256_bytes(canonical(obj).encode())


def git_rev():
    try:
        return Proc(
            ["git", "rev-parse", "HEAD"], timeout=15, cwd=REPO
        ).stdout.strip()
    except Exception as e:  # noqa: BLE001
        return "unknown: %s" % e


def binary_digest(path):
    try:
        return sha256_file(path)
    except OSError:
        return "missing"


def harness_digest():
    """Digest of this file, so a cached verdict cannot outlive the harness logic."""
    try:
        return sha256_file(os.path.abspath(__file__))
    except OSError:
        return "missing"


def case_identity(phase, case, rep, argv_scope, config):
    """Everything that decides whether a cached verdict may be reused.

    The resume key is a digest of this whole dict, so any change to the source
    revision, the harness, either binary, the case's configuration or the scope of
    the invocation produces a different key and forces a re-execution.
    """
    ident = {
        "schema": SCHEMA_VERSION,
        "phase": phase,
        "case": case,
        "rep": rep,
        "rev": git_rev(),
        "harness_sha256": harness_digest(),
        "daemon_sha256": binary_digest(DAEMON),
        "cli_sha256": binary_digest(COWFS),
        "config": config,
        "argv_scope": argv_scope,
    }
    ident["key"] = digest_of({k: v for k, v in ident.items() if k != "key"})
    return ident


# --------------------------------------------------------------------------
# evidence: append, flush, fsync per record
# --------------------------------------------------------------------------


def read_records(path):
    """Parse a records file. A torn last line is tolerated and skipped.

    Read-only: nothing here opens the file for writing, so validating a cache entry
    cannot itself modify the evidence it is validating.
    """
    out = []
    if not os.path.exists(path):
        return out
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except ValueError:
                continue
    return out


class Recorder:
    def __init__(self, out_dir, resume=True):
        os.makedirs(out_dir, exist_ok=True)
        self.path = os.path.join(out_dir, "records.jsonl")
        self.out_dir = out_dir
        self.prior = read_records(self.path) if resume else []
        self.step = 0
        for r in self.prior:
            self.step = max(self.step, int(r.get("step", 0)))
        self._f = open(self.path, "a", encoding="utf-8")

    def record(self, name, ok, **fields):
        self.step += 1
        rec = {"step": self.step, "name": name, "ok": bool(ok), "t": time.time()}
        rec.update(fields)
        self._f.write(json.dumps(rec, sort_keys=True) + "\n")
        self._f.flush()
        os.fsync(self._f.fileno())
        if ok is not True:
            log("  [%03d] %-46s %s" % (self.step, name, "ok" if ok else "FAIL"))
        return rec

    def close(self):
        try:
            self._f.flush()
            os.fsync(self._f.fileno())
            self._f.close()
        except OSError:
            pass


# --------------------------------------------------------------------------
# receipts: an append-only ledger. Nothing is ever reclassified.
# --------------------------------------------------------------------------


class Receipt:
    __slots__ = ("path", "sha256", "size", "kind", "boundary", "seq", "t")

    def __init__(self, path, sha, size, kind, boundary, seq, t=None):
        self.path = path
        self.sha256 = sha
        self.size = size
        self.kind = kind
        self.boundary = boundary
        self.seq = seq
        self.t = time.time() if t is None else t

    def as_dict(self):
        return {
            "path": self.path,
            "sha256": self.sha256,
            "size": self.size,
            "kind": self.kind,
            "boundary": self.boundary,
            "seq": self.seq,
        }


class Receipts:
    """Promises made to the filesystem before the crash, and nothing else.

    There is deliberately no method that changes a receipt's kind. A caller's
    promise stands or the case fails; it is never re-read after the crash.
    """

    def __init__(self):
        self.items = []
        self.names = []
        self._seq = 0

    def _add(self, path, sha, size, kind, boundary):
        self._seq += 1
        r = Receipt(path, sha, size, kind, boundary, self._seq)
        self.items.append(r)
        return r

    def durable(self, path, sha, size, how, rec=None):
        r = self._add(path, sha, size, "durable", how)
        if rec is not None:
            rec.record(
                "receipt.issued",
                True,
                receipt=r.as_dict(),
                meaning="caller's sync returned success; loss after SIGKILL is a FAILURE",
            )
        return r

    def applied(self, path, sha, size, how, rec=None):
        r = self._add(path, sha, size, "applied", how)
        if rec is not None:
            rec.record(
                "receipt.issued",
                True,
                receipt=r.as_dict(),
                meaning="no sync was issued; loss is permitted by docs/design.md",
            )
        return r

    def removed(self, path, sha, size, how, rec=None):
        r = self._add(path, sha, size, "removed", how)
        if rec is not None:
            rec.record(
                "receipt.issued",
                True,
                receipt=r.as_dict(),
                meaning="this harness deleted it on purpose; absence is required",
            )
        return r

    def promise_snapshot(self, name, level, how, rec=None):
        self.names.append({"name": name, "level": level, "boundary": how})
        if rec is not None:
            rec.record(
                "receipt.snapshot",
                True,
                snapshot=name,
                level=level,
                boundary=how,
                meaning=(
                    "durable: name must survive; applied: control acks Ack::Applied"
                ),
            )

    def repath_by_path(self, old, new, rec=None):
        """Re-file a receipt under a new name after a rename, matched by path.

        The change is written to the ledger as an immutable event. Without it the
        ledger holds the pre-rename path while the manifest holds the post-rename
        one, and the case's own evidence stops matching itself.
        """
        for r in self.items:
            if r.path == old:
                r.path = new
                if rec is not None:
                    rec.record(
                        "receipt.repath",
                        True,
                        receipt=r.as_dict(),
                        **{"from": old, "to": new},
                    )
                return True
        return False

    def by_kind(self, kind):
        return [r for r in self.items if r.kind == kind]


# --------------------------------------------------------------------------
# NFSv3 client RPC counters, read with nfsstat
# --------------------------------------------------------------------------

NFSSTAT = "/usr/bin/nfsstat"
NFSSTAT_LAST_ERROR = None
# The output carries an NLM section with its own Commit counter, so the section is
# matched explicitly and a column is located by token index.
V3_SECTION = "NFSv3 RPC Counts:"


def _is_int(s):
    try:
        int(s)
        return True
    except ValueError:
        return False


def parse_rpc_counts(text, section=V3_SECTION):
    """Return {rpc_name: count} for one section of `nfsstat -c` output.

    Separated from the measurement so it can be unit tested against recorded output
    with no mount present.
    """
    counts = {}
    lines = text.splitlines()
    try:
        start = next(i for i, l in enumerate(lines) if section in l)
    except StopIteration:
        raise ValueError("section %r not found" % section)
    i = start + 1
    while i < len(lines):
        header = lines[i].strip()
        if not header:
            i += 1
            continue
        parts = header.split()
        if ":" in header and not all(_is_int(p) for p in parts):
            break
        if i + 1 >= len(lines):
            break
        values = lines[i + 1].split()
        for idx, name in enumerate(parts):
            if idx < len(values) and _is_int(values[idx]):
                counts.setdefault(name, 0)
                counts[name] += int(values[idx])
        i += 2
    return counts


def commit_count():
    """Current NFSv3 Commit counter, or None when it cannot be read.

    The reason is kept in NFSSTAT_LAST_ERROR so a failed measurement is attributed
    rather than silently reported as "no COMMIT".
    """
    global NFSSTAT_LAST_ERROR
    try:
        out = subprocess.run([NFSSTAT, "-c"], capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.SubprocessError) as e:
        NFSSTAT_LAST_ERROR = "%s: %s" % (type(e).__name__, e)
        return None
    if out.returncode != 0:
        NFSSTAT_LAST_ERROR = "exit %d: %s" % (out.returncode, out.stderr.strip()[:200])
        return None
    try:
        counts = parse_rpc_counts(out.stdout)
    except ValueError as e:
        NFSSTAT_LAST_ERROR = "parse: %s" % e
        return None
    if "Commit" not in counts:
        NFSSTAT_LAST_ERROR = "no Commit column in the NFSv3 section"
        return None
    NFSSTAT_LAST_ERROR = None
    return counts["Commit"]


class Probe:
    """Measures the NFSv3 Commit delta around a named step.

    The macOS kernel NFS client emits COMMIT only when it has dirty pages for an
    inode, which is the mechanism behind issue #90. Measuring it directly is better
    than inferring it from a rename coming back lost.

    Two limits, stated rather than papered over: `nfsstat -c` counters are
    **host-wide**, not per mount, so every figure is a delta around one
    single-threaded step, bracketed by an idle baseline; and a delta of 0 means no
    COMMIT attributable to that step, not no COMMIT on the host. When nfsstat is
    missing the probe reports itself unavailable and this harness makes no claim
    about COMMIT.
    """

    IDLE_BASELINE_SECS = 0.5

    def __init__(self, rec, case, reader=None, baseline_secs=None):
        """`reader` is the counter source. Injectable so the unavailable branch and
        each drift figure can be tested deterministically, with no real nfsstat."""
        self.rec = rec
        self.case = case
        self._reader = reader or commit_count
        self._baseline_secs = (
            self.IDLE_BASELINE_SECS if baseline_secs is None else baseline_secs
        )
        first = self._read(tries=1)
        self.available = isinstance(first, int)
        self.baseline = self._idle_baseline()
        self.marks = {}

    def _idle_baseline(self):
        if not self.available:
            return None
        before = self._read()
        time.sleep(self._baseline_secs)
        after = self._read()
        if not (isinstance(before, int) and isinstance(after, int)):
            self.rec.record(
                "wire.idle_baseline",
                True,
                case=self.case,
                commit_delta_over_idle=None,
                error="idle baseline unreadable: before=%r after=%r" % (before, after),
            )
            return None
        drift = after - before
        self.rec.record(
            "wire.idle_baseline",
            True,
            case=self.case,
            commit_delta_over_idle=drift,
            note="host-wide counter; nonzero is other activity on this machine, not this run",
        )
        return drift

    def _read(self, tries=3):
        """Read the counter, retrying: nfsstat occasionally fails under host load.

        A reader that raises is treated exactly like one that returns nothing, so
        an unavailable counter can never be mistaken for a reading of zero.
        """
        last = None
        for _ in range(tries):
            try:
                v = self._reader()
            except Exception as e:  # noqa: BLE001 - any failure means unavailable
                v = None
                last = "%s: %s" % (type(e).__name__, e)
            else:
                if isinstance(v, int):
                    return v
                last = v if isinstance(v, str) else "counter reader returned %r" % (v,)
            time.sleep(0.2)
        return last  # the error string, not a number

    def sample(self, label):
        self.marks[label] = {"before": self._read(), "after": None}
        return label

    def mark(self, label):
        """Close the window opened by `sample(label)`.

        The label must be the one that was sampled. A mismatch used to be a silent
        no-op that left every delta None, so it is now reported instead.
        """
        m = self.marks.get(label)
        if m is None:
            self.rec.record(
                "wire.mark_unmatched",
                False,
                case=self.case,
                label=label,
                sampled=sorted(self.marks),
                error="mark() label was never sampled; this step's delta would be None",
            )
            return
        m["after"] = self._read()

    @staticmethod
    def attributable_for(delta, drift):
        """Is a positive COMMIT signal attributable to this step?

        The rule, stated once so it can be tested as a truth table instead of
        inferred from a precedence accident:

          * ``delta <= 0`` is never positive attribution. A zero delta is an
            observation of no COMMIT attributable to the step, not evidence of
            activity, and a negative delta means the counter was reset (or the
            tool changed under us), which invalidates the reading entirely.
          * with ``drift == 0`` any positive delta is attributable.
          * with ``drift > 0`` the host was already moving the counter, so the
            signal must stand at ``3 * drift`` or more to be separable.

        Returns None when either figure is unknown. The numbers are a raw
        difference of two cumulative host-wide readings; the drift figure is
        reported alongside so a reader can judge the separation themselves.
        """
        if delta is None or drift is None:
            return None
        if delta <= 0:
            return False
        if drift == 0:
            return True
        return delta >= 3 * drift

    def report(self, label):
        m = self.marks.get(label)
        if m is None:
            rec = {
                "label": label,
                "available": self.available,
                "commit_delta": None,
                "before": None,
                "after": None,
                "attributable": None,
                "error": "no sample taken for this label",
                "note": "this step was never sampled; no claim about COMMIT",
            }
            self.rec.record("wire.report", True, case=self.case, **rec)
            return rec
        before, after = m["before"], m["after"]
        numeric = isinstance(before, int) and isinstance(after, int)
        drift = self.baseline if isinstance(self.baseline, int) else None
        delta = (after - before) if numeric else None
        rec = {
            "label": label,
            "available": self.available,
            "before": before if numeric else None,
            "after": after if numeric else None,
            "commit_delta": delta,
            "delta_is_background_subtracted": False,
            "idle_baseline_drift": drift,
            "attributable": self.attributable_for(delta, drift) if numeric else None,
            "error": None
            if numeric
            else "unreadable: before=%r after=%r" % (before, after),
            "note": (
                "host-wide NFSv3 client counter, raw difference across one "
                "single-threaded step, not background-subtracted; a step is only "
                "attributable when its positive delta stands clear of the idle "
                "drift, and a zero delta is never positive attribution"
            ),
        }
        self.rec.record("wire.report", True, case=self.case, **rec)
        return rec


# --------------------------------------------------------------------------
# process helpers: verify before any signal, never a broad pattern
# --------------------------------------------------------------------------


def pid_cmdline(pid):
    return Proc(["/bin/ps", "-o", "command=", "-p", str(pid)], timeout=15).stdout.strip()


def pid_start_time(pid):
    """Start time from `ps -o lstart=`, so a recycled pid cannot be mistaken for ours."""
    return Proc(["/bin/ps", "-o", "lstart=", "-p", str(pid)], timeout=15).stdout.strip()


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def child_exited(child, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if child.poll() is not None:
            return True
        time.sleep(0.2)
    return False


def process_identity(child):
    """pid, start time and argv captured once, for comparison before any signal."""
    return {
        "pid": child.pid,
        "lstart": pid_start_time(child.pid),
        "cmdline": pid_cmdline(child.pid),
    }


def kill_verified(child, rec, sock, store, expect=None, sig=signal.SIGKILL):
    """Signal only a pid that is our own child AND carries this fixture's paths.

    `expect` is the identity captured at spawn. A pid whose start time or command
    line has changed is refused, so a recycled pid is never signalled.
    """
    pid = child.pid
    if child.poll() is not None:
        return rec.record("kill.already_dead", True, pid=pid, sig=sig.name)
    cmd = pid_cmdline(pid)
    lstart = pid_start_time(pid)
    if sock not in cmd or store not in cmd or os.path.basename(DAEMON) not in cmd:
        rec.record(
            "kill.refused_foreign_pid",
            True,
            pid=pid,
            reason="command line is not this fixture; not signalling",
            cmdline=cmd,
        )
        raise ForeignProcess("refusing to signal pid %d: %r" % (pid, cmd))
    if expect is not None:
        if expect["pid"] != pid or expect["lstart"] != lstart or expect["cmdline"] != cmd:
            rec.record(
                "kill.refused_identity_changed",
                True,
                pid=pid,
                reason="pid start time or command line changed since spawn; not signalling",
                expected=expect,
                observed={"pid": pid, "lstart": lstart, "cmdline": cmd},
            )
            raise ForeignProcess("pid %d identity changed since spawn" % pid)
    rec.record(
        "kill.verified_target",
        True,
        pid=pid,
        sig=sig.name,
        lstart=lstart,
        cmdline=cmd,
        store=store,
        socket=sock,
    )
    os.kill(pid, sig)
    gone = child_exited(child, timeout=60)
    rec.record("kill.exited", gone, pid=pid, sig=sig.name)
    if not gone:
        raise RuntimeError("daemon %d survived %s" % (pid, sig.name))
    return rec


class BoundedTimeout(Exception):
    """A command did not finish in its budget and was abandoned."""


def run_bounded(argv, timeout, check=False):
    """Run a command with a hard wall-clock bound that is actually enforceable.

    `subprocess.run(timeout=...)` is not enough: it signals the child and then
    blocks in `wait()`, and a command stuck in uninterruptible sleep (an
    `umount` against a dead NFS mount) never dies, so the caller blocks forever
    anyway. This polls instead, and on timeout kills the child's process group and
    returns without reaping it. The child may survive as a zombie-free orphan; it is
    reported, not waited on.
    """
    p = subprocess.Popen(
        argv,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        stdin=subprocess.DEVNULL,
        start_new_session=True,
    )
    deadline = time.time() + timeout
    while time.time() < deadline:
        if p.poll() is not None:
            out, err = p.communicate()
            return Proc.Result(p.returncode, out, err)
        time.sleep(0.2)
    try:
        os.killpg(os.getpgid(p.pid), signal.SIGKILL)
    except (OSError, ProcessLookupError):
        pass
    raise BoundedTimeout("`%s` exceeded %ds and was abandoned" % (argv[0], timeout))


def _mount_keys(mount):
    """Literal and resolved forms of `mount`, computed while it is safe to do so.

    Resolving a path that is currently a stale NFS mount can block indefinitely,
    so the resolved form is cached at construction time, before the mount exists,
    and only the literal form is used afterwards.
    """
    literal = os.path.normpath(os.path.abspath(mount))
    try:
        resolved = os.path.realpath(mount)
    except OSError:
        resolved = literal
    return [k for k in (literal, resolved) if k]


def is_our_mount(mount, keys=None):
    """True only if the mount table lists this path as a mount point.

    `keys` lets a caller pass forms cached earlier, avoiding realpath on a path
    that may now be a dead mount.
    """
    candidates = keys or _mount_keys(mount)
    try:
        table = run_bounded(["/sbin/mount"], timeout=20).stdout
    except BoundedTimeout:
        return False
    return any((" on %s " % k) in line for k in candidates for line in table.splitlines())


def unmount_private(mount, rec, note, keys=None):
    """Unmount only a path that is verifiably one of our own mounts.

    Uses `keys` resolved earlier and a bounded umount, because this runs right
    after a SIGKILL when the mount may be a dead NFS mount: resolving the path then
    can block forever, and an unbounded umount can block in uninterruptible sleep.
    """
    if not is_our_mount(mount, keys):
        rec.record(
            "unmount.refused_not_our_mount",
            True,
            mount=mount,
            reason=("not in the mount table; leaving it alone"
                    if os.path.lexists(mount) else "no such path"),
        )
        return
    # The server is dead, so the client mount is stale: -f is required here, and
    # bounded, because a stale NFS mount can leave umount uninterruptible.
    try:
        p = run_bounded(["/sbin/umount", "-f", mount], timeout=30)
        rc, err, abandoned = p.returncode, p.stderr.strip()[:400], False
    except BoundedTimeout as e:
        rc, err, abandoned = None, str(e), True
    rec.record(
        "unmount." + note,
        not is_our_mount(mount, keys),
        mount=mount,
        rc=rc,
        abandoned=abandoned,
        stderr=err,
    )


# --------------------------------------------------------------------------
# the daemon under test
# --------------------------------------------------------------------------


class PrivateDaemon:
    def __init__(self, store, mount, socket, log_path):
        self.store = store
        self.mount = mount
        # Resolved before the mount exists. Doing it later, once the path may be a
        # dead NFS mount, can block forever.
        self.mount_keys = _mount_keys(mount)
        self.socket = socket
        self.log_path = log_path
        self.log = None
        self.child = None
        self.identity = None

    def start(self, rec):
        os.makedirs(self.store, mode=0o700, exist_ok=True)
        os.makedirs(self.mount, exist_ok=True)
        self.log = open(self.log_path, "ab")
        # start_new_session detaches from this shell's process group so a tool
        # timeout that SIGTERMs the group cannot reap the daemon under test.
        self.child = subprocess.Popen(
            [
                DAEMON,
                "--store",
                self.store,
                "--mount",
                self.mount,
                "--socket",
                self.socket,
                "--backend",
                "core",
            ],
            stdout=self.log,
            stderr=self.log,
            stdin=subprocess.DEVNULL,
            start_new_session=True,
        )
        self.identity = process_identity(self.child)
        rec.record("daemon.spawned", True, identity=self.identity, session="new")
        self._wait_ready(rec)

    def _log_text(self):
        try:
            with open(self.log_path, "r", errors="replace") as f:
                return f.read()
        except OSError:
            return ""

    def _wait_ready(self, rec):
        deadline = time.time() + DAEMON_READY_SECS
        last_progress = time.time()
        last_seen = self._log_text()
        while time.time() < deadline:
            if self.child.poll() is not None:
                rec.record(
                    "daemon.died",
                    False,
                    pid=self.child.pid,
                    rc=self.child.returncode,
                    log=self._log_text()[-2000:],
                )
                raise RuntimeError("daemon exited before serving")
            text = self._log_text()
            if text != last_seen:
                last_seen = text
                last_progress = time.time()
            if "FATAL" in text or "panicked at" in text:
                rec.record("daemon.fatal", False, log=text[-2000:])
                raise RuntimeError("daemon log shows a fatal error")
            if is_our_mount(self.mount, self.mount_keys) and self.answer():
                rec.record(
                    "daemon.ready",
                    True,
                    pid=self.child.pid,
                    store=self.store,
                    socket=self.socket,
                    mount=self.mount,
                )
                return
            if time.time() - last_progress > NO_PROGRESS_SECS:
                rec.record("daemon.no_progress", False, pid=self.child.pid)
                raise RuntimeError("daemon made no progress in %ds" % NO_PROGRESS_SECS)
            time.sleep(0.25)
        rec.record("daemon.timeout", False, mount=self.mount)
        raise RuntimeError("daemon did not mount and serve in %ds" % DAEMON_READY_SECS)

    def cli(self, args, timeout=CLI_TIMEOUT_SECS):
        return Proc([COWFS, "--socket", self.socket, "--json"] + args, timeout=timeout)

    def cli_json(self, args, timeout=CLI_TIMEOUT_SECS):
        p = self.cli(args, timeout=timeout)
        if p.returncode != 0:
            raise RuntimeError("cli %r exited %d: %s" % (args, p.returncode, p.stderr.strip()))
        return json.loads(p.stdout)

    def answer(self):
        try:
            return self.cli(["status"], timeout=10).returncode == 0
        except RuntimeError:
            return False

    def close_log(self):
        if self.log:
            try:
                self.log.close()
            except OSError:
                pass
            self.log = None


# --------------------------------------------------------------------------
# the operation model
# --------------------------------------------------------------------------


def deterministic_body(n, seed):
    """Reproducible bytes, so the expected set can be rebuilt without the mount."""
    out = bytearray()
    s = seed
    while len(out) < n:
        out.extend(hashlib.blake2b(digest_size=64, key=s.to_bytes(8, "little")).digest())
        s += 1
    return bytes(out[:n])


def fsync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def write_file(mount, snap, name, data, do_fsync, rec, receipts, how="nfs_commit"):
    """Write through the real mount. `do_fsync` makes it a durability receipt."""
    if len(data) > MAX_OP_BYTES:
        raise Budget("op of %d bytes exceeds MAX_OP_BYTES" % len(data))
    d = os.path.join(mount, snap)
    os.makedirs(d, exist_ok=True)
    p = os.path.join(d, name)
    with open(p, "wb") as f:
        f.write(data)
        f.flush()
        if do_fsync:
            os.fsync(f.fileno())
    rel = "%s/%s" % (snap, name)
    sha = sha256_bytes(data)
    if do_fsync:
        receipts.durable(rel, sha, len(data), how, rec)
    else:
        receipts.applied(rel, sha, len(data), "nfs_write_only", rec)
    return p


# --------------------------------------------------------------------------
# verification after a fresh reopen
# --------------------------------------------------------------------------


class CaseResult:
    def __init__(self, name):
        self.name = name
        self.assertions = []
        self.failures = []

    def check(self, rec, ok, label, **fields):
        """Record one named assertion. `ok=False` is a case failure, not a note."""
        ok = bool(ok)
        self.assertions.append({"label": label, "ok": ok})
        rec.record(
            "assert." + label,
            ok,
            case=self.name,
            **fields,
        )
        if not ok:
            self.failures.append(label)
        return ok


def verify_readback(fresh, receipts, res, rec):
    for r in receipts.by_kind("durable"):
        p = os.path.join(fresh.mount, r.path)
        present = os.path.exists(p)
        got = sha256_file(p) if present else None
        matched = present and got == r.sha256 and os.path.getsize(p) == r.size
        # A semantic record, independent of any pass/fail flag: this is what the
        # durable promise actually resolved to, and the cached-verdict check
        # re-derives the outcome from these fields rather than trusting `ok`.
        rec.record(
            "readback.durable",
            True,
            case=res.name,
            path=r.path,
            want=r.sha256,
            want_size=r.size,
            present=present,
            got=got,
            matched=bool(matched),
            boundary=r.boundary,
        )
        if not present:
            parent = os.path.dirname(p)
            try:
                siblings = sorted(os.listdir(parent))
            except OSError as e:
                siblings = ["<listdir failed: %s>" % e]
            res.check(
                rec,
                False,
                "durable_present",
                path=r.path,
                boundary=r.boundary,
                parent_entries=siblings[:40],
                detail="caller's sync returned success and the bytes are gone",
            )
            continue
        res.check(
            rec,
            matched,
            "durable_match",
            path=r.path,
            want=r.sha256,
            got=got,
            want_size=r.size,
            got_size=os.path.getsize(p),
        )
    for r in receipts.by_kind("applied"):
        p = os.path.join(fresh.mount, r.path)
        present = os.path.exists(p)
        rec.record(
            "applied.outcome",
            True,
            case=res.name,
            path=r.path,
            present=present,
            note="no sync was issued; docs/design.md permits bounded loss of recent writes",
        )
    for r in receipts.by_kind("removed"):
        p = os.path.join(fresh.mount, r.path)
        present = os.path.exists(p)
        res.check(
            rec,
            not present,
            "removed_absent",
            path=r.path,
            present=present,
            detail="this harness removed it on purpose",
        )


def verify_snapshot_names(fresh, receipts, res, rec):
    listed = {s.get("name") for s in fresh.cli_json(["snapshot", "list"]).get("snapshots", [])}
    for n in receipts.names:
        present = n["name"] in listed
        if n["level"] == "durable":
            res.check(
                rec,
                present,
                "snapshot_name_present",
                snapshot=n["name"],
                present=present,
                boundary=n["boundary"],
            )
        else:
            rec.record(
                "snapshot.outcome",
                True,
                case=res.name,
                snapshot=n["name"],
                present=present,
                note="control acks Ack::Applied; absence permitted, presence recorded",
            )
    rec.record("snapshot.set", True, case=res.name, listed=sorted(x for x in listed if x))
    return listed


def verify_no_torn_tree(fresh, res, rec):
    listed = {
        s.get("name") for s in fresh.cli_json(["snapshot", "list"]).get("snapshots", [])
    }
    for name in sorted(x for x in listed if x):
        d = os.path.join(fresh.mount, name)
        if not res.check(
            rec, os.path.isdir(d), "snapshot_is_dir", snapshot=name, present=os.path.isdir(d)
        ):
            continue
        try:
            entries = sorted(os.listdir(d))
        except OSError as e:
            res.check(rec, False, "snapshot_listable", snapshot=name, err=str(e))
            continue
        rec.record("snapshot.listing", True, case=res.name, snapshot=name, entries=entries[:60])


def verify_fsck(fresh, res, rec):
    out = fresh.cli_json(["fsck"], timeout=120)
    problems = out.get("problems") or []
    res.check(
        rec,
        len(problems) == 0,
        "fsck_clean",
        problems=problems[:20],
        n_problems=len(problems),
        blocks=out.get("blocks"),
        snapshots=out.get("snapshots"),
    )


# --------------------------------------------------------------------------
# one crash case
# --------------------------------------------------------------------------


def run_case(identity, ops, run_dir, parent_rec, case_dir=None):
    """ops(d, receipts, res, rec, probe) does the work; then the daemon is SIGKILLed
    and the same store is reopened by a fresh daemon and verified."""
    key = identity["key"]
    case = case_name_for(identity)
    if case_dir is None:
        case_dir = next_case_dir(run_dir, case, identity)
    rec = Recorder(case_dir)
    parent_rec.record(
        "case.begin", True, key=key, case=case, identity=identity, dir=case_dir
    )

    store = os.path.join(case_dir, "store")
    mount1 = os.path.join(case_dir, "mnt1")
    mount2 = os.path.join(case_dir, "mnt2")
    sock1 = sock_path(case)
    sock2 = sock_path(case + "-r2")
    log1 = os.path.join(case_dir, "daemon1.log")
    log2 = os.path.join(case_dir, "daemon2.log")
    for s in (sock1, sock2):
        if os.path.exists(s):
            os.unlink(s)

    d1 = PrivateDaemon(store, mount1, sock1, log1)
    d2 = None
    res = CaseResult(case)
    receipts = Receipts()
    probe = Probe(rec, case)
    started = time.time()
    outcome = "aborted"
    error = None

    try:
        d1.start(rec)
        ops(d1, receipts, res, rec, probe)
        if time.time() - started > CASE_DEADLINE_SECS:
            raise Budget("case exceeded CASE_DEADLINE_SECS")

        if receipts.items:
            rec.record(
                "crash.window",
                True,
                case=case,
                secs_since_last_receipt=round(time.time() - receipts.items[-1].t, 3),
                last_kind=receipts.items[-1].kind,
                note="background flusher default is 500ms; below it the data was still in daemon memory",
            )

        kill_verified(
            d1.child, rec, sock1, store, expect=d1.identity, sig=signal.SIGKILL
        )
        d1.close_log()
        unmount_private(mount1, rec, "after_kill", d1.mount_keys)

        d2 = PrivateDaemon(store, mount2, sock2, log2)
        d2.start(rec)

        verify_readback(d2, receipts, res, rec)
        verify_snapshot_names(d2, receipts, res, rec)
        verify_no_torn_tree(d2, res, rec)
        verify_fsck(d2, res, rec)
        outcome = "fail" if res.failures else "pass"
    except CaseFailure as e:
        outcome = "fail"
        error = str(e)
        rec.record("case.assertion_failed", False, case=case, err=str(e))
    except (ForeignProcess, Budget) as e:
        outcome = "aborted"
        error = str(e)
        rec.record("case.aborted", False, case=case, err=str(e))
    except Exception as e:  # noqa: BLE001 - a case error must be recorded, not raised
        outcome = "error"
        error = "%s: %s" % (type(e).__name__, e)
        rec.record("case.error", False, case=case, err=error)
    finally:
        for d, mnt in ((d1, mount1), (d2, mount2)):
            if d is None:
                continue
            try:
                if d.child is not None and d.child.poll() is None:
                    kill_verified(
                        d.child, rec, d.socket, d.store, expect=d.identity,
                        sig=signal.SIGKILL,
                    )
            except Exception:  # noqa: BLE001
                pass
            d.close_log()
            unmount_private(mnt, rec, "teardown", d.mount_keys)
            for victim in (d.socket, d.socket + ".lock"):
                if os.path.exists(victim):
                    try:
                        os.unlink(victim)
                        rec.record("teardown.socket_removed", True, socket=victim)
                    except OSError:
                        pass

    manifest = {
        "identity": identity,
        "case": case,
        "outcome": outcome,
        "error": error,
        "receipts": [r.as_dict() for r in receipts.items],
        "assertions": res.assertions,
        "failures": res.failures,
        "steps": [rec.step],
        "secs": round(time.time() - started, 1),
    }
    # The verdict is persisted to the ledger as it was computed, before the
    # terminal record. A cached verdict is checked against this and against the
    # semantic readback records, so editing manifest.json alone cannot relabel a
    # failure as a pass.
    rec.record(
        "case.verdict",
        outcome == "pass",
        outcome=outcome,
        failures=list(res.failures),
        assertions=[{"label": a["label"], "ok": a["ok"]} for a in res.assertions],
        receipt_state=[r.as_dict() for r in receipts.items],
    )
    mpath = os.path.join(case_dir, "manifest.json")
    with open(mpath, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    rec.record("case.terminal", outcome == "pass", key=key, manifest=manifest)
    rec.close()
    return outcome, manifest, mpath


# --------------------------------------------------------------------------
# the cases
# --------------------------------------------------------------------------


def ctl_create(d, name, frm=None):
    args = ["snapshot", "create", name]
    if frm:
        args += ["--from", frm]
    return d.cli_json(args)


def case_write_fsync(d, receipts, res, rec, probe):
    """Level durable: write known bytes, fsync, then crash."""
    ctl_create(d, "live")
    receipts.promise_snapshot("live", "applied", "ctl_snapshot_create", rec)
    for i in range(SAMPLE_FILES):
        write_file(
            d.mount, "live", "d%02d.bin" % i,
            deterministic_body(SAMPLE_BYTES, 100 + i), True, rec, receipts,
        )
    fsync_dir(os.path.join(d.mount, "live"))
    receipts.promise_snapshot("live", "durable", "file_fsync", rec)
    rec.record("case.phase", True, phase="wrote and fsynced")


def case_write_nofsync(d, receipts, res, rec, probe):
    """Level applied: no sync issued at all."""
    ctl_create(d, "live")
    for i in range(SAMPLE_FILES):
        write_file(
            d.mount, "live", "a%02d.bin" % i,
            deterministic_body(SAMPLE_BYTES, 200 + i), False, rec, receipts,
        )
    rec.record("case.phase", True, phase="wrote without any fsync")


def case_write_race(d, receipts, res, rec, probe):
    """Kill as close to an un-fsynced write as possible, inside the flusher window."""
    ctl_create(d, "live")
    for i in range(SAMPLE_FILES):
        write_file(
            d.mount, "live", "r%02d.bin" % i,
            deterministic_body(SAMPLE_BYTES, 1000 + i), False, rec, receipts,
        )
    rec.record("case.phase", True, phase="wrote without fsync, returning immediately")


def case_mixed(d, receipts, res, rec, probe):
    """One durable file and one genuinely un-fsynced file in the same snapshot."""
    ctl_create(d, "live")
    write_file(d.mount, "live", "durable.bin", deterministic_body(SAMPLE_BYTES, 300), True, rec, receipts)
    write_file(d.mount, "live", "recent.bin", deterministic_body(SAMPLE_BYTES, 301), False, rec, receipts)
    fsync_dir(os.path.join(d.mount, "live"))
    rec.record("case.phase", True, phase="one durable, one applied")


def _rename_setup(d, receipts, res, rec, probe, seed):
    ctl_create(d, "live")
    write_file(
        d.mount, "live", "orig.bin", deterministic_body(SAMPLE_BYTES, seed), True, rec, receipts
    )
    src = os.path.join(d.mount, "live", "orig.bin")
    dst = os.path.join(d.mount, "live", "moved.bin")
    probe.sample("rename")
    os.rename(src, dst)
    probe.mark("rename")
    return dst


def case_rename_posix_durability(d, receipts, res, rec, probe):
    """POSIX rename + parent-directory fsync. Expected to FAIL on the current tree.

    `docs/design.md` lists atomic `rename` under full POSIX, and POSIX says a
    successful `fsync` on the parent directory makes the new name durable. This case
    asserts exactly that, so on a tree where the macOS NFS client emits no COMMIT for
    a directory fsync the rename comes back lost and the case FAILS. It is written to
    be falsifiable and is not reclassified afterwards. See issue #90.
    """
    _rename_setup(d, receipts, res, rec, probe, 400)
    probe.sample("fsync_parent_dir")
    fsync_dir(os.path.join(d.mount, "live"))
    probe.mark("fsync_parent_dir")
    receipts.repath_by_path("live/orig.bin", "live/moved.bin", rec)
    rec.record(
        "case.phase",
        True,
        phase="renamed then fsynced the parent directory",
        wire=probe.report("fsync_parent_dir"),
        note="caller's fsync returned 0; the new name is promised durable",
    )


def case_rename_posix_durability_ro(d, receipts, res, rec, probe):
    """POSIX rename + fsync of a read-only descriptor on the renamed file.

    Same promise, second spelling. Expected to FAIL on the current tree for the same
    reason. See issue #90.
    """
    dst = _rename_setup(d, receipts, res, rec, probe, 401)
    probe.sample("fsync_readonly_fd")
    fd = os.open(dst, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    probe.mark("fsync_readonly_fd")
    receipts.repath_by_path("live/orig.bin", "live/moved.bin", rec)
    rec.record(
        "case.phase",
        True,
        phase="renamed then fsynced a read-only descriptor",
        wire=probe.report("fsync_readonly_fd"),
        note="caller's fsync returned 0; the new name is promised durable",
    )


def case_rename_committed(d, receipts, res, rec, probe):
    """Positive control: a COMMIT that does arrive commits the queued rename.

    Forces one by dirtying and fsyncing a sibling file in the same snapshot. This
    passes on the current tree and is what makes the two failing cases attributable
    to the missing COMMIT rather than to the rename path being broken.
    """
    _rename_setup(d, receipts, res, rec, probe, 402)
    probe.sample("trigger_fsync")
    write_file(
        d.mount, "live", "trigger.bin", deterministic_body(SAMPLE_BYTES, 403), True, rec, receipts
    )
    probe.mark("trigger_fsync")
    receipts.repath_by_path("live/orig.bin", "live/moved.bin", rec)
    rec.record(
        "case.phase",
        True,
        phase="renamed then forced a COMMIT via a sibling write+fsync",
        wire=probe.report("trigger_fsync"),
    )


def case_snapshot_fork(d, receipts, res, rec, probe):
    """Fork, then force a real COMMIT so the fork name becomes durable."""
    ctl_create(d, "base")
    receipts.promise_snapshot("base", "applied", "ctl_snapshot_create", rec)
    for i in range(SAMPLE_FILES):
        write_file(
            d.mount, "base", "f%02d.bin" % i,
            deterministic_body(SAMPLE_BYTES, 500 + i), True, rec, receipts,
        )
    fsync_dir(os.path.join(d.mount, "base"))
    receipts.promise_snapshot("base", "durable", "file_fsync", rec)
    ctl_create(d, "fork", frm="base")
    receipts.promise_snapshot("fork", "applied", "ctl_snapshot_create", rec)
    write_file(d.mount, "fork", "marker.bin", deterministic_body(SAMPLE_BYTES, 550), True, rec, receipts)
    receipts.promise_snapshot("fork", "durable", "write_fsync_inside_fork", rec)
    rec.record("case.phase", True, phase="forked and committed the fork name")


def case_snapshot_remove(d, receipts, res, rec, probe):
    """Remove a fork; the base's durable bytes must survive."""
    ctl_create(d, "base")
    receipts.promise_snapshot("base", "applied", "ctl_snapshot_create", rec)
    for i in range(SAMPLE_FILES):
        write_file(
            d.mount, "base", "r%02d.bin" % i,
            deterministic_body(SAMPLE_BYTES, 600 + i), True, rec, receipts,
        )
    fsync_dir(os.path.join(d.mount, "base"))
    receipts.promise_snapshot("base", "durable", "file_fsync", rec)
    ctl_create(d, "doomed", frm="base")
    rm = d.cli(["snapshot", "rm", "doomed"])
    res.check(rec, rm.returncode == 0, "snapshot_rm_ok", rc=rm.returncode,
              stderr=rm.stderr.strip()[:200])
    rec.record("case.phase", True, phase="removed a fork")


def case_mmap(d, receipts, res, rec, probe):
    """mmap a file, msync, fsync, then crash."""
    ctl_create(d, "live")
    data = deterministic_body(MAX_OP_BYTES, 700)
    p = os.path.join(d.mount, "live", "mapped.bin")
    with open(p, "wb") as f:
        f.write(b"\0" * len(data))
        f.flush()
        os.fsync(f.fileno())
    with open(p, "r+b") as f:
        mm = mmap.mmap(f.fileno(), len(data))
        mm.write(data)
        mm.flush()
        mm.close()
        os.fsync(f.fileno())
    receipts.durable("live/mapped.bin", sha256_bytes(data), len(data), "mmap_msync+fsync", rec)
    rec.record("case.phase", True, phase="mmap write, msync, fsync")


def case_gc_crash(d, receipts, res, rec, probe):
    """A bounded GC cycle, then a crash. Survivor integrity, not reclamation."""
    ctl_create(d, "live")
    write_file(d.mount, "live", "survivor.bin", deterministic_body(SAMPLE_BYTES, 799), True, rec, receipts)
    ctl_create(d, "garbage")
    data = deterministic_body(GC_CASE_BYTES, 800)
    with open(os.path.join(d.mount, "garbage", "big.bin"), "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    sha = sha256_bytes(data)
    receipts.removed("garbage/big.bin", sha, len(data), "snapshot_rm", rec)
    fsync_dir(os.path.join(d.mount, "garbage"))
    rm = d.cli(["snapshot", "rm", "garbage"])
    res.check(rec, rm.returncode == 0, "gc_rm_garbage_ok", rc=rm.returncode)
    got = sha256_file(os.path.join(d.mount, "live", "survivor.bin"))
    res.check(rec, got == receipts.by_kind("durable")[-1].sha256, "gc_survivor_before", sha256=got)
    out = d.cli_json(["gc"], timeout=180)
    rec.record(
        "gc.report", True,
        candidate_blocks=out.get("candidate_blocks"),
        candidate_bytes=out.get("candidate_bytes"),
        freed_blocks=out.get("freed_blocks"),
        freed_bytes=out.get("freed_bytes"),
        gross_removed_bytes=out.get("gross_removed_bytes"),
        note="a small fixture lives in the open pack, which cannot be unlinked; freed is 0 by construction",
    )
    got = sha256_file(os.path.join(d.mount, "live", "survivor.bin"))
    res.check(rec, got == receipts.by_kind("durable")[-1].sha256, "gc_survivor_after", sha256=got)
    rec.record("case.phase", True, phase="collected, then crashing")


def case_kill_control(d, receipts, res, rec, probe):
    """Do-nothing control: an idle daemon is killed and the store reopens clean."""
    ctl_create(d, "live")
    write_file(d.mount, "live", "c.bin", deterministic_body(SAMPLE_BYTES, 900), True, rec, receipts)
    fsync_dir(os.path.join(d.mount, "live"))
    rec.record("case.phase", True, phase="idle kill control")


CASES = {
    "write_fsync": case_write_fsync,
    "write_nofsync": case_write_nofsync,
    "write_race": case_write_race,
    "mixed": case_mixed,
    "rename_posix_durability": case_rename_posix_durability,
    "rename_posix_durability_ro": case_rename_posix_durability_ro,
    "rename_committed": case_rename_committed,
    "snapshot_fork": case_snapshot_fork,
    "snapshot_remove": case_snapshot_remove,
    "mmap": case_mmap,
    "gc_crash": case_gc_crash,
    "kill_control": case_kill_control,
}

SAMPLE_CASES = ["write_fsync", "rename_posix_durability"]
MATRIX_CASES = list(CASES)

# Cases this harness expects to fail on the current tree, with the reason. Listed so
# the summary can separate "the product fails here" from "the harness is broken", not
# to excuse them: each one is a real failing assertion and the run exits non-zero.
KNOWN_FAILING = {
    "rename_posix_durability": "issue #90: a successful parent-directory fsync emits no COMMIT on this client, so the promised name is lost",
    "rename_posix_durability_ro": "issue #90: same, via a read-only descriptor fsync",
}


# --------------------------------------------------------------------------
# native (APFS) controls, same operations
# --------------------------------------------------------------------------


def internal_native_writer(root, expect, mode):
    """Child half of the native control.

    Performs the same operations as the rename cases: a durable write, a rename, a
    parent-directory fsync, then a read-only descriptor fsync, recording every
    receipt durably beside the data before optionally killing itself.
    """
    os.makedirs(expect, exist_ok=True)
    os.makedirs(root, exist_ok=True)
    d = os.path.join(root, "snap")
    os.makedirs(d, exist_ok=True)
    receipts = Receipts()
    rpath = os.path.join(expect, "receipts.json")

    def flush_receipts():
        with open(rpath, "w", encoding="utf-8") as f:
            json.dump([r.as_dict() for r in receipts.items], f, indent=2)
            f.flush()
            os.fsync(f.fileno())

    data = deterministic_body(SAMPLE_BYTES, 9000)
    with open(os.path.join(d, "orig.bin"), "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    receipts.durable("snap/orig.bin", sha256_bytes(data), len(data), "apfs_fsync")
    flush_receipts()

    os.rename(os.path.join(d, "orig.bin"), os.path.join(d, "moved.bin"))
    receipts.repath_by_path("snap/orig.bin", "snap/moved.bin")
    fsync_dir(d)
    fd = os.open(os.path.join(d, "moved.bin"), os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    flush_receipts()

    if mode == "kill":
        sys.stderr.write("native writer: SIGKILL self\n")
        sys.stderr.flush()
        os.kill(os.getpid(), signal.SIGKILL)
        os._exit(70)  # unreachable
    return 0


def run_native_case(identity, run_dir, parent_rec, case_dir=None):
    """APFS control: a real writer kill and a clean restart, same operations as cowfs."""
    case = "native-%s-r%d" % (identity["phase"], identity["rep"])
    if case_dir is None:
        case_dir = next_case_dir(run_dir, case, identity)
    rec = Recorder(case_dir)
    res = CaseResult(case)
    started = time.time()
    outcomes = {}

    for mode, label in (("kill", "native_kill"), ("clean", "native_restart_control")):
        root = os.path.join(case_dir, "apfs-" + mode)
        expect = os.path.join(case_dir, "expected-" + mode)
        shutil.rmtree(root, ignore_errors=True)
        shutil.rmtree(expect, ignore_errors=True)
        os.makedirs(root, exist_ok=True)
        os.makedirs(expect, exist_ok=True)
        argv = [
            sys.executable, os.path.abspath(__file__),
            "--internal-native-writer", root, expect, mode,
        ]
        logf = open(os.path.join(case_dir, "writer-%s.log" % mode), "ab")
        child = subprocess.Popen(
            argv, stdout=logf, stderr=logf,
            stdin=subprocess.DEVNULL, start_new_session=True,
        )
        ident = process_identity(child)
        rec.record("native.spawned", True, mode=mode, identity=ident)
        exited = child_exited(child, timeout=120)
        expected_rc = -signal.SIGKILL if mode == "kill" else 0
        res.check(
            rec, exited and child.returncode == expected_rc,
            "native_writer_exit_" + mode, rc=child.returncode, expected_rc=expected_rc,
        )
        logf.close()

        receipts = Receipts()
        rpath = os.path.join(expect, "receipts.json")
        if not res.check(
            rec, os.path.exists(rpath), "native_receipts_present_" + mode,
            present=os.path.exists(rpath),
        ):
            continue
        with open(rpath, "r", encoding="utf-8") as f:
            receipts.items = [
                Receipt(r["path"], r["sha256"], r["size"], r["kind"], r["boundary"], r["seq"])
                for r in json.load(f)
            ]
        for r in receipts.by_kind("durable"):
            p = os.path.join(root, r.path)
            got = sha256_file(p) if os.path.exists(p) else None
            # Native must keep the promised bytes: this is the divergence point.
            res.check(
                rec, got == r.sha256,
                "native_durable_match_" + mode, path=r.path, want=r.sha256, got=got,
            )
        outcomes[label] = res.failures == []

    outcome = "fail" if res.failures else "pass"
    manifest = {
        "identity": identity,
        "case": case,
        "outcome": outcome,
        "receipts": [],
        "assertions": res.assertions,
        "failures": res.failures,
        "secs": round(time.time() - started, 1),
    }
    # Same ledger contract as a cowfs case, so a cached native verdict is checked
    # against the same things rather than being trusted.
    rec.record(
        "case.verdict",
        outcome == "pass",
        outcome=outcome,
        failures=list(res.failures),
        assertions=[{"label": a["label"], "ok": a["ok"]} for a in res.assertions],
        receipt_state=[],
    )
    mpath = os.path.join(case_dir, "manifest.json")
    with open(mpath, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    rec.record("case.terminal", outcome == "pass", key=identity["key"], manifest=manifest)
    rec.close()
    parent_rec.record(
        "case.begin", True, key=identity["key"], case=case, identity=identity,
        dir=case_dir, category="native",
    )
    parent_rec.record(
        "native.terminal", outcome == "pass", key=identity["key"], case=case, outcome=outcome
    )
    return outcome, manifest, mpath


# --------------------------------------------------------------------------
# cache: fail closed, or do not reuse at all
# --------------------------------------------------------------------------

MAX_ATTEMPTS = 64


def attempt_base(identity):
    return "k%s" % identity["key"][:12]


def candidate_dirs(run_dir, case, identity):
    """Every directory this identity could legitimately have written, oldest first."""
    base = os.path.join(run_dir, "cases", case)
    out = [os.path.join(base, attempt_base(identity))]
    out += [
        os.path.join(base, "%s-a%d" % (attempt_base(identity), i))
        for i in range(1, MAX_ATTEMPTS)
    ]
    return out


def next_case_dir(run_dir, case, identity):
    """A fresh directory for this identity, preserving every earlier attempt.

    Nothing is wiped: a re-execution lands in a new attempt directory so a store
    left by an earlier attempt cannot collide with it (a case that creates snapshot
    `live` would otherwise fail on its own residue), and the earlier evidence stays
    on disk for inspection.
    """
    base = os.path.join(run_dir, "cases", case)
    os.makedirs(base, exist_ok=True)
    for d in candidate_dirs(run_dir, case, identity):
        if not os.path.isdir(d):
            # First gap is where a new attempt goes.
            os.makedirs(d, exist_ok=True)
            return d
    raise RuntimeError("exhausted %d attempts for %s" % (MAX_ATTEMPTS, case))


def case_name_for(identity):
    return "%s-%s-r%d" % (identity["case"], identity["phase"], identity["rep"])


REQUIRED_MANIFEST_FIELDS = (
    "identity", "case", "outcome", "receipts", "assertions", "failures",
)


def _expected_from_assert_record(r):
    """Recompute what an assertion record's own fields say it should be.

    Returns None when the record carries no field that determines the answer, so
    this never invents an expectation. Every case that returns an expectation is
    derivable from data the record already carries, which is what makes a flipped
    `ok` flag detectable: the verdict comes from `want`/`got`, `present`,
    `n_problems`, `rc` and the recorded parent listing, not from the flag.
    """
    label = str(r.get("name", ""))[len("assert."):]
    if label == "durable_match":
        return r.get("want") == r.get("got") and r.get("want_size") == r.get("got_size")
    if label == "durable_present":
        # The failure path records the parent listing, so presence of the file is
        # checkable against it. An ok=True claim that also carries a listing which
        # omits the file is a contradiction.
        entries = r.get("parent_entries")
        if entries is None:
            return None
        base = os.path.basename(str(r.get("path", "")))
        return base in entries
    if label == "fsck_clean":
        return (r.get("n_problems") or 0) == 0
    if label in ("snapshot_name_present", "snapshot_is_dir"):
        if "present" not in r:
            return None
        return bool(r["present"])
    if label == "removed_absent":
        # The assertion is that the path is gone, so ok tracks the absence.
        if "present" not in r:
            return None
        return not r["present"]
    if label in ("snapshot_rm_ok", "gc_rm_garbage_ok"):
        return r.get("rc") == 0
    if label.startswith("native_writer_exit_"):
        return r.get("rc") == r.get("expected_rc")
    if label.startswith("native_durable_match_"):
        return r.get("want") == r.get("got")
    if label.startswith("native_receipts_present_"):
        return bool(r.get("present", True))
    return None


def derive_verdict(prior, manifest_receipts):
    """Recompute the case outcome from the ledger, ignoring every `ok` flag.

    Returns (derived_outcome, derived_failure_labels, contradictions).

    `contradictions` lists assertion records whose `ok` flag disagrees with what
    the record's own fields say. Those are reported separately from the derived
    failures and are never deduplicated against them, because a flag that
    contradicts its own evidence is a defect even when the outcome happens to come
    out the same either way.
    """
    failures = []
    contradictions = []
    readback = {
        r["path"]: r for r in prior if r.get("name") == "readback.durable"
    }
    # Every durable receipt must have a semantic readback record. One without is
    # a promise that was never checked.
    for rc in manifest_receipts:
        if rc.get("kind") != "durable":
            continue
        rb = readback.get(rc["path"])
        if rb is None:
            failures.append("durable_present")
            continue
        if not rb.get("present"):
            failures.append("durable_present")
        elif rb.get("got") != rc["sha256"]:
            failures.append("durable_match")

    for r in prior:
        n = str(r.get("name", ""))
        if not n.startswith("assert."):
            continue
        label = n[len("assert."):]
        want = _expected_from_assert_record(r)
        if want is not None and bool(r.get("ok")) != want:
            contradictions.append(label)
            failures.append(label)

    derived = "fail" if failures else "pass"
    return derived, sorted(set(failures)), sorted(set(contradictions))


def validate_cached(identity, case_dir):
    """Decide whether a previous verdict in `case_dir` may be reused.

    Returns (ok, reason, manifest). This is artifact consistency, not
    authentication: anyone who can rewrite both `manifest.json` and
    `records.jsonl` coherently can still forge a verdict, because there is no
    signature and none is claimed. What it does guarantee is that the specific
    laundering the review performed -- editing a manifest to turn a recorded
    failure into a pass -- is rejected, because the outcome is re-derived from
    the ledger's semantic readback fields and from a verdict record written at
    execution time.
    """
    mpath = os.path.join(case_dir, "manifest.json")
    epath = os.path.join(case_dir, "records.jsonl")
    if not os.path.exists(mpath):
        return False, "no manifest", None
    try:
        with open(mpath, "r", encoding="utf-8") as f:
            man = json.load(f)
    except (OSError, ValueError) as e:
        return False, "manifest unreadable: %s" % e, None

    for field in REQUIRED_MANIFEST_FIELDS:
        if field not in man:
            return False, "manifest missing %r" % field, None
    if man["identity"] != identity:
        return False, "identity mismatch", None
    if man["outcome"] not in ("pass", "fail"):
        return False, "outcome not conclusive: %r" % man["outcome"], None
    if not os.path.exists(epath):
        return False, "evidence file absent", None

    prior = read_records(epath)
    term = [r for r in prior if r.get("name") == "case.terminal"]
    if not term:
        return False, "no terminal record in evidence", None
    if term[-1].get("manifest", {}).get("identity") != identity:
        return False, "terminal record identity mismatch", None

    verdict = [r for r in prior if r.get("name") == "case.verdict"]
    if not verdict:
        return False, "no case.verdict in evidence", None
    v = verdict[-1]

    # Receipts: kind, resolved path, hash and size must all agree, and the
    # resolved path must follow the recorded repath chain rather than be asserted.
    issued = {}
    for r in prior:
        if r.get("name") == "receipt.issued":
            issued[r["receipt"]["path"]] = r["receipt"]
    renamed = {}
    for r in prior:
        if r.get("name") == "receipt.repath":
            renamed[r["to"]] = r["from"]
    ledger_state = v.get("receipt_state") or []
    if len(ledger_state) != len(man["receipts"]):
        return False, "receipt count differs between ledger and manifest", None
    for want, got in zip(ledger_state, man["receipts"]):
        for field in ("path", "sha256", "size", "kind", "boundary"):
            if want.get(field) != got.get(field):
                return False, "receipt %r field %r differs" % (
                    want.get("path"), field,
                ), None
        # A renamed path is only legitimate if the ledger recorded the rename and
        # the path it came from was genuinely issued.
        if got["path"] in renamed:
            origin = renamed[got["path"]]
            if origin not in issued:
                return False, "receipt %r claims a rename from %r, which was never issued" % (
                    got["path"], origin,
                ), None

    # Assertions: the manifest must match the verdict record exactly, values included.
    v_asserts = v.get("assertions") or []
    if [(a["label"], a["ok"]) for a in v_asserts] != [
        (a["label"], a["ok"]) for a in man["assertions"]
    ]:
        return False, "manifest assertions differ from the ledger verdict", None

    # The outcome must be the one the ledger's own facts imply.
    derived, derived_failures, contradictions = derive_verdict(prior, man["receipts"])
    if contradictions:
        return False, "assertion flags contradict their own fields: %r" % (
            contradictions,
        ), None
    if derived != man["outcome"]:
        return False, "manifest outcome %r but the ledger derives %r" % (
            man["outcome"], derived,
        ), None
    if sorted(man["failures"]) != derived_failures:
        return False, "manifest failures %r but the ledger derives %r" % (
            sorted(man["failures"]), derived_failures,
        ), None
    if v.get("outcome") != man["outcome"]:
        return False, "ledger verdict %r but manifest says %r" % (
            v.get("outcome"), man["outcome"],
        ), None
    if sorted(v.get("failures") or []) != sorted(man["failures"]):
        return False, "ledger verdict failures differ from the manifest", None

    checked = set()
    for r in prior:
        n = str(r.get("name", ""))
        if n.startswith("assert."):
            checked.add(n[len("assert."):])
    for a in man["assertions"]:
        if a["label"] not in checked:
            return False, "assertion %r absent from evidence" % a["label"], None
    return True, "verified from evidence", man


# --------------------------------------------------------------------------
# self-tests: the harness must fail closed
# --------------------------------------------------------------------------


def selftest(rec, run_dir):
    ok = True
    ghost = sock_path("ghost")
    if os.path.exists(ghost):
        os.unlink(ghost)
    p = Proc([COWFS, "--socket", ghost, "--json", "status"], timeout=15)
    classified = p.returncode == 3 and "not_running" in p.stdout
    rec.record("selftest.ghost_socket_is_not_success", classified,
               rc=p.returncode, out=p.stdout.strip()[:200])
    ok = ok and classified

    # A foreign pid must be refused and must still be alive afterwards.
    helper = subprocess.Popen(["sleep", "30"], start_new_session=True)
    try:
        class _Fake:
            pid = helper.pid

            def poll(self):
                return None

        refused = False
        try:
            kill_verified(_Fake(), rec, "/nonexistent-a", "/nonexistent-b")
        except ForeignProcess:
            refused = True
        time.sleep(0.3)
        still_alive = pid_alive(helper.pid)
        rec.record(
            "selftest.kill_refuses_foreign_pid", refused,
            pid=helper.pid, still_alive_after_300ms=still_alive,
        )
        ok = ok and refused and still_alive
    finally:
        helper.kill()
        helper.wait()

    plain = os.path.join(run_dir, "plain")
    os.makedirs(plain, exist_ok=True)
    rec.record("selftest.plain_dir_is_not_a_mount", not is_our_mount(plain), path=plain)
    ok = ok and not is_our_mount(plain)

    # A forged cache entry must be refused, not reused.
    forged = case_identity("selftest", "write_fsync", 0, ["selftest"], {"forged": True})
    good, reason, _ = validate_cached(forged, run_dir)
    rec.record("selftest.forged_cache_refused", not good, reason=reason)
    ok = ok and not good
    return ok


# --------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------


def binaries_present(rec):
    missing = [b for b in (DAEMON, COWFS) if not os.path.exists(b)]
    if missing:
        rec.record("build.missing", False, missing=missing)
        raise SystemExit(
            "missing binaries: %s. Run: cargo build -p cowfs-cli -p cowfs-daemon" % missing
        )
    rec.record("build.present", True, daemon=DAEMON, cli=COWFS)


def source_identity(rec):
    rev = git_rev()
    rec.record(
        "source.identity",
        True,
        rev=rev,
        harness_sha256=harness_digest(),
        daemon_sha256=binary_digest(DAEMON),
        cli_sha256=binary_digest(COWFS),
        note="the harness is not present at the base commit; this run is bound to this rev",
    )


OUTCOME_TO_COUNT = {"pass": "passed", "fail": "failed", "aborted": "aborted", "error": "error"}


def tally(counts, outcome):
    counts[OUTCOME_TO_COUNT.get(outcome, "error")] += 1


def main():
    global SOCK_DIR
    ap = argparse.ArgumentParser()
    ap.add_argument("--stage", choices=["sample", "matrix", "native", "all"], default="sample")
    ap.add_argument("--reps", type=int, default=1)
    ap.add_argument("--run-id", default=None)
    ap.add_argument("--only", default=None, help="comma-separated case names")
    ap.add_argument(
        "--accept-cached",
        action="store_true",
        help="exit 0 on a cached-only run; the summary still reports fresh_acceptance false",
    )
    ap.add_argument("--internal-native-writer", nargs=3, metavar=("ROOT", "EXPECT", "MODE"))
    args = ap.parse_args()

    if args.internal_native_writer:
        return internal_native_writer(*args.internal_native_writer)

    run_id = args.run_id or ("run-%d" % int(time.time()))
    run_dir = os.path.join(OUT_ROOT, run_id)
    os.makedirs(run_dir, exist_ok=True)
    rec = Recorder(run_dir)
    log("run dir: %s" % run_dir)

    SOCK_DIR = os.path.join(SOCK_ROOT, "cowfs-crash88-%s" % run_id)
    os.makedirs(SOCK_DIR, mode=0o700, exist_ok=True)
    os.chmod(SOCK_DIR, 0o700)
    rec.record("sockdir.ready", True, dir=SOCK_DIR)

    counts = {"executed": 0, "reused": 0, "rejected": 0, "passed": 0, "failed": 0,
              "aborted": 0, "error": 0, "unknown_case": 0}
    failures = {}
    exit_code = 3

    try:
        binaries_present(rec)
        source_identity(rec)

        if not selftest(rec, run_dir):
            rec.record("selftest.failed", False)
            log("selftest failed; not running cases")
            return 3

        plan = []
        if args.only:
            for rep in range(args.reps):
                for c in [x.strip() for x in args.only.split(",") if x.strip()]:
                    plan.append((c, "only", rep))
        else:
            if args.stage in ("sample", "all"):
                for rep in range(args.reps):
                    for c in SAMPLE_CASES:
                        plan.append((c, "sample", rep))
            if args.stage in ("matrix", "all"):
                for rep in range(args.reps):
                    for c in MATRIX_CASES:
                        plan.append((c, "matrix", rep))
        native = (not args.only) and args.stage in ("native", "all")
        argv_scope = sorted(
            a for a in (args.stage, args.only, str(args.reps)) if a is not None
        )

        # The native control is a planned execution too, so it is counted here
        # rather than appearing in the totals without ever being in the plan.
        planned_cowfs = len(plan)
        planned_native = 1 if native else 0
        planned_total = planned_cowfs + planned_native
        rec.record(
            "plan.enumerated",
            True,
            planned_total=planned_total,
            planned_cowfs=planned_cowfs,
            planned_native=planned_native,
            entries=["%s/%s/r%d" % (c, ph, r) for c, ph, r in plan]
            + (["native/native/r0"] if native else []),
            note="planned_total must equal executed + reused for the accounting to hold",
        )
        log("plan: %d executions (%d cowfs + %d native)"
            % (planned_total, planned_cowfs, planned_native))

        for cname, phase, rep in plan:
            if cname not in CASES:
                rec.record("plan.unknown_case", False, case=cname)
                failures["%s-%s-r%d" % (cname, phase, rep)] = "unknown case"
                counts["unknown_case"] += 1
                continue
            ident = case_identity(phase, cname, rep, argv_scope, {"files": SAMPLE_FILES})
            case = case_name_for(ident)
            reused = None
            reasons = []
            for d in candidate_dirs(run_dir, case, ident):
                if not os.path.isdir(d):
                    break
                good, reason, man = validate_cached(ident, d)
                if good:
                    reused = (d, man, reason)
                    break
                reasons.append("%s: %s" % (os.path.basename(d), reason))
            if reused is not None:
                d, man, reason = reused
                counts["reused"] += 1
                if man["outcome"] == "pass":
                    counts["passed"] += 1
                else:
                    counts["failed"] += 1
                    failures[case] = "cached " + ",".join(man["failures"])
                rec.record(
                    "cache.reused", True, key=ident["key"], dir=d, reason=reason,
                    outcome=man["outcome"],
                )
                continue
            counts["rejected"] += 1
            rec.record(
                "cache.rejected", True, key=ident["key"],
                reasons=reasons or ["no attempt directory yet"],
            )
            log("== %s %s r%d (%s)" % (cname, phase, rep, reasons[0] if reasons else "new"))
            outcome, manifest, _ = run_case(ident, CASES[cname], run_dir, rec)
            counts["executed"] += 1
            tally(counts, outcome)
            if outcome == "fail":
                failures[case] = ",".join(manifest["failures"])

        if native:
            ident = case_identity("native", "native", 0, argv_scope, {"files": SAMPLE_FILES})
            case = case_name_for(ident)
            reused = None
            reasons = []
            for d in candidate_dirs(run_dir, case, ident):
                if not os.path.isdir(d):
                    break
                good, reason, man = validate_cached(ident, d)
                if good:
                    reused = (d, man)
                    break
                reasons.append("%s: %s" % (os.path.basename(d), reason))
            if reused is not None:
                d, man = reused
                counts["reused"] += 1
                counts["passed" if man["outcome"] == "pass" else "failed"] += 1
                rec.record("cache.reused", True, key=ident["key"], dir=d)
            else:
                counts["rejected"] += 1
                rec.record(
                    "cache.rejected", True, key=ident["key"],
                    reasons=reasons or ["no attempt directory yet"],
                )
                outcome, manifest, _ = run_native_case(ident, run_dir, rec)
                counts["executed"] += 1
                tally(counts, outcome)
                if outcome == "fail":
                    failures[case] = ",".join(manifest["failures"])

        fresh = counts["executed"] > 0
        any_failed = bool(
            counts["failed"] or counts["aborted"] or counts["error"]
            or counts["unknown_case"]
        )
        if not fresh and any_failed:
            # A cached-only run whose cached verdicts include a failure is not
            # UNMEASURABLE and must never exit 0. It is a complete, declared set
            # that includes a real failure, so it fails.
            exit_code = 1
            verdict = "cached_only_with_failures"
        elif not fresh:
            exit_code = 0 if args.accept_cached else 2
            verdict = "cached_only"
        elif any_failed:
            exit_code = 1
            verdict = "executed_with_failures"
        else:
            exit_code = 0
            verdict = "executed_all_passed"

        accounted = counts["executed"] + counts["reused"] + counts["unknown_case"]
        summary = {
            "run_id": run_id,
            "verdict": verdict,
            "fresh_acceptance": fresh,
            "exit": exit_code,
            "counts": counts,
            "planned_total": planned_total,
            "planned_cowfs": planned_cowfs,
            "planned_native": planned_native,
            "accounted_total": accounted,
            "accounting_balances": accounted == planned_total,
            "failures": failures,
            "known_failing_cases": {
                k: v for k, v in KNOWN_FAILING.items()
                if any(f.startswith(k + "-") for f in failures)
            },
            "rev": git_rev(),
            "harness_sha256": harness_digest(),
            "daemon_sha256": binary_digest(DAEMON),
            "cli_sha256": binary_digest(COWFS),
        }
        rec.record("run.summary", exit_code == 0, summary=summary)
        with open(os.path.join(run_dir, "summary.json"), "w", encoding="utf-8") as f:
            json.dump(summary, f, indent=2, sort_keys=True)
            f.flush()
            os.fsync(f.fileno())

        log("")
        log("verdict           : %s (exit %d)" % (verdict, exit_code))
        if verdict == "cached_only_with_failures":
            log("")
            log("This run executed ZERO cases and reused %d cached verdict(s), of which"
                % counts["reused"])
            log("%d failed. A cached failure stays a failure: exit 1, not 0."
                % (counts["failed"] + counts["aborted"] + counts["error"]))
        log("fresh acceptance  : %s" % ("yes" if fresh else "NO - zero cases executed"))
        log("executed / reused : %d / %d" % (counts["executed"], counts["reused"]))
        log("accounting        : %d planned, %d accounted, balanced=%s"
            % (planned_total, accounted, accounted == planned_total))
        log("passed / failed   : %d / %d" % (counts["passed"], counts["failed"]))
        if failures:
            log("failing cases:")
            for k, v in sorted(failures.items()):
                why = next((w for n, w in KNOWN_FAILING.items() if n in k), "")
                log("  %-38s %s%s" % (k, v, ("  <- " + why) if why else ""))
        if not fresh:
            log("")
            log("This run executed ZERO cases. It reused %d verified cached verdict(s)." % counts["reused"])
            log("That is not a fresh acceptance. Re-run with a different --run-id to measure.")
        return exit_code
    finally:
        try:
            for name in os.listdir(SOCK_DIR):
                victim = os.path.join(SOCK_DIR, name)
                if os.path.isfile(victim) and not os.path.islink(victim):
                    os.unlink(victim)
            left = sorted(os.listdir(SOCK_DIR))
            rec.record("sockdir.leaked", not left, dir=SOCK_DIR, entries=left)
            if not left:
                os.rmdir(SOCK_DIR)
                rec.record("sockdir.removed", True, dir=SOCK_DIR)
        except OSError as e:
            log("sockdir cleanup: %s" % e)
        rec.close()


if __name__ == "__main__":
    sys.exit(main())