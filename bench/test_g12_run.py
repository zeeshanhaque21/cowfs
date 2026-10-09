"""Non-timed checks of bench/g12_run.py validation logic. Starts no daemon, builds nothing."""
import hashlib
import json
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

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


PS = """  1     0  0.0 launchd
100     1 20.0 python3
101   100 390.0 cargo
102   101 380.0 rustc
200     1 400.0 someone-elses-build
201   200  10.0 cc1
"""


def cargo_json(name, exe, opt="3", da=False):
    return json.dumps({"reason": "compiler-artifact", "target": {"name": name}, "executable": exe,
                       "profile": {"opt_level": opt, "debug_assertions": da}})


def fake_sh(lines, rc=0):
    return lambda *a, **k: types.SimpleNamespace(returncode=rc, stdout="\n".join(["{not json", *lines]) + "\n", stderr="")


class Foreign(unittest.TestCase):
    def test_own_tree_high_foreign_low_is_valid(self):
        rows = g.parse_ps(PS.replace("400.0 someone-elses-build", "3.0 someone-elses-build"))
        f, ind, top = g.foreign_cpu(rows, [100])
        self.assertAlmostEqual(f, 13.0)  # launchd 0 + 3.0 + 10.0
        self.assertEqual(top[0][0], "cc1")
        self.assertEqual(g.foreign_problems([f] * 5, 0.0 + 20 + g.FOREIGN_MARGIN), [])

    def test_foreign_high_is_invalid(self):
        f, ind, top = g.foreign_cpu(g.parse_ps(PS), [100])
        self.assertAlmostEqual(f, 410.0)
        self.assertTrue(g.foreign_problems([f] * 5, 20 + g.FOREIGN_MARGIN))
        self.assertTrue(g.foreign_problems([], 100))

    def test_induced_cpu_is_reported_not_gated(self):
        rows = g.parse_ps(PS.replace("400.0 someone-elses-build", "3.0 someone-elses-build") + "300     1 250.0 /kernel/kernel_task\n301     1 90.0 mds_stores\n")
        f, ind, top = g.foreign_cpu(rows, [100])
        self.assertAlmostEqual(f, 13.0)
        self.assertAlmostEqual(ind, 340.0)
        self.assertEqual(g.foreign_problems([f] * 4, 70), [])
        f2, _, _ = g.foreign_cpu(g.parse_ps(PS), [100])  # real foreign load still breaches
        self.assertTrue(g.foreign_problems([f2] * 4, 70))

    def test_cool_down(self):
        loads = iter([9, 9, 3.9, 3.0, 3.1, 3.0, 3.1, 3.0, 3.1, 3.0])
        self.assertEqual(g.cool_down(4.0, 600, getload=lambda: next(loads), sleep=lambda s: None), (True, 3.0))
        steady_but_decaying = iter([3.9 - 0.2 * i for i in range(30)])  # at/below the cap but still falling
        t = iter(range(0, 10000, 10))
        ok, _ = g.cool_down(4.0, 25, getload=lambda: next(steady_but_decaying), sleep=lambda s: None, clock=lambda: next(t))
        self.assertFalse(ok)
        t = iter(range(0, 10000, 100))
        ok, load = g.cool_down(4.0, 300, getload=lambda: 9, sleep=lambda s: None, clock=lambda: next(t))
        self.assertFalse(ok)


class Build(unittest.TestCase):
    def test_release_json_accepted(self):
        with mock.patch.object(g, "sh", fake_sh([cargo_json("cowfs-daemon", "/x/target/release/cowfs-daemon"),
                                                 cargo_json("cowfs", "/x/target/release/cowfs")])):
            self.assertEqual(set(g.build_release()), {"cowfs-daemon", "cowfs"})

    def test_debug_variants_rejected(self):
        ok = cargo_json("cowfs", "/x/target/release/cowfs")
        for bad in (cargo_json("cowfs-daemon", "/x/target/debug/cowfs-daemon", "0", True),
                    cargo_json("cowfs-daemon", "/x/target/release/cowfs-daemon", "0"),
                    cargo_json("cowfs-daemon", "/x/target/release/cowfs-daemon", "3", True)):
            with mock.patch.object(g, "sh", fake_sh([bad, ok])), self.assertRaises(SystemExit):
                g.build_release()
        with mock.patch.object(g, "sh", fake_sh([ok])), self.assertRaises(SystemExit):  # daemon artifact missing
            g.build_release()
        with mock.patch.object(g, "sh", fake_sh([], rc=101)), self.assertRaises(SystemExit):
            g.build_release()


