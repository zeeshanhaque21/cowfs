#!/usr/bin/env python3
"""Full-stack daemon crash recovery acceptance for cowfs (issue #88).

Drives the real `cowfs-daemon --backend core`, the real `cowfs` control CLI and a
real macOS NFS loopback mount over a private store, kills the daemon with SIGKILL at
declared public boundaries, then reopens the same store with a fresh daemon and
compares every promised byte with a source hash taken before the crash.

Durability receipts
-------------------
An NFS WRITE ack is NOT durable. `Vfs::write` (`cowfs_core::io::op_write`) writes
into the node's in-memory state and returns; nothing is fsynced. The control API's
mutating calls ack `Ack::Applied` (`cowfs_meta::db::Ack`, default, and nothing in
the tree sets `Ack::Durable`), so a `snapshot create` that returned 0 is applied,
not durable. Only these are durability receipts:

  * `os.fsync(fd)` on a file in the mount, which the NFS adapter maps to
    `fsync(ino, false)` (documented in `crates/cowfs-nfs/src/lib.rs`), reaching
    `CowfsNfs::fsync` -> `cowfs_core::op_fsync` -> `flush_snapshot` (blocks into
    the pack, then the metadata commit) and `meta.sync()`, whose `before_sync`
    hook is `cowfs_core::store_sync_hook` -> `Store::sync()` -> `fsync(pack file)`
    then `watermark.advance`.
  * `cowfs shutdown`, which is `Core::close`.

So the model is two levels. Level B ("durable"): fsync returned, or shutdown
returned. Bytes and names promised at level B must survive the kill; losing them
is a hard failure. Level A ("applied"): the write returned, or the control call
returned, with no fsync. Losing level A is permitted, because the design accepts
bounded loss of recent writes; the harness records what actually survived.

SIGKILL scope
-------------
SIGKILL kills a process, not the kernel. Bytes the daemon already `write(2)`ed
into the store's packs are in the host page cache and survive any process death.
So this harness samples process-crash recovery, not power loss: it can prove a
level-B receipt survives a killed daemon, and it can show level-A bytes lost
while they were still only in the daemon's memory. It cannot, and does not claim
to, show power-loss loss of un-fsynced pack bytes.

Usage
-----
    scripts/verify-daemon-crash.py --stage sample
    scripts/verify-daemon-crash.py --stage matrix --reps 2
    scripts/verify-daemon-crash.py --stage native

Everything lives under `bench/out/crash88/` (gitignored). No shared daemon, store,
mount, socket, lease or runner is touched: every signal goes to a pid this process
spawned, and only after its command line is confirmed to carry this run's own
store and socket.
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
DAEMON = os.path.join(REPO, "target/debug/cowfs-daemon")
COWFS = os.path.join(REPO, "target/debug/cowfs")
OUT_ROOT = os.path.join(REPO, "bench/out/crash88")

# A Unix socket path must fit sun_path (104 bytes on macOS). The worktree path is
# far longer, so the socket is the one thing that lives outside the run directory.
# Its parent must also be a private 0700 directory, which /private/tmp is not, so
# each run makes its own short-lived one there and removes it at teardown.
SOCK_ROOT = "/private/tmp"
SOCK_DIR = None


def sock_path(name):
    return os.path.join(SOCK_DIR, name + ".sock")

MAX_OP_BYTES = 64 * 1024
MAX_OPS_PER_CASE = 16
MAX_CONCURRENT_DAEMONS = 2
CASE_DEADLINE_SECS = 180
NO_PROGRESS_SECS = 90
DAEMON_READY_SECS = 60
CLI_TIMEOUT_SECS = 60

# Only the gc case writes more than MAX_OP_BYTES, and only enough to pass the
# collector's own default floor (cowfs_gc::Options::min_dead_bytes = 8 MiB).
GC_CASE_BYTES = 12 * 1024 * 1024

SAMPLE_FILES = 4
SAMPLE_BYTES = 4096


def log(msg):
    sys.stderr.write("%s\n" % msg)
    sys.stderr.flush()


class Budget(Exception):
    """A declared budget was exceeded. Stops the run instead of growing it."""


class ForeignProcess(RuntimeError):
    """The pid's command line does not carry this fixture's exact paths."""


class Proc:
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
# evidence: append, flush, fsync per record, resumable
# --------------------------------------------------------------------------


class Recorder:
    def __init__(self, out_dir):
        os.makedirs(out_dir, exist_ok=True)
        self.path = os.path.join(out_dir, "records.jsonl")
        self.out_dir = out_dir
        self.prior = self._load_prior()
        self.step = 0
        for r in self.prior:
            self.step = max(self.step, int(r.get("step", 0)))
        self._f = open(self.path, "a", encoding="utf-8")

    def _load_prior(self):
        """A partial run leaves usable records: read them back so a resume can skip."""
        out = []
        if not os.path.exists(self.path):
            return out
        with open(self.path, "r", encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    out.append(json.loads(line))
                except ValueError:
                    # A crash mid-write can leave one torn last line. Keep the rest.
                    continue
        return out

    def done_cases(self):
        """Resume keys of cases whose terminal record is present.

        Keyed on the explicit `key` field rather than the case name, because the
        case name and the resume key are different strings and comparing them
        silently never matches.
        """
        names = set()
        for r in self.prior:
            if r.get("terminal") and r.get("key"):
                names.add(r["key"])
        return names

    def record(self, name, ok, **fields):
        self.step += 1
        rec = {"step": self.step, "name": name, "ok": bool(ok), "t": time.time()}
        rec.update(fields)
        self._f.write(json.dumps(rec, sort_keys=True) + "\n")
        self._f.flush()
        os.fsync(self._f.fileno())
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
# process helpers: verify before any signal, never a broad pattern
# --------------------------------------------------------------------------


def pid_cmdline(pid):
    return Proc(["/bin/ps", "-o", "command=", "-p", str(pid)], timeout=15).stdout.strip()


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


def kill_verified(child, rec, sock, store, sig=signal.SIGKILL):
    """Signal only a pid that is our own child AND carries this fixture's paths.

    Returns the record. Raises ForeignProcess (recorded as a refusal, ok=True)
    when the pid is not provably ours; the caller must then not signal anything.
    """
    pid = child.pid
    if child.poll() is not None:
        return rec.record("kill.already_dead", True, pid=pid, sig=sig.name)
    cmd = pid_cmdline(pid)
    if sock not in cmd or store not in cmd or os.path.basename(DAEMON) not in cmd:
        rec.record(
            "kill.refused_foreign_pid",
            True,
            pid=pid,
            reason="command line is not this fixture; not signalling",
            cmdline=cmd,
        )
        raise ForeignProcess("refusing to signal pid %d: %r" % (pid, cmd))
    rec.record(
        "kill.verified_target",
        True,
        pid=pid,
        sig=sig.name,
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


def is_our_mount(mount):
    """True only if the mount table lists exactly this path as a mount point."""
    resolved = os.path.realpath(mount)
    table = Proc(["/sbin/mount"], timeout=30).stdout
    return any((" on %s " % resolved) in line for line in table.splitlines())


def unmount_private(mount, rec, note):
    """Unmount only a path that is verifiably one of our own mounts."""
    if not os.path.exists(mount) and not is_our_mount(mount):
        rec.record("unmount.absent", True, mount=mount)
        return
    if not is_our_mount(mount):
        rec.record(
            "unmount.refused_not_our_mount",
            True,
            mount=mount,
            reason="not in the mount table; leaving it alone",
        )
        return
    # The server is dead, so the client mount is stale: -f is required and safe here.
    p = Proc(["/sbin/umount", "-f", mount], timeout=60)
    rec.record(
        "unmount." + note,
        p.returncode == 0 and not is_our_mount(mount),
        mount=mount,
        rc=p.returncode,
        stderr=p.stderr.strip()[:400],
    )


# --------------------------------------------------------------------------
# the daemon under test
# --------------------------------------------------------------------------


class PrivateDaemon:
    def __init__(self, store, mount, socket, log_path):
        self.store = store
        self.mount = mount
        self.socket = socket
        self.log_path = log_path
        self.log = None
        self.child = None

    def start(self, rec):
        os.makedirs(self.store, exist_ok=True)
        os.makedirs(self.mount, exist_ok=True)
        os.makedirs(self.store, mode=0o700, exist_ok=True)
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
            if is_our_mount(self.mount) and self.answer():
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

    def shutdown(self, rec):
        p = self.cli(["shutdown"])
        rec.record(
            "cli.shutdown",
            p.returncode == 0,
            rc=p.returncode,
            stdout=p.stdout.strip()[:300],
            stderr=p.stderr.strip()[:300],
        )
        if not child_exited(self.child, timeout=90):
            rec.record("shutdown.timeout", False, pid=self.child.pid)
            raise RuntimeError("daemon did not exit after shutdown")
        rec.record("shutdown.exited", True, pid=self.child.pid)
        self.close_log()
        rec.record(
            "shutdown.socket_gone",
            not os.path.exists(self.socket),
            socket=self.socket,
        )

    def close_log(self):
        if self.log:
            try:
                self.log.close()
            except OSError:
                pass
            self.log = None


# --------------------------------------------------------------------------
# the operation model, and receipts for it
# --------------------------------------------------------------------------


def deterministic_body(n, seed):
    """Reproducible bytes, so the expected set can be rebuilt without the mount."""
    out = bytearray()
    h = hashlib.blake2b(digest_size=32, key=seed.to_bytes(8, "little"))
    while len(out) < n:
        h = hashlib.blake2b(digest_size=64, key=seed.to_bytes(8, "little"))
        out.extend(h.digest())
        seed += 1
    return bytes(out[:n])


def sha256_bytes(b):
    return hashlib.sha256(b).hexdigest()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(1 << 20)
            if not b:
                break
            h.update(b)
    return h.hexdigest()


class Receipts:
    """What was promised, and at which level.

    kind="durable" means an fsync (or shutdown) returned: a hard requirement.
    kind="applied" means only a write or a control call returned: loss is
    permitted by the design and is recorded, never failed.
    """

    def __init__(self):
        self.items = []
        self.names = []

    def promise_snapshot(self, name, level, how):
        """Record that a snapshot name is promised to exist after the crash."""
        self.names.append({"name": name, "level": level, "boundary": how, "t": time.time()})

    def durable(self, path, sha, size, how):
        self.items.append(
            {
                "path": path,
                "sha256": sha,
                "size": size,
                "kind": "durable",
                "boundary": how,
                "t": time.time(),
            }
        )

    def applied(self, path, sha, size, how):
        self.items.append(
            {
                "path": path,
                "sha256": sha,
                "size": size,
                "kind": "applied",
                "boundary": how,
                "t": time.time(),
            }
        )

    def durable_items(self):
        return [i for i in self.items if i["kind"] == "durable"]

    def applied_items(self):
        return [i for i in self.items if i["kind"] == "applied"]

    def repath(self, old, new):
        """Re-file a receipt under a new name after a rename.

        Matched by path, never by position: after a rename the receipt that named
        the old name is the one that must follow the bytes, and picking "the last
        one" silently re-files an unrelated receipt instead.
        """
        for i in self.items:
            if i["path"] == old:
                i["path"] = new
                return True
        return False

    def downgrade(self, path, why):
        """Reclassify a receipt from durable to applied.

        Used where the harness issued a sync that measurement showed does not reach
        the server on this transport, so it is not a durability promise the product
        ever made. Recorded with the reason instead of being quietly dropped.
        """
        for i in self.items:
            if i["path"] == path and i["kind"] == "durable":
                i["kind"] = "applied"
                i["boundary"] = "not_durable:" + why
                return True
        return False


def write_file(mount, snap, name, data, do_fsync, rec, receipts, how="nfs_commit"):
    """Write through the real mount. do_fsync makes the write a durability receipt."""
    if len(data) > MAX_OP_BYTES:
        raise Budget("op of %d bytes exceeds MAX_OP_BYTES" % len(data))
    d = os.path.join(mount, snap)
    os.makedirs(d, exist_ok=True)
    p = os.path.join(d, name)
    with open(p, "wb") as f:
        f.write(data)
        f.flush()
        if do_fsync:
            # NFS COMMIT: the adapter maps this to fsync(ino, false).
            os.fsync(f.fileno())
    sha = sha256_bytes(data)
    if do_fsync:
        receipts.durable("%s/%s" % (snap, name), sha, len(data), how)
        rec.record(
            "receipt.durable",
            True,
            path="%s/%s" % (snap, name),
            sha256=sha,
            size=len(data),
            boundary=how,
        )
    else:
        receipts.applied("%s/%s" % (snap, name), sha, len(data), "nfs_write_only")
        rec.record(
            "receipt.applied",
            True,
            path="%s/%s" % (snap, name),
            sha256=sha,
            size=len(data),
            boundary="nfs_write_only",
        )
    return p


def fsync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


# --------------------------------------------------------------------------
# verification after a fresh reopen
# --------------------------------------------------------------------------


def verify_readback(fresh, receipts, rec, label):
    """Every durable receipt must be present byte for byte. Applied ones are recorded."""
    dur = receipts.durable_items()
    app = receipts.applied_items()
    ok = True
    for item in dur:
        p = os.path.join(fresh.mount, item["path"])
        if not os.path.exists(p):
            # Record what is actually there, so a loss can be told apart from a
            # rename that went to a different name.
            parent = os.path.dirname(p)
            try:
                siblings = sorted(os.listdir(parent))
            except OSError as e:
                siblings = ["<listdir failed: %s>" % e]
            rec.record(
                "readback.durable_missing",
                False,
                label=label,
                path=item["path"],
                boundary=item["boundary"],
                parent_entries=siblings[:40],
            )
            ok = False
            continue
        got = sha256_file(p)
        good = got == item["sha256"] and os.path.getsize(p) == item["size"]
        rec.record(
            "readback.durable_match" if good else "readback.durable_mismatch",
            good,
            label=label,
            path=item["path"],
            want=item["sha256"],
            got=got,
            size=os.path.getsize(p),
        )
        ok = ok and good
    for item in app:
        p = os.path.join(fresh.mount, item["path"])
        present = os.path.exists(p)
        rec.record(
            "readback.applied_survived" if present else "readback.applied_lost_permitted",
            True,
            label=label,
            path=item["path"],
            present=present,
            note="level A loss is permitted by docs/design.md (bounded loss of recent writes)",
        )
    rec.record(
        "readback.summary",
        ok,
        label=label,
        durable_total=len(dur),
        durable_matched=sum(
            1
            for i in dur
            if os.path.exists(os.path.join(fresh.mount, i["path"]))
            and sha256_file(os.path.join(fresh.mount, i["path"])) == i["sha256"]
        ),
        applied_total=len(app),
        applied_present=sum(
            1 for i in app if os.path.exists(os.path.join(fresh.mount, i["path"]))
        ),
    )
    return ok


def verify_no_torn_tree(fresh, rec, label):
    """Every snapshot must list and every directory must be readable without error."""
    snaps = fresh.cli_json(["snapshot", "list"])
    names = [s.get("name") for s in snaps.get("snapshots", [])]
    rec.record("tree.snapshot_list", True, label=label, snapshots=names)
    ok = True
    for name in names:
        d = os.path.join(fresh.mount, name)
        if not os.path.isdir(d):
            rec.record("tree.snapshot_not_a_dir", False, label=label, snapshot=name)
            ok = False
            continue
        try:
            entries = sorted(os.listdir(d))
        except OSError as e:
            rec.record("tree.listdir_failed", False, label=label, snapshot=name, err=str(e))
            ok = False
            continue
        rec.record(
            "tree.listing_ok",
            True,
            label=label,
            snapshot=name,
            entries=entries[:60],
        )
    return ok


def verify_fsck(fresh, rec, label):
    """fsck must report zero problems on the reopened store."""
    out = fresh.cli_json(["fsck"], timeout=120)
    problems = out.get("problems") or []
    ok = len(problems) == 0
    rec.record(
        "fsck.clean" if ok else "fsck.problems",
        ok,
        label=label,
        problems=problems[:20],
        n_problems=len(problems),
        blocks=out.get("blocks"),
        snapshots=out.get("snapshots"),
    )
    return ok


def verify_snapshot_names(fresh, receipts, rec, label):
    """A snapshot name promised durably must resolve on the fresh daemon.

    A name promised only at `Ack::Applied` is recorded either way, because the
    control plane's mutating calls do not promise durability by default.
    """
    listed = {s.get("name") for s in fresh.cli_json(["snapshot", "list"]).get("snapshots", [])}
    ok = True
    required = set()
    for n in receipts.names:
        present = n["name"] in listed
        if n["level"] == "durable":
            required.add(n["name"])
            rec.record(
                "reopen.snapshot_present" if present else "reopen.snapshot_missing_required",
                present,
                label=label,
                snapshot=n["name"],
                boundary=n["boundary"],
            )
            ok = ok and present
        else:
            rec.record(
                "reopen.snapshot_applied_present" if present else "reopen.snapshot_applied_lost_permitted",
                True,
                label=label,
                snapshot=n["name"],
                boundary=n["boundary"],
            )
    rec.record("reopen.snapshot_set", True, label=label, listed=sorted(x for x in listed if x))
    return ok


# --------------------------------------------------------------------------
# one crash case
# --------------------------------------------------------------------------


def run_case(name, ops, rec, run_dir, phase):
    """ops(d, receipts, rec) performs the work; then the daemon is SIGKILLed and
    the store is reopened by a fresh daemon and verified."""
    case_dir = os.path.join(run_dir, "cases", name)
    store = os.path.join(case_dir, "store")
    mount1 = os.path.join(case_dir, "mnt1")
    sock1 = sock_path(name)
    sock2 = sock_path(name + "-r2")
    mount2 = os.path.join(case_dir, "mnt2")
    log1 = os.path.join(case_dir, "daemon1.log")
    log2 = os.path.join(case_dir, "daemon2.log")

    if os.path.exists(sock1):
        os.unlink(sock1)
    if os.path.exists(sock2):
        os.unlink(sock2)

    d1 = PrivateDaemon(store, mount1, sock1, log1)
    d2 = None
    started = time.time()
    receipts = Receipts()
    case_ok = False
    try:
        d1.start(rec)
        ops(d1, receipts, rec, case_dir)
        if time.time() - started > CASE_DEADLINE_SECS:
            raise Budget("case exceeded CASE_DEADLINE_SECS")

        # How long the last receipt sat before the kill. This is the observable part
        # of the crash window: cowfs's background flusher runs every 500ms by default,
        # so a delay under that is a genuinely un-flushed crash, and a delay over it
        # is a crash after the flusher had a chance. Recorded either way.
        if receipts.items:
            delay = time.time() - receipts.items[-1]["t"]
            rec.record(
                "crash.window",
                True,
                case=name,
                secs_since_last_receipt=round(delay, 3),
                last_kind=receipts.items[-1]["kind"],
                note="background flusher default is 500ms; below it the data was still in daemon memory",
            )

        # The crash. Verify the target is our own child carrying our paths first.
        kill_verified(d1.child, rec, sock1, store, sig=signal.SIGKILL)
        d1.close_log()
        unmount_private(mount1, rec, "after_kill")

        # Reopen the same store with a fresh daemon and a fresh mount.
        d2 = PrivateDaemon(store, mount2, sock2, log2)
        d2.start(rec)

        ok = True
        ok &= verify_readback(d2, receipts, rec, name)
        ok &= verify_snapshot_names(d2, receipts, rec, name)
        ok &= verify_no_torn_tree(d2, rec, name)
        ok &= verify_fsck(d2, rec, name)

        case_ok = bool(ok)
        rec.record(
            "case.result",
            case_ok,
            case=name,
            phase=phase,
            durable=len(receipts.durable_items()),
            applied=len(receipts.applied_items()),
            secs=round(time.time() - started, 1),
        )
    except (ForeignProcess, Budget) as e:
        rec.record("case.aborted", False, case=name, phase=phase, err=str(e))
        case_ok = False
    except Exception as e:  # noqa: BLE001 - a case failure must be recorded, not raised
        rec.record(
            "case.error",
            False,
            case=name,
            phase=phase,
            err="%s: %s" % (type(e).__name__, e),
        )
        case_ok = False
    finally:
        for d, mnt in ((d1, mount1), (d2, mount2)):
            if d is None:
                continue
            try:
                if d.child is not None and d.child.poll() is None:
                    kill_verified(d.child, rec, d.socket, d.store, sig=signal.SIGKILL)
            except Exception:
                pass
            d.close_log()
            unmount_private(mnt, rec, "teardown")
            for s in (d.socket,):
                # The control server leaves a lock file beside its socket. Both are
                # inside this run's own private socket dir, so both are ours.
                for victim in (s, s + ".lock"):
                    if os.path.exists(victim):
                        try:
                            os.unlink(victim)
                            rec.record("teardown.socket_removed", True, socket=victim)
                        except OSError:
                            pass
    return case_ok


# --------------------------------------------------------------------------
# native (APFS) baseline
# --------------------------------------------------------------------------


def native_ops(root, rec, receipts):
    """The same operation model on the host filesystem, for comparison.

    The cowfs daemon has no APFS analogue, so the process that is SIGKILLed here is
    the *writer*, not a server. That difference is stated, not hidden: on APFS a
    successful write() is already in the host page cache, so level A usually
    survives, whereas cowfs level A can be lost while it is still in daemon memory.
    """
    d = os.path.join(root, "snap")
    os.makedirs(d, exist_ok=True)
    n = 0
    for i in range(SAMPLE_FILES):
        data = deterministic_body(SAMPLE_BYTES, 9000 + i)
        p = os.path.join(d, "n%02d.bin" % i)
        with open(p, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())
        rel = "snap/n%02d.bin" % i
        receipts.durable(rel, sha256_bytes(data), len(data), "apfs_fsync")
        if rec is not None:
            rec.record(
                "native.durable", True, path=rel, sha256=receipts.items[-1]["sha256"]
            )
        n += 1
    fsync_dir(d)
    if rec is not None:
        rec.record("native.ops", True, files=n, root=root)


def run_native_case(name, rec, run_dir):
    """Run the native (APFS) control: a real writer kill and a clean restart.

    The cowfs daemon has no APFS analogue, so the process killed here is the
    *writer*, not a server. That difference is stated, not hidden: on APFS a
    returned write() is already in the host page cache, so an un-fsynced file
    usually survives, while a cowfs level-A write can be lost while it is still
    only in daemon memory. The comparison is the point.
    """
    results = {}
    for mode, label in (("kill", "native-kill"), ("clean", "native-restart-control")):
        case_dir = os.path.join(run_dir, "cases", name + "-" + mode)
        root = os.path.join(case_dir, "apfs")
        expect = os.path.join(case_dir, "expected")
        shutil.rmtree(root, ignore_errors=True)
        shutil.rmtree(expect, ignore_errors=True)
        os.makedirs(root, exist_ok=True)
        os.makedirs(expect, exist_ok=True)

        argv = [
            sys.executable,
            os.path.abspath(__file__),
            "--internal-native-writer",
            root,
            expect,
            mode,
        ]
        logf = open(os.path.join(case_dir, "writer.log"), "ab")
        child = subprocess.Popen(
            argv, stdout=logf, stderr=logf, stdin=subprocess.DEVNULL, start_new_session=True
        )
        started = time.time()
        try:
            exited = child_exited(child, timeout=120)
            rc = child.returncode
            expected_rc = -signal.SIGKILL if mode == "kill" else 0
            rec.record(
                "native.writer_exited",
                exited and rc == expected_rc,
                mode=mode,
                rc=rc,
                expected_rc=expected_rc,
                secs=round(time.time() - started, 1),
            )
            receipts = Receipts()
            receipts_path = os.path.join(expect, "receipts.json")
            if not os.path.exists(receipts_path):
                rec.record("native.receipts_missing", False, mode=mode)
                results[label] = False
                continue
            with open(receipts_path, "r", encoding="utf-8") as f:
                receipts.items = json.load(f)
            ok = True
            for item in receipts.durable_items():
                p = os.path.join(root, item["path"])
                got = sha256_file(p) if os.path.exists(p) else None
                good = got == item["sha256"]
                rec.record(
                    "native.durable_match" if good else "native.durable_lost",
                    good,
                    mode=mode,
                    path=item["path"],
                    want=item["sha256"],
                    got=got,
                )
                ok = ok and good
            for item in receipts.applied_items():
                p = os.path.join(root, item["path"])
                present = os.path.exists(p)
                rec.record(
                    "native.applied_survived" if present else "native.applied_lost_permitted",
                    True,
                    mode=mode,
                    path=item["path"],
                    present=present,
                )
            results[label] = bool(ok)
            rec.record(
                "case.result",
                bool(ok),
                case=name + "-" + mode,
                phase="native",
                mode=mode,
                durable=len(receipts.durable_items()),
                applied=len(receipts.applied_items()),
                secs=round(time.time() - started, 1),
            )
        except Exception as e:  # noqa: BLE001
            rec.record("case.error", False, case=name + "-" + mode, mode=mode, err=str(e))
            results[label] = False
        finally:
            logf.close()
    return all(results.values())


def internal_native_writer(root, expect, mode):
    """Child half of the native baseline.

    mode="kill": write one fsynced file and one un-fsynced file, record both
    receipts durably beside the data, then SIGKILL itself. The parent verifies the
    APFS outcome, which is the control the cowfs results are read against.
    mode="clean": write, fsync, record, exit 0. The do-nothing restart control.
    """
    os.makedirs(expect, exist_ok=True)
    os.makedirs(root, exist_ok=True)
    d = os.path.join(root, "snap")
    os.makedirs(d, exist_ok=True)
    receipts = Receipts()
    rpath = os.path.join(expect, "receipts.json")

    def flush_receipts():
        with open(rpath, "w", encoding="utf-8") as f:
            json.dump(receipts.items, f, indent=2)
            f.flush()
            os.fsync(f.fileno())

    data = deterministic_body(SAMPLE_BYTES, 9000)
    with open(os.path.join(d, "durable.bin"), "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    receipts.durable("snap/durable.bin", sha256_bytes(data), len(data), "apfs_fsync")
    flush_receipts()

    data2 = deterministic_body(SAMPLE_BYTES, 9001)
    with open(os.path.join(d, "recent.bin"), "wb") as f:
        f.write(data2)
        f.flush()
    receipts.applied("snap/recent.bin", sha256_bytes(data2), len(data2), "apfs_write_only")
    flush_receipts()

    if mode == "kill":
        # A real crash of the writer process, on purpose.
        sys.stderr.write("native writer: SIGKILL self\n")
        sys.stderr.flush()
        os.kill(os.getpid(), signal.SIGKILL)
        os._exit(70)  # unreachable
    fsync_dir(d)
    return 0


# --------------------------------------------------------------------------
# the cases
# --------------------------------------------------------------------------


def ctl_snapshot_create(d, name, frm=None):
    args = ["snapshot", "create", name]
    if frm:
        args += ["--from", frm]
    return d.cli_json(args)


def case_write_fsync(d, receipts, rec, case_dir):
    """Level-B: write known bytes, fsync (a durability receipt), then crash."""
    ctl_snapshot_create(d, "live")
    n = 0
    for i in range(SAMPLE_FILES):
        data = deterministic_body(SAMPLE_BYTES, 100 + i)
        write_file(d.mount, "live", "d%02d.bin" % i, data, True, rec, receipts)
        n += 1
    fsync_dir(os.path.join(d.mount, "live"))
    rec.record("case.write_fsync", True, files=n)


def case_write_nofsync(d, receipts, rec, case_dir):
    """Level-A: write with no fsync. Loss is permitted; survival is recorded."""
    ctl_snapshot_create(d, "live")
    for i in range(SAMPLE_FILES):
        data = deterministic_body(SAMPLE_BYTES, 200 + i)
        write_file(d.mount, "live", "a%02d.bin" % i, data, False, rec, receipts)
    rec.record("case.write_nofsync", True, files=SAMPLE_FILES)


def case_mixed(d, receipts, rec, case_dir):
    """A durable file and an applied file in the same snapshot: only the first is required."""
    ctl_snapshot_create(d, "live")
    data = deterministic_body(SAMPLE_BYTES, 300)
    write_file(d.mount, "live", "durable.bin", data, True, rec, receipts)
    data2 = deterministic_body(SAMPLE_BYTES, 301)
    write_file(d.mount, "live", "recent.bin", data2, False, rec, receipts)
    fsync_dir(os.path.join(d.mount, "live"))
    rec.record("case.mixed", True)


def case_rename(d, receipts, rec, case_dir):
    """Rename a durable file, fsync the parent directory, then crash.

    Measured, not assumed: on this transport the parent fsync does not reach the
    server, so the rename is only ever acked, never made durable, and losing it is
    permitted. This case records which of the two outcomes occurred instead of
    failing on either one.
    """
    ctl_snapshot_create(d, "live")
    data = deterministic_body(SAMPLE_BYTES, 400)
    write_file(d.mount, "live", "orig.bin", data, True, rec, receipts)
    src = os.path.join(d.mount, "live", "orig.bin")
    dst = os.path.join(d.mount, "live", "moved.bin")
    os.rename(src, dst)
    fsync_dir(os.path.join(d.mount, "live"))
    receipts.repath("live/orig.bin", "live/moved.bin")
    receipts.downgrade(
        "live/moved.bin",
        "dir_fsync_produced_no_commit_on_this_client",
    )
    rec.record(
        "case.rename",
        True,
        boundary="dir_fsync_only",
        level="applied",
        case=os.path.basename(case_dir),
        **{"from": "live/orig.bin", "to": "live/moved.bin"},
    )


def case_rename_filefsync(d, receipts, rec, case_dir):
    """Rename, then fsync the renamed file's own read-only descriptor.

    Measured: also does not reach the server, so the rename stays level A.
    """
    ctl_snapshot_create(d, "live")
    data = deterministic_body(SAMPLE_BYTES, 401)
    write_file(d.mount, "live", "orig.bin", data, True, rec, receipts)
    src = os.path.join(d.mount, "live", "orig.bin")
    dst = os.path.join(d.mount, "live", "moved.bin")
    os.rename(src, dst)
    fd = os.open(dst, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    fsync_dir(os.path.join(d.mount, "live"))
    receipts.repath("live/orig.bin", "live/moved.bin")
    receipts.downgrade(
        "live/moved.bin",
        "readonly_fd_fsync_produced_no_commit_on_this_client",
    )
    rec.record(
        "case.rename",
        True,
        boundary="readonly_fd_fsync_after_rename",
        level="applied",
        case=os.path.basename(case_dir),
        **{"from": "live/orig.bin", "to": "live/moved.bin"},
    )


def case_snapshot_fork(d, receipts, rec, case_dir):
    """Fork a snapshot, force a real COMMIT, then crash. Both trees must be readable.

    `snapshot create` acks `Ack::Applied`, so on its own it promises nothing about
    durability. A marker file written and fsynced inside the fork afterwards is what
    turns the fork into a real durability receipt, because that COMMIT drains the
    queue and commits the fork's name and refs.
    """
    ctl_snapshot_create(d, "base")
    receipts.promise_snapshot("base", "applied", "ctl_snapshot_create")
    for i in range(SAMPLE_FILES):
        data = deterministic_body(SAMPLE_BYTES, 500 + i)
        write_file(d.mount, "base", "f%02d.bin" % i, data, True, rec, receipts)
    fsync_dir(os.path.join(d.mount, "base"))
    ctl_snapshot_create(d, "fork", frm="base")
    receipts.promise_snapshot("fork", "applied", "ctl_snapshot_create")
    marker = deterministic_body(SAMPLE_BYTES, 550)
    write_file(d.mount, "fork", "marker.bin", marker, True, rec, receipts)
    receipts.promise_snapshot("fork", "durable", "write_fsync_inside_fork")
    rec.record(
        "case.snapshot_fork",
        True,
        base="base",
        fork="fork",
        note="marker fsync is what commits the fork name; the create call alone acks Applied",
    )


def case_snapshot_remove(d, receipts, rec, case_dir):
    """Remove a fork, then crash. The base's durable bytes must survive.

    `snapshot rm` acks `Ack::Applied` too, so whether the removal survived is a
    recorded observation, not a requirement. The base's acknowledged bytes are the
    requirement.
    """
    ctl_snapshot_create(d, "base")
    receipts.promise_snapshot("base", "applied", "ctl_snapshot_create")
    for i in range(SAMPLE_FILES):
        data = deterministic_body(SAMPLE_BYTES, 600 + i)
        write_file(d.mount, "base", "r%02d.bin" % i, data, True, rec, receipts)
    fsync_dir(os.path.join(d.mount, "base"))
    receipts.promise_snapshot("base", "durable", "file_fsyncs")
    ctl_snapshot_create(d, "doomed", frm="base")
    receipts.promise_snapshot("doomed", "applied", "ctl_snapshot_create")
    rm = d.cli(["snapshot", "rm", "doomed"])
    rec.record(
        "case.snapshot_rm",
        rm.returncode == 0,
        rc=rm.returncode,
        stderr=rm.stderr.strip()[:200],
    )
    # The removal is applied, not durable: record it as level A.
    receipts.names = [n for n in receipts.names if n["name"] != "doomed"]


def case_mmap(d, receipts, rec, case_dir):
    """mmap a durable file, flush and fsync it, then crash."""
    ctl_snapshot_create(d, "live")
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
    receipts.durable("live/mapped.bin", sha256_bytes(data), len(data), "mmap_msync+fsync")
    rec.record(
        "receipt.durable", True, path="live/mapped.bin", sha256=receipts.items[-1]["sha256"],
        size=len(data), boundary="mmap_msync+fsync",
    )
    rec.record("case.mmap", True)


def case_gc_crash(d, receipts, rec, case_dir):
    """A bounded GC cycle, crash, then require survivors by hash and a clean fsck.

    The dead bytes are made by writing into a snapshot of their own and then
    removing it, so nothing still references them; a fork of `live` would share
    every block and leave nothing dead to collect.

    A small fixture lives in the open pack, which the collector cannot unlink, so
    freed bytes are expected to be 0 while candidate bytes are not. What this case
    establishes is that a collect cycle plus a crash does not damage acknowledged
    survivors, not that space came back. Real reclamation is covered by
    docs/verification/gc-daemon-e2e.md, which seeds sealed packs.
    """
    ctl_snapshot_create(d, "live")
    survivor = deterministic_body(SAMPLE_BYTES, 799)
    write_file(d.mount, "live", "survivor.bin", survivor, True, rec, receipts)

    # Garbage in a snapshot of its own, then remove it: these blocks become dead.
    ctl_snapshot_create(d, "garbage")
    data = deterministic_body(GC_CASE_BYTES, 800)
    p = os.path.join(d.mount, "garbage", "big.bin")
    with open(p, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    sha = sha256_bytes(data)
    receipts.applied("garbage/big.bin", sha, len(data), "nfs_commit_then_snapshot_rm")
    rec.record(
        "case.gc_garbage_written",
        True,
        case=os.path.basename(case_dir),
        sha256=sha,
        size=len(data),
        note="this snapshot is about to be removed, so its bytes are the dead set",
    )
    fsync_dir(os.path.join(d.mount, "garbage"))
    rm = d.cli(["snapshot", "rm", "garbage"])
    rec.record(
        "case.gc_rm_garbage",
        rm.returncode == 0,
        case=os.path.basename(case_dir),
        rc=rm.returncode,
    )
    # live must still be intact, before gc and after it.
    got = sha256_file(os.path.join(d.mount, "live", "survivor.bin"))
    rec.record(
        "case.gc_survivor_before",
        got == receipts.durable_items()[-1]["sha256"],
        case=os.path.basename(case_dir),
        sha256=got,
    )
    out = d.cli_json(["gc"], timeout=180)
    rec.record(
        "case.gc_report",
        True,
        case=os.path.basename(case_dir),
        dry_run=out.get("dry_run"),
        candidate_blocks=out.get("candidate_blocks"),
        candidate_bytes=out.get("candidate_bytes"),
        freed_blocks=out.get("freed_blocks"),
        freed_bytes=out.get("freed_bytes"),
        gross_removed_bytes=out.get("gross_removed_bytes"),
        note=(
            "a small fixture lives in the open pack, which cannot be unlinked, so freed "
            "bytes are expected to be 0 here; survivor integrity, not space, is under test"
        ),
    )
    got = sha256_file(os.path.join(d.mount, "live", "survivor.bin"))
    rec.record(
        "case.gc_survivor_after",
        got == receipts.durable_items()[-1]["sha256"],
        case=os.path.basename(case_dir),
        sha256=got,
    )


def case_kill_control(d, receipts, rec, case_dir):
    """Do-nothing control: an idle daemon is killed; the store must reopen clean."""
    ctl_snapshot_create(d, "live")
    data = deterministic_body(SAMPLE_BYTES, 900)
    write_file(d.mount, "live", "c.bin", data, True, rec, receipts)
    fsync_dir(os.path.join(d.mount, "live"))
    rec.record("case.kill_control", True)


def case_write_race(d, receipts, rec, case_dir):
    """Kill as close to an un-fsynced write as the harness can get.

    No fsync and no directory fsync, and the case returns immediately, so the kill
    lands inside the background flusher's 500ms window when the machine allows it.
    Level-A loss here is permitted and is recorded, never failed; the point is to
    measure whether the boundary is observable rather than to assert it.
    """
    ctl_snapshot_create(d, "live")
    for i in range(SAMPLE_FILES):
        data = deterministic_body(SAMPLE_BYTES, 1000 + i)
        write_file(d.mount, "live", "r%02d.bin" % i, data, False, rec, receipts)
    rec.record("case.write_race", True, files=SAMPLE_FILES)


def case_rename_writefsync(d, receipts, rec, case_dir):
    """Rename, then force a real COMMIT by dirtying and fsyncing a second file.

    A file rename is queued (`Op::Rename` in `cowfs_core::ns`), so any fsync that
    actually reaches the server drains the queue and commits the rename. The two
    cases above show that fsync on a directory or on a read-only descriptor does
    not get there on this transport; this case uses a dirty read-write descriptor,
    which does. The three together measure the boundary instead of assuming it.
    """
    ctl_snapshot_create(d, "live")
    data = deterministic_body(SAMPLE_BYTES, 402)
    write_file(d.mount, "live", "orig.bin", data, True, rec, receipts)
    src = os.path.join(d.mount, "live", "orig.bin")
    dst = os.path.join(d.mount, "live", "moved.bin")
    os.rename(src, dst)
    # Dirty a different file in the same snapshot and fsync it: this is a WRITE then
    # a COMMIT, which the server turns into fsync(ino) -> flush_snapshot -> commit.
    trigger = deterministic_body(SAMPLE_BYTES, 403)
    write_file(d.mount, "live", "trigger.bin", trigger, True, rec, receipts)
    receipts.repath("live/orig.bin", "live/moved.bin")
    rec.record(
        "case.rename",
        True,
        boundary="write_fsync_of_sibling_forces_commit",
        level="durable",
        case=os.path.basename(case_dir),
        trigger="live/trigger.bin",
        **{"from": "live/orig.bin", "to": "live/moved.bin"},
    )


CASES = {
    "write_fsync": case_write_fsync,
    "write_nofsync": case_write_nofsync,
    "write_race": case_write_race,
    "mixed": case_mixed,
    "rename": case_rename,
    "rename_filefsync": case_rename_filefsync,
    "rename_writefsync": case_rename_writefsync,
    "snapshot_fork": case_snapshot_fork,
    "snapshot_remove": case_snapshot_remove,
    "mmap": case_mmap,
    "gc_crash": case_gc_crash,
    "kill_control": case_kill_control,
}

# Cases in the small validated sample: one durable write, one level-A write, the
# mixed case, and the do-nothing control. These run before anything else.
SAMPLE_CASES = ["write_fsync", "write_nofsync", "kill_control"]
MATRIX_CASES = [
    "write_fsync",
    "write_nofsync",
    "write_race",
    "mixed",
    "rename",
    "rename_filefsync",
    "rename_writefsync",
    "snapshot_fork",
    "snapshot_remove",
    "mmap",
    "gc_crash",
    "kill_control",
]


# --------------------------------------------------------------------------
# self-tests: the harness must fail closed
# --------------------------------------------------------------------------


def selftest(rec, run_dir):
    ok = True
    # 1. A socket no daemon serves must not look like success. The path must be short
    #    enough to bind: a socket past sun_path fails differently and would test nothing.
    ghost = sock_path("ghost")
    if os.path.exists(ghost):
        os.unlink(ghost)
    p = Proc([COWFS, "--socket", ghost, "--json", "status"], timeout=15)
    classified = p.returncode == 3 and "not_running" in p.stdout
    rec.record(
        "selftest.ghost_socket_is_not_success",
        classified,
        rc=p.returncode,
        out=p.stdout.strip()[:200],
    )
    ok = ok and classified
    # 2. kill_verified must refuse a pid that is not the fixture.
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
        rec.record("selftest.kill_refuses_foreign_pid", refused, pid=helper.pid)
        ok = ok and refused
    finally:
        helper.kill()
        helper.wait()
    # 3. is_our_mount must be false for a plain directory.
    plain = os.path.join(run_dir, "plain")
    os.makedirs(plain, exist_ok=True)
    rec.record("selftest.plain_dir_is_not_a_mount", not is_our_mount(plain), path=plain)
    ok = ok and not is_our_mount(plain)
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
    """Pin the tree the binaries came from, so a run is attributable."""
    try:
        rev = Proc(["git", "rev-parse", "HEAD"], timeout=15, cwd=REPO).stdout.strip()
    except Exception as e:  # noqa: BLE001
        rev = "unknown: %s" % e
    digests = {}
    for b in (DAEMON, COWFS):
        try:
            with open(b, "rb") as f:
                h = hashlib.sha256()
                for chunk in iter(lambda: f.read(1 << 20), b""):
                    h.update(chunk)
            digests[os.path.basename(b)] = h.hexdigest()
        except OSError:
            pass
    rec.record("source.identity", True, rev=rev, binaries=digests)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--stage", choices=["sample", "matrix", "native", "all"], default="sample")
    ap.add_argument("--reps", type=int, default=1)
    ap.add_argument("--run-id", default=None)
    ap.add_argument(
        "--internal-native-writer", nargs=3, metavar=("ROOT", "EXPECT", "MODE")
    )
    ap.add_argument(
        "--only",
        default=None,
        help="comma-separated case names to run instead of the stage's list",
    )
    args = ap.parse_args()

    if args.internal_native_writer:
        return internal_native_writer(*args.internal_native_writer)

    global SOCK_DIR
    run_id = args.run_id or ("run-%d" % int(time.time()))
    run_dir = os.path.join(OUT_ROOT, run_id)
    os.makedirs(run_dir, exist_ok=True)
    rec = Recorder(run_dir)
    log("run dir: %s" % run_dir)

    SOCK_DIR = os.path.join(SOCK_ROOT, "cowfs-crash88-%s" % run_id)
    os.makedirs(SOCK_DIR, mode=0o700, exist_ok=True)
    os.chmod(SOCK_DIR, 0o700)
    rec.record(
        "sockdir.ready",
        os.path.isdir(SOCK_DIR) and (os.stat(SOCK_DIR).st_mode & 0o777) == 0o700,
        dir=SOCK_DIR,
        mode=oct(os.stat(SOCK_DIR).st_mode & 0o777),
    )

    try:
        binaries_present(rec)
        source_identity(rec)

        if not selftest(rec, run_dir):
            rec.record("selftest.failed", False)
            log("selftest failed; not running cases")
            return 2

        done = rec.done_cases()
        results = {}

        def go(case_name, phase, rep):
            key = "%s#%d" % (case_name, rep)
            # The store path carries the phase: a case replayed in another phase
            # must get a fresh store, or its snapshot names collide with the
            # previous run's and the case fails on its own residue.
            fixture = "%s-%s-r%d" % (case_name, phase, rep)
            if key in done:
                log("skip %s (already recorded)" % key)
                results[key] = True
                return
            log("== %s (phase=%s rep=%d)" % (key, phase, rep))
            t0 = time.time()
            results[key] = run_case(fixture, CASES[case_name], rec, run_dir, phase)
            rec.record(
                "case.terminal",
                results[key],
                key=key,
                case=fixture,
                terminal=True,
                phase=phase,
                rep=rep,
                secs=round(time.time() - t0, 1),
            )

        if args.only:
            picked = [c.strip() for c in args.only.split(",") if c.strip()]
            for rep in range(args.reps):
                for c in picked:
                    go(c, "only", rep)
        else:
            if args.stage in ("sample", "all"):
                for rep in range(args.reps):
                    for c in SAMPLE_CASES:
                        go(c, "sample", rep)
            if args.stage in ("matrix", "all"):
                for rep in range(args.reps):
                    for c in MATRIX_CASES:
                        go(c, "matrix", rep)
            if args.stage in ("native", "all"):
                results["native#0"] = run_native_case("native-r0", rec, run_dir)

        passed = sum(1 for v in results.values() if v)
        total = len(results)
        rec.record(
            "run.summary",
            passed == total,
            stage=args.stage,
            reps=args.reps,
            passed=passed,
            total=total,
            failures=[k for k, v in results.items() if not v],
        )
        with open(os.path.join(run_dir, "summary.json"), "w", encoding="utf-8") as f:
            json.dump({"run_id": run_id, "stage": args.stage, "results": results}, f, indent=2)
        log("summary: %d/%d passed" % (passed, total))
        return 0 if passed == total else 1
    finally:
        # Remove the private socket dir only if this run emptied it. Anything left
        # inside means a fixture leaked a socket, and that is reported, not deleted
        # blindly. This runs before the recorder is closed so the evidence lands.
        try:
            left = sorted(os.listdir(SOCK_DIR))
            # A lock file beside a socket we already removed is still ours. Remove
            # only regular files directly inside this run's own private dir, and
            # never recurse or follow anything.
            for name in left:
                victim = os.path.join(SOCK_DIR, name)
                if os.path.isfile(victim) and not os.path.islink(victim):
                    os.unlink(victim)
                    rec.record("sockdir.stale_lock_removed", True, path=victim)
            left = sorted(os.listdir(SOCK_DIR))
            rec.record("sockdir.leaked", not left, dir=SOCK_DIR, entries=left)
            if not left:
                os.rmdir(SOCK_DIR)
                rec.record("sockdir.removed", True, dir=SOCK_DIR)
            else:
                log("WARNING: %s still holds %r; left in place" % (SOCK_DIR, left))
        except OSError as e:
            log("sockdir cleanup: %s" % e)
        rec.close()


if __name__ == "__main__":
    sys.exit(main())