#!/usr/bin/env python3
"""Tests for bench/xfstests_gate.py. Run: python3 -m unittest discover -s bench -v

Everything here is synthetic: a fake xfstests tree in a temp dir, no suite, no
mount, no root, no network. What is tested is the harness, including the paths
where it must refuse rather than pass.
"""

import contextlib
import io
import json
import os
import stat
import tempfile
import unittest
from pathlib import Path

import xfstests_gate as gate


def write(path, text, executable=False):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    if executable:
        path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return path


PRELUDE = "#! /bin/sh\n. ./common/preamble\n_begin_fstest auto quick\n. ./common/filter\n\n"

# The harness checks the suite's startup gate by tool name. The Mac has no mkfs
# or xfs_io, so a test that wants the gate open narrows it to what exists here.
MAC_GATE = [("bash", "test"), ("sh", "test")]


class Tree:
    """A fake xfstests tree: tests/generic/NNN, common/ at the root, ltp/."""

    def __init__(self, root):
        self.root = Path(root)
        write(self.root / "common" / "preamble", "_begin_fstest() { :; }\n")
        write(self.root / "common" / "rc", "# fake rc\n")
        write(self.root / "common" / "filter", "# fake filter\n")
        write(self.root / "common" / "promotion", "# fake promotion\n")
        write(self.root / "ltp" / "fsstress", "#!/bin/sh\nexit 0\n", executable=True)
        write(self.root / "ltp" / "fsx", "#!/bin/sh\nexit 0\n", executable=True)

    def case(self, cid, body, extra=""):
        return write(self.root / "tests" / "generic" / cid, PRELUDE + extra + body + "\nexit $status\n",
                     executable=True)

    def passing(self, cid):
        self.case(cid, "status=0")

    def failing(self, cid):
        self.case(cid, "status=1", extra='echo "not run" >&2\n')


class HarnessCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.tmp_path = Path(self.tmp.name)
        self.tree = Tree(self.tmp_path / "xfstests")
        self.native = write(self.tmp_path / "native" / ".keep", "")
        self.cowfs = write(self.tmp_path / "cowfs" / ".keep", "")

    def out(self, name="out"):
        return str(self.tmp_path / name)

    def open_gate(self):
        gate.STARTUP_GATE = list(MAC_GATE)
        gate.STARTUP_GATE_FILES = list(gate.STARTUP_GATE_FILES)

    def args(self, cases=None, **kw):
        class A:
            pass
        a = A()
        a.xfstests = str(self.tree.root)
        a.out = self.out()
        a.native_root = str(self.native.parent)
        a.cowfs_root = str(self.cowfs.parent)
        a.cases = cases
        a.timeout = 60
        a.require_full = False
        for k, v in kw.items():
            setattr(a, k, v)
        return a


