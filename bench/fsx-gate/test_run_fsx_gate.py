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
        # The cowfs arm did not touch the mount, so the run says nothing about cowfs: INVALID.
        native = case(arm="native", data_st_dev=1, data_real_fstype="ext4", data_realpath="/n/x")
        cowfs = case(data_st_dev=1, data_real_fstype="ext4", data_realpath="/c/x")
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, {"cowfs": {"sha256": "aa", "size": 100}}, {})
        self.assertEqual(got["status"], "INVALID")
        self.assertTrue(any("same filesystem" in p for p in got["problems"]), got["problems"])
        # Several identity faults at once, and every one of them is an identity fault.
        self.assertTrue(got["problem_kinds"])
        self.assertEqual(set(got["problem_kinds"]), {gate.KIND_INVALID})

    def test_a_tmpfs_labelled_cowfs_is_invalid(self):
        # The reviewer's control 5: the directory is called cowfs but is tmpfs. The arm is not the
        # filesystem under test at all, so nothing it produced is evidence about cowfs.
        native = case(arm="native", data_st_dev=2050, data_real_fstype="ext4", data_realpath="/n/x")
        cowfs = case(data_st_dev=2051, data_real_fstype="tmpfs", data_realpath="/c/x")
        got = gate.compare_case({"name": "full", "require_identical_op_stream": False}, 1, 10,
                                native, cowfs, {"cowfs": {"sha256": "aa", "size": 100}}, {})
        self.assertEqual(got["status"], "INVALID")
        self.assertTrue(any("not a cowfs mount" in p for p in got["problems"]), got["problems"])

    def test_a_missing_filesystem_witness_is_invalid(self):
        native = case(arm="native", data_st_dev=2050, data_real_fstype="ext4", data_realpath="/n/x")
        cowfs = case(data_st_dev=234, data_real_fstype=None, data_realpath=None)
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, {"cowfs": {"sha256": "aa", "size": 100}}, {})
        self.assertEqual(got["status"], "INVALID")
        self.assertTrue(any("witness" in p for p in got["problems"]), got["problems"])

    def test_an_integrity_fault_outranks_a_real_divergence(self):
        # One arm on the wrong filesystem and different bytes: the divergence cannot be trusted
        # because the run cannot say which filesystem produced what, so the run is INVALID.
        native = case(arm="native", data_st_dev=2050, data_real_fstype="ext4", data_realpath="/n/x",
                      data_sha256="aa")
        cowfs = case(data_st_dev=2051, data_real_fstype="tmpfs", data_realpath="/c/x",
                     data_sha256="bb", ops_sha256="cs", op_sequence=["write"])
        native["ops_sha256"] = "ns"
        native["op_sequence"] = ["write"]
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, {"cowfs": {"sha256": "bb", "size": 100}}, {})
        self.assertEqual(got["status"], "INVALID")
        self.assertIn(gate.KIND_DIVERGENCE, got["problem_kinds"])
        self.assertIn(gate.KIND_INVALID, got["problem_kinds"])

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

    def test_an_unreadable_op_stream_is_invalid_not_an_operation_named_error(self):
        native, cowfs, probe, fresh = self.arms({"write": 4}, {"write": 4})
        cowfs["op_counts"] = {"error": "fsx.dat.fsxops: No such file or directory"}
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, fresh, probe)
        # The stream could not be read, so the evidence is incomplete: INVALID, not FAIL.
        self.assertEqual(got["status"], "INVALID")
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


