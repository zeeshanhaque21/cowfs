"""Checks for bench/pjdfstest.py.

Every record here is marked synthetic. The verdict refuses synthetic records, so a fixture can
never be scored as conformance evidence; these tests prove the refusal and the pairing rules
directly instead.

The pairing tests come first on purpose: an identity that survives a shifted, added, missing or
duplicated result is the property the whole gate rests on.
"""

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import pjdfstest as p


def case(ok, detail="", root=False, n=1):
    return {"n": n, "ok": ok, "todo": False, "detail": detail, "root_required": root}


def record(arm, test, cases, plan=None, rc=0, **extra):
    """A synthetic record in the shape cases.jsonl holds."""
    return {"arm": arm, "test": test, "cases": cases, "plan": plan if plan is not None else len(cases),
            "ok": sum(1 for c in cases if c["ok"]), "not_ok": sum(1 for c in cases if not c["ok"]),
            "rc": rc, "timed_out": False, "bail_out": 0, "malformed": [], "synthetic": True, **extra}


SCRIPT_LOOP = """#!/bin/sh
. ../misc.sh
for type in regular dir fifo block char socket symlink; do
\tcreate_file ${type} ${n0}
\texpect EEXIST open ${n0} O_CREAT,O_EXCL 0644
\texpect 0 unlink ${n0}
done
"""
SCRIPT_PLAIN = """#!/bin/sh
. ../misc.sh
expect 0 create ${n0} 0644
expect regular,0644 stat ${n0} type,mode
expect 0 unlink ${n0}
"""


def args_of(detail):
    m = p.TRIED_RE.search(detail)
    return m.group(1) if m else None


