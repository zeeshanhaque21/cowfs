#!/usr/bin/env python3
"""Focused tests for scripts/verify-git-index-integrity.py. No third-party deps.

Every gate in the harness gets a negative control here: a reader, a bound or an integrity check
that cannot fail is not evidence. Nothing in this file starts a daemon, mounts anything, sends a
signal or reads a real mount table, so it is safe to run on a machine serving other agents.

Run: python3 scripts/test_verify_git_index_integrity.py
"""

import importlib.util
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "verify-git-index-integrity.py"

_spec = importlib.util.spec_from_file_location("verify_git_index_integrity", SCRIPT)
vgi = importlib.util.module_from_spec(_spec)
sys.argv = ["verify-git-index-integrity.py"]
_spec.loader.exec_module(vgi)


# Real `mount`(8) output captured on this host, so the parser is tested against the shape it will
# actually read rather than an idealised one.
REAL_MOUNT_LINES = [
    "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)",
    "devfs on /dev (devfs, local, nobrowse)",
    "map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)",
    ("localhost:/cowfs-6952209e13d24561f35488be1c887d41 "
     "on /Users/zeeshanhaque/.cowfs/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)"),
]


def reader_returning(rc, stdout="", stderr=""):
    return mock.Mock(return_value=SimpleNamespace(
        returncode=rc, stdout=stdout.encode(), stderr=stderr.encode()))


class MountLineParsing(unittest.TestCase):
    def test_every_real_line_parses(self):
        """A line this parser cannot read makes the whole table UNKNOWN, so all of them must parse."""
        for line in REAL_MOUNT_LINES:
            with self.subTest(line=line):
                self.assertIsNotNone(vgi.split_mount_line(line))

    def test_source_with_a_space(self):
        """`map auto_home on /path (...)` has no ` on ` in its source, which a naive head split
        mistakes for a non-mount line and turns the whole table UNKNOWN."""
        self.assertEqual(
            vgi.split_mount_line(REAL_MOUNT_LINES[2]),
            ("map auto_home", "/System/Volumes/Data/home", "autofs"))

    def test_single_character_mountpoint(self):
        """`/` is a one-character mountpoint; an off-by-one in the bracket rejects it."""
        self.assertEqual(
            vgi.split_mount_line(REAL_MOUNT_LINES[0]),
            ("/dev/disk3s1s1", "/", "apfs"))

    def test_fstype_comes_from_the_options_list(self):
        self.assertEqual(vgi.split_mount_line(REAL_MOUNT_LINES[3])[2], "nfs")

    def test_escaped_space_decodes_once(self):
        line = "localhost:/exp on /tmp/mnt\\040a (nfs, nodev)"
        self.assertEqual(vgi.split_mount_line(line)[1], "/tmp/mnt a")

    def test_mountpoint_containing_open_paren(self):
        line = "localhost:/exp on /tmp/mnt\\040(dir) (nfs, nodev)"
        self.assertEqual(vgi.split_mount_line(line)[1], "/tmp/mnt (dir)")

    def test_non_mount_lines_are_refused(self):
        for line in ("", "   ", "no separator here", "/dev/disk3s1s1", " on / (apfs)"):
            with self.subTest(line=line):
                self.assertIsNone(vgi.split_mount_line(line))

    def test_escape_decoder_is_single_pass(self):
        """`\\040040` is a space then the literal text 040, never a second decode to two spaces."""
        self.assertEqual(vgi.unescape_mount_path("\\040040"), " 040")
        self.assertEqual(vgi.unescape_mount_path("\\134040"), "\\040")