class Files(unittest.TestCase):
    def test_newest_is_anchored(self):
        with tempfile.TemporaryDirectory() as t:
            out = Path(t) / "out"
            out.mkdir()
            (out / "g12-x-n1-20261009-010101.jsonl").write_text("")
            (out / "g12-x-n1-n1-20261009-020202.jsonl").write_text("")  # run id x-n1, arm n1
            with mock.patch.object(g, "BENCH", Path(t)):
                self.assertEqual(g.newest("g12-x-n1").name, "g12-x-n1-20261009-010101.jsonl")

    def test_stale_lock_reclaimed_and_live_lock_respected(self):
        with tempfile.TemporaryDirectory() as t:
            lock = Path(t) / "cpu.lock"
            lock.mkdir()
            (lock / "owner").write_text("u pid 999999 at now")
            with mock.patch.object(g, "LOCK", lock), mock.patch.object(g, "pid_alive", lambda p: False):
                g.acquire_lock(0)
                self.assertTrue(lock.is_dir())
            (lock / "owner").write_text("u pid 1 at now")
            with mock.patch.object(g, "LOCK", lock), mock.patch.object(g, "pid_alive", lambda p: True), mock.patch.object(g.time, "sleep", lambda s: None), self.assertRaises(SystemExit):
                g.acquire_lock(-1)


class FakeDaemon:
    launched = True
    pre = []

    def __init__(self, out, args, arts):
        self.proc = types.SimpleNamespace(pid=4242)
        self.mnt = out

    def start(self):
        return Path(self.mnt) / "cowfs-root"

    def prov(self):
        return {"launched_from_built_binary": self.launched, "mount_fstype": "nfs"}

    def arm_state(self, dev):
        return {"problems": list(self.pre)}

    def stop(self):
        return []


class FakeQuiet:
    ok = True
    good = True

    def __init__(self, args):
        self.foreign_limit = 100.0
        self.limit = 4.0
        self.base = {}

    def baseline(self):
        return self.ok

    def check(self, pid):
        return self.good, {}


class FakeSampler:
    value = 0.0

    def __init__(self, roots):
        pass

    def start(self):
        pass

    def finish(self):
        return [(self.value, 0.0, [])] * 3


class RunPaths(unittest.TestCase):
    def go(self, **over):
        FakeDaemon.launched, FakeDaemon.pre, FakeQuiet.ok, FakeQuiet.good, FakeSampler.value = True, [], True, True, 0.0
        for k, v in over.items():
            for cls in (FakeDaemon, FakeQuiet, FakeSampler):
                if hasattr(cls, k):
                    setattr(cls, k, v)
        calls = []
        args = types.SimpleNamespace(run_id="t", scale=100, gates="g1,g2,g3", sample=False, reps=5, load_cap=4.0, baseline_window=300, wait=0)
        verdict = {"problems": []}
        with tempfile.TemporaryDirectory() as t, mock.patch.object(g, "acquire_lock", lambda w: None), \
                mock.patch.object(g, "release_lock", lambda: None), mock.patch.object(g, "build_release", lambda: {}), \
                mock.patch.object(g, "Daemon", FakeDaemon), mock.patch.object(g, "Quiet", FakeQuiet), \
                mock.patch.object(g, "ArmSampler", FakeSampler), mock.patch.object(g, "cool_down", lambda *a, **k: (True, 0.5)), \
                mock.patch.object(g, "fstype", lambda p: "apfs"), mock.patch.object(g, "LOCK", Path(t) / "lock"), \
                mock.patch.object(g.subprocess, "run", lambda cmd, **k: calls.append(cmd) or types.SimpleNamespace(returncode=0)):
            (Path(t) / "lock").mkdir()
            res = g.run(args, Path(t), verdict)
        return res, verdict, calls

    def test_baseline_refused_stops_before_any_arm(self):
        res, v, calls = self.go(ok=False)
        self.assertEqual((res, calls), ("INVALID", []))

    def test_daemon_not_from_built_binary_stops(self):
        res, v, calls = self.go(launched=False)
        self.assertEqual((res, calls), ("INVALID", []))
        self.assertTrue(v["problems"])

    def test_host_not_quiet_stops(self):
        res, v, calls = self.go(good=False)
        self.assertEqual((res, calls), ("INVALID", []))

    def test_cowfs_arm_unsound_stops_after_native_arm(self):
        res, v, calls = self.go(pre=["daemon not running"])
        self.assertEqual((res, len(calls)), ("INVALID", 1))  # only n1 ran

    def test_foreign_cpu_high_stops_the_arm(self):
        res, v, calls = self.go(value=500.0)
        self.assertEqual((res, len(calls)), ("INVALID", 1))
        self.assertTrue(any("foreign CPU" in p for p in v["problems"]))


if __name__ == "__main__":
    unittest.main()
