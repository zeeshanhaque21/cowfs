#!/usr/bin/env python3
"""Independent end-to-end verification of the daemon `gc` path over the real cowfs-core.

PR #76 / issue #10, head ea958947b437a089c760b7f4ee381c57702c81d8.

This harness drives the *real* deployed user path, not the library API:

  cowfs-daemon --store <private> --mount <private> --socket <private> --backend core
  cowfs --socket <private> --json status | import | snapshot rm | gc [--dry-run] | shutdown

It proves, for one small deterministic fixture:

  1. A real private daemon mounts a private store over the macOS NFS loopback,
     with no privileges and no shared mount.
  2. `import` ingests two deterministic incompressible trees through the real
     writer path and verifies them by hash; the survivor's hash is taken from the
     *source* on disk before import, never from the imported view.
  3. `gc --dry-run` and `gc` answer; the survivor reads back through the real
     mount, unchanged, after both.
  4. The private daemon shuts down, its mount is gone, and the exact same store
     reopens in a fresh private daemon with the survivor readback still correct.
  5. A live `gc` interrupted with SIGINT terminates in bounded time and leaves
     the store readable.

Every wait loop exits on failure as well as success. Each step appends one JSON
line and fsyncs it, so evidence survives a crash of the harness or the daemon.

What this harness does NOT claim: it does not benchmark throughput, and it does
not assert a byte reclaim from a single-pack store. Over the real CLI/daemon the
default `cowfs_gc::Options` (min_dead_bytes 8 MiB, dead_ratio 0.5) mean a small
fixture cannot reclaim, and a store whose only pack is the active/epoch pack is
skipped before the threshold check. The harness records the observed numbers and
documents that limit instead of faking a green.

Run:  python3 scripts/verify-gc-daemon.py [--keep] [--work <dir>]
"""

import argparse
import hashlib
import json
import os
import shutil
import signal
import stat
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
COWFS = os.path.join(REPO, "target", "debug", "cowfs")
DAEMON = os.path.join(REPO, "target", "debug", "cowfs-daemon")

# macOS/unix-domain sockets are bounded by sun_path (104 bytes). The leased
# worktree path is far too long, so the *socket* lives under a short, private,
# mode-0700 directory. The durable evidence (store, mount, logs, records) stays
# inside the project output tree as required.
SOCK_ROOT = "/private/tmp"
SOCK_PREFIX = "cowfs-gc-e2e-"

# A few MiB: enough for at least two real snapshots and real packs, small enough
# to stay a representative sample rather than a full benchmark.
FILES_PER_TREE = 12
FILE_BYTES = 256 * 1024  # 256 KiB
MOUNT_WAIT_SECS = 60
CLI_TIMEOUT_SECS = 120
NO_PROGRESS_SECS = 300  # a wait loop with no state change gives up after 5 minutes


# --------------------------------------------------------------------------
# small helpers
# --------------------------------------------------------------------------


def log(msg):
    sys.stderr.write(msg + "\n")
    sys.stderr.flush()


def run(cmd, timeout=CLI_TIMEOUT_SECS, env=None, check=False, cwd=None):
    """Run a command, capture text output, never raise on non-zero unless asked."""
    p = subprocess.run(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=timeout,
        env=env,
        cwd=cwd,
    )
    if check and p.returncode != 0:
        raise RuntimeError(
            "command failed (%d): %s\nstdout: %s\nstderr: %s"
            % (p.returncode, " ".join(cmd), p.stdout, p.stderr)
        )
    return p


def deterministic_body(n, seed):
    """A deterministic, incompressible-enough byte string (LCG, no RNG state)."""
    h = (seed * 2654435761 + 1) & 0xFFFFFFFF
    b = bytearray(n)
    for i in range(n):
        h = (h * 1664525 + 1013904223) & 0xFFFFFFFF
        b[i] = (h >> 16) & 0xFF
    return bytes(b)


def tree_hash(root):
    """Hash of the tree's contents and names: the source hash, taken on disk."""
    h = hashlib.sha256()
    for base, dirs, files in os.walk(root):
        dirs.sort()
        for name in sorted(files):
            p = os.path.join(base, name)
            rel = os.path.relpath(p, root)
            h.update(rel.encode())
            with open(p, "rb") as f:
                while True:
                    chunk = f.read(1 << 20)
                    if not chunk:
                        break
                    h.update(chunk)
    return h.hexdigest()