class Classification(HarnessCase):
    def test_plain_case_is_safe(self):
        write(self.tree.root / "tests" / "generic" / "500", PRELUDE + "status=0\nexit $status\n")
        rec = gate.classify_case(self.tree.root / "tests" / "generic" / "500")
        self.assertEqual(rec["verdict"], "SAFE", rec)

    def test_mkfs_case_is_refused_with_the_line(self):
        write(self.tree.root / "tests" / "generic" / "501", PRELUDE + 'mkfs.ext4 -F $TEST_DEV\nstatus=0\n')
        rec = gate.classify_case(self.tree.root / "tests" / "generic" / "501")
        self.assertEqual(rec["verdict"], "NEEDS_DEVICE")
        self.assertTrue(any("formats a filesystem" in r for r in rec["reasons"]), rec)
        self.assertTrue(any(r.startswith("line ") for r in rec["reasons"]), rec)

    def test_loop_and_scratch_are_refused(self):
        write(self.tree.root / "tests" / "generic" / "502", PRELUDE + "losetup /dev/loop0 x\n")
        self.assertEqual(gate.classify_case(self.tree.root / "tests" / "generic" / "502")["verdict"],
                         "NEEDS_DEVICE")
        write(self.tree.root / "tests" / "generic" / "503", PRELUDE + "_require_scratch\n")
        self.assertEqual(gate.classify_case(self.tree.root / "tests" / "generic" / "503")["verdict"],
                         "NEEDS_SCRATCH")

    def test_root_case_is_refused(self):
        write(self.tree.root / "tests" / "generic" / "504", PRELUDE + "sudo mkfs.xfs $TEST_DEV\n")
        self.assertEqual(gate.classify_case(self.tree.root / "tests" / "generic" / "504")["verdict"],
                         "NEEDS_ROOT")

    def test_destructive_absolute_path_is_refused(self):
        write(self.tree.root / "tests" / "generic" / "505",
              PRELUDE + "rm -rf /home/other/pool\nstatus=0\n")
        rec = gate.classify_case(self.tree.root / "tests" / "generic" / "505")
        self.assertEqual(rec["verdict"], "UNSAFE")
        self.assertTrue(any("/home/other/pool" in r for r in rec["reasons"]), rec)

    def test_removing_the_test_dir_is_allowed(self):
        write(self.tree.root / "tests" / "generic" / "506",
              PRELUDE + "rm -rf $TEST_DIR/tmp\nrm -fr $TEST_DIR/*\nstatus=0\n")
        self.assertEqual(gate.classify_case(self.tree.root / "tests" / "generic" / "506")["verdict"],
                         "SAFE")

    def test_system_binaries_are_not_destructive(self):
        write(self.tree.root / "tests" / "generic" / "507",
              PRELUDE + "cp /bin/true $TEST_DIR/true\nstatus=0\n")
        self.assertEqual(gate.classify_case(self.tree.root / "tests" / "generic" / "507")["verdict"],
                         "SAFE")

    def test_helper_case_is_refused(self):
        write(self.tree.root / "tests" / "generic" / "508", PRELUDE + "_run_aiodio helper\nstatus=0\n")
        self.assertEqual(gate.classify_case(self.tree.root / "tests" / "generic" / "508")["verdict"],
                         "NEEDS_HELPER")

    def test_a_mount_wrapper_is_refused(self):
        # The suite calls a mount through a wrapper, so a bare \bmount\b misses it.
        write(self.tree.root / "tests" / "generic" / "510", PRELUDE + "_test_cycle_mount\nstatus=0\n")
        self.assertEqual(gate.classify_case(self.tree.root / "tests" / "generic" / "510")["verdict"],
                         "NEEDS_DEVICE")

    def test_a_helper_program_variable_is_refused(self):
        write(self.tree.root / "tests" / "generic" / "511",
              PRELUDE + "$FSX_PROG -a 4096\nstatus=0\n")
        rec = gate.classify_case(self.tree.root / "tests" / "generic" / "511")
        self.assertEqual(rec["verdict"], "NEEDS_HELPER")

    def test_a_workspace_size_cap_is_refused(self):
        write(self.tree.root / "tests" / "generic" / "512", PRELUDE + "truncate -s 4G $TEST_DIR/junk\n")
        rec = gate.classify_case(self.tree.root / "tests" / "generic" / "512")
        self.assertEqual(rec["verdict"], "NEEDS_BIG_SPACE")
        write(self.tree.root / "tests" / "generic" / "513", PRELUDE + "truncate -s 64M $TEST_DIR/junk\n")
        self.assertNotEqual(gate.classify_case(self.tree.root / "tests" / "generic" / "513")["verdict"],
                            "NEEDS_BIG_SPACE")

    def test_ownership_change_is_refused(self):
        write(self.tree.root / "tests" / "generic" / "514", PRELUDE + "chown 100:100 $TEST_DIR/f\nstatus=0\n")
        self.assertEqual(gate.classify_case(self.tree.root / "tests" / "generic" / "514")["verdict"],
                         "NEEDS_ROOT")

    def test_unreviewed_sourced_code_is_refused(self):
        write(self.tree.root / "tests" / "generic" / "509",
              PRELUDE + ". ./common/secret_helpers\nstatus=0\n")
        rec = gate.classify_case(self.tree.root / "tests" / "generic" / "509")
        self.assertEqual(rec["verdict"], "UNREAD_SOURCE")
        self.assertTrue(any("common/secret_helpers" in r for r in rec["reasons"]), rec)


