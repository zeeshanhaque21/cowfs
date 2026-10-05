#!/usr/bin/env python3
"""Unit tests for the g4 fsx gate runner: the parsing and the decision, without a mount.

    python3 -m unittest discover -s bench/fsx-gate -p 'test_*.py'

These cover the rules that make the gate honest: a nonzero exit is a failure, an empty or
missing result is a failure, a capability the filesystem lacks is recorded rather than counted,
and a required operation that never happened is a failure unless the tool said why.
"""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("run_fsx_gate", os.path.join(HERE, "run-fsx-gate.py"))
gate = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(gate)

CAPS = {"max_file_bytes": 262144, "max_op_bytes": 16384}


def case(**kw):
    """A case row as the new run_case writes one: witnesses, freshness and the cap field."""
    base = {"kind": "case", "mode": "mixed", "seed": 1, "arm": "cowfs", "exit": 0,
            "timed_out": False, "seconds": 1.0, "run_tag": "t-1",
            "ops_requested": 10, "ops_executed": 10, "log_dump_total": 10,
            "case_dir": "/tmp/fsx.dat", "case_dir_fresh": True,
            "data_path": "/tmp/fsx.dat", "data_realpath": "/tmp/fsx.dat",
            "data_sha256": "aa", "data_size": 100, "data_real_fstype": "fuse.cowfs",
            "data_st_dev": 234, "root_st_dev": 234, "over_cap": [],
            "ops_file": "/tmp/fsx.dat.fsxops", "ops_sha256": "bb",
            "op_counts": {"write": 4, "read": 3, "mapread": 1, "mapwrite": 1, "truncate": 1},
            "op_sequence": ["write", "read", "mapread", "mapwrite", "truncate"],
            "op_skips": {}, "op_stream_error": None,
            "fsx_reported_unsupported": []}
    base.update(kw)
    return base


def cmp_case(**kw):
    """A compare row as the new compare_case writes one."""
    base = {"kind": "compare", "mode": "matched", "seed": 1, "status": "PASS",
            "require_identical_op_stream": True, "ops_requested": 10,
            "ops_executed": {"native": 10, "cowfs": 10}, "ops_skipped": {"native": 0, "cowfs": 0},
            "unsupported_reasons": {"native": [], "cowfs": [], "probe": []},
            "data_sha256": {"native": "aa", "cowfs": "aa"},
            "data_size": {"native": 100, "cowfs": 100},
            "data_st_dev": {"native": 2050, "cowfs": 234},
            "data_fstype": {"native": "ext4", "cowfs": "fuse.cowfs"},
            "ops_stream_sha256": {"native": "bb", "cowfs": "bb"},
            "ops_stream_match": True, "hashes_compared": True,
            "hash_comparison": "compared, streams identical",
            "op_count_deltas": {}, "skip_delta": {"native": 0, "cowfs": 0},
            "deltas_explained_by_capability": [], "deltas_unexplained": [],
            "byte_bearing_deltas_unexplained": [],
            "cowfs_capability_gaps": [], "native_capability_gaps": [],
            "fresh_open_sha256": {"cowfs": {"sha256": "aa", "size": 100}},
            "unmeasurable": [], "problems": []}
    base.update(kw)
    return base


class Attribution(unittest.TestCase):
    def test_every_delta_is_attributed_on_its_own(self):
        explained, unexplained = gate.attribute_deltas(
            {"read": {"native": 10, "cowfs": 9}, "punch_hole": {"native": 5, "cowfs": 0}},
            {"punch_hole"})
        self.assertEqual(explained, ["punch_hole"])
        self.assertEqual(unexplained, ["read"])

    def test_the_whole_fallocate_family_is_in_the_map(self):
        for op in ("punch_hole", "zero_range", "write_zeroes", "fallocate",
                   "collapse_range", "insert_range"):
            self.assertIn(op, gate.OP_CAPABILITY, op)

    def test_byte_bearing_ops_are_named(self):
        for op in ("read", "write", "mapread", "mapwrite", "truncate"):
            self.assertIn(op, gate.BYTE_BEARING_OPS)


class CapabilityEvidence(unittest.TestCase):
    """A gap needs evidence the filesystem wrote, not evidence this script assumed."""

    def case(self, arm, skips=None):
        return {"arm": arm, "op_skips": dict(skips or {}), "fsx_reported_unsupported": []}

    def test_a_recorded_skip_is_the_evidence(self):
        probe = {"cowfs": {}}
        got = gate.capability_evidence(self.case("cowfs", {"exchange_range": 12}), probe)
        self.assertIn("exchange_range", got)

    def test_a_probe_alone_is_also_evidence(self):
        probe = {"cowfs": {"punch_hole": False}}
        self.assertIn("punch_hole", gate.capability_evidence(self.case("cowfs"), probe))

    def test_neither_recorded_nor_probed_is_no_gap(self):
        probe = {"cowfs": {"punch_hole": True}}
        self.assertEqual(gate.capability_evidence(self.case("cowfs"), probe), set())

    def test_the_probe_of_one_arm_does_not_speak_for_the_other(self):
        probe = {"native": {"punch_hole": False}, "cowfs": {"punch_hole": True}}
        self.assertIn("punch_hole", gate.capability_evidence(self.case("native"), probe))
        self.assertEqual(gate.capability_evidence(self.case("cowfs"), probe), set())