class PairingInvariant(unittest.TestCase):
    """One script, one arm's results shifted, added to or removed: identity must not move."""

    def setUp(self):
        self.profile = p.script_profile(SCRIPT_PLAIN)

    def test_a_shift_after_a_removed_result_does_not_repair_a_pair(self):
        native = record("native", "x.t", [
            case(True, "tried 'create pjdfstest_aaaaaaaa 0644', expected 0, got 0"),
            case(True, "tried 'stat pjdfstest_aaaaaaaa type,mode', expected regular,0644, got regular,0644"),
            case(True, "tried 'unlink pjdfstest_aaaaaaaa', expected 0, got 0"),
        ])
        cowfs = record("cowfs", "x.t", [
            case(True, "tried 'create pjdfstest_bbbbbbbb 0644', expected 0, got 0"),
            case(False, "tried 'stat pjdfstest_bbbbbbbb type,mode', expected regular,0644, got ENOENT"),
        ])
        result = p.pair_case(native, cowfs, self.profile)
        keys = [(pair["operation"], pair["occurrence"]) for pair in result["pairs"]]
        self.assertEqual(keys, [("create <gen1> 0644", 0), ("stat <gen1> type,mode", 0)])
        self.assertEqual([u["reason"] for u in result["unpairable"]],
                         ["'unlink <gen1>' appears 1 times natively and 0 times on the mount"])

    def test_an_added_result_does_not_shift_the_next_identity(self):
        native = record("native", "x.t", [
            case(True, "tried 'create pjdfstest_aaaaaaaa 0644', expected 0, got 0"),
            case(True, "tried 'stat pjdfstest_aaaaaaaa type,mode', expected regular,0644, got regular,0644"),
        ])
        cowfs = record("cowfs", "x.t", [
            case(True, "tried 'create pjdfstest_bbbbbbbb 0644', expected 0, got 0"),
            case(True, "tried 'symlink test pjdfstest_cccccccc', expected 0, got 0"),
            case(True, "tried 'stat pjdfstest_bbbbbbbb type,mode', expected regular,0644, got regular,0644"),
        ])
        result = p.pair_case(native, cowfs, self.profile)
        paired = {pair["operation"]: pair for pair in result["pairs"]}
        self.assertEqual(paired["stat <gen1> type,mode"]["native_ok"], True)
        self.assertEqual(paired["stat <gen1> type,mode"]["cowfs_ok"], True)
        self.assertEqual([u["reason"] for u in result["unpairable"]][:1],
                         ["'symlink test <gen1>' appears 0 times natively and 1 times on the mount"])

    def test_a_repeated_operation_is_unpairable_without_a_literal_loop(self):
        native = record("native", "x.t", [case(True, "tried 'unlink pjdfstest_aaaaaaaa', expected 0, got 0"),
                                          case(True, "tried 'unlink pjdfstest_bbbbbbbb', expected 0, got 0")])
        cowfs = record("cowfs", "x.t", [case(True, "tried 'unlink pjdfstest_cccccccc', expected 0, got 0"),
                                        case(True, "tried 'unlink pjdfstest_dddddddd', expected 0, got 0")])
        result = p.pair_case(native, cowfs, self.profile)
        self.assertEqual(result["pairs"], [])
        self.assertTrue(all("no literal for loop" in u["reason"] for u in result["unpairable"]))

    def test_a_literal_loop_orders_its_repeats_and_keeps_its_suffix(self):
        profile = p.script_profile(SCRIPT_LOOP)
        native = record("native", "open/22.t", [
            case(True, "tried 'open pjdfstest_aaaaaaaa O_CREAT,O_EXCL 0644', expected EEXIST, got EEXIST"),
            case(False, "tried 'open pjdfstest_aaaaaaaa O_CREAT,O_EXCL 0644', expected EEXIST, got 0"),
        ])
        cowfs = record("cowfs", "open/22.t", [
            case(True, "tried 'open pjdfstest_bbbbbbbb O_CREAT,O_EXCL 0644', expected EEXIST, got EEXIST"),
            case(False, "tried 'open pjdfstest_bbbbbbbb O_CREAT,O_EXCL 0644', expected EEXIST, got 0"),
        ])
        result = p.pair_case(native, cowfs, profile)
        self.assertEqual([(pair["occurrence"], pair["confidence"]) for pair in result["pairs"]],
                         [(0, p.ESTABLISHED), (1, p.ESTABLISHED)])
        self.assertEqual(result["unpairable"], [])

    def test_generated_names_do_not_change_identity_but_argument_order_does(self):
        self.assertEqual(p.normalize_operation("create pjdfstest_aaaaaaaa 0644"),
                         p.normalize_operation("create pjdfstest_bbbbbbbb 0644"))
        # Both orderings canonicalise to the same string, which is why an all-generated
        # operation is a candidate rather than an established pair.
        self.assertEqual(p.normalize_operation("link pjdfstest_aaaaaaaa pjdfstest_bbbbbbbb"),
                         p.normalize_operation("link pjdfstest_bbbbbbbb pjdfstest_aaaaaaaa"))
        self.assertFalse(p.literal_pinned(p.normalize_operation("link pjdfstest_aaaaaaaa pjdfstest_bbbbbbbb")))
        self.assertTrue(p.literal_pinned(p.normalize_operation("create pjdfstest_aaaaaaaa 0644")))

    def test_error_text_is_never_part_of_identity(self):
        native = record("native", "x.t", [case(True, "tried 'unlink pjdfstest_aaaaaaaa', expected 0, got EPERM")])
        cowfs = record("cowfs", "x.t", [case(False, "tried 'unlink pjdfstest_bbbbbbbb', expected 0, got EIO")])
        result = p.pair_case(native, cowfs, self.profile)
        self.assertEqual(len(result["pairs"]), 1)
        # One generated name and nothing else pins the operation, so this is a candidate.
        self.assertEqual(result["pairs"][0]["confidence"], p.CANDIDATE)

    def test_textless_assertions_are_never_paired(self):
        native = record("native", "x.t", [case(True, "")])
        cowfs = record("cowfs", "x.t", [case(False, "")])
        result = p.pair_case(native, cowfs, self.profile)
        self.assertEqual(result["pairs"], [])
        self.assertEqual(len(result["unpairable"]), 2)

    def test_a_text_duplicate_is_ambiguous_rather_than_paired_arbitrarily(self):
        native = record("native", "x.t", [case(True, "tried 'unlink pjdfstest_aaaaaaaa', expected 0, got 0")])
        cowfs = record("cowfs", "x.t", [case(False, "tried 'unlink pjdfstest_aaaaaaaa', expected 0, got 0"),
                                        case(False, "tried 'unlink pjdfstest_aaaaaaaa', expected 0, got 0")])
        result = p.pair_case(native, cowfs, self.profile)
        self.assertEqual(result["pairs"], [])
        self.assertIn("appears 1 times natively and 2 times on the mount",
                      result["unpairable"][0]["reason"])