class Allowlist(HarnessCase):
    def test_drift_is_reported(self):
        self.tree.passing("300")
        self.tree.case("301", "mkfs.ext4 -F $TEST_DEV\n")
        records = gate.classify_group(self.tree.root / "tests")
        gate.ALLOWLIST_FILE = self.tmp_path / "allowlist.txt"
        write(gate.ALLOWLIST_FILE, "999\n")
        allow, drift = gate.check_allowlist_drift(records)
        self.assertEqual(allow, ["300"])
        self.assertIn("added ['300']", drift)
        self.assertIn("removed ['999']", drift)

    def test_no_drift_when_the_file_matches(self):
        self.tree.passing("300")
        self.tree.case("301", "mkfs.ext4 -F $TEST_DEV\n")
        records = gate.classify_group(self.tree.root / "tests")
        gate.ALLOWLIST_FILE = self.tmp_path / "allowlist.txt"
        write(gate.ALLOWLIST_FILE, "# reviewed\n300\n")
        allow, drift = gate.check_allowlist_drift(records)
        self.assertEqual(allow, ["300"])
        self.assertIsNone(drift)


class Preflight(HarnessCase):
    def test_missing_helper_makes_every_case_unrunnable(self):
        saved = gate.STARTUP_GATE_FILES
        self.addCleanup(setattr, gate, "STARTUP_GATE_FILES", saved)
        gate.STARTUP_GATE_FILES = [("ltp/fsstress", "common/config:123 fsstress not found or executable")]
        (self.tree.root / "ltp" / "fsstress").unlink()
        self.open_gate()
        self.tree.passing("010")
        rec = gate.preflight(self.tree.root, self.out())
        self.assertEqual(rec["verdict"], "UNMEASURABLE")
        self.assertTrue(any(b["key"] == "ltp/fsstress" for b in rec["blocking"]), rec)
        self.assertIn("not found or executable", rec["reason"])
        record = json.loads((self.out_path() / "preflight.jsonl").read_text().splitlines()[0])
        self.assertEqual(record["verdict"], "UNMEASURABLE")

    def out_path(self):
        return Path(self.out())

    def test_present_gate_and_passing_probe_is_ok(self):
        self.open_gate()
        self.tree.passing("010")
        rec = gate.preflight(self.tree.root, self.out())
        self.assertEqual(rec["verdict"], "PASS")
        self.assertEqual(rec["blocking"], [])
        self.assertEqual(rec["suite_probe"]["rc"], 0)

    def test_probe_failure_is_unmeasurable_not_pass(self):
        self.open_gate()
        self.tree.failing("010")
        rec = gate.preflight(self.tree.root, self.out())
        self.assertEqual(rec["verdict"], "UNMEASURABLE")
        self.assertEqual(rec["suite_probe"]["rc"], 1)

    def test_probe_uses_a_real_private_directory(self):
        self.open_gate()
        self.tree.passing("010")
        rec = gate.preflight(self.tree.root, self.out())
        probe = rec["suite_probe"]
        self.assertTrue(Path(probe["test_dir"]).is_dir())
        self.assertTrue(str(probe["test_dir"]).startswith(str(Path(self.out()).resolve())))
        self.assertIn("arm=native", Path(probe["log"]).read_text().splitlines()[0])