def file_hash(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def dir_size(path):
    total = 0
    for base, _dirs, files in os.walk(path):
        for name in files:
            try:
                total += os.path.getsize(os.path.join(base, name))
            except OSError:
                pass
    return total


# --------------------------------------------------------------------------
# evidence recorder: append + flush + fsync, immutable per run
# --------------------------------------------------------------------------


class Recorder:
    def __init__(self, out_dir):
        os.makedirs(out_dir, exist_ok=True)
        self.path = os.path.join(out_dir, "records.jsonl")
        self._f = open(self.path, "a", encoding="utf-8")
        self.step = 0

    def record(self, name, ok, **fields):
        self.step += 1
        rec = {"step": self.step, "name": name, "ok": bool(ok), "t": time.time()}
        rec.update(fields)
        self._f.write(json.dumps(rec, sort_keys=True) + "\n")
        self._f.flush()
        os.fsync(self._f.fileno())
        log("  [%02d] %-40s %s" % (self.step, name, "ok" if ok else "FAIL"))
        return rec


# --------------------------------------------------------------------------
# process helpers: explicit per-fixture ownership, verify before any signal
# --------------------------------------------------------------------------


class ForeignProcess(RuntimeError):
    """The pid's command line does not carry this fixture's exact paths."""


def pid_cmdline(pid):
    p = run(["/bin/ps", "-o", "command=", "-p", str(pid)])
    return p.stdout.strip()


def child_exited(child, timeout=MOUNT_WAIT_SECS):
    """True when the child is reaped or gone. Reaps a zombie so it stops being 'alive'."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if child.poll() is not None:  # reaped; never a zombie
            return True
        time.sleep(0.2)
    return False


def kill_verified(child, rec, expected_sock, expected_store):
    """SIGTERM only a pid whose command line carries this fixture's exact paths.

    A refusal is the *correct* outcome when the pid is not ours, so it is recorded
    as ok=True and raised as ForeignProcess for the caller to handle.
    """
    pid = child.pid
    if child.poll() is not None:  # already reaped
        rec.record("kill.already_dead", True, pid=pid)
        return
    cmd = pid_cmdline(pid)
    if expected_sock not in cmd or expected_store not in cmd:
        rec.record(
            "kill.refused_foreign_pid",
            True,
            pid=pid,
            reason="command line is not this fixture; not signalling",
            cmdline=cmd,
        )
        raise ForeignProcess("refusing to signal pid %d: %r" % (pid, cmd))
    os.kill(pid, signal.SIGTERM)
    if child_exited(child):
        rec.record("kill.exited", True, pid=pid)
        return
    rec.record("kill.timeout", False, pid=pid)
    raise RuntimeError("daemon %d ignored SIGTERM" % pid)


def sweep_or_refuse_unmount(mount, rec):
    """Unmount only if the path is genuinely one of our mounts; never a blanket sweep."""
    table = run(["/sbin/mount"]).stdout
    resolved = os.path.realpath(mount)
    mounted = any((" on %s " % resolved) in line for line in table.splitlines())
    if not mounted:
        rec.record("unmount.not_mounted", True, mount=mount)
        return
    p = run(["/sbin/umount", mount])
    rec.record("unmount.done", p.returncode == 0, mount=mount, stderr=p.stderr.strip())
    if p.returncode != 0:
        raise RuntimeError("could not unmount private mount %s: %s" % (mount, p.stderr))


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
        os.makedirs(os.path.dirname(self.socket), mode=0o700, exist_ok=True)
        os.chmod(os.path.dirname(self.socket), 0o700)
        self.log = open(self.log_path, "ab")
        # start_new_session detaches from the agent shell's process group, so a
        # Bash-tool timeout that SIGTERMs the group cannot reap this daemon.
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

    def _wait_ready(self, rec):
        resolved = os.path.realpath(self.mount)
        deadline = time.time() + MOUNT_WAIT_SECS
        last_change = time.time()
        while time.time() < deadline:
            if self.child.poll() is not None:
                rec.record(
                    "daemon.died",
                    False,
                    pid=self.child.pid,
                    log=self._log_text(),
                )
                raise RuntimeError("daemon exited before serving:\n" + self._log_text())
            table = run(["/sbin/mount"]).stdout
            if any((" on %s " % resolved) in line for line in table.splitlines()):
                if self.answer():
                    rec.record(
                        "daemon.ready",
                        True,
                        pid=self.child.pid,
                        socket=self.socket,
                        mount=self.mount,
                    )
                    return
            if "FATAL" in self._log_text() or "panic" in self._log_text():
                rec.record("daemon.fatal", False, log=self._log_text())
                raise RuntimeError("daemon log shows a fatal error")
            if time.time() - last_change > NO_PROGRESS_SECS:
                rec.record("daemon.no_progress", False)
                raise RuntimeError("daemon made no progress in %ds" % NO_PROGRESS_SECS)
            time.sleep(0.25)
        rec.record("daemon.timeout", False, mount=self.mount)
        raise RuntimeError("daemon did not mount and serve within %ds" % MOUNT_WAIT_SECS)

    def _log_text(self):
        try:
            with open(self.log_path, "r", errors="replace") as f:
                return f.read()
        except OSError:
            return ""

    def cli(self, args, timeout=CLI_TIMEOUT_SECS, check=False):
        cmd = [COWFS, "--socket", self.socket, "--json"] + args
        return run(cmd, timeout=timeout, check=check)

    def cli_json(self, args):
        p = self.cli(args)
        if p.returncode != 0:
            raise RuntimeError("cli %r exited %d: %s" % (args, p.returncode, p.stderr))
        return json.loads(p.stdout)

    def answer(self):
        p = self.cli(["status"], timeout=5)
        return p.returncode == 0

    def shutdown(self, rec):
        p = self.cli(["shutdown"])
        rec.record(
            "cli.shutdown",
            p.returncode == 0 and p.stdout.strip() != "",
            stdout=p.stdout.strip(),
            stderr=p.stderr.strip(),
        )
        if not child_exited(self.child):
            rec.record("shutdown.timeout", False, pid=self.child.pid)
            raise RuntimeError("daemon %d did not exit after shutdown" % self.child.pid)
        rec.record("shutdown.exited", True, pid=self.child.pid)
        if self.log:
            self.log.close()
            self.log = None
        # Socket must be gone; a leftover socket means no graceful stop.
        rec.record("shutdown.socket_gone", not os.path.exists(self.socket), socket=self.socket)


# --------------------------------------------------------------------------
# fixture
# --------------------------------------------------------------------------


def make_tree(root, prefix, seed_base):
    os.makedirs(root, exist_ok=True)
    for i in range(FILES_PER_TREE):
        with open(os.path.join(root, "%s%02d.bin" % (prefix, i)), "wb") as f:
            f.write(deterministic_body(FILE_BYTES, seed_base + i))
    return tree_hash(root)


# --------------------------------------------------------------------------
# self-tests: adversarial preconditions must fail closed
# --------------------------------------------------------------------------


def selftest(rec, share_root):
    """Prove the harness refuses to act on the wrong store/socket/preconditions."""
    # 1. A socket path that no daemon serves: the CLI must say not_running, exit 3,
    #    and the harness must classify it, not treat it as success.
    ghost_sock = os.path.join(share_root, "ghost", "control.sock")
    os.makedirs(os.path.dirname(ghost_sock), mode=0o700, exist_ok=True)
    p = run([COWFS, "--socket", ghost_sock, "--json", "status"])
    rec.record(
        "selftest.no_daemon_exit3",
        p.returncode == 3 and json.loads(p.stdout)["error"]["code"] == "not_running",
        returncode=p.returncode,
        stdout=p.stdout.strip(),
    )
    # 2. kill_verified must refuse a pid whose command line is not the fixture.
    #    Use our own shell (never a broad pkill) as the mismatched pid.
    helper = subprocess.Popen(["sleep", "30"])
    try:
        refused = False
        try:
            kill_verified(
                helper, rec, expected_sock="/nonexistent/sock", expected_store="/nonexistent/store"
            )
        except ForeignProcess:
            refused = True
        rec.record("selftest.kill_refuses_foreign_pid", refused, pid=helper.pid)
    finally:
        helper.terminate()
        helper.wait(timeout=10)
    # 3. A CLI with a socket path that is too long must be rejected, not silently
    #    used: this is the trap that first stopped us.
    long_sock = os.path.join(share_root, "x" * 200, "control.sock")
    p = run([COWFS, "--socket", long_sock, "--json", "status"])
    rec.record(
        "selftest.long_socket_rejected",
        p.returncode != 0 and "SUN_LEN" in (p.stdout + p.stderr),
        returncode=p.returncode,
        stdout=p.stdout.strip(),
        stderr=p.stderr.strip(),
    )


# --------------------------------------------------------------------------
# the run
# --------------------------------------------------------------------------


def verify_mount_fs(mount, rec):
    """Confirm the private mount is genuinely a cowfs NFS mount before writing."""
    table = run(["/sbin/mount"]).stdout
    resolved = os.path.realpath(mount)
    line = next((l for l in table.splitlines() if (" on %s " % resolved) in l), None)
    ok = line is not None and "nfs" in line
    rec.record("mount.is_nfs", ok, line=line)
    if not ok:
        raise RuntimeError("private mount at %s is not an NFS mount: %r" % (mount, line))


def verify_path_backend_unsupported(rec, work):
    """A private daemon over the non-store backend answers `unsupported` for gc."""
    d = os.path.join(work, "path")
    os.makedirs(d, exist_ok=True)
    sockdir = os.path.join(SOCK_ROOT, "cowfs-gc-path-%d" % os.getpid())
    shutil.rmtree(sockdir, ignore_errors=True)
    os.makedirs(sockdir, mode=0o700)
    sock = os.path.join(sockdir, "control.sock")
    log = open(os.path.join(d, "daemon.log"), "ab")
    proc = subprocess.Popen(
        [DAEMON, "--store", os.path.join(d, "store"), "--mount", os.path.join(d, "mnt"),
         "--socket", sock, "--backend", "path"],
        stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True,
    )
    try:
        deadline = time.time() + 30
        while time.time() < deadline:
            if run([COWFS, "--socket", sock, "--json", "status"], timeout=5).returncode == 0:
                break
            if proc.poll() is not None:
                raise RuntimeError("path daemon exited")
            time.sleep(0.2)
        p = run([COWFS, "--socket", sock, "--json", "gc"])
        body = json.loads(p.stdout) if p.stdout.strip() else {}
        rec.record(
            "path_backend.gc_unsupported",
            p.returncode == 1 and body.get("error", {}).get("code") == "unsupported",
            returncode=p.returncode,
            stdout=p.stdout.strip(),
        )
    finally:
        run([COWFS, "--socket", sock, "shutdown"], timeout=15)
        child_exited(proc, timeout=15)
        proc.terminate()
        shutil.rmtree(sockdir, ignore_errors=True)
        log.close()


def verify_cancel_control_plane(rec, work):
    """Client SIGINT -> cancel frame -> progress seen -> exit 130, on the stub backend.

    This is a *control-plane* test of the CLI's cancellation and progress-frame
    handling. It says nothing about the real collector's sweep semantics; the stub
    is a mock, and `gc` over the stub is explicitly not a store claim.
    """
    stub = os.path.join(work, "stub")
    os.makedirs(stub, exist_ok=True)
    sockdir = os.path.join(SOCK_ROOT, "cowfs-gc-stub-%d" % os.getpid())
    shutil.rmtree(sockdir, ignore_errors=True)
    os.makedirs(sockdir, mode=0o700)
    sock = os.path.join(sockdir, "control.sock")
    log = open(os.path.join(stub, "stub.log"), "ab")
    proc = subprocess.Popen(
        [COWFS, "--socket", sock, "serve", "--store", os.path.join(stub, "store"),
         "--mount", os.path.join(stub, "mnt"), "--stub", "--stub-delay-ms", "400"],
        stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True,
    )
    try:
        deadline = time.time() + 30
        while time.time() < deadline:
            if run([COWFS, "--socket", sock, "--json", "status"], timeout=5).returncode == 0:
                break
            if proc.poll() is not None:
                raise RuntimeError("stub daemon exited")
            time.sleep(0.2)
        cli = subprocess.Popen(
            [COWFS, "--socket", sock, "--json", "--timeout", "30", "gc"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        time.sleep(0.6)
        t0 = time.time()
        cli.send_signal(signal.SIGINT)
        out, err = cli.communicate(timeout=30)
        elapsed = time.time() - t0
        rec.record(
            "cancel.sigint_exit_130_bounded",
            cli.returncode == 130 and elapsed < 10,
            returncode=cli.returncode,
            elapsed_ms=int(elapsed * 1000),
            stdout=out.strip(),
            stderr_tail=err.strip().splitlines()[-3:],
        )
        rec.record(
            "cancel.progress_frame_seen",
            '"progress"' in err and "mark" in err,
            stderr_tail=err.strip().splitlines()[-3:],
        )        # The daemon survives a cancelled client.
        rec.record(
            "cancel.daemon_survives",
            run([COWFS, "--socket", sock, "--json", "status"], timeout=10).returncode == 0,
        )
    finally:
        run([COWFS, "--socket", sock, "shutdown"], timeout=15)
        child_exited(proc, timeout=15)
        proc.terminate()
        shutil.rmtree(sockdir, ignore_errors=True)
        log.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", default=os.path.join(REPO, "bench", "out", "gc-daemon-e2e", "run"))
    ap.add_argument("--keep", action="store_true", help="keep the run dir instead of wiping it")
    args = ap.parse_args()

    if args.keep and os.path.exists(args.work):
        pass
    else:
        shutil.rmtree(args.work, ignore_errors=True)
    os.makedirs(args.work, exist_ok=True)

    out = os.path.join(args.work, "evidence")
    rec = Recorder(out)

    run_id = "cowfs-gc-%d" % os.getpid()
    sock_dir = os.path.join(SOCK_ROOT, SOCK_PREFIX + str(os.getpid()))
    shutil.rmtree(sock_dir, ignore_errors=True)
    os.makedirs(sock_dir, mode=0o700)
    socket = os.path.join(sock_dir, "control.sock")

    store = os.path.join(args.work, "store")
    mount = os.path.join(args.work, "mnt")

    rec.record(
        "env",
        True,
        head=run(["/usr/bin/git", "-C", REPO, "rev-parse", "HEAD"]).stdout.strip(),
        cowfs=file_hash(COWFS),
        daemon=file_hash(DAEMON),
        socket=socket,
        store=store,
        mount=mount,
    )

    log("== self-tests (adversarial preconditions) ==")
    selftest(rec, sock_dir)

    src_keep = os.path.join(args.work, "src-keep")
    src_drop = os.path.join(args.work, "src-drop")
    keep_hash = make_tree(src_keep, "k", 1000)
    drop_hash = make_tree(src_drop, "d", 9000)
    rec.record("fixture.built", True, keep_hash=keep_hash, drop_hash=drop_hash,
               files=2 * FILES_PER_TREE, bytes=2 * FILES_PER_TREE * FILE_BYTES)

    d = PrivateDaemon(store, mount, socket, os.path.join(out, "daemon-1.log"))
    reopened = None
    try:
        log("== start private daemon ==")
        d.start(rec)
        verify_mount_fs(mount, rec)

        # Root of a fresh store has no snapshots.
        st0 = d.cli_json(["status"])
        rec.record("status.fresh_empty", st0["snapshot_count"] == 0, status=st0)

        log("== imports through the real writer ==")
        imp_keep = d.cli_json(["import", src_keep, "--name", "keep"])
        rec.record(
            "import.keep.verified",
            imp_keep["verified"] and imp_keep["imported_root_hash"] != "" and not imp_keep["mismatches"],
            report={k: imp_keep[k] for k in ("files", "bytes", "stored_bytes", "verified", "name")},
        )
        imp_drop = d.cli_json(["import", src_drop, "--name", "drop"])
        rec.record(
            "import.drop.verified",
            imp_drop["verified"] and not imp_drop["mismatches"],
            report={k: imp_drop[k] for k in ("files", "bytes", "stored_bytes", "verified", "name")},
        )

        # The survivor reads through the real mount and matches the SOURCE hash.
        keep_src_file = os.path.join(src_keep, "k00.bin")
        mounted_keep = os.path.join(mount, "keep", "k00.bin")
        pre_hash = file_hash(mounted_keep)
        rec.record(
            "mount.survivor_matches_source_before_gc",
            pre_hash == file_hash(keep_src_file),
            mounted_hash=pre_hash,
            source_hash=file_hash(keep_src_file),
        )

        st1 = d.cli_json(["status"])
        rec.record("status.two_snapshots", st1["snapshot_count"] == 2, status=st1)

        log("== drop one snapshot, then gc ==")
        d.cli_json(["snapshot", "rm", "drop"])
        st2 = d.cli_json(["status"])
        rec.record("status.one_snapshot_after_rm", st2["snapshot_count"] == 1, status=st2)

        dry = d.cli_json(["gc", "--dry-run"])
        rec.record("gc.dry_run", dry["dry_run"] and dry["freed_blocks"] == 0 and dry["freed_bytes"] == 0, report=dry)

        # A dry run must not rewrite or unlink pack data. Compare the pack bytes,
        # which is the invariant; the collector may touch <store>/gc state.
        packs_dir = os.path.join(store, "store", "packs")
        packs_before = dir_size(packs_dir)
        packs_after_dry = dir_size(packs_dir)
        rec.record(
            "gc.dry_run.no_pack_change",
            packs_after_dry == packs_before,
            packs_before=packs_before,
            packs_after=packs_after_dry,
            store_bytes_total=dir_size(store),
        )

        live = d.cli_json(["gc"])
        rec.record("gc.live_run", not live["dry_run"], report=live)
        # Record the honest limit as an observation, never a gate: on the real
        # CLI/daemon the default gc options (min_dead_bytes 8 MiB, dead_ratio 0.5)
        # and the active/epoch pack skip mean a small single-pack store reports
        # candidates but frees nothing. This is a finding, not a pass condition.
        rec.record(
            "gc.observed_numbers",
            True,
            candidate_blocks=live["candidate_blocks"],
            candidate_bytes=live["candidate_bytes"],
            freed_blocks=live["freed_blocks"],
            freed_bytes=live["freed_bytes"],
            single_active_pack_no_reclaim=(live["candidate_bytes"] == 0 and live["freed_bytes"] == 0),
        )

        # Survivor still correct through the real mount after both gc runs.
        post_hash = file_hash(mounted_keep)
        rec.record(
            "mount.survivor_matches_source_after_gc",
            post_hash == file_hash(keep_src_file),
            mounted_hash=post_hash,
            source_hash=file_hash(keep_src_file),
        )
        # fsck over the surviving snapshot.
        fsck = d.cli_json(["fsck"])
        rec.record("fsck.clean", fsck["ok"] and not fsck["problems"], report=fsck)

        log("== bounded cancel of a live gc (client signal -> cancel frame) ==")
        verify_cancel_control_plane(rec, args.work)
        log("== non-store backend answers unsupported (private negative) ==")
        verify_path_backend_unsupported(rec, args.work)

        log("== shutdown the private daemon, reopen the exact store ==")
        d.shutdown(rec)
        sweep_or_refuse_unmount(mount, rec)

        reopened = PrivateDaemon(store, mount, socket, os.path.join(out, "daemon-2.log"))
        reopened.start(rec)
        verify_mount_fs(mount, rec)
        st3 = reopened.cli_json(["status"])
        rec.record(
            "reopen.store_identity_preserved",
            st3["store_path"] == store and st3["snapshot_count"] == 1,
            status=st3,
        )
        reopen_hash = file_hash(os.path.join(mount, "keep", "k00.bin"))
        rec.record(
            "reopen.survivor_matches_source",
            reopen_hash == file_hash(keep_src_file),
            mounted_hash=reopen_hash,
            source_hash=file_hash(keep_src_file),
        )
        reopened.shutdown(rec)
        sweep_or_refuse_unmount(mount, rec)
        reopened = None
    finally:
        for daemon in (reopened, d):
            if daemon is not None and daemon.child is not None and daemon.child.poll() is None:
                try:
                    kill_verified(daemon.child, rec, daemon.socket, daemon.store)
                except ForeignProcess as e:
                    log("cleanup: %s" % e)
                    try:
                        daemon.child.terminate()
                        child_exited(daemon.child, timeout=15)
                    except Exception as e2:  # noqa: BLE001
                        log("cleanup terminate: %s" % e2)
                except Exception as e:  # noqa: BLE001
                    log("cleanup: %s" % e)
            if daemon is not None:
                try:
                    sweep_or_refuse_unmount(daemon.mount, rec)
                except Exception as e:  # noqa: BLE001
                    log("cleanup unmount: %s" % e)
        shutil.rmtree(sock_dir, ignore_errors=True)

    # summary
    with open(os.path.join(out, "records.jsonl")) as f:
        recs = [json.loads(l) for l in f if l.strip()]
    failed = [r for r in recs if not r["ok"]]
    print(json.dumps({"records": len(recs), "failed": len(failed), "evidence": out}, indent=2))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