class MountTableTriState(unittest.TestCase):
    def target(self):
        return Path("/tmp/mnt-target")

    def test_present_on_exact_match(self):
        line = "localhost:/exp on /tmp/mnt-target (nfs, nodev)"
        t = vgi.MountTable(0, "\n".join(REAL_MOUNT_LINES + [line]) + "\n", "", self.target())
        self.assertEqual(t.state, vgi.MountTable.PRESENT)
        self.assertEqual(t.matches, [line])

    def test_present_wins_over_unreadable_neighbours(self):
        """One unreadable neighbour line must not hide a target that itself parsed."""
        t = vgi.MountTable(0, "garbage line\n" + REAL_MOUNT_LINES[3].replace(
            "/Users/zeeshanhaque/.cowfs/mnt", "/tmp/mnt-target") + "\n", "", self.target())
        self.assertEqual(t.state, vgi.MountTable.PRESENT)

    def test_absent_only_from_a_clean_read(self):
        t = vgi.MountTable(0, "\n".join(REAL_MOUNT_LINES) + "\n", "", self.target())
        self.assertEqual(t.state, vgi.MountTable.ABSENT)

    def test_unknown_when_reader_failed(self):
        t = vgi.MountTable(1, "\n".join(REAL_MOUNT_LINES) + "\n", "boom", self.target())
        self.assertEqual(t.state, vgi.MountTable.UNKNOWN)

    def test_unknown_when_output_is_empty(self):
        self.assertEqual(vgi.MountTable(0, "", "", self.target()).state,
                         vgi.MountTable.UNKNOWN)

    def test_unknown_when_any_line_is_unreadable(self):
        t = vgi.MountTable(0, "garbage\n" + "\n".join(REAL_MOUNT_LINES) + "\n", "", self.target())
        self.assertEqual(t.state, vgi.MountTable.UNKNOWN)

    def test_unknown_on_timeout_and_oserror(self):
        def timeout(*a, **k):
            raise subprocess.TimeoutExpired(cmd="mount", timeout=30)

        def oserror(*a, **k):
            raise OSError(2, "No such file or directory", "mount")

        self.assertEqual(
            vgi.read_mount_table(self.target(), reader=timeout).state, vgi.MountTable.UNKNOWN)
        self.assertEqual(
            vgi.read_mount_table(self.target(), reader=oserror).state, vgi.MountTable.UNKNOWN)

    def test_prefix_is_not_a_match(self):
        """The old reader was a substring test, so `/tmp/mnt-a` matched `/tmp/mnt-ab`."""
        line = "localhost:/exp on /tmp/mnt-ab (nfs, nodev)"
        t = vgi.MountTable(0, line + "\n", "", Path("/tmp/mnt-a"))
        self.assertEqual(t.state, vgi.MountTable.ABSENT)


class WindowBounds(unittest.TestCase):
    def test_available_is_forty_four(self):
        self.assertEqual(len(vgi.build_ops()), 44)

    def test_accepts_the_bounds(self):
        for size in (vgi.OPS_MIN, len(vgi.build_ops())):
            with self.subTest(size=size):
                self.assertEqual(len(vgi.OpWindow(size).ops), size)

    def test_rejects_a_window_that_does_not_exist(self):
        """Silently slicing to 44 while 60 was requested would report a smaller declared window
        than the caller asked for, and then pass its own planned==executed check."""
        for size in (0, vgi.OPS_MIN - 1, 45, 60, 1000):
            with self.subTest(size=size), self.assertRaises(vgi.BadWindow):
                vgi.OpWindow(size)

    def test_rejection_names_the_real_ceiling(self):
        try:
            vgi.OpWindow(45)
        except vgi.BadWindow as exc:
            self.assertIn("44", str(exc))
        else:
            self.fail("BadWindow not raised")

    def test_declared_equals_requested(self):
        w = vgi.OpWindow(30)
        self.assertEqual(len(w.ops), w.requested)
        self.assertEqual(w.available, len(vgi.build_ops()))


class FakeReader:
    """Stands in for `mount`(8) so the tri-state can be driven without a real mount."""

    def __init__(self, rc, stdout):
        self.rc, self.stdout = rc, stdout

    def __call__(self, argv, **kwargs):
        assert argv == ["mount"], argv
        return SimpleNamespace(returncode=self.rc,
                               stdout=self.stdout.encode(), stderr=b"")