class FirstDivergence(unittest.TestCase):
    """F2: the attribution is mechanical. It turns on the first operation the streams disagree on."""

    def test_identical_streams_have_no_divergence(self):
        self.assertIsNone(gate.first_divergence(["a", "b"], ["a", "b"], set()))

    def test_the_index_and_both_operations_are_reported(self):
        got = gate.first_divergence(["a", "b", "c"], ["a", "x", "c"], set())
        self.assertEqual((got["index"], got["native"], got["cowfs"]), (1, "b", "x"))
        self.assertFalse(got["caused_by_capability"])

    def test_a_recorded_skip_at_that_index_is_a_capability_difference(self):
        got = gate.first_divergence(["exchange_range", "read"],
                                    ["skip exchange_range", "read"], {"exchange_range"})
        self.assertTrue(got["caused_by_capability"])
        self.assertEqual(got["operations"], ["exchange_range"])

    def test_a_skip_of_an_operation_nobody_lacks_is_not_a_capability_difference(self):
        got = gate.first_divergence(["exchange_range", "read"],
                                    ["skip exchange_range", "read"], {"punch_hole"})
        self.assertFalse(got["caused_by_capability"])

    def test_only_the_first_divergence_matters(self):
        # The streams agree on the first two, so the third is where the difference starts.
        got = gate.first_divergence(["a", "b", "punch_hole"], ["a", "b", "read"], {"punch_hole"})
        self.assertEqual(got["index"], 2)
        # And an unrelated operation taking the hole operation's place is not a skip of it.
        self.assertFalse(got["caused_by_capability"])

    def test_a_different_operation_in_the_place_of_a_gap_is_not_a_skip_of_it(self):
        got = gate.first_divergence(["a", "b", "punch_hole"], ["a", "b", "skip punch_hole"],
                                    {"punch_hole"})
        self.assertTrue(got["caused_by_capability"])
        got = gate.first_divergence(["a", "b", "punch_hole"], ["a", "b", "read"], {"punch_hole"})
        self.assertFalse(got["caused_by_capability"])

    def test_one_stream_ending_early_is_a_divergence(self):
        got = gate.first_divergence(["a", "b", "c"], ["a", "b"], set())
        self.assertTrue(got["one_stream_ended"])
        self.assertEqual(got["index"], 2)


