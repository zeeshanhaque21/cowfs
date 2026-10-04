#!/usr/bin/env python3
"""Unit tests for the g4 fsx gate runner: the parsing and the decision, without a mount.

    python3 -m unittest discover -s bench/fsx-gate -p 'test_*.py'

These cover the rules that make the gate honest: a nonzero exit is a failure, an empty or
missing result is a failure, a capability the filesystem lacks is recorded rather than counted,
and a required operation that never happened is a failure unless the tool said why.
"""

import importlib.util
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("run_fsx_gate", os.path.join(HERE, "run-fsx-gate.py"))
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

CAPS = {"max_file_bytes": 262144, "max_op_bytes": 16384}


def case(**kw):
    base = {"kind": "case", "mode": "mixed", "seed": 1, "arm": "cowfs", "exit": 0,
            "timed_out": False, "seconds": 1.0, "ops_declared": 10, "ops_executed": 10,
            "data_path": "/tmp/fsx.dat", "data_sha256": "aa", "data_size": 100,
            "ops_file": "/tmp/fsx.dat.fsxops", "ops_sha256": "bb",
            "op_counts": {"write": 4, "read": 3, "mapread": 1, "mapwrite": 1, "truncate": 1},
            "fsx_reported_unsupported": []}
    base.update(kw)
    return base


def compare(**kw):
    base = {"kind": "compare", "mode": "mixed", "seed": 1, "ops_declared": 10,
            "ops_executed": {"native": 10, "cowfs": 10},
            "data_sha256": {"native": "aa", "cowfs": "aa"},
            "data_size": {"native": 100, "cowfs": 100},
            "data_st_dev": {"native": 1, "cowfs": 2},
            "ops_stream_sha256": {"native": "bb", "cowfs": "bb"},
            "ops_stream_match": True, "hashes_compared": True,
            "op_count_deltas": {}, "skip_delta": {"native": 0, "cowfs": 0},
            "explained_by_capability": [], "consequent_deltas": [], "hash_comparison": "compared",
            "require_identical_op_stream": True,
            "cowfs_capability_gaps": [], "native_capability_gaps": [],
            "fresh_open_sha256": {"cowfs": "aa"}, "problems": []}
    base.update(kw)
    return base


class ParseOpsExecuted(unittest.TestCase):
    def test_reads_the_count_fsx_reported(self):
        self.assertEqual(gate.parse_ops_executed("All 200 operations completed A-OK!\n"), 200)

    def test_takes_the_last_report_not_the_first(self):
        self.assertEqual(gate.parse_ops_executed("All 5 operations completed A-OK!\nAll 9 operations completed A-OK!\n"), 9)

    def test_absent_report_is_none_so_a_run_cannot_pass_on_a_guess(self):
        self.assertIsNone(gate.parse_ops_executed("seed 1\n"))
        self.assertIsNone(gate.parse_ops_executed(""))


class ParseLogDump(unittest.TestCase):
    def test_total_operations(self):
        self.assertEqual(gate.parse_log_dump_total("LOG DUMP (10000 total operations):\n"), 10000)

    def test_absent(self):
        self.assertIsNone(gate.parse_log_dump_total("no dump here"))


class ParseDisabledModes(unittest.TestCase):
    def test_keeps_the_tools_own_wording_and_the_mode(self):
        line = ("main: filesystem does not support fallocate mode FALLOC_FL_PUNCH_HOLE | "
                "FALLOC_FL_KEEP_SIZE, disabling!\n")
        got = gate.parse_disabled_modes(line)
        self.assertEqual(len(got), 1)
        self.assertIn("FALLOC_FL_PUNCH_HOLE", got[0])
        self.assertIn("disabling", got[0])

    def test_a_clean_run_reports_nothing(self):
        self.assertEqual(gate.parse_disabled_modes("All 10 operations completed A-OK!\n"), [])