class DaemonTeardown(unittest.TestCase):
    """`stop()` must keep a dead process and a mount it cannot prove absent apart."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="r21-test-"))
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.mount = self.root / "mnt"
        self.mount.mkdir()
        self.d = vgi.PrivateDaemon(self.root, self.root, vgi.Run(self.root / "log.jsonl"), "t")
        # Start the socket directory the real constructor makes, then pin a pid that cannot exist.
        self.d.sock_dir.mkdir(parents=True, exist_ok=True)
        self.d.log.touch()
        self.d.pid = 999_999_999
        self.d.spawn = {"lstart_at_start": "never", "exe": str(self.root / "cowfs-daemon")}

    def test_absent_pid_with_absent_mount_is_clean(self):
        with mock.patch.object(vgi, "read_mount_table", lambda p: vgi.MountTable(
                0, f"localhost:/e on {p} (nfs, nodev)\n", "", Path("/somewhere/else"))):
            out = self.d.stop()
        self.assertTrue(out["process_stopped"])
        self.assertEqual(out["mount_state"], vgi.MountTable.ABSENT)
        self.assertTrue(out["clean"])
        self.assertNotIn("quarantine", out)

    def test_absent_pid_with_present_mount_is_quarantined_not_clean(self):
        """The failure this guards: a dead pid reported as a clean teardown while its NFS mount is
        still in the kernel, where every later `ls` on this machine hangs."""
        with mock.patch.object(vgi, "read_mount_table", lambda p: vgi.MountTable(
                0, f"localhost:/e on {p} (nfs, nodev)\n", "", p)):
            out = self.d.stop()
        self.assertTrue(out["process_stopped"])
        self.assertEqual(out["mount_state"], vgi.MountTable.PRESENT)
        self.assertFalse(out["clean"])
        self.assertIn("quarantine", out)

    def test_absent_pid_with_unreadable_table_is_not_clean(self):
        with mock.patch.object(vgi, "read_mount_table", lambda p: vgi.MountTable(
                1, "localhost:/e on /elsewhere (nfs)\n", "boom", p)):
            out = self.d.stop()
        self.assertTrue(out["process_stopped"])
        self.assertEqual(out["mount_state"], vgi.MountTable.UNKNOWN)
        self.assertFalse(out["clean"])

    def test_never_started_is_not_a_stop(self):
        self.d.pid = None
        out = self.d.stop()
        self.assertFalse(out["process_stopped"])
        self.assertNotIn("clean", out)

    def test_a_recycled_pid_is_not_ours(self):
        """A live pid whose command line does not carry this run's store and socket is refused, so
        a later teardown can never signal the shared daemon through a pid collision."""
        self.d.pid = 1
        self.d.spawn = {"lstart_at_start": "x", "exe": "x"}
        with mock.patch.object(vgi, "pid_argv", lambda pid: "/usr/sbin/sshd"):
            self.assertFalse(self.d.owned(1))
            out = self.d.stop()
        self.assertFalse(out["process_stopped"])


class MountAttestation(unittest.TestCase):
    """The attestation must fail closed, and its own negative controls must be able to fail."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="r21-att-"))
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)

    def test_local_fallback_is_refused(self):
        """Two APFS directories with no mount table line are exactly the silent-fallback shape."""
        a, b = self.root / "a", self.root / "b"
        a.mkdir()
        b.mkdir()
        r = vgi.attest_mount_arm(a, b, b, SimpleNamespace(
            pid=None, store=Path("/nonexistent/s"), sock=Path("/nonexistent/k"),
            identity=lambda p: {"argv": None}, owned=lambda p: False), b)
        self.assertFalse(r["pass"])
        self.assertTrue(r["why"])

    def test_same_device_is_refused(self):
        """A same-device mount arm is a second native arm; two APFS runs of a deterministic seed do
        agree, so the verdict would read clean for a run that never touched cowfs."""
        a, b = self.root / "a", self.root / "b"
        a.mkdir()
        b.mkdir()
        r = vgi.attest_mount_arm(a, a, a, SimpleNamespace(
            pid=None, store=Path("/nonexistent/s"), sock=Path("/nonexistent/k"),
            identity=lambda p: {"argv": None}, owned=lambda p: False), a)
        self.assertFalse(r["pass"])

    def test_negative_controls_report_ok(self):
        a = self.root / "native"
        a.mkdir()
        c = vgi.attest_negative_controls(vgi.attest_mount_arm, a, a, a, None, a)
        self.assertEqual(len(c), 2)
        for row in c:
            with self.subTest(control=row["control"]):
                self.assertTrue(row["pass"])
                self.assertFalse(row["attested_pass"])


def build_packed_repo(root: Path) -> Path:
    """A small real repo with a real pack and a real idx, so the idx gate reads real git."""
    root.mkdir(parents=True)
    run = vgi.proc
    assert run(["git", "init", "-q", "-b", "main"], cwd=root)["rc"] == 0
    assert run(["git", "config", "gc.auto", "0"], cwd=root)["rc"] == 0
    for name in ("a.txt", "b.txt"):
        (root / name).write_text(name * 2000)
    assert run(["git", "add", "-A"], cwd=root)["rc"] == 0
    assert run(["git", "commit", "-q", "-m", "seed"], cwd=root)["rc"] == 0
    assert run(["git", "gc", "-q", "--aggressive"], cwd=root)["rc"] == 0
    return root