class CompareCase(unittest.TestCase):
    """The reviewer's controls 12, 13 and 14, plus the guard the first version lacked."""

    def arms(self, native_counts, cowfs_counts, native_gaps=(), cowfs_gaps=(), disabled=(),
             same_stream=True, native_sha="aa", cowfs_sha="aa", readback="aa",
             native_seq=None, cowfs_seq=None, native_skips=None, cowfs_skips=None):
        """Two arms that are identical except for what a test varies.

        The recorded op streams default to the same sequence on both arms, so a test that cares
        about counts alone is not accidentally testing the divergence rule, and a test that cares
        about divergence says where the streams part company.
        """
        base_seq = ["write", "read", "mapread", "mapwrite", "truncate"]
        probe = {"native": {op: False for op in native_gaps},
                 "cowfs": {op: False for op in cowfs_gaps},
                 "_probe_rows": []}
        line = "filesystem does not support fallocate mode FALLOC_%s, disabling"
        # Same stream digest means the streams match; different digests mean they do not. The two
        # arms get different digests so "the streams differ" is what a differing case means.
        native_stream, cowfs_stream = ("same", "same") if same_stream else ("ns", "cs")
        native = case(arm="native", data_st_dev=2050, ops_sha256=native_stream, op_counts=native_counts,
                      data_real_fstype="ext4", data_realpath="/n/fsx.dat",
                      op_sequence=list(native_seq if native_seq is not None else base_seq),
                      op_skips=dict(native_skips or {}),
                      fsx_reported_unsupported=[line % ("PUNCH_HOLE | FALLOC_FL_KEEP_SIZE")]
                      if "punch_hole" in disabled else [])
        cowfs = case(arm="cowfs", data_st_dev=234, ops_sha256=cowfs_stream, op_counts=cowfs_counts,
                     data_real_fstype="fuse.cowfs", data_realpath="/c/fsx.dat",
                     op_sequence=list(cowfs_seq if cowfs_seq is not None else base_seq),
                     op_skips=dict(cowfs_skips or {}),
                     fsx_reported_unsupported=[line % "PUNCH_HOLE"] if "punch_hole" in disabled else [])
        return native, cowfs, probe, {"cowfs": {"sha256": readback, "size": 100}}

    def test_control_12_an_explained_hole_does_not_excuse_900_fewer_reads(self):
        # The reviewer's control 12: punch_hole explained, read differs, bytes differ. Old code
        # passed it. It must not: a difference in a byte-bearing operation is a failure.
        native, cowfs, probe, fresh = self.arms(
            {"write": 4, "read": 1000, "punch_hole": 50, "skip": 0},
            {"write": 4, "read": 100, "skip": 50},
            cowfs_gaps=("punch_hole",), disabled=("punch_hole",),
            same_stream=False, cowfs_sha="cc", readback="cc",
            # The hole operations agree, so the first disagreement is a read, which is not one
            # of them.
            native_seq=["punch_hole", "read", "write"], cowfs_seq=["punch_hole", "read"])
        got = gate.compare_case({"name": "full", "require_identical_op_stream": False}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "FAIL", got["problems"])
        self.assertEqual(got["first_stream_divergence"]["index"], 2)
        self.assertFalse(got["first_stream_divergence"]["caused_by_capability"])
        self.assertIn("read", got["deltas_unexplained"])
        self.assertIn("read", got["byte_bearing_deltas_unexplained"])

    def test_control_13_the_same_delta_with_no_gap_is_also_a_failure(self):
        native, cowfs, probe, fresh = self.arms(
            {"write": 4, "read": 1000, "skip": 0}, {"write": 4, "read": 100, "skip": 50},
            # Ten recorded operations on both arms, so this is a whole stream and not a tail:
            # one arm's runs out one operation early, which is the divergence.
            same_stream=False,
            native_seq=["read"] * 10, cowfs_seq=["read"] * 9)
        got = gate.compare_case({"name": "full", "require_identical_op_stream": False}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "FAIL", got["problems"])
        self.assertTrue(got["first_stream_divergence"]["one_stream_ended"])
        self.assertTrue(any("part company" in p for p in got["problems"]), got["problems"])

    def test_control_14_the_gap_on_the_native_arm_alone_is_not_an_excuse_either(self):
        native, cowfs, probe, fresh = self.arms(
            {"write": 4, "read": 1000, "skip": 0}, {"write": 4, "read": 100, "skip": 50},
            native_gaps=("punch_hole",), disabled=("punch_hole",), same_stream=False,
            native_skips={"punch_hole": 50},
            # Ten operations: the native arm performed a punch hole the cowfs arm did not reach,
            # so its stream is one operation longer. The gap is on the other arm and explains
            # nothing about where these two streams part company.
            native_seq=["read"] * 9 + ["punch_hole"], cowfs_seq=["read"] * 9)
        got = gate.compare_case({"name": "full", "require_identical_op_stream": False}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "FAIL", got["problems"])
        self.assertIn("punch_hole", got["native_capability_gaps"])
        self.assertEqual(got["first_stream_divergence"]["index"], 9)
        self.assertTrue(got["first_stream_divergence"]["one_stream_ended"])

    def test_a_pure_capability_difference_is_unmeasurable_not_a_pass(self):
        # Only hole-family ops differ: the arms did genuinely different work, so fsx exiting 0 on
        # both is not execution equivalence and this cannot be PASS.
        native, cowfs, probe, fresh = self.arms(
            {"write": 4, "read": 4, "punch_hole": 50, "skip": 0},
            {"write": 4, "read": 4, "skip": 50},
            cowfs_gaps=("punch_hole",), disabled=("punch_hole",), same_stream=False,
            cowfs_skips={"punch_hole": 50},
            native_seq=["punch_hole", "read", "write"],
            cowfs_seq=["skip punch_hole", "read", "write"])
        got = gate.compare_case({"name": "full", "require_identical_op_stream": False}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "UNMEASURABLE", got["problems"])
        self.assertEqual(got["problems"], [])
        self.assertEqual(got["first_stream_divergence"]["index"], 0)
        self.assertTrue(got["first_stream_divergence"]["caused_by_capability"])
        self.assertTrue(got["unmeasurable"])

    def test_the_shape_of_a_real_full_mode_difference(self):
        """The measured shape: the streams part company on the first hole operation fsx attempts,
        and everything after it moves with the file. Not a pass, and not corruption either."""
        native, cowfs, probe, fresh = self.arms(
            {"write": 100, "read": 900, "punch_hole": 12, "exchange_range": 12, "skip": 0},
            {"write": 110, "read": 800, "skip": 24},
            cowfs_gaps=("punch_hole", "exchange_range"), same_stream=False,
            cowfs_skips={"punch_hole": 12, "exchange_range": 12},
            native_seq=["exchange_range", "read", "write"],
            cowfs_seq=["skip exchange_range", "read", "write"])
        got = gate.compare_case({"name": "full", "require_identical_op_stream": False}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "UNMEASURABLE", got["problems"])
        self.assertEqual(got["first_stream_divergence"]["index"], 0)
        self.assertEqual(got["first_stream_divergence"]["operations"], ["exchange_range"])
        # Every count delta is still recorded, the ones no capability names included.
        self.assertIn("read", got["op_count_deltas"])
        self.assertEqual(got["hash_comparison"], "not comparable: the two arms ran different "
                                                "operations")

    def long_run_pair(self, executed, native_seq, cowfs_seq, skips):
        """Two arms of a run longer than the recorded window, built directly."""
        probe = {"native": {}, "cowfs": {}, "_probe_rows": []}
        native = case(arm="native", data_st_dev=2050, ops_executed=executed,
                      data_real_fstype="ext4", data_realpath="/n/fsx.dat",
                      ops_sha256="ns", op_sequence=native_seq)
        cowfs = case(data_st_dev=234, ops_executed=executed, ops_sha256="cs",
                     data_real_fstype="fuse.cowfs", data_realpath="/c/fsx.dat",
                     op_sequence=cowfs_seq, op_skips=skips)
        return native, cowfs, probe, {"cowfs": {"sha256": "aa", "size": 100}}

    def test_a_recorded_stream_shorter_than_the_run_cannot_be_attributed(self):
        # fsx keeps only the last LOGSIZE operations it records. A run longer than that leaves a
        # tail on each arm, and where two arms part company cannot be located in a tail, so the
        # pair is UNMEASURABLE and never PASS, whatever the counts say.
        native, cowfs, probe, fresh = self.long_run_pair(
            20000, ["read"] * 10000, ["skip punch_hole"] + ["read"] * 9999, {"punch_hole": 24})
        got = gate.compare_case({"name": "full", "require_identical_op_stream": False}, 1, 20000,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "UNMEASURABLE", got["problems"])
        self.assertTrue(any("tail of" in u for u in got["unmeasurable"]), got["unmeasurable"])
        self.assertEqual(got["ops_stream_lengths"], {"native": 10000, "cowfs": 10000})

    def test_a_whole_stream_at_the_same_length_can_be_attributed(self):
        # The same difference inside the recorded window is locatable, and a capability gap at the
        # first divergence makes the pair UNMEASURABLE with the reason rather than a silent pass.
        native, cowfs, probe, fresh = self.long_run_pair(
            10000, ["punch_hole"] + ["read"] * 9999, ["skip punch_hole"] + ["read"] * 9999,
            {"punch_hole": 24})
        got = gate.compare_case({"name": "full", "require_identical_op_stream": False}, 1, 10000,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "UNMEASURABLE", got["problems"])
        self.assertEqual(got["first_stream_divergence"]["index"], 0)
        self.assertTrue(got["first_stream_divergence"]["caused_by_capability"])

    def test_a_difference_after_a_gap_is_still_attributed_to_the_gap_and_recorded(self):
        # The gap is at the first divergence, so everything after it moves with the file. That is
        # UNMEASURABLE with the deltas on the record, and it is not a failure: nothing asserted
        # anything false.
        native, cowfs, probe, fresh = self.long_run_pair(
            10000, ["punch_hole"] + ["read"] * 9999, ["skip punch_hole"] + ["read"] * 9998,
            {"punch_hole": 24})
        native["op_sequence"][1] = "truncate"
        native["op_counts"] = {"write": 100, "read": 900, "truncate": 12, "skip": 0}
        cowfs["op_counts"] = {"write": 110, "read": 800, "truncate": 11, "skip": 24}
        got = gate.compare_case({"name": "full", "require_identical_op_stream": False}, 1, 10000,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "UNMEASURABLE", got["problems"])
        self.assertEqual(got["problems"], [])
        self.assertEqual(got["first_stream_divergence"]["index"], 0)
        self.assertIn("every difference after it follows from that", got["unmeasurable"][0])
        self.assertEqual(got["ops_stream_lengths"], {"native": 10000, "cowfs": 9999})
        # The unexplained deltas are still recorded even though they are consequences.
        self.assertIn("truncate", got["op_count_deltas"])

    def test_a_matched_mode_with_a_stream_difference_is_a_failure(self):
        native, cowfs, probe, fresh = self.arms({"write": 4}, {"write": 5}, same_stream=False)
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "FAIL", got["problems"])
        self.assertTrue(any("declares one operation mix" in p for p in got["problems"]),
                        got["problems"])

    def test_matching_arms_pass(self):
        native, cowfs, probe, fresh = self.arms({"write": 4, "read": 4}, {"write": 4, "read": 4})
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "PASS")
        self.assertEqual(got["problems"], [])
        self.assertEqual(got["unmeasurable"], [])

    def test_two_arms_on_one_device_are_a_contaminated_run(self):
        native = case(arm="native", data_st_dev=1, data_real_fstype="ext4", data_realpath="/n/x")
        cowfs = case(data_st_dev=1, data_real_fstype="ext4", data_realpath="/c/x")
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, {"cowfs": {"sha256": "aa", "size": 100}}, {})
        self.assertEqual(got["status"], "FAIL")
        self.assertTrue(any("same filesystem" in p for p in got["problems"]), got["problems"])

    def test_a_tmpfs_labelled_cowfs_is_a_failure(self):
        # The reviewer's control 5: the directory is called cowfs but is tmpfs.
        native = case(arm="native", data_st_dev=2050, data_real_fstype="ext4", data_realpath="/n/x")
        cowfs = case(data_st_dev=2051, data_real_fstype="tmpfs", data_realpath="/c/x")
        got = gate.compare_case({"name": "full", "require_identical_op_stream": False}, 1, 10,
                                native, cowfs, {"cowfs": {"sha256": "aa", "size": 100}}, {})
        self.assertEqual(got["status"], "FAIL")
        self.assertTrue(any("not a cowfs mount" in p for p in got["problems"]), got["problems"])

    def test_a_missing_filesystem_witness_is_a_failure(self):
        native = case(arm="native", data_st_dev=2050, data_real_fstype="ext4", data_realpath="/n/x")
        cowfs = case(data_st_dev=234, data_real_fstype=None, data_realpath=None)
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, {"cowfs": {"sha256": "aa", "size": 100}}, {})
        self.assertEqual(got["status"], "FAIL")
        self.assertTrue(any("witness" in p for p in got["problems"]), got["problems"])

    def test_a_nonzero_exit_is_a_problem(self):
        native, cowfs, probe, fresh = self.arms({"write": 4}, {"write": 4})
        cowfs["exit"] = 201
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "FAIL")
        self.assertIn("cowfs fsx exited 201", got["problems"])

    def test_a_fresh_open_in_a_different_shape_is_still_checked(self):
        native, cowfs, probe, fresh = self.arms({"write": 4}, {"write": 4}, readback="ee")
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertTrue(any("reopened the file and read ee" in p for p in got["problems"]),
                        got["problems"])

    def test_a_separate_process_reading_a_different_size_is_a_problem(self):
        native, cowfs, probe, fresh = self.arms({"write": 4}, {"write": 4})
        fresh = {"cowfs": {"sha256": "aa", "size": 99}}
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertTrue(any("read 99 bytes" in p for p in got["problems"]), got["problems"])

    def test_an_unreadable_op_stream_is_a_problem_not_an_operation_named_error(self):
        native, cowfs, probe, fresh = self.arms({"write": 4}, {"write": 4})
        cowfs["op_counts"] = {"error": "fsx.dat.fsxops: No such file or directory"}
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["status"], "FAIL")
        self.assertTrue(any("op stream could not be read" in p for p in got["problems"]),
                        got["problems"])
        self.assertNotIn("error", got["op_count_deltas"])

    def test_a_missing_ops_file_counts_as_no_operations(self):
        native, cowfs, probe, fresh = self.arms({"write": 4}, {"write": 4})
        self.assertEqual(gate.counts_or_empty(case(op_counts=None)), {})
        self.assertEqual(gate.counts_or_empty(case(op_counts={"write": 2})), {"write": 2})

    def test_each_pair_reports_requested_executed_and_skipped_counts(self):
        native, cowfs, probe, fresh = self.arms({"write": 4, "skip": 3}, {"write": 4, "skip": 3})
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 7, 10,
                                native, cowfs, fresh, probe)
        self.assertEqual(got["ops_requested"], 10)
        self.assertEqual(got["ops_executed"], {"native": 10, "cowfs": 10})
        self.assertEqual(got["ops_skipped"], {"native": 3, "cowfs": 3})
        self.assertIn("unsupported_reasons", got)