class OpCounts(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="fsx-gate-test-")

    def write(self, text):
        path = os.path.join(self.dir, "fsx.dat.fsxops")
        with open(path, "w") as f:
            f.write(text)
        return path

    def test_counts_each_op_and_keeps_skips_apart(self):
        path = self.write("write 0x0 0x10\nread 0x0 0x10\nread 0x20 0x10\nskip\ntruncate 0x0\n")
        self.assertEqual(gate.op_counts(path),
                         {"write": 1, "read": 2, "truncate": 1, "skip": 1})

    def test_longer_names_are_not_counted_as_their_prefix(self):
        path = self.write("read_dontcache 0x0 0x10\nwrite_dontcache 0x0 0x10\nmapread 0x0 0x10\n")
        counts = gate.op_counts(path)
        self.assertEqual(counts.get("read"), None)
        self.assertEqual(counts.get("write"), None)
        self.assertEqual(counts["read_dontcache"], 1)
        self.assertEqual(counts["mapread"], 1)

    def test_a_missing_ops_file_is_an_error_not_an_empty_pass(self):
        counts = gate.op_counts(os.path.join(self.dir, "absent.fsxops"))
        self.assertIn("error", counts)


class Sha256File(unittest.TestCase):
    def test_missing_file_is_a_reason_not_a_digest(self):
        digest, detail = gate.sha256_file("/nonexistent/fsx.dat")
        self.assertIsNone(digest)
        self.assertIn("fsx.dat", detail)

    def test_digest_and_size(self):
        with tempfile.NamedTemporaryFile(delete=False) as f:
            f.write(b"abc" * 100)
            path = f.name
        digest, size = gate.sha256_file(path)
        os.unlink(path)
        self.assertEqual(size, 300)
        self.assertEqual(len(digest), 64)


class FsxArgv(unittest.TestCase):
    def test_both_arms_get_the_same_argv_except_the_attempt_directory(self):
        mode = {"name": "mixed", "flags": ["-f"]}
        native = gate.fsx_argv("/bin/fsx", mode["flags"], 3, 2000, CAPS, "/native/att", "fsx.dat")
        cowfs = gate.fsx_argv("/bin/fsx", mode["flags"], 3, 2000, CAPS, "/cowfs/att", "fsx.dat")
        # The attempt directory appears twice, in -P and in the data path, and it is the only
        # thing that differs: the matched part of the gate is the argv, not the location.
        self.assertEqual([a.replace("/native/", "/ARM/") for a in native],
                         [a.replace("/cowfs/", "/ARM/") for a in cowfs])
        self.assertNotEqual(native, cowfs)
        for token in ("-S", "3", "-N", "2000", "-l", "262144", "-o", "16384", "-f", "--record-ops"):
            self.assertIn(token, native)

    def test_the_declared_caps_reach_the_command_line(self):
        argv = gate.fsx_argv("/bin/fsx", [], 1, 5, CAPS, "/a", "fsx.dat")
        self.assertEqual(argv[argv.index("-l") + 1], "262144")
        self.assertEqual(argv[argv.index("-o") + 1], "16384")