class ScriptProfile(unittest.TestCase):
    def test_result_dependent_control_flow_is_detected(self):
        self.assertTrue(p.script_profile("expect 0 stat x mode\n[ $? -eq 0 ] && expect 0 unlink x\n")
                         ["result_dependent_control_flow"])
        self.assertFalse(p.script_profile(SCRIPT_PLAIN)["result_dependent_control_flow"])

    def test_slot_identity_is_provable_only_without_a_blocker(self):
        self.assertEqual(p.script_profile(SCRIPT_PLAIN)["slot_identity_blockers"], [])
        blocked = p.script_profile("if [ -e x ]; then expect 0 unlink x; fi\n")["slot_identity_blockers"]
        self.assertTrue(any("branches" in b for b in blocked))
        helper = p.script_profile("create_file ${type} ${n0}\n")["slot_identity_blockers"]
        self.assertTrue(any("create_file" in b for b in helper))

    def test_a_script_slot_labels_an_assertion_the_stream_leaves_textless(self):
        native = record("native", "x.t", [case(True, ""), case(True, "")])
        # A failing test_check prints no operation text either, so the slot label is all there is.
        cowfs = record("cowfs", "x.t", [case(True, ""), case(False, "")])
        profile = p.script_profile("expect 0 create ${n0} 0644\ntest_check $a -lt $b\n")
        result = p.pair_case(native, cowfs, profile)
        self.assertEqual(result["route"], "script slot")
        self.assertEqual([pair["confidence"] for pair in result["pairs"]], [p.ESTABLISHED, p.ESTABLISHED])
        self.assertEqual(result["pairs"][1]["identity"], "script slot 1 (test_check (no operation text))")
        self.assertEqual(result["pairs"][1]["operation"], "(test_check)")

    def test_a_stream_that_contradicts_the_script_slot_is_refused(self):
        native = record("native", "x.t", [case(True, "")])
        cowfs = record("cowfs", "x.t", [case(False, "tried 'mkdir pjdfstest_bbbbbbbb 0755', expected 0, got EPERM")])
        profile = p.script_profile("expect 0 unlink ${n0}\n")
        result = p.pair_case(native, cowfs, profile)
        self.assertEqual(result["pairs"], [])
        self.assertIn("where the script's slot 0 is 'unlink <gen1>'", result["unpairable"][0]["reason"])

    def test_helper_expansion_is_recorded(self):
        self.assertTrue(p.script_profile("create_file ${type} ${n0}\n")["helper_expands_assertions"])
        self.assertFalse(p.script_profile(SCRIPT_PLAIN)["helper_expands_assertions"])


class MountState(unittest.TestCase):
    TABLE = ("localhost:/cowfs-abc on /private/tmp/m (nfs, nodev, nosuid)\n"
             "map auto_home on /System/Volumes/Data/home (autofs, nosuid)\n")

    def test_exact_decoded_match_is_mounted(self):
        self.assertEqual(p.mount_entries(self.TABLE)[0][1], "/private/tmp/m")
        self.assertEqual(p._unescape("/private/tmp/a\\040b"), "/private/tmp/a b")

    def test_a_prefix_is_not_a_match(self):
        state = p.mount_state.__wrapped__(Path("/private/tmp/m")) if hasattr(p.mount_state, "__wrapped__") else None
        self.assertIsNone(state)
        entries = p.mount_entries(self.TABLE)
        self.assertNotIn("/private/tmp", [e[1] for e in entries])
        self.assertNotIn("/private/tmp/m/inner", [e[1] for e in entries])

    def test_unknown_table_is_not_absence(self):
        self.assertEqual(p.INVALID, "INVALID")
        self.assertEqual(p.UNKNOWN, "UNKNOWN")
        self.assertIn("UNKNOWN", (p.UNKNOWN, p.MOUNTED, p.NOT_MOUNTED))

    def test_exit_taxonomy_is_fixed(self):
        self.assertEqual(p.EXIT_STATUS, {p.PASS: 0, p.FAIL: 1, p.UNMEASURABLE: 2, p.INVALID: 3})