class RestartLeg(unittest.TestCase):
    """F3: the reviewer's control 17 was --restart-cmd /bin/true, which passed."""

    class Args:
        restart_cmd = "/usr/bin/true"
        cowfs_root = "/mount"
        daemon_pid_file = None
        expect_backend = "core"

    @unittest.skipUnless(os.path.isdir("/proc/self"), "the daemon generation is read from /proc")
    def test_a_manifest_helper_reads_a_generation(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-pid-") as tmp:
            pid = os.getpid()
            pidfile = os.path.join(tmp, "daemon.pid")
            with open(pidfile, "w") as f:
                f.write(str(pid))
            got, error = gate.daemon_generation(pidfile)
        self.assertIsNone(error)
        self.assertEqual(got["pid"], pid)
        self.assertIsNotNone(got["starttime"])

    def test_an_unreadable_pid_file_is_an_error_not_a_pass(self):
        got, error = gate.daemon_generation("/nonexistent/daemon.pid")
        self.assertIsNone(got)
        self.assertIn("pid file", error)

    def test_the_unchanged_pid_is_reported_as_no_replacement(self):
        args = self.Args()
        generation = {"pid": 42, "starttime": "100", "store": "/s", "socket": "/k",
                      "mount": "/m", "backend": "core", "binary_sha256": "bb"}
        args.restart_cmd = "/bin/true"
        original = gate.daemon_generation
        gate.daemon_generation = lambda pidfile: (dict(generation), None)
        original_attest = gate.manifest_module
        gate.manifest_module = lambda: type("M", (), {"attest": staticmethod(
            lambda root, pid, backend: {"ok": True, "st_dev": 234})})()
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                row = gate.restart_leg(args, [], dict(generation), {"st_dev": 234})
        finally:
            gate.daemon_generation = original
            gate.manifest_module = original_attest
        self.assertTrue(any("unchanged after the restart hook" in p for p in row["problems"]),
                        row["problems"])
        self.assertTrue(any("same generation" in p for p in row["problems"]), row["problems"])

    def test_a_new_pid_with_a_new_start_time_on_the_same_store_passes(self):
        args = self.Args()
        before = {"pid": 42, "starttime": "100", "store": "/s", "socket": "/k",
                  "mount": "/m", "backend": "core", "binary_sha256": "bb"}
        after = dict(before, pid=99, starttime="200")
        original = gate.daemon_generation
        gate.daemon_generation = lambda pidfile: (dict(after), None)
        original_attest = gate.manifest_module
        gate.manifest_module = lambda: type("M", (), {"attest": staticmethod(
            lambda root, pid, backend: {"ok": True, "st_dev": 234})})()
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                row = gate.restart_leg(args, [], before, {"st_dev": 234})
        finally:
            gate.daemon_generation = original
            gate.manifest_module = original_attest
        self.assertEqual(row["problems"], [])
        self.assertEqual(row["generation_after"]["pid"], 99)

    def test_a_changed_store_across_the_restart_is_a_problem(self):
        args = self.Args()
        before = {"pid": 42, "starttime": "100", "store": "/s", "socket": "/k",
                  "mount": "/m", "backend": "core", "binary_sha256": "bb"}
        after = dict(before, pid=99, starttime="200", store="/other")
        original = gate.daemon_generation
        gate.daemon_generation = lambda pidfile: (dict(after), None)
        original_attest = gate.manifest_module
        gate.manifest_module = lambda: type("M", (), {"attest": staticmethod(
            lambda root, pid, backend: {"ok": True, "st_dev": 234})})()
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                row = gate.restart_leg(args, [], before, {"st_dev": 234})
        finally:
            gate.daemon_generation = original
            gate.manifest_module = original_attest
        self.assertTrue(any("changed across the restart" in p for p in row["problems"]),
                        row["problems"])

    def test_the_mount_losing_its_attestation_is_a_problem(self):
        args = self.Args()
        before = {"pid": 42, "starttime": "100", "store": "/s", "socket": "/k",
                  "mount": "/m", "backend": "core", "binary_sha256": "bb"}
        after = dict(before, pid=99, starttime="200")
        original = gate.daemon_generation
        gate.daemon_generation = lambda pidfile: (dict(after), None)
        original_attest = gate.manifest_module
        gate.manifest_module = lambda: type("M", (), {"attest": staticmethod(
            lambda root, pid, backend: {"ok": False, "reason": "not a mount"})})()
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                row = gate.restart_leg(args, [], before, {"st_dev": 234})
        finally:
            gate.daemon_generation = original
            gate.manifest_module = original_attest
        self.assertTrue(any("not attested after the restart" in p for p in row["problems"]),
                        row["problems"])


class ImmutableAttemptDir(unittest.TestCase):
    """F4: the reviewer's control 19 reused a case directory, so a stale file passed as work."""

    def test_a_fresh_directory_is_created_and_empty(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-fresh-") as tmp:
            got = gate.fresh_attempt_dir(tmp, "matched-seed1-cowfs")
            self.assertTrue(os.path.isdir(got))
            self.assertEqual(os.listdir(got), [])
            self.assertEqual(os.stat(got).st_mode & 0o777, 0o700)

    def test_two_calls_never_return_the_same_path(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-fresh2-") as tmp:
            first = gate.fresh_attempt_dir(tmp, "x")
            second = gate.fresh_attempt_dir(tmp, "x")
            self.assertNotEqual(first, second)
            self.assertTrue(os.path.isdir(first))

    def test_stale_content_does_not_satisfy_a_new_attempt(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-fresh3-") as tmp:
            gate.fresh_attempt_dir(tmp, "x")
            other = gate.fresh_attempt_dir(tmp, "x")
            self.assertEqual(os.listdir(other), [])


class ByteCaps(unittest.TestCase):
    """F6: the config declared a per-arm byte budget that nothing compared anything against."""

    def test_the_config_budget_covers_the_declared_batch(self):
        gate_json = json.load(open(os.path.join(HERE, "fsx-gate.json")))
        caps = gate_json["caps"]
        pairs = sum(len(m["seeds"]) for m in gate_json["modes"])
        self.assertEqual(pairs, 15)
        self.assertEqual(caps["max_bytes_written_per_arm"], pairs * 2 * caps["max_file_bytes"])
        self.assertGreaterEqual(caps["max_bytes_written_per_arm"], caps["max_file_bytes"])

    def test_a_data_file_over_the_declared_cap_is_reported(self):
        caps = json.load(open(os.path.join(HERE, "fsx-gate.json")))["caps"]
        with tempfile.TemporaryDirectory(prefix="fsx-gate-cap-") as tmp:
            got = gate.fresh_attempt_dir(tmp, "cap")
            data = os.path.join(got, "fsx.dat")
            with open(data, "wb") as f:
                f.write(b"\0" * (caps["max_file_bytes"] + 1))
            digest, size = gate.sha256_file(data)
        self.assertEqual(size, caps["max_file_bytes"] + 1)
        self.assertIsNotNone(digest)
        over = size > caps["max_file_bytes"]
        self.assertTrue(over)

    def test_the_arm_budget_is_a_whole_run_figure_not_a_per_file_one(self):
        gate_json = json.load(open(os.path.join(HERE, "fsx-gate.json")))
        caps = gate_json["caps"]
        self.assertIn("caps_note", caps)
        self.assertIn("max_bytes_written_per_arm", caps["caps_note"])
        self.assertIn("reported FAIL", caps["caps_note"])


class ToolPin(unittest.TestCase):
    """F5: the reviewer's control 16 ran a synthetic child because the binary was never pinned."""

    def manifest_gate(self, binary_sha, extra=()):
        gate_json = json.load(open(os.path.join(HERE, "fsx-gate.json")))
        args = ["--native-root", "/tmp", "--cowfs-root", "/tmp", "--fsx-bin", "/bin/sh",
                "--out", tempfile.mkdtemp(prefix="fsx-gate-pin-"), "--config",
                os.path.join(HERE, "fsx-gate.json")]
        return gate_json, args, list(extra)

    def test_the_gate_config_carries_a_manifest(self):
        gate_json, _, _ = self.manifest_gate(None)
        manifest = gate_json["tool"]["manifest"]
        self.assertEqual(len(manifest["expected_binary_sha256"]), 64)
        self.assertEqual(len(manifest["expected_source_sha256"]), 3)
        for digest in manifest["expected_source_sha256"].values():
            self.assertEqual(len(digest), 64)
        self.assertTrue(manifest["expected_compile"])

    def test_a_mismatched_binary_is_unmeasurable_before_anything_runs(self):
        out = tempfile.mkdtemp(prefix="fsx-gate-pinout-")
        proc = subprocess.run(
            [sys.executable, os.path.join(HERE, "run-fsx-gate.py"),
             "--native-root", out, "--cowfs-root", out, "--fsx-bin", "/bin/sh",
             "--config", os.path.join(HERE, "fsx-gate.json"), "--out", out],
            capture_output=True, text=True, timeout=300)
        self.assertEqual(proc.returncode, 3, proc.stdout + proc.stderr)
        self.assertIn("approved manifest pins", proc.stdout)

    def test_the_recorded_tool_check_says_whether_the_pin_was_applied(self):
        gate_json = json.load(open(os.path.join(HERE, "fsx-gate.json")))
        self.assertIn("manifest", gate_json["tool"])

    def test_allow_unpinned_is_a_mutation_control_not_an_acceptance(self):
        out = tempfile.mkdtemp(prefix="fsx-gate-unpinned-")
        proc = subprocess.run(
            [sys.executable, os.path.join(HERE, "run-fsx-gate.py"),
             "--native-root", out, "--cowfs-root", out, "--fsx-bin", "/bin/sh",
             "--config", os.path.join(HERE, "fsx-gate.json"), "--out", out,
             "--allow-unpinned-fsx"],
            capture_output=True, text=True, timeout=300)
        # The arm attestation still refuses a directory that is not a cowfs mount.
        self.assertEqual(proc.returncode, 3)
        record = os.path.join(out, "cases.jsonl")
        rows = [json.loads(line) for line in open(record)]
        meta = [r for r in rows if r["kind"] == "meta"][0]
        self.assertFalse(meta["tool_check"]["pinned"])
        self.assertIn("mutation control", meta["tool_check"]["note"])


class ArmAttestation(unittest.TestCase):
    """F1: the reviewer's control 5 pointed the cowfs arm at tmpfs and it passed."""

    def test_a_plain_directory_is_not_attested_as_a_cowfs_mount(self):
        module = gate.manifest_module()
        with tempfile.TemporaryDirectory(prefix="fsx-gate-attest-") as tmp:
            # No pid file and no declared backend: the question is only "is this a cowfs mount",
            # and a temp directory is not one.
            got = module.attest(tmp, None, None, module.COWFS_FSTYPES)
            # The same call with no filesystem types required has to resolve, or the native
            # control could never be attested at all.
            loose = module.attest(tmp, None, None, None)
        self.assertIn(got["status"], ("UNKNOWN", "WRONG_FSTYPE"))
        self.assertFalse(got["ok"])
        self.assertTrue(got.get("reason"))
        self.assertTrue(loose["ok"])
        self.assertIsNone(loose.get("reason"))

    def test_the_fstype_check_names_what_it_found(self):
        module = gate.manifest_module()
        self.assertIn("fuse.cowfs", module.COWFS_FSTYPES)
        source = open(os.path.join(HERE, "mount-manifest.py")).read()
        self.assertIn("WRONG_FSTYPE", source)
        self.assertIn("no mount table entry contains", source)

    def test_resolve_mount_picks_the_longest_prefix(self):
        module = gate.manifest_module()
        rows = [{"source": "a", "mountpoint": "/", "fstype": "ext4"},
                {"source": "b", "mountpoint": "/mnt/inner", "fstype": "fuse.cowfs"}]
        self.assertEqual(module.resolve_mount("/mnt/inner/deep", rows)["fstype"], "fuse.cowfs")
        self.assertEqual(module.resolve_mount("/mnt/other", rows)["fstype"], "ext4")
        self.assertIsNone(module.resolve_mount("/elsewhere", []))


class ReadbackComparesAgainstTheRecord(unittest.TestCase):
    """The readback after a restart must be compared with what this run recorded, not with a
    value the reader was handed."""

    def test_the_row_carries_both_generations_and_the_expectations(self):
        row = {"kind": "restart", "generation_before": {"pid": 1, "starttime": "10"},
               "generation_after": {"pid": 2, "starttime": "20"},
               "rehashed": {"/m/fsx.dat": {"sha256": "aa", "size": 100}},
               "problems": []}
        self.assertEqual(row["generation_before"]["starttime"], "10")
        self.assertEqual(row["generation_after"]["starttime"], "20")
        self.assertEqual(row["rehashed"]["/m/fsx.dat"]["size"], 100)

    def test_a_readback_that_matches_the_record_is_not_a_problem(self):
        want = {"sha256": "aa", "size": 100}
        with tempfile.NamedTemporaryFile(delete=False) as f:
            f.write(b"x" * 100)
            path = f.name
        got, error = gate.readback_in_new_process(path)
        os.unlink(path)
        self.assertIsNone(error)
        self.assertEqual(got["size"], want["size"])


class SeparateProcessReadback(unittest.TestCase):
    def test_a_missing_file_is_an_error_not_a_digest(self):
        got, error = gate.readback_in_new_process("/nonexistent/fsx.dat")
        self.assertIsNone(got)
        self.assertIn("failed", error)


class ReadbackIsAFilesystemRead(unittest.TestCase):
    def test_the_readback_hashes_the_file_not_a_stated_expectation(self):
        import hashlib
        payload = b"actual bytes on the filesystem"
        with tempfile.NamedTemporaryFile(delete=False) as f:
            f.write(payload)
            path = f.name
        got, error = gate.readback_in_new_process(path)
        os.unlink(path)
        self.assertIsNone(error)
        self.assertEqual(got["sha256"], hashlib.sha256(payload).hexdigest())
        self.assertEqual(got["size"], len(payload))

    def test_the_readback_of_an_empty_file_is_zero_bytes(self):
        with tempfile.NamedTemporaryFile(delete=False) as f:
            path = f.name
        got, error = gate.readback_in_new_process(path)
        os.unlink(path)
        self.assertIsNone(error)
        self.assertEqual(got["size"], 0)
        self.assertEqual(got["sha256"], hashlib.sha256(b"").hexdigest())


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
                f.write("#!/bin/sh\nprintf 'usage: fsx [-ad]\\n\\t-H: no punch hole\\n"
                        "\\t-u Do not use unshare range\\n'\nexit 90\n")
            os.chmod(fake, 0o755)
            identity = gate.fsx_identity(fake)
        self.assertEqual(identity["usage_exit"], 90)
        # -u has no colon after it, and it is still a flag the binary compiled in.
        self.assertEqual(identity["flags"], ["H", "u"])
        self.assertEqual(len(identity["sha256"]), 64)


class Summary(unittest.TestCase):
    def test_writes_a_table_with_the_verdict(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-summary-") as tmp:
            path = os.path.join(tmp, "summary.md")
            result = {"status": "FAIL", "cases": 1, "failures": ["boom"],
                      "unsupported": ["EOPNOTSUPP"], "required_ops_missing": []}
            gate.summary(path, result, {"fsx": {"sha256": "ab"}, "roots": {}}, [cmp_case()])
            with open(path) as f:
                text = f.read()
        self.assertIn("status: FAIL", text)
        self.assertIn("| matched | 1 | 10 | 10/10 | 0/0 |", text)
        self.assertIn("ext4/fuse.cowfs", text)
        self.assertIn("EOPNOTSUPP", text)
        self.assertIn("boom", text)

    def test_the_pair_table_carries_the_verdict_and_the_counts(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-summary2-") as tmp:
            path = os.path.join(tmp, "summary.md")
            result = {"status": "UNMEASURABLE", "cases": 2, "pairs_passed": 1,
                      "pairs_failed": 0, "pairs_unmeasurable": 1, "failures": [],
                      "unmeasurable": ["arms ran different operations"], "unsupported": [],
                      "capability_gaps": ["punch_hole gap"]}
            gate.summary(path, result, {"fsx": {"sha256": "ab"}, "roots": {}},
                         [cmp_case(), cmp_case(seed=2, status="UNMEASURABLE")])
            with open(path) as f:
                text = f.read()
        self.assertIn("pairs: 1 passed, 0 failed, 1 unmeasurable", text)
        self.assertIn("arms ran different operations", text)
        self.assertIn("UNMEASURABLE", text)


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