class CompareCase(unittest.TestCase):
    def compare(self, native=None, cowfs=None, fresh=None, ops=10, probe=None):
        return gate.compare_case({"name": "mixed"}, 1, ops,
                                 native or case(arm="native", data_st_dev=1),
                                 cowfs or case(data_st_dev=2),
                                 fresh or {"cowfs": "aa"}, probe)

    def test_matching_arms_have_no_problems(self):
        self.assertEqual(self.compare()["problems"], [])

    def test_two_arms_on_one_device_are_a_contaminated_run(self):
        # The false pass this catches: fsx wrote both arms inside the evidence directory, so the
        # "cowfs" arm never touched the mount and matched native by construction.
        got = self.compare(cowfs=case(data_st_dev=1, data_sha256="aa"))
        self.assertTrue(any("same filesystem" in p for p in got["problems"]), got["problems"])

    def test_an_identical_op_stream_must_produce_identical_bytes(self):
        got = self.compare(cowfs=case(data_sha256="cc"))
        self.assertTrue(any("identical op streams produced different bytes" in p for p in got["problems"]))

    def test_a_hole_op_missing_on_one_arm_is_explained_by_the_probe(self):
        native = case(arm="native", data_st_dev=1, ops_sha256="bb",
                      op_counts={"write": 4, "read": 3, "punch_hole": 2, "skip": 0})
        cowfs = case(data_st_dev=2, ops_sha256="cc",
                     op_counts={"write": 4, "read": 3, "skip": 2})
        probe = {"cowfs": {"punch_hole": False}}
        got = self.compare(native=native, cowfs=cowfs, probe=probe)
        self.assertIn("punch_hole", got["explained_by_capability"])
        self.assertFalse(got["ops_stream_match"])
        self.assertFalse(got["hashes_compared"])
        self.assertEqual(got["problems"], [])

    def test_a_non_hole_op_delta_with_no_gap_is_unexplained_and_fails(self):
            native = case(arm="native", data_st_dev=1, ops_sha256="bb",
                          op_counts={"write": 4, "read": 3, "mapread": 2, "skip": 0})
            cowfs = case(data_st_dev=2, ops_sha256="cc",
                         op_counts={"write": 4, "read": 3, "skip": 0})
            got = self.compare(native=native, cowfs=cowfs)
            self.assertEqual(got["consequent_deltas"], ["mapread"])
            self.assertTrue(any("no capability gap" in p for p in got["problems"]))

    def test_a_skip_delta_alone_never_excuses_a_different_op(self):
            # The rule this replaced let any difference smaller than the skip count pass as
            # "absorbed", which would have excused a stream that diverged for any reason at all.
            native = case(arm="native", data_st_dev=1, ops_sha256="bb",
                          op_counts={"write": 40, "read": 30, "mapread": 10, "skip": 100})
            cowfs = case(data_st_dev=2, ops_sha256="cc",
                         op_counts={"write": 41, "read": 30, "mapread": 9, "skip": 200})
            got = self.compare(native=native, cowfs=cowfs)
            self.assertTrue(any("no capability gap" in p for p in got["problems"]), got["problems"])

    def test_a_mode_that_declares_one_mix_fails_on_any_stream_difference(self):
            native = case(arm="native", data_st_dev=1, ops_sha256="bb",
                          op_counts={"write": 4, "punch_hole": 2})
            cowfs = case(data_st_dev=2, ops_sha256="cc", op_counts={"write": 4})
            probe = {"cowfs": {"punch_hole": False}}
            got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                    native, cowfs, {"cowfs": "aa"}, probe)
            self.assertTrue(any("declares one operation mix" in p for p in got["problems"]), got["problems"])

    def test_a_nonzero_exit_is_a_problem(self):
        got = self.compare(cowfs=case(exit=201))["problems"]
        self.assertIn("cowfs fsx exited 201", got)

    def test_a_timeout_is_a_problem(self):
        got = self.compare(cowfs=case(timed_out=True, exit=None))["problems"]
        self.assertTrue(any("timed out" in p for p in got))

    def test_a_missing_op_count_is_a_problem(self):
        got = self.compare(cowfs=case(ops_executed=None))["problems"]
        self.assertIn("cowfs fsx never reported its op count", got)

    def test_fewer_ops_than_declared_is_a_problem(self):
        got = self.compare(cowfs=case(ops_executed=7), ops=10)["problems"]
        self.assertIn("cowfs fsx executed 7 ops, 10 declared", got)

    def test_differing_bytes_are_a_problem_when_the_streams_match(self):
        got = self.compare(cowfs=case(data_sha256="cc"))["problems"]
        self.assertTrue(any("identical op streams produced different bytes" in p for p in got), got)

    def test_an_empty_result_is_a_problem(self):
        got = self.compare(cowfs=case(data_size=0))["problems"]
        self.assertIn("cowfs data file is empty", got)

    def test_a_differing_op_stream_alone_is_not_a_failure(self):
        # fsx picks its operations after probing the filesystem, so two honest arms may record
        # different streams. What has to hold is that the difference is explained, not absent.
        got = self.compare(cowfs=case(ops_sha256="dd"))
        self.assertEqual(got["problems"], [])
        self.assertFalse(got["ops_stream_match"])

    def test_a_fresh_open_that_reads_something_else_is_a_problem(self):
        got = self.compare(fresh={"cowfs": "ee"})["problems"]
        self.assertTrue(any("fresh open read ee" in p for p in got))