class Guards(unittest.TestCase):
    def raw(self, text):
        return text

    def test_truncated_stream_is_refused(self):
        record_ = {"test": "x.t", "rc": 0, "timed_out": False, "raw": "present",
                   "cases": [], "plan": None, "malformed": []}
        problems = p.guard_case(record_, "x.t", self.raw("ok 1\nnot ok 2 - boom\n"))
        self.assertIn("no plan line, so the stream cannot be scored", problems)

    def test_plan_count_mismatch_is_refused(self):
        problems = p.guard_case({"test": "x.t", "rc": 0, "timed_out": False, "raw": "present"},
                                "x.t", self.raw("1..5\nok 1\n"))
        self.assertIn("plan 5 does not match the 1 assertions emitted", problems)

    def test_duplicate_and_non_contiguous_ids_are_refused(self):
        dupe = "1..3\nok 1\nok 1\nok 2\n"
        self.assertIn("duplicate assertion ids", p.guard_case(
            {"test": "x.t", "rc": 0, "timed_out": False, "raw": "present"}, "x.t", self.raw(dupe)))
        gap = "1..3\nok 1\nok 3\n"
        self.assertIn("assertion ids are not contiguous from 1", p.guard_case(
            {"test": "x.t", "rc": 0, "timed_out": False, "raw": "present"}, "x.t", self.raw(gap)))

    def test_bail_out_and_nonzero_child_are_refused(self):
        stream = "1..2\nok 1\nBail out! server died\n"
        problems = p.guard_case({"test": "x.t", "rc": 0, "timed_out": False, "raw": "present"},
                                "x.t", self.raw(stream))
        self.assertTrue(any("Bail out!" in c for c in problems))
        self.assertIn("child exited 2, not 0", p.guard_case(
            {"test": "x.t", "rc": 2, "timed_out": False, "raw": "present"}, "x.t",
            self.raw("1..1\nok 1\n")))

    def test_wrong_case_name_is_refused(self):
        self.assertTrue(any("the run listed it as" in c for c in p.guard_case(
            {"test": "y.t", "rc": 0, "timed_out": False, "raw": "present"}, "x.t",
            self.raw("1..1\nok 1\n"))))

    def test_synthetic_records_are_never_conformance(self):
        self.assertIn("synthetic fixture: a unit-test record is not conformance evidence",
                      p.guard_case(record("native", "x.t", [case(True, "tried 'x'")]), "x.t", None))

    def test_legacy_records_are_flagged_but_not_per_case_refused(self):
        legacy = {"test": "x.t", "rc": 0, "timed_out": False,
                  "cases": [case(True, "tried 'unlink pjdfstest_a', expected 0, got 0")], "plan": 1}
        self.assertEqual(p.guard_case(legacy, "x.t", None, legacy=True), [])
        self.assertTrue(p.guard_case(legacy, "x.t", None, legacy=False))


GOOD_IDISTRY = {"path": "/tmp/mnt/pjd", "st_dev": 436207620,
              "fstype": "nfs", "mountpoint": "/tmp/mnt",
              "source": "localhost:/cowfs-x", "problem": None}
GOOD_IDENTITY = {
    "native": {"path": "/tmp/native", "st_dev": 16777234, "fstype": "apfs", "mountpoint": "/",
               "source": "/dev/disk3s1s1", "problem": None},
    "cowfs": {"path": "/tmp/mnt/pjd", "st_dev": 436207620, "fstype": "nfs",
              "mountpoint": "/tmp/mnt", "source": "localhost:/cowfs-x", "problem": None},
}


