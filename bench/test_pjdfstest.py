#!/usr/bin/env python3
"""Checks for the matched-arm logic in bench/pjdfstest.py.

Only the parts that can decide a verdict are here: what a TAP stream means, which difference
counts as a regression, and which runs are refused instead of called a pass.
"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import pjdfstest  # noqa: E402


def rec(arm, test, cases, plan=None, rc=0):
    return {"arm": arm, "test": test, "cases": cases, "plan": plan, "ok": sum(1 for c in cases if c["ok"]),
            "not_ok": sum(1 for c in cases if not c["ok"]), "rc": rc, "todo": 0, "timed_out": False}


def case(ok, detail="", root=False, n=1):
    return {"n": n, "ok": ok, "todo": False, "detail": detail, "root_required": root}


class ParseTap(unittest.TestCase):
    def test_plan_and_counts(self):
        tap = pjdfstest.parse_tap(
            "1..3\nok 1\nok 2\nnot ok 3 - tried 'mkdir x', expected 0, got EPERM\n")
        self.assertEqual(tap["plan"], 3)
        self.assertEqual((tap["ok"], tap["not_ok"]), (2, 1))

    def test_todo_marker_is_a_pass(self):
        tap = pjdfstest.parse_tap("1..2\nok 1 # TODO x\nnot ok 2 - boom\n")
        self.assertEqual(tap["ok"], 1)
        self.assertEqual(tap["todo"], 1)
        self.assertTrue(tap["cases"][0]["todo"])

    def test_quick_exit_is_one_ok_not_a_run(self):
        tap = pjdfstest.parse_tap("1..1\nok 1\n")
        self.assertEqual((tap["plan"], tap["ok"]), (1, 1))

    def test_no_plan_yields_no_cases(self):
        self.assertEqual(pjdfstest.parse_tap("not ok - could not find pjdfstest app\n")["cases"], [])


class RootRequired(unittest.TestCase):
    def test_uid_switch_needs_privilege(self):
        self.assertTrue(pjdfstest.ROOT_REQUIRED_RE.search("tried '-u 65534 -g 65534 mkdir x 0755'"))
        self.assertTrue(pjdfstest.ROOT_REQUIRED_RE.search("tried 'mknod x b 0644 1 2'"))
        self.assertTrue(pjdfstest.ROOT_REQUIRED_RE.search("not root"))
        self.assertFalse(pjdfstest.ROOT_REQUIRED_RE.search("tried 'rename a b'"))


class Compare(unittest.TestCase):
    def test_native_pass_cowfs_fail_is_a_regression(self):
        native = {"open/00.t": rec("native", "open/00.t", [case(True, n=1), case(True, n=2)])}
        cowfs = {"open/00.t": rec("cowfs", "open/00.t", [case(True, n=1), case(False, n=2)])}
        diff = pjdfstest.compare(native, cowfs)
        self.assertEqual([r["n"] for r in diff["regressions"]], [2])

    def test_cowfs_looser_is_reported_separately(self):
        native = {"x.t": rec("native", "x.t", [case(False)])}
        cowfs = {"x.t": rec("cowfs", "x.t", [case(True)])}
        diff = pjdfstest.compare(native, cowfs)
        self.assertEqual(diff["regressions"], [])
        self.assertEqual([r["n"] for r in diff["cowfs_looser_than_native"]], [1])

    def test_identical_arms_have_no_differences(self):
        same = [case(True), case(False, "boom")]
        diff = pjdfstest.compare({"x.t": rec("native", "x.t", same)}, {"x.t": rec("cowfs", "x.t", same)})
        self.assertEqual((diff["regressions"], diff["cowfs_looser_than_native"]), ([], []))

    def test_root_required_regression_is_not_a_clean_failure(self):
        native = {"x.t": rec("native", "x.t", [case(True, "chown . 65534 65534", True)])}
        cowfs = {"x.t": rec("cowfs", "x.t", [case(False, "chown . 65534 65534", True)])}
        diff = pjdfstest.compare(native, cowfs)
        self.assertEqual(len(diff["regressions"]), 1)
        self.assertEqual(diff["regressions_outside_root_required"], [])

    def test_case_on_one_arm_only_is_unpaired(self):
        diff = pjdfstest.compare({"a.t": rec("native", "a.t", [case(True)])}, {})
        self.assertEqual(diff["unpaired_cases"], ["a.t"])


class Verdict(unittest.TestCase):
    totals = {"native": {"executed": 2, "empty_output": 0, "timeouts": 0},
              "cowfs": {"executed": 2, "empty_output": 0, "timeouts": 0}}
    clean = {"regressions": [], "regressions_outside_root_required": [], "cowfs_looser_than_native": [],
             "unpaired_cases": []}

    def test_matched_passes(self):
        v = pjdfstest.verdict(self.totals, self.clean, ["a.t"], ["HAVE_OPENAT"])
        self.assertEqual(v["state"], "PASS")

    def test_regression_fails(self):
        diff = {**self.clean, "regressions": [{"test": "a.t", "n": 1, "root_required": False}],
                "regressions_outside_root_required": [{"test": "a.t", "n": 1, "root_required": False}]}
        self.assertEqual(pjdfstest.verdict(self.totals, diff, ["a.t"], [])["state"], "FAIL")

    def test_empty_run_is_unmeasurable_not_a_pass(self):
        totals = {"native": {"executed": 0, "empty_output": 1, "timeouts": 0},
                  "cowfs": {"executed": 2, "empty_output": 0, "timeouts": 0}}
        self.assertEqual(pjdfstest.verdict(totals, self.clean, ["a.t"], [])["state"], "UNMEASURABLE")

    def test_timeout_is_unmeasurable_not_a_verdict(self):
        totals = {"native": {"executed": 2, "empty_output": 0, "timeouts": 1},
                  "cowfs": {"executed": 2, "empty_output": 0, "timeouts": 0}}
        self.assertEqual(pjdfstest.verdict(totals, self.clean, ["a.t"], [])["state"], "UNMEASURABLE")


class TestList(unittest.TestCase):
    def test_groups_and_explicit_cases_filter(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "tests"
            for grp, name in (("open", "00.t"), ("link", "00.t")):
                (root / grp).mkdir(parents=True)
                (root / grp / name).touch()
            self.assertEqual(pjdfstest.test_list(root, ["link"], None), ["link/00.t"])
            self.assertEqual(pjdfstest.test_list(root, None, ["open/00.t"]), ["open/00.t"])
            self.assertEqual(pjdfstest.test_list(root, ["nope"], None), [])


if __name__ == "__main__":
    unittest.main(verbosity=2)