class Verdict(unittest.TestCase):
    REQUIRED = {"mixed": ["read", "write", "mapread", "mapwrite", "truncate"]}

    def verdict(self, cases=None, compares=None, restarts=None, probe=None, required=None):
        return gate.verdict(cases if cases is not None else [case()],
                            compares if compares is not None else [compare()],
                            restarts or [], required if required is not None else self.REQUIRED,
                            probe or [])

    def test_a_clean_run_passes(self):
        self.assertEqual(self.verdict()["status"], "PASS")

    def test_no_case_at_all_fails_rather_than_passing(self):
        got = gate.verdict([], [], [], {}, [])
        self.assertEqual(got["status"], "FAIL")
        self.assertIn("no case ran", got["failures"])

    def test_a_failing_compare_fails(self):
        got = self.verdict(compares=[compare(problems=["cowfs fsx exited 201"])])
        self.assertEqual(got["status"], "FAIL")
        self.assertIn("mixed seed 1: cowfs fsx exited 201", got["failures"])

    def test_an_unsupported_probe_alone_does_not_fail_or_pass_by_itself(self):
        probe = [{"arm": "cowfs", "op": "punch_hole", "ok": False, "detail": "EOPNOTSUPP"}]
        got = self.verdict(probe=probe)
        self.assertEqual(got["status"], "PASS")
        self.assertEqual(got["unsupported"], ["EOPNOTSUPP"])

    def test_a_required_op_that_never_happened_fails(self):
        got = self.verdict(cases=[case(op_counts={"write": 4, "read": 3})])
        self.assertEqual(got["status"], "FAIL")
        self.assertTrue(any("never performed mapread" in f for f in got["failures"]))

    def test_a_hole_op_the_filesystem_lacks_is_a_recorded_gap_not_a_pass_or_a_failure(self):
        counts = {"write": 4, "read": 3, "mapread": 1, "mapwrite": 1, "truncate": 1, "zero_range": 1}
        cases = [case(op_counts=counts),
                 case(arm="native", op_counts=counts,
                      fsx_reported_unsupported=["fallocate mode FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, disabling!"]),
                 case(op_counts=counts,
                      fsx_reported_unsupported=["fallocate mode FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, disabling!"])]
        got = self.verdict(cases=cases, required={"mixed": ["punch_hole"]})
        self.assertEqual(got["status"], "PASS")
        self.assertEqual(got["required_ops_missing"], [])
        self.assertEqual(len(got["capability_gaps"]), 1)
        self.assertIn("never performed punch_hole", got["capability_gaps"][0])
        self.assertIn("fsx reported", got["capability_gaps"][0])

    def test_the_probe_alone_can_explain_a_missing_hole_op(self):
        probe = [{"arm": "cowfs", "op": "punch_hole", "ok": False, "detail": "EOPNOTSUPP"}]
        got = self.verdict(probe=probe, required={"mixed": ["punch_hole"]})
        self.assertEqual(got["status"], "PASS")
        self.assertEqual(len(got["capability_gaps"]), 1)

    def test_a_restart_leg_that_failed_fails(self):
        got = self.verdict(restarts=[{"exit": 1, "error": "unmount failed", "problems": []}])
        self.assertEqual(got["status"], "FAIL")
        self.assertTrue(any("restart leg exited 1" in f for f in got["failures"]))

    def test_a_restart_that_changed_bytes_fails(self):
        got = self.verdict(restarts=[{"exit": 0, "problems": ["fsx.dat read cc after the restart, aa before"]}])
        self.assertEqual(got["status"], "FAIL")

    def test_a_clean_restart_still_passes(self):
        got = self.verdict(restarts=[{"exit": 0, "problems": []}])
        self.assertEqual(got["status"], "PASS")


