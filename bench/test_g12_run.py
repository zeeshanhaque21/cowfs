"""Non-timed checks of bench/g12_run.py validation logic. Starts no daemon, builds nothing."""
import hashlib
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import g12_run as g  # noqa: E402

REL = {"executable": "/x/target/release/cowfs-daemon", "profile": {"opt_level": "3", "debug_assertions": False}}
DBG = {"executable": "/x/target/debug/cowfs-daemon", "profile": {"opt_level": "0", "debug_assertions": True}}
TABLE = """/dev/disk3s1s1 on / (apfs, sealed, local, read-only)
localhost:/cowfs-abc on /a/mnt (nfs, nodev, nosuid, mounted by u)
/dev/disk3s5 on /System/Volumes/Data (apfs, local)"""


class G12(unittest.TestCase):
    def test_release_accepted_debug_rejected(self):
        self.assertEqual(g.profile_problems(REL), [])
        self.assertEqual(len(g.profile_problems(DBG)), 3)
        self.assertTrue(g.profile_problems({**REL, "executable": "/x/target/debug/cowfs-daemon"}))
        self.assertTrue(g.profile_problems({**REL, "profile": {"opt_level": "3"}}))  # unknown debug_assertions

    def test_digest_mismatch_and_missing(self):
        with tempfile.TemporaryDirectory() as t:
            f = Path(t) / "b"
            f.write_bytes(b"x")
            want = hashlib.sha256(b"x").hexdigest()
            self.assertEqual(g.digest_problems(f, want), [])
            f.write_bytes(b"y")
            self.assertTrue(g.digest_problems(f, want))
            self.assertTrue(g.digest_problems(Path(t) / "none", want))

    def test_params(self):
        self.assertEqual(g.param_problems(["g3"], 1, 1, 1000, True), [])
        self.assertEqual(g.param_problems(["g1", "g2", "g3"], 100, 5, 4.0, False), [])
        self.assertEqual(len(g.param_problems(["g3"], 1, 1, 1000, False)), 4)
        self.assertTrue(g.param_problems(["g1", "g3"], 100, 5, 4.0, False))  # g2 missing
        self.assertTrue(g.param_problems(["g1", "g2", "g3"], 100, 5, 4.0, False, baseline_window=10))

    def test_arm_problems(self):
        ok = dict(alive=True, kind="nfs", source="localhost:/cowfs-abc", dev=5, native_dev=1, digest_bad=[])
        self.assertEqual(g.arm_problems(**ok), [])
        self.assertTrue(g.arm_problems(**{**ok, "alive": False}))
        self.assertTrue(g.arm_problems(**{**ok, "kind": "apfs", "source": "/dev/disk3s5"}))
        self.assertTrue(g.arm_problems(**{**ok, "dev": 1}))
        self.assertTrue(g.arm_problems(**{**ok, "digest_bad": ["x"]}))

    def test_mount_entry_longest_prefix(self):
        self.assertEqual(g.mount_entry("/a/mnt/base/g12", TABLE), ("localhost:/cowfs-abc", "nfs", "/a/mnt"))
        self.assertEqual(g.mount_entry("/System/Volumes/Data/x", TABLE)[1], "apfs")

    def test_stale_lock(self):
        self.assertTrue(g.stale_lock("u pid 99 at now", lambda p: False))
        self.assertFalse(g.stale_lock("u pid 99 at now", lambda p: True))
        self.assertFalse(g.stale_lock("garbage", lambda p: False))
        self.assertTrue(g.stale_lock("pid 0", g.pid_alive))  # owner-less lock marker

    def test_ratio_table(self):
        n = [{"gate": "g1", "wall_s": 2.0}]
        c = [{"gate": "g1", "wall_s": 3.0}]
        self.assertEqual(g.ratio_table(n, c, ["g1", "g2"]), {"g1": 1.5, "g2": None})


if __name__ == "__main__":
    unittest.main()