class VerdictStates(unittest.TestCase):
    def run_verdict(self, native_cases, cowfs_cases, identity=None, extra=None):
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp)
            lines = []
            for arm, cases in (("native", native_cases), ("cowfs", cowfs_cases)):
                record_ = record(arm, "x.t", cases)
                record_.pop("synthetic")
                record_.update({"bail_out": 0, "malformed": []})
                if extra:
                    record_.update(extra)
                lines.append(json.dumps(record_, sort_keys=True))
            (run / "cases.jsonl").write_text("\n".join(lines) + "\n")
            return p.verdict(run, identity=identity if identity is not None else GOOD_IDENTITY)

    def established_regression_is_fail(self):
        good = case(True, "tried 'unlink pjdfstest_a', expected 0, got 0")
        bad = case(False, "tried 'unlink pjdfstest_b', expected 0, got EPERM")
        report = self.run_verdict([good], [bad])
        self.assertEqual(report["state"], p.FAIL)
        self.assertEqual(report["exit_status"], 1)

    def a_pass_needs_a_pairable_scope(self):
        good = case(True, "tried 'unlink pjdfstest_a', expected 0, got 0")
        report = self.run_verdict([good], [case(True, "tried 'unlink pjdfstest_b', expected 0, got 0")])
        self.assertEqual(report["state"], p.UNMEASURABLE)
        self.assertTrue(any("cannot be paired" in r or "not established" in r for r in report["reasons"]))

    def synthetic_records_are_refused_by_the_verdict_itself(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp)
            lines = []
            for arm in ("native", "cowfs"):
                lines.append(json.dumps(record(arm, "x.t", [case(True, "tried 'unlink pjdfstest_a'")]),
                                         sort_keys=True))
            (run / "cases.jsonl").write_text("\n".join(lines) + "\n")
            report = p.verdict(run, identity=GOOD_IDENTITY)
            self.assertEqual((report["state"], report["exit_status"]), (p.INVALID, 3))
            self.assertTrue(any("synthetic fixture" in problem for problem in report["guard_problems"]))

    def malformed_json_is_invalid_input_not_a_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp)
            (run / "cases.jsonl").write_text("{not json\n")
            self.assertEqual(p.verdict(run, identity=GOOD_IDENTITY)["exit_status"], 3)

    def mixed_raw_formats_are_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp)
            first = record("native", "x.t", [case(True, "tried 'unlink pjdfstest_a'")])
            second = record("cowfs", "x.t", [case(True, "tried 'unlink pjdfstest_b'")])
            first.pop("synthetic")
            second.pop("synthetic")
            first["raw"] = str(run / "a.tap")
            (run / "a.tap").write_text("1..1\nok 1\n")
            first["raw_sha256"] = p.sha256(run / "a.tap")
            (run / "cases.jsonl").write_text(json.dumps(first, sort_keys=True) + "\n" +
                                              json.dumps(second, sort_keys=True) + "\n")
            report = p.verdict(run, identity=GOOD_IDENTITY)
            self.assertEqual((report["state"], report["exit_status"]), (p.INVALID, 3))
            self.assertTrue(any("mixes cases" in problem for problem in report["guard_problems"]))