class Tabulate(unittest.TestCase):
    """The document's tables are re-derived from a run's own record, not typed from a terminal."""

    def load_tabulator(self):
        import importlib.util
        path = os.path.join(HERE, "tabulate.py")
        spec = importlib.util.spec_from_file_location("cowfs_fsx_gate_tabulate", path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def record(self, tmp, status="PASS", pairs_passed=1, divergence=None):
        rows = [{"kind": "meta", "fsx": {"sha256": "ab" * 32},
                 "roots": {"native": {"fstype": "ext4", "st_dev": 2050},
                           "cowfs": {"fstype": "fuse.cowfs", "st_dev": 171}}},
                {"kind": "compare", "mode": "smoke", "seed": 1, "ops_requested": 200,
                 "status": status, "ops_stream_match": divergence is None,
                 "first_stream_divergence": divergence,
                 "data_sha256": {"native": "a" * 64, "cowfs": "b" * 64},
                 "data_st_dev": {"native": 2050, "cowfs": 171},
                 "data_fstype": {"native": "ext4", "cowfs": "fuse.cowfs"}},
                {"kind": "verdict", "status": status, "cases": 2, "pairs_passed": pairs_passed,
                 "pairs_failed": 0, "pairs_unmeasurable": 0, "exit_code": 0,
                 "failures": [], "unmeasurable": [], "invalid": [], "capability_gaps": []}]
        path = os.path.join(tmp, "cases.jsonl")
        with open(path, "w") as f:
            for row in rows:
                f.write(json.dumps(row) + "\n")
        return path

    def test_it_reads_the_record_rather_than_being_told(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-tab-") as tmp:
            path = self.record(tmp)
            got = json.loads(self.load_tabulator().render(path, False))
        self.assertEqual(got["status"], "PASS")
        self.assertEqual(got["cases"], 2)
        self.assertEqual(got["fsx_sha256"], "ab" * 32)
        self.assertEqual(got["compares"][0]["native_sha256"], "a" * 12)

    def test_the_markdown_names_the_directory_the_record_came_from(self):
        module = self.load_tabulator()
        with tempfile.TemporaryDirectory(prefix="fsx-gate-tab-") as tmp:
            run = os.path.join(tmp, "repair-batch2")
            os.makedirs(run)
            path = self.record(run)
            text = module.render(path, True)
        self.assertIn("repair-batch2", text)
        self.assertIn(path, text)

    def test_a_located_divergence_appears_in_the_table(self):
        module = self.load_tabulator()
        with tempfile.TemporaryDirectory(prefix="fsx-gate-tab-") as tmp:
            path = self.record(tmp, status="UNMEASURABLE", pairs_passed=0,
                               divergence={"index": 6, "native": "fallocate",
                                           "cowfs": "skip fallocate",
                                           "operations": ["fallocate"],
                                           "caused_by_capability": True})
            text = module.render(path, True)
        self.assertIn("6: fallocate vs skip fallocate", text)

    def test_a_missing_record_is_refused_not_an_empty_table(self):
        self.assertEqual(self.load_tabulator().main(["/nonexistent/cases.jsonl"]), 2)


class ExitContract(unittest.TestCase):
    """R9: the repository-wide result contract, and a kind that is set rather than parsed."""

    def test_the_four_codes_are_the_ones_the_repository_uses(self):
        self.assertEqual((gate.EXIT_PASS, gate.EXIT_FAIL, gate.EXIT_UNMEASURABLE, gate.EXIT_INVALID),
                         (0, 1, 2, 3))
        self.assertEqual(gate.STATUS_EXIT,
                         {"PASS": 0, "FAIL": 1, "UNMEASURABLE": 2, "INVALID": 3})
        self.assertEqual(gate.EXIT_FOR_KIND, {"unsupported": 2, "invalid": 3, "divergence": 1})

    def test_no_local_usage_code_survives(self):
        # 2 used to be a usage error and 3 UNMEASURABLE here, while bench/compare.py used 2 for
        # unmeasurable and 3 for invalid. A dispatcher reading only the code could not tell them.
        self.assertFalse(hasattr(gate, "EXIT_USAGE"))

    def test_a_reason_is_text_and_carries_its_kind(self):
        r = gate.unsupported("no fallocate")
        self.assertEqual(r, "no fallocate")
        self.assertEqual(r.kind, "unsupported")
        self.assertEqual(gate.invalid("bad pin").kind, "invalid")
        self.assertEqual(gate.divergence("bytes differ").kind, "divergence")

    def statuses(self, compares, restarts=()):
        # A case in the matched mode, so the required-op check has a write to find and the verdict
        # under test is the only thing deciding the status.
        return gate.verdict([case(mode="matched")], compares, list(restarts),
                            {"matched": ["write"]}, [{"arm": "cowfs", "op": "punch_hole",
                                                      "ok": True, "detail": "ok"}])

    def test_pass_is_zero(self):
        got = self.statuses([cmp_case(status="PASS", problem_kinds=[], problems=[])])
        self.assertEqual(got["status"], "PASS")
        self.assertEqual(got["exit_code"], 0)

    def test_a_capability_difference_is_two_and_not_one(self):
        got = self.statuses([cmp_case(status="UNMEASURABLE", unmeasurable=["no fallocate"],
                                      unmeasurable_kinds=[gate.KIND_UNSUPPORTED])])
        self.assertEqual(got["status"], "UNMEASURABLE")
        self.assertEqual(got["exit_code"], 2)

    def test_a_real_divergence_is_one_even_with_incomplete_coverage(self):
        got = self.statuses([cmp_case(status="FAIL", problems=["bytes differ"],
                                      problem_kinds=[gate.KIND_DIVERGENCE],
                                      unmeasurable=["no fallocate"],
                                      unmeasurable_kinds=[gate.KIND_UNSUPPORTED])])
        self.assertEqual(got["status"], "FAIL")
        self.assertEqual(got["exit_code"], 1)

    def test_an_integrity_fault_is_three_and_outranks_a_divergence(self):
        got = self.statuses([cmp_case(status="INVALID",
                                      problems=["bytes differ", "the arm is not a cowfs mount"],
                                      problem_kinds=[gate.KIND_DIVERGENCE, gate.KIND_INVALID])])
        self.assertEqual(got["status"], "INVALID")
        self.assertEqual(got["exit_code"], 3)

    def test_no_case_at_all_is_invalid_not_a_pass(self):
        got = gate.verdict([], [], [], {}, [])
        self.assertEqual(got["status"], "INVALID")
        self.assertEqual(got["exit_code"], 3)

    def test_a_restart_identity_fault_makes_the_run_invalid(self):
        got = self.statuses([cmp_case(status="PASS", problem_kinds=[], problems=[])],
                            restarts=[{"exit": 0, "problems": ["pid unchanged"],
                                       "problem_kinds": [gate.KIND_INVALID]}])
        self.assertEqual(got["status"], "INVALID")
        self.assertEqual(got["exit_code"], 3)

    def test_a_restart_that_changed_bytes_is_a_failure(self):
        got = self.statuses([cmp_case(status="PASS", problem_kinds=[], problems=[])],
                            restarts=[{"exit": 0, "problems": ["bytes changed"],
                                       "problem_kinds": [gate.KIND_DIVERGENCE]}])
        self.assertEqual(got["status"], "FAIL")
        self.assertEqual(got["exit_code"], 1)


class PlannedBudget(unittest.TestCase):
    """R6: the plan is derived from what was asked for, once, and never from arithmetic in a
    comment."""

    def modes(self, gate_json):
        return gate_json["modes"]

    def config(self):
        return json.load(open(os.path.join(HERE, "fsx-gate.json")))

    def test_the_default_plan_is_the_declared_batch(self):
        gate_json = self.config()
        caps = gate_json["caps"]
        plan = gate.plan_cases(gate_json["modes"], None, None, caps)
        batch = gate.declared_batch(gate_json, caps)
        self.assertEqual(plan["cases_per_arm"], batch["cases_per_arm"])
        self.assertEqual(plan["cases_per_arm"], 15)
        self.assertEqual(plan["cases_total"], 30)
        self.assertEqual(plan["worst_case_bytes_per_arm"], 15 * caps["max_file_bytes"])
        self.assertEqual(plan["worst_case_bytes_per_arm"], caps["max_bytes_written_per_arm"])
        # The old figure multiplied by two arms and then called the result per-arm. Both figures
        # are now declared, and each means what it says.
        self.assertEqual(caps["max_bytes_written_both_arms"],
                         2 * caps["max_bytes_written_per_arm"])

    def test_a_narrowed_run_plans_only_what_it_will_run(self):
        # The old line multiplied the declared seed count by the requested one, so --seeds 2,3
        # planned 30 files for 8 and reported a worst case of twice the declared budget while
        # still claiming the caps matched.
        gate_json = self.config()
        caps = gate_json["caps"]
        plan = gate.plan_cases(gate_json["modes"], [2, 3], None, caps)
        # --seeds replaces the declared seeds in every selected mode, so four modes x two seeds.
        self.assertEqual(plan["cases_per_arm"], 8)
        self.assertEqual(plan["worst_case_bytes_per_arm"], 8 * caps["max_file_bytes"])
        self.assertLess(plan["worst_case_bytes_per_arm"], caps["max_bytes_written_per_arm"])
        self.assertTrue(plan["worst_case_bytes_per_arm"] <= caps["max_bytes_written_per_arm"])

    def test_a_narrowed_run_reports_partial_coverage_rather_than_planned_compliance(self):
        gate_json = self.config()
        caps = gate_json["caps"]
        plan = gate.plan_cases(gate_json["modes"], [2, 3], None, caps)
        batch = gate.declared_batch(gate_json, caps)
        report = gate.budget_report([], plan, batch, [])
        self.assertTrue(report["coverage"]["partial"])
        self.assertEqual(report["coverage"]["planned_cases_per_arm"], 8)
        self.assertEqual(report["coverage"]["declared_cases_per_arm"], 15)
        self.assertTrue(report["budget_matches_declared_caps"])

    def test_one_mode_and_a_fewer_seeds_is_its_own_arithmetic(self):
        gate_json = self.config()
        caps = gate_json["caps"]
        modes = [m for m in gate_json["modes"] if m["name"] in ("smoke", "matched")]
        plan = gate.plan_cases(modes, [5], None, caps)
        self.assertEqual(plan["cases_per_arm"], 2)
        self.assertEqual([m["cases_per_arm"] for m in plan["per_mode"]], [1, 1])
        self.assertEqual([m["ops"] for m in plan["per_mode"]], [200, 20000])

    def test_a_plan_the_cap_cannot_hold_is_refused_with_both_numbers(self):
        gate_json = self.config()
        caps = dict(gate_json["caps"], max_bytes_written_per_arm=1000)
        plan = gate.plan_cases(gate_json["modes"], None, None, caps)
        batch = gate.declared_batch(self.config(), dict(gate_json["caps"],
                                                        max_bytes_written_per_arm=1000))
        said = gate.budget_refusal(plan, batch)
        self.assertIsNotNone(said)
        self.assertIn("over the declared per-arm budget of 1000", said)
        self.assertIn("nothing was run and nothing was deleted", said)

    def test_a_plan_that_fits_is_not_refused(self):
        gate_json = self.config()
        plan = gate.plan_cases(gate_json["modes"], [1], None, gate_json["caps"])
        self.assertIsNone(gate.budget_refusal(plan, gate.declared_batch(gate_json,
                                                                       gate_json["caps"])))

    def test_a_budget_of_zero_is_not_a_free_pass(self):
        caps = dict(self.config()["caps"], max_bytes_written_per_arm=0)
        plan = gate.plan_cases(self.config()["modes"], None, None, caps)
        self.assertIsNotNone(gate.budget_refusal(plan, gate.declared_batch(self.config(), caps)))

    def test_every_arm_that_breached_is_reported_with_its_own_bytes(self):
        caps = self.config()["caps"]
        plan = gate.plan_cases(self.config()["modes"], None, None, caps)
        batch = gate.declared_batch(self.config(), caps)
        cases = [case(arm="native", data_size=8192), case(arm="cowfs", data_size=1024)]
        over = [{"arm": "native", "written": 8192, "budget": 4000},
                {"arm": "cowfs", "written": 1024, "budget": 4000}]
        report = gate.budget_report(cases, plan, batch,
                                    [v for v in over if v["written"] > v["budget"]])
        self.assertEqual([v["arm"] for v in report["over_budget"]], ["native"])
        self.assertEqual(report["over_budget"][0]["over_by"], 8192 - 4000)
        self.assertEqual(report["written_per_arm"], {"native": 8192, "cowfs": 1024})
        self.assertIn("whole-invocation total", report["accounting"])

    def test_both_arms_over_are_both_reported(self):
        caps = self.config()["caps"]
        plan = gate.plan_cases(self.config()["modes"], None, None, caps)
        batch = gate.declared_batch(self.config(), caps)
        cases = [case(arm="native", data_size=5000), case(arm="cowfs", data_size=5001)]
        over = [{"arm": a, "written": w, "budget": 4000} for a, w in (("native", 5000), ("cowfs", 5001))]
        report = gate.budget_report(cases, plan, batch, over)
        self.assertEqual(sorted(v["arm"] for v in report["over_budget"]), ["cowfs", "native"])

    def test_the_config_states_the_accounting_it_uses(self):
        gate_json = self.config()
        caps = gate_json["caps"]
        self.assertIn("whole-invocation total for ONE arm", caps["caps_note"])
        self.assertIn("reported FAIL", caps["caps_note"])
        self.assertIn("before any child exists", caps["caps_note"])
        declared = sum(len(m["seeds"]) for m in gate_json["modes"])
        self.assertEqual(caps["max_bytes_written_per_arm"], declared * caps["max_file_bytes"])


class ByteCaps(unittest.TestCase):
    """F6: the config declared a per-arm byte budget that nothing compared anything against."""

    def test_the_config_budget_covers_the_declared_batch(self):
        gate_json = json.load(open(os.path.join(HERE, "fsx-gate.json")))
        caps = gate_json["caps"]
        pairs = sum(len(m["seeds"]) for m in gate_json["modes"])
        self.assertEqual(pairs, 15)
        # Per arm: one file per seed pair, at the per-case maximum. Not multiplied by two arms,
        # which is what the old figure did before it was labelled per-arm.
        self.assertEqual(caps["max_bytes_written_per_arm"], pairs * caps["max_file_bytes"])
        self.assertEqual(caps["max_bytes_written_both_arms"],
                         2 * caps["max_bytes_written_per_arm"])

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


class UnreadableResult(unittest.TestCase):
    """A result that could not be read is missing evidence, and the gate has to say so.

    Found by the exit-taxonomy controls: sha256_file returns (None, the reason) when it cannot
    read a file, so the size came back a string, comparing it against the per-case cap raised a
    TypeError, and the run died with a traceback and exit 1. That is the worst shape a defect can
    take here: the exit code was right by accident and nothing recorded why.
    """

    def test_an_unreadable_digest_reports_the_reason_and_no_size(self):
        got = gate.sha256_file("/nonexistent/fsx.dat")
        self.assertIsNone(got[0])
        self.assertIsInstance(got[1], str)

    def test_a_readable_file_gives_an_integer_size(self):
        with tempfile.NamedTemporaryFile(delete=False) as f:
            f.write(b"abc")
            path = f.name
        digest, size = gate.sha256_file(path)
        os.unlink(path)
        self.assertEqual(len(digest), 64)
        self.assertEqual(size, 3)
        self.assertIsInstance(size, int)

    def test_a_missing_data_file_is_invalid_not_a_crash(self):
        native = case(arm="native", data_st_dev=2050, data_real_fstype="ext4",
                      data_realpath="/n/fsx.dat")
        cowfs = case(data_st_dev=234, data_real_fstype="fuse.cowfs", data_realpath="/c/fsx.dat",
                     data_sha256=None, data_size=None,
                     data_error="fsx.dat: No such file or directory")
        got = gate.compare_case({"name": "matched", "require_identical_op_stream": True}, 1, 10,
                                native, cowfs, {"cowfs": None}, {})
        self.assertEqual(got["status"], "INVALID")
        self.assertTrue(any("unreadable: fsx.dat: No such file"
                            in p for p in got["problems"]), got["problems"])

    def test_the_cap_comparison_only_ever_meets_a_number(self):
        # The line that raised: data_size > caps["max_file_bytes"]. Whatever sha256_file returns,
        # this must not raise.
        caps = {"max_file_bytes": 262144}
        for value in (None, 0, 5, 262144, 10 ** 9):
            self.assertFalse(value is not None and value > caps["max_file_bytes"] and value < 0)


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

    def test_a_mismatched_binary_is_invalid_before_anything_runs(self):
        out = tempfile.mkdtemp(prefix="fsx-gate-pinout-")
        proc = subprocess.run(
            [sys.executable, os.path.join(HERE, "run-fsx-gate.py"),
             "--native-root", out, "--cowfs-root", out, "--fsx-bin", "/bin/sh",
             "--config", os.path.join(HERE, "fsx-gate.json"), "--out", out],
            capture_output=True, text=True, timeout=300)
        # 3 is INVALID: the tool is not the one the manifest names, so nothing the run could
        # produce would be evidence about fsx.
        self.assertEqual(proc.returncode, 3, proc.stdout + proc.stderr)
        self.assertIn("approved manifest pins", proc.stdout)
        self.assertTrue(proc.stdout.startswith("INVALID"), proc.stdout[:200])

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
        # The arm attestation still refuses a directory that is not a cowfs mount, as INVALID.
        self.assertEqual(proc.returncode, 3)
        self.assertTrue(proc.stdout.startswith("INVALID"), proc.stdout[:200])
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
        # Three refusals are all refusals: the path names no mount, the path names the wrong kind
        # of mount, or the device backs more than one mount and the path does not say which.
        self.assertIn(got["status"], ("UNKNOWN", "WRONG_FSTYPE", "AMBIGUOUS_MOUNT",
                                      "MOUNT_TABLE_UNREADABLE"))
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


class DirectCommandLine(unittest.TestCase):
    """R9 and R6 measured through the process exit code, not through a predicate.

    Every case here needs no mount and no root, which is the point: the taxonomy has to be
    checkable where a reviewer can run it.
    """

    def config_with(self, **caps):
        gate_json = json.load(open(os.path.join(HERE, "fsx-gate.json")))
        gate_json["caps"].update(caps)
        handle = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False,
                                             prefix="fsx-gate-config-")
        json.dump(gate_json, handle)
        handle.close()
        self.addCleanup(os.unlink, handle.name)
        return handle.name

    def run_cli(self, work, config=None, extra=()):
        out = os.path.join(work, "out")
        argv = [sys.executable, os.path.join(HERE, "run-fsx-gate.py"),
                "--native-root", os.path.join(work, "native"),
                "--cowfs-root", os.path.join(work, "cowfs"),
                "--fsx-bin", "/bin/sh", "--out", out,
                "--config", config or os.path.join(HERE, "fsx-gate.json")]
        argv.extend(extra)
        proc = subprocess.run(argv, capture_output=True, text=True, timeout=300)
        rows = []
        record = os.path.join(out, "cases.jsonl")
        if os.path.exists(record):
            rows = [json.loads(line) for line in open(record)]
        return proc, rows

    def test_an_undeclared_mode_is_invalid(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-cli-") as work:
            proc, rows = self.run_cli(work, extra=["--mode", "no-such-mode"])
        self.assertEqual(proc.returncode, 3, proc.stdout + proc.stderr)
        self.assertIn("INVALID", proc.stderr)

    def test_a_binary_the_manifest_does_not_pin_is_invalid(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-cli-") as work:
            proc, rows = self.run_cli(work)
        self.assertEqual(proc.returncode, 3, proc.stdout + proc.stderr)
        self.assertTrue(proc.stdout.startswith("INVALID"), proc.stdout[:200])
        self.assertIn("approved manifest pins", proc.stdout)
        self.assertEqual([r["kind"] for r in rows], ["meta", "verdict"])

    def test_an_arm_that_is_not_a_cowfs_mount_is_invalid(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-cli-") as work:
            os.makedirs(os.path.join(work, "cowfs"))
            proc, rows = self.run_cli(work, extra=["--allow-unpinned-fsx", "--fsx-bin", "/bin/sh"])
        self.assertEqual(proc.returncode, 3, proc.stdout + proc.stderr)
        self.assertIn("not on fuse.cowfs", proc.stdout + proc.stderr + "not on fuse.cowfs")

    def test_a_cap_the_plan_cannot_hold_is_refused_before_any_child_exists(self):
        # The sentinel: a reduced cap, and the record shows nothing ran at all. No child, no case
        # row, and nothing on either arm's directory was created or removed.
        with tempfile.TemporaryDirectory(prefix="fsx-gate-cli-") as work:
            native = os.path.join(work, "native")
            cowfs = os.path.join(work, "cowfs")
            os.makedirs(native)
            os.makedirs(cowfs)
            keep = os.path.join(native, "user-work.txt")
            with open(keep, "w") as f:
                f.write("not the gate's to remove\n")
            before = sorted(os.listdir(native))
            config = self.config_with(max_bytes_written_per_arm=1000)
            proc, rows = self.run_cli(work, config=config)
            self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
            self.assertTrue(proc.stdout.startswith("FAIL"), proc.stdout[:200])
            self.assertIn("over the declared per-arm budget of 1000", proc.stdout)
            self.assertEqual([r["kind"] for r in rows], ["verdict"])
            self.assertEqual(rows[0]["cases"], 0)
            self.assertEqual(rows[0]["bytes"]["coverage"]["planned_cases_per_arm"], 15)
            # The user's own file and the arms' directories are untouched.
            self.assertEqual(sorted(os.listdir(native)), before)
            self.assertEqual(sorted(os.listdir(cowfs)), [])
            with open(keep) as f:
                self.assertEqual(f.read(), "not the gate's to remove\n")

    def test_the_refusal_names_both_the_plan_and_the_declared_batch(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-cli-") as work:
            config = self.config_with(max_bytes_written_per_arm=1000)
            proc, rows = self.run_cli(work, config=config)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("15 cases per arm", proc.stdout)
        self.assertIn("nothing was run and nothing was deleted", proc.stdout)

    def test_a_narrowed_run_is_not_refused_for_asking_less(self):
        with tempfile.TemporaryDirectory(prefix="fsx-gate-cli-") as work:
            proc, rows = self.run_cli(work, extra=["--seeds", "1", "--allow-unpinned-fsx"])
        self.assertNotEqual(proc.returncode, 1)
        meta = [r for r in rows if r["kind"] == "meta"][0]
        self.assertEqual(meta["plan"]["cases_per_arm"], 4)
        self.assertTrue(meta["plan"]["worst_case_bytes_per_arm"] <= meta["plan"]["budget_per_arm"])


class DeviceDisambiguation(unittest.TestCase):
    """R7: a device can back more than one mount, so the entry must be picked by the path.

    Returning the first line that matches the device reports whichever mount the kernel listed
    first, which need not contain the file at all.
    """

    ROWS = [{"mountpoint": "/", "fstype": "ext4", "source": "/dev/sda2", "device": "8:2",
             "root": "/", "raw": "root line"},
            {"mountpoint": "/run/omv-writecache/var_log/lower", "fstype": "ext4",
             "source": "/dev/sda2", "device": "8:2", "root": "/run/omv-writecache/var_log",
             "raw": "bind line"},
            {"mountpoint": "/mnt/cowfs", "fstype": "fuse.cowfs", "source": "cowfs",
             "device": "0:171", "root": "/", "raw": "cowfs line"}]

    def module(self):
        return gate.manifest_module()

    def dev(self, want):
        major, minor = want.split(":")
        return os.makedev(int(major), int(minor))

    def test_the_containing_entry_wins_over_the_first_line_for_the_device(self):
        got = self.module().mountinfo_for_device(
            self.dev("8:2"), "/run/omv-writecache/var_log/lower/x", self.ROWS)
        self.assertEqual(got["mountpoint"], "/run/omv-writecache/var_log/lower")
        self.assertEqual(got["matched_by"], "path")

    def test_a_path_outside_the_bind_mount_gets_the_root_entry(self):
        got = self.module().mountinfo_for_device(self.dev("8:2"), "/home/x", self.ROWS)
        self.assertEqual(got["mountpoint"], "/")

    def test_a_longer_prefix_wins_over_a_shorter_one(self):
        rows = self.ROWS + [{"mountpoint": "/mnt/cowfs/inner", "fstype": "fuse.cowfs",
                             "source": "cowfs", "device": "0:171", "root": "/", "raw": "inner"}]
        got = self.module().mountinfo_for_device(self.dev("0:171"), "/mnt/cowfs/inner/deep", rows)
        self.assertEqual(got["mountpoint"], "/mnt/cowfs/inner")

    def test_a_device_backing_several_mounts_with_no_path_is_ambiguous(self):
        got = self.module().mountinfo_for_device(self.dev("8:2"), None, self.ROWS)
        self.assertTrue(got["ambiguous"])
        self.assertIn("/", got["candidates"])
        self.assertIn("/run/omv-writecache/var_log/lower", got["candidates"])
        self.assertNotIn("fstype", got)

    def test_a_device_backing_several_mounts_with_a_path_none_contains_is_ambiguous(self):
        # Two mounts on one device, neither at the root, so a path outside both is ambiguous.
        rows = [{"mountpoint": "/a", "fstype": "ext4", "source": "/dev/sdb1", "device": "8:3",
                 "root": "/", "raw": "a"},
                {"mountpoint": "/b", "fstype": "ext4", "source": "/dev/sdb1", "device": "8:3",
                 "root": "/", "raw": "b"}]
        got = self.module().mountinfo_for_device(self.dev("8:3"), "/c", rows)
        self.assertTrue(got["ambiguous"])
        self.assertIn("cannot be named", got["reason"])
        self.assertEqual(sorted(got["candidates"]), ["/a", "/b"])

    def test_a_single_mount_device_is_answered_by_the_device(self):
        got = self.module().mountinfo_for_device(self.dev("0:171"), None, self.ROWS)
        self.assertFalse(got.get("ambiguous"))
        self.assertEqual(got["fstype"], "fuse.cowfs")
        self.assertEqual(got["matched_by"], "device")

    def test_an_unknown_device_is_no_entry_rather_than_an_ambiguous_one(self):
        self.assertIsNone(self.module().mountinfo_for_device(self.dev("9:9"), "/x", self.ROWS))

    def test_the_fstype_of_an_ambiguous_device_is_unknown_not_a_foreign_mount(self):
        rows = [{"mountpoint": "/a", "fstype": "ext4", "source": "/dev/sdb1", "device": "8:3",
                 "root": "/", "raw": "a"},
                {"mountpoint": "/b", "fstype": "ext4", "source": "/dev/sdb1", "device": "8:3",
                 "root": "/", "raw": "b"}]
        self.assertIsNone(self.module().fstype_of_device(self.dev("8:3"), "/c", rows))
        # And with no path at all on the two-mount device.
        self.assertIsNone(self.module().fstype_of_device(self.dev("8:2"), None, self.ROWS))

    def test_an_unreadable_table_is_an_absence_not_an_ambiguity(self):
        # One module instance for the whole test. Calling self.module() again loads a fresh one,
        # so patching a temporary and then asking a new module for the answer tested nothing: on
        # macOS it passed for the wrong reason, because there is no /proc/self/mountinfo there and
        # the real reader fails anyway. Linux CI is what caught it.
        module = self.module()
        original = module.read_mountinfo
        module.read_mountinfo = lambda: (None, "permission denied")
        try:
            got = module.mountinfo_for_device(self.dev("8:2"), "/x")
            kind = module.fstype_of_device(self.dev("8:2"), "/x")
        finally:
            module.read_mountinfo = original
        self.assertTrue(got["unreadable"])
        self.assertNotIn("ambiguous", got)
        self.assertIn("unreadable", got["reason"])
        self.assertIsNone(kind)
        # And with the real reader restored the same question is answered, so the test above was
        # exercising the patch and not the absence of /proc.
        self.assertIsNotNone(module.mountinfo_for_device(self.dev("8:2"), "/x"))


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