class CaseRun(HarnessCase):
    def test_exit_code_comes_from_the_child(self):
        self.tree.case("010", "status=7")
        rec = gate.run_case(self.tree.root / "tests", self.tree.root / "tests" / "generic" / "010",
                            self.tmp_path / "w", "native", 60)
        self.assertEqual(rec["rc"], 7)

    def test_case_directory_is_immutable_per_attempt(self):
        self.tree.passing("010")
        work = self.tmp_path / "w"
        gate.run_case(self.tree.root / "tests", self.tree.root / "tests" / "generic" / "010",
                      work, "native", 60)
        with self.assertRaises(FileExistsError):
            gate.run_case(self.tree.root / "tests", self.tree.root / "tests" / "generic" / "010",
                          work, "native", 60)

    def test_timeout_is_bounded_and_recorded(self):
        self.tree.case("010", "sleep 30")
        rec = gate.run_case(self.tree.root / "tests", self.tree.root / "tests" / "generic" / "010",
                            self.tmp_path / "w", "native", 2)
        self.assertTrue(rec["timed_out"])
        self.assertEqual(rec["rc"], -9)

    def test_environment_is_the_same_on_both_arms(self):
        self.tree.case("010", 'echo "dir=$TEST_DIR fstyp=[$FSTYP] scratch=[$SCRATCH_DEV]"\nstatus=0')
        rec = gate.run_case(self.tree.root / "tests", self.tree.root / "tests" / "generic" / "010",
                            self.tmp_path / "w", "cowfs", 60)
        log = Path(rec["log"]).read_text()
        self.assertIn("fstyp=[]", log)
        self.assertIn("scratch=[]", log)

    def test_a_silent_skip_is_caught(self):
        self.tree.case("010", 'echo "generic/010: [: 3: unary operator expected"\nstatus=0')
        rec = gate.run_case(self.tree.root / "tests", self.tree.root / "tests" / "generic" / "010",
                            self.tmp_path / "w", "native", 60)
        self.assertTrue(any("malformed" in s for s in rec["skips"]), rec)

    def test_a_missing_helper_is_caught(self):
        self.tree.case("010", 'echo "common/rc: line 9: /xfstests/src/mkfile: No such file or directory"\nstatus=0')
        rec = gate.run_case(self.tree.root / "tests", self.tree.root / "tests" / "generic" / "010",
                            self.tmp_path / "w", "native", 60)
        self.assertTrue(any("helper binary" in s for s in rec["skips"]), rec)

    def test_an_expected_enoent_is_not_a_skip(self):
        # A case that asserts ENOENT on its own data file prints that message on
        # purpose. Flagging it would turn a real pass into a false INVALID.
        self.tree.case("010", 'echo "ls: cannot access \'$TEST_DIR/gone\': No such file or directory"\nstatus=0')
        rec = gate.run_case(self.tree.root / "tests", self.tree.root / "tests" / "generic" / "010",
                            self.tmp_path / "w", "native", 60)
        self.assertEqual(rec["skips"], [], rec)


class Verdicts(unittest.TestCase):
    def case(self, rc, skips=None):
        return {"rc": rc, "skips": skips or []}

    def test_both_pass(self):
        v, _ = gate.verdict_for_case(self.case(0), self.case(0))
        self.assertEqual(v, "PASS")

    def test_cowfs_worse_is_fail(self):
        v, why = gate.verdict_for_case(self.case(0), self.case(1))
        self.assertEqual(v, "FAIL")
        self.assertIn("cowfs rc=1", why)

    def test_native_failure_is_never_a_cowfs_verdict(self):
        v, why = gate.verdict_for_case(self.case(1), self.case(0))
        self.assertEqual(v, "PASS")
        self.assertIn("native failed", why)
        v, why = gate.verdict_for_case(self.case(1), self.case(1))
        self.assertEqual(v, "UNMEASURABLE")

    def test_a_skipped_assertion_is_invalid_on_either_arm(self):
        v, why = gate.verdict_for_case(self.case(0, ["command not found"]), self.case(0))
        self.assertEqual(v, "INVALID")
        self.assertIn("did not assert", why)
        v, why = gate.verdict_for_case(self.case(0), self.case(0, ["command not found"]))
        self.assertEqual(v, "INVALID")

    def test_cowfs_failure_with_a_skip_is_invalid_not_fail(self):
        v, _ = gate.verdict_for_case(self.case(0), self.case(1, ["command not found"]))
        self.assertEqual(v, "INVALID")