class MountIdentity(unittest.TestCase):
    def test_a_path_resolves_to_its_own_mount(self):
        found = gate.mount_identity("/")
        self.assertIsInstance(found, dict)
        self.assertEqual(found["mountpoint"], "/")
        self.assertTrue(found["fstype"])

    def test_a_directory_reports_the_filesystem_that_holds_it(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-native-") as tmp:
            found = gate.mount_identity(tmp)
        self.assertIsInstance(found, dict)
        self.assertTrue(found["mountpoint"], "a plain directory still sits on some filesystem")
        self.assertNotIn("fuse", found["fstype"])

    def test_a_nonexistent_path_still_names_the_filesystem_that_would_hold_it(self):
        # realpath of a missing path is itself, so this reports the enclosing filesystem rather
        # than failing. The gate's own directory checks are what keep a missing root out.
        found = gate.mount_identity("/nonexistent/gate/path")
        self.assertIsInstance(found, dict)
        self.assertTrue(found["mountpoint"].startswith("/"))


class FsxIdentity(unittest.TestCase):
    def test_a_missing_binary_is_not_an_identity(self):
        self.assertIsNone(gate.fsx_identity("/nonexistent/fsx"))

    def test_flags_come_from_the_binary_own_usage_text(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-fakefsx-") as tmp:
            fake = os.path.join(tmp, "fsx")
            with open(fake, "w") as f:
                f.write("#!/bin/sh\nprintf 'usage: fsx [-ad]\\n\\t-H: no punch hole\\n\\t-z: no zero range\\n'\nexit 90\n")
            os.chmod(fake, 0o755)
            identity = gate.fsx_identity(fake)
        self.assertEqual(identity["usage_exit"], 90)
        self.assertEqual(identity["flags"], ["H", "z"])
        self.assertEqual(len(identity["sha256"]), 64)


class Summary(unittest.TestCase):
    def test_writes_a_table_with_the_verdict(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-summary-") as tmp:
            path = os.path.join(tmp, "summary.md")
            result = {"status": "FAIL", "cases": 1, "failures": ["boom"],
                      "unsupported": ["EOPNOTSUPP"], "required_ops_missing": []}
            gate.summary(path, result, {"fsx": {"sha256": "ab"}, "roots": {}}, [compare()])
            with open(path) as f:
                text = f.read()
        self.assertIn("status: FAIL", text)
        self.assertIn("| mixed | 1 |", text)
        self.assertIn("EOPNOTSUPP", text)
        self.assertIn("boom", text)


class Probe(unittest.TestCase):
    def test_probes_run_and_report_every_declared_op(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-probe-") as tmp:
            rows = gate.Probe(tmp, "native").run()
        names = {r["op"] for r in rows}
        for op in ("fallocate", "punch_hole", "zero_range", "keep_size",
                   "truncate_shrink", "truncate_grow", "truncate_to_hole", "mmap_rw", "fsync_readback"):
            self.assertIn(op, names, "%s was not probed" % op)
        for r in rows:
            self.assertIn("ok", r)
            self.assertTrue(r["detail"], "%s has no detail" % r["op"])

    def test_truncate_hole_and_mmap_and_fsync_pass_on_a_native_directory(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-probe2-") as tmp:
            rows = {r["op"]: r for r in gate.Probe(tmp, "native").run()}
        for op in ("truncate_shrink", "truncate_grow", "truncate_to_hole", "mmap_rw", "fsync_readback"):
            self.assertTrue(rows[op]["ok"], "%s: %s" % (op, rows[op]["detail"]))

    def test_the_probe_leaves_nothing_behind(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-probe3-") as tmp:
            gate.Probe(tmp, "native").run()
            self.assertEqual(os.listdir(tmp), [])


if __name__ == "__main__":
    unittest.main()