class IdxGate(unittest.TestCase):
    """`git show-index` returns 0 on the historical shape, so the gate must not trust it alone."""

    @classmethod
    def setUpClass(cls):
        cls.root = Path(tempfile.mkdtemp(prefix="r21-idx-"))
        cls.repo = build_packed_repo(cls.root / "repo")
        cls.pristine = {p.name: p.read_bytes() for p in vgi.idx_files(cls.repo)}
        assert cls.pristine, "no idx produced"

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.root, ignore_errors=True)

    def setUp(self):
        self.restore()

    def restore(self):
        for p in vgi.idx_files(self.repo):
            # git writes pack files 0444, so a corrupt-then-restore test needs the mode back.
            p.chmod(0o644)
            p.write_bytes(self.pristine[p.name])

    def gate(self):
        return vgi.idx_integrity(self.repo, "t")

    @staticmethod
    def corrupt(p: Path, data: bytes):
        p.chmod(0o644)
        p.write_bytes(data)

    def test_pristine_passes(self):
        g = self.gate()
        self.assertTrue(g["pass"], g["rows"])
        self.assertTrue(g["count"])

    def test_whole_idx_zeroed_is_detected(self):
        for p in vgi.idx_files(self.repo):
            self.corrupt(p, b"\0" * self.pristine[p.name].__len__())
        rows = vgi.run_idx_check(self.repo, "idx_integrity")
        # The historical shape: show-index is happy with a zeroed idx, so verify-pack has to carry it.
        self.assertTrue(all(r["show_index"]["rc"] == 0 for r in rows), rows)
        self.assertFalse(self.gate()["pass"])

    def test_trailer_byte_flip_is_detected(self):
        for p in vgi.idx_files(self.repo):
            b = bytearray(self.pristine[p.name])
            b[-1] ^= 0xFF
            self.corrupt(p, bytes(b))
        rows = vgi.run_idx_check(self.repo, "idx_integrity")
        self.assertTrue(all(r["show_index"]["rc"] == 0 for r in rows), rows)
        self.assertFalse(self.gate()["pass"])

    def test_missing_pack_is_detected(self):
        packs = [p.with_suffix(".pack") for p in vgi.idx_files(self.repo)]
        saved = [p.read_bytes() for p in packs]
        for p in packs:
            p.unlink()
        try:
            g = self.gate()
            self.assertFalse(g["pass"])
            self.assertTrue(all(any("sibling .pack absent" in w for w in row["why_fail"])
                                for row in g["rows"]))
        finally:
            for p, b in zip(packs, saved):
                p.write_bytes(b)

    def test_no_idx_is_a_fail_not_a_vacuous_pass(self):
        d = vgi.pack_dir(self.repo)
        saved = {p.name: p.read_bytes() for p in d.iterdir() if p.is_file()}
        for p in list(d.iterdir()):
            p.unlink()
        try:
            self.assertFalse(self.gate()["pass"])
        finally:
            for name, b in saved.items():
                (d / name).write_bytes(b)

    def test_pack_verify_op_reads_the_nested_exit_code(self):
        """`verify-pack` rows carry their rc inside `verify_pack`, so reading a top-level `rc`
        made every `pack.verify` op report a false failure."""
        win = SimpleNamespace(ops=[{"name": "pack.verify", "check": "verify-pack"}])
        run = vgi.Run(self.root / "oplog.jsonl")
        out = vgi.run_window(win, self.repo, "t", run)
        run.close()
        self.assertEqual(out["failed_ops"], [])
        self.assertEqual(out["executed"], 1)
        self.assertEqual(out["results"][0]["rc"], 0)


class CookieGate(unittest.TestCase):
    """The cookie probe must fail when both arms leave files behind, not merely when they agree."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="r21-ck-"))
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)

    def test_matching_arms_pass(self):
        r = vgi.readdir_cookie_probe(self.root, self.root, 64)
        self.assertTrue(r["pass"], r)
        self.assertEqual(r["native"]["remaining"], 0)

    def test_both_arms_nonzero_fails(self):
        """With the unlink neutered both arms leave files behind and still agree. Agreement alone
        must not read as integrity."""
        with mock.patch.object(vgi.os, "unlink", lambda p: None):
            r = vgi.readdir_cookie_probe(self.root, self.root, 64)
        self.assertEqual(r["native"]["remaining"], 64)
        self.assertEqual(r["mount"]["remaining"], 64)
        self.assertFalse(r["pass"])

    def test_the_scan_really_pages(self):
        """One libc buffer would drain without ever resuming from a cookie, proving nothing."""
        d = self.root / "d"
        d.mkdir()
        for i in range(400):
            (d / f"f{i:04d}").write_text("x")
        r = vgi._getdents_scan(d, 512, unlink_inside=False)
        self.assertEqual(r["names"], 400)
        self.assertGreater(r["pages"], 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)