class EndToEnd(HarnessCase):
    def run_gate(self, args):
        """gate.run prints its verdict; capture it so the test output stays readable."""
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            rc = gate.run(args)
        return rc, buf.getvalue()

    def test_matched_run_records_exact_codes(self):
        self.open_gate()
        self.tree.passing("010")
        self.tree.passing("020")
        gate.ALLOWLIST_FILE = self.tmp_path / "allowlist.txt"
        write(gate.ALLOWLIST_FILE, "010\n020\n")
        rc, out = self.run_gate(self.args(cases="010,020"))
        self.assertEqual(rc, 0)
        self.assertIn("VERDICT: PASS", out)
        run_dir = next(Path(self.out()).glob("run-*"))
        rows = [json.loads(l) for l in (run_dir / "results.jsonl").read_text().splitlines()]
        self.assertEqual([r["kind"] for r in rows[1:]], ["case_verdict", "case_verdict"])
        self.assertTrue(all(r["verdict"] == "PASS" for r in rows[1:]))
        self.assertTrue(all(r["native_rc"] == 0 and r["cowfs_rc"] == 0 for r in rows[1:]))
        for rec in rows[1:]:
            self.assertTrue(Path(rec["native_log"]).is_file())
            self.assertTrue(Path(rec["cowfs_log"]).is_file())

    def test_cowfs_failure_exits_one_and_keeps_the_evidence(self):
        self.open_gate()
        # A case whose result depends on the arm: it fails when the test
        # directory sits on the cowfs side, which is the arm that matters.
        self.tree.case("010", 'case "$TEST_DIR" in *cowfs*) status=1 ;; *) status=0 ;; esac')
        gate.ALLOWLIST_FILE = self.tmp_path / "allowlist.txt"
        write(gate.ALLOWLIST_FILE, "010\n")
        rc, _ = self.run_gate(self.args(cases="010"))
        run_dir = next(Path(self.out()).glob("run-*"))
        rows = [json.loads(l) for l in (run_dir / "results.jsonl").read_text().splitlines()]
        verdict = rows[-1]
        self.assertEqual(verdict["native_rc"], 0)
        self.assertEqual(verdict["cowfs_rc"], 1)
        self.assertEqual(verdict["verdict"], "FAIL")
        self.assertEqual(rc, 1)

    def test_case_outside_the_allowlist_is_invalid(self):
        self.open_gate()
        self.tree.passing("010")
        self.tree.passing("020")
        gate.ALLOWLIST_FILE = self.tmp_path / "allowlist.txt"
        write(gate.ALLOWLIST_FILE, "010\n")
        rc, _ = self.run_gate(self.args(cases="010,020"))
        self.assertEqual(rc, 3)

    def test_require_full_refuses_partial_coverage(self):
        self.open_gate()
        self.tree.passing("010")
        self.tree.passing("020")
        gate.ALLOWLIST_FILE = self.tmp_path / "allowlist.txt"
        write(gate.ALLOWLIST_FILE, "010\n020\n")
        rc, out = self.run_gate(self.args(cases="010", require_full=True))
        self.assertEqual(rc, 2)

    def test_allowlist_drift_stops_the_run(self):
        self.open_gate()
        self.tree.passing("010")
        gate.ALLOWLIST_FILE = self.tmp_path / "allowlist.txt"
        write(gate.ALLOWLIST_FILE, "777\n")
        rc, _ = self.run_gate(self.args(cases="010"))
        self.assertEqual(rc, 3)

    def test_report_counts(self):
        run_dir = self.tmp_path / "run-x"
        gate.write_jsonl(run_dir / "results.jsonl", [
            {"kind": "meta", "allowlist_sha": "a" * 64, "cases": ["010"]},
            {"kind": "case_verdict", "case": "generic/010", "verdict": "PASS", "why": "both arms exited 0",
             "native_rc": 0, "cowfs_rc": 0, "native_log": "n", "cowfs_log": "c",
             "native_skips": [], "cowfs_skips": [], "native_wall_s": 1.0, "cowfs_wall_s": 1.0},
        ])

        class A:
            run = str(run_dir)

        self.assertEqual(gate.report(A()), 0)


if __name__ == "__main__":
    unittest.main()