class TestList(unittest.TestCase):
    def test_groups_and_explicit_cases_filter(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "tests"
            for grp in ("open", "link"):
                (root / grp).mkdir(parents=True)
                (root / grp / "00.t").touch()
            self.assertEqual(p.test_list(root, ["link"], None), ["link/00.t"])
            self.assertEqual(p.test_list(root, None, ["open/00.t"]), ["open/00.t"])
            self.assertEqual(p.test_list(root, ["nope"], None), [])


class RuntimeIdentityIsFailClosed(unittest.TestCase):
    """A run is refused before any child exists unless both arms are on identified filesystems."""

    def setUp(self):
        self.spawned = []

    def spawn(self, *args, **_kwargs):
        """Stands in for the first case process. A refused identity must never reach it."""
        self.spawned.append(args)

    def check(self, identity, expected_cowfs_mount=None):
        """What main() does: refuse before anything is spawned, or proceed to run cases."""
        problems = p.validate_runtime_identity(identity, expected_cowfs_mount)
        if not problems:
            self.spawn("native", "cowfs")
            return p.PASS
        for problem in problems:
            self.assertEqual(problem["kind"], p.INTEGRITY)
        return p.state_from(problems)

    def test_a_valid_identity_passes_and_proceeds(self):
        self.assertEqual(self.check(GOOD_IDENTITY), p.PASS)
        self.assertEqual(self.spawned, [("native", "cowfs")])

    def test_no_identity_at_all_is_invalid(self):
        self.assertEqual(self.check(None), p.INVALID)

    def test_missing_native_arm_is_invalid(self):
        identity = {"native": GOOD_IDENTITY["native"]}
        self.assertEqual(self.check(identity), p.INVALID)

    def test_missing_cowfs_arm_is_invalid(self):
        self.assertEqual(self.check({"native": GOOD_IDENTITY["native"]}), p.INVALID)

    def test_a_stat_problem_is_invalid(self):
        identity = {"native": GOOD_IDENTITY["native"],
                    "cowfs": {**GOOD_IDENTITY["cowfs"], "problem": "stat failed: No such file"}}
        self.assertEqual(self.check(identity), p.INVALID)

    def test_a_null_device_is_invalid_and_equal_devices_alone_would_have_passed(self):
        identity = {"native": {**GOOD_IDENTITY["native"], "st_dev": None},
                    "cowfs": {**GOOD_IDENTITY["cowfs"], "st_dev": None}}
        # Both null devices are equal, so an equality check alone would have accepted this.
        self.assertEqual(self.check(identity), p.INVALID)
        self.assertTrue(any("no st_dev" in problem["message"] for problem in
                            p.validate_runtime_identity(identity)))

    def test_a_null_type_is_invalid(self):
        identity = {"native": GOOD_IDENTITY["native"], "cowfs": {**GOOD_IDISTRY, "fstype": None}}
        self.assertEqual(self.check(identity), p.INVALID)

    def test_a_missing_mount_point_is_invalid(self):
        identity = {"native": GOOD_IDENTITY["native"],
                    "cowfs": {**GOOD_IDISTRY, "mountpoint": None}}
        self.assertEqual(self.check(identity), p.INVALID)

    def test_a_nonpositive_or_noninteger_device_is_invalid(self):
        for device in (0, -1, "16777234", True):
            identity = {"native": {**GOOD_IDENTITY["native"], "st_dev": device},
                        "cowfs": GOOD_IDISTRY}
            self.assertEqual(self.check(identity), p.INVALID, f"device {device!r} was accepted")

    def test_the_wrong_mount_is_invalid(self):
        identity = {"native": GOOD_IDENTITY["native"],
                    "cowfs": {**GOOD_IDISTRY, "mountpoint": "/somewhere/else"}}
        self.assertEqual(self.check(identity, expected_cowfs_mount="/tmp/mnt"), p.INVALID)
        # The same identity is fine when nothing says where the mount should have been.
        self.assertEqual(p.validate_runtime_identity(identity), [])

    def test_equal_devices_are_invalid(self):
        identity = {"native": GOOD_IDENTITY["native"],
                    "cowfs": {**GOOD_IDISTRY, "st_dev": GOOD_IDENTITY["native"]["st_dev"]}}
        self.assertEqual(self.check(identity), p.INVALID)


class ExitTaxonomy(unittest.TestCase):
    def test_integrity_outranks_divergence_and_capability(self):
        self.assertEqual(p.state_from([{"kind": p.INTEGRITY}, {"kind": p.DIVERGENCE}]), p.INVALID)
        self.assertEqual(p.state_from([{"kind": p.DIVERGENCE}, {"kind": p.CAPABILITY}]), p.FAIL)
        self.assertEqual(p.state_from([{"kind": p.CAPABILITY}]), p.UNMEASURABLE)
        self.assertEqual(p.state_from([{"kind": p.COVERAGE}]), p.PASS)

    def test_statuses(self):
        self.assertEqual(p.EXIT_STATUS, {p.PASS: 0, p.FAIL: 1, p.UNMEASURABLE: 2, p.INVALID: 3})

    def test_cli_exit_codes_are_read_from_the_process_not_from_a_predicate(self):
        done = subprocess.run([sys.executable, str(Path(p.__file__)), "--reconcile",
                               "/nonexistent-run-dir-for-the-exit-check"],
                              capture_output=True, text=True, timeout=120, check=False)
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("INVALID", done.stdout + done.stderr)


class ReconcileNeverOverwrites(unittest.TestCase):
    """An analysis reads evidence; it never writes into it.

    Every case here runs against a fixture copied into this lane's own tree, never against a real
    run directory, so a mistake in the writer cannot touch preserved evidence.
    """

    FIXTURE_ROOT = Path(__file__).resolve().parents[1] / "bench" / "out" / "ready-g3" / \
        "reconcile-safety"
    SENTINEL = "this file is evidence and must not change\n"

    def setUp(self):
        self.run_dir = self.FIXTURE_ROOT / "fixture-run"
        if not (self.run_dir / "cases.jsonl").is_file():
            self.skipTest("fixture run is missing; copy a receipted run into " + str(self.FIXTURE_ROOT))
        self.before = self.hashes(self.run_dir)

    @staticmethod
    def hashes(root: Path) -> dict:
        return {str(p.relative_to(root)): p.stat().st_size for p in sorted(root.rglob("*")) if p.is_file()}

    @staticmethod
    def digests(root: Path) -> dict:
        return {str(p.relative_to(root)): p.sha256 if hasattr(p, "sha256") else
                __import__("hashlib").sha256(p.read_bytes()).hexdigest()
                for p in sorted(root.rglob("*")) if p.is_file()}

    def reconcile(self, *extra):
        return subprocess.run([sys.executable, str(Path(p.__file__)), "--reconcile",
                               str(self.run_dir), *extra], capture_output=True, text=True,
                              timeout=300, check=False)

    def test_an_existing_default_receipt_is_refused_and_not_one_byte_changes(self):
        sentinel = self.run_dir / "reconciliation.json"
        self.assertFalse(sentinel.exists(), "the fixture is expected to start without one")
        sentinel.write_text(self.SENTINEL)
        before = (sentinel.read_bytes(), self.digests(self.run_dir))
        try:
            done = self.reconcile()
            self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
            self.assertIn("already exists", done.stdout + done.stderr)
            self.assertEqual(sentinel.read_bytes(), before[0], "the sentinel was modified")
            self.assertEqual(self.digests(self.run_dir), before[1], "a byte of the run changed")
        finally:
            sentinel.unlink()

    def test_the_input_run_is_unchanged_by_a_successful_fresh_analysis(self):
        out_dir = self.FIXTURE_ROOT / "analysis-out"
        out_dir.mkdir(parents=True, exist_ok=True)
        out = out_dir / "fresh.json"
        if out.exists():
            out.unlink()
        before = self.digests(self.run_dir)
        done = self.reconcile("--output", str(out))
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("FAIL", done.stdout)
        self.assertEqual(self.digests(self.run_dir), before, "the analysis touched its own input")
        payload = json.loads(out.read_text())
        self.assertEqual(payload["state"], p.FAIL)
        self.assertEqual(payload["exit_status"], 1)
        self.assertTrue(payload["analysis"]["analyser_sha256"])
        self.assertEqual(payload["analysis"]["inputs"]["cases.jsonl"],
                         p.sha256(self.run_dir / "cases.jsonl"))
        self.assertIn("fresh reading", payload["analysis"]["note"])
        out.unlink()

    def test_an_output_inside_the_run_is_refused(self):
        done = self.reconcile("--output", str(self.run_dir / "cases.jsonl"))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("inside the run directory", done.stdout + done.stderr)

    def test_an_output_equal_to_a_raw_stream_is_refused(self):
        raw = min((self.run_dir / "raw").iterdir(), key=lambda q: q.name)
        before = raw.read_bytes()
        done = self.reconcile("--output", str(raw))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertEqual(raw.read_bytes(), before, "a raw stream was overwritten")

    def test_an_existing_explicit_output_is_refused(self):
        out_dir = self.FIXTURE_ROOT / "analysis-out"
        out_dir.mkdir(parents=True, exist_ok=True)
        out = out_dir / "taken.json"
        out.write_text(self.SENTINEL)
        done = self.reconcile("--output", str(out))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertEqual(out.read_text(), self.SENTINEL, "an existing output was overwritten")
        out.unlink()

    def test_a_run_without_an_identity_receipt_stays_invalid_three(self):
        run_dir = self.FIXTURE_ROOT / "no-identity-run"
        run_dir.mkdir(parents=True, exist_ok=True)
        shutil.copy(self.run_dir / "cases.jsonl", run_dir / "cases.jsonl")
        shutil.copytree(self.run_dir / "raw", run_dir / "raw", dirs_exist_ok=True)
        out = self.FIXTURE_ROOT / "analysis-out" / "no-identity.json"
        if out.exists():
            out.unlink()
        done = subprocess.run([sys.executable, str(Path(p.__file__)), "--reconcile", str(run_dir),
                               "--output", str(out)], capture_output=True, text=True, timeout=300,
                              check=False)
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("INVALID", done.stdout)

    def test_write_exclusive_leaves_no_staged_file_behind_on_success(self):
        out_dir = self.FIXTURE_ROOT / "analysis-out"
        out_dir.mkdir(parents=True, exist_ok=True)
        out = out_dir / "exclusive.json"
        if out.exists():
            out.unlink()
        written, message = p.write_exclusive(out, "{}\n")
        self.assertTrue(written, message)
        self.assertEqual(out.read_text(), "{}\n")
        self.assertEqual([q.name for q in out_dir.glob("*.staged-*")], [])
        ok, message = p.write_exclusive(out, "second\n")
        self.assertFalse(ok)
        self.assertEqual(out.read_text(), "{}\n", "the second write changed the file")
        out.unlink()


if __name__ == "__main__":
    unittest.main(verbosity=2)