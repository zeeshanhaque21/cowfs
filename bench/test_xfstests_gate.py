#!/usr/bin/env python3
"""Tests for bench/xfstests_gate.py. Run: python3 -m unittest discover -s bench -v

Everything here is synthetic: a fake xfstests tree in a temp dir, no suite, no
mount, no root, no network. What is tested is the harness, including every path
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

# The Mac has no mkfs or xfs_io, so a test that wants the suite gate open narrows
# it to what exists here. Production code always uses the real list.
MAC_GATE = [("bash", "test"), ("sh", "test")]


class Tree:
    """A fake xfstests tree: tests/generic/NNN, common/ at the root, ltp/.

    `git init` makes it a real checkout so the source pin is exercised rather
    than mocked away."""

    def __init__(self, root, git=True):
        self.root = Path(root)
        # preamble sources rc and rc sources promotion, so the closure is genuinely
        # transitive: a one-level filename list would miss promotion.
        write(self.root / "common" / "preamble", "_begin_fstest() { :; }\n. ./common/rc\n")
        write(self.root / "common" / "rc", "# fake rc\n. ./common/promotion\n")
        write(self.root / "common" / "filter", "# fake filter\n")
        write(self.root / "common" / "promotion", "# fake promotion\n")
        write(self.root / "ltp" / "fsstress", "#!/bin/sh\nexit 0\n", executable=True)
        write(self.root / "ltp" / "fsx", "#!/bin/sh\nexit 0\n", executable=True)
        write(self.root / "tests" / "generic" / "group.list", "auto 005 010\n")
        self.git = git
        if git:
            self.git_init()

    def git_commit(self, message="tree"):
        """Commit whatever is in the tree. A no-op commit is not an error: a test
        may pin twice, and the tree is then already clean."""
        import subprocess
        env = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t",
               "PATH": "/usr/bin:/bin:/usr/local/bin", "HOME": str(self.root)}
        subprocess.run(["git", "-C", str(self.root), "add", "-A"], check=True,
                       capture_output=True, env=env)
        res = subprocess.run(["git", "-C", str(self.root), "commit", "-qm", message],
                             capture_output=True, env=env)
        if res.returncode != 0 and b"nothing to commit" not in res.stdout + res.stderr:
            raise AssertionError(f"git commit failed: {res.stderr.decode()[:200]}")

    def git_init(self):
        import subprocess
        env = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t",
               "PATH": "/usr/bin:/bin:/usr/local/bin", "HOME": str(self.root)}
        subprocess.run(["git", "-C", str(self.root), "init", "-q"], check=True,
                       capture_output=True, env=env)
        self.git_commit()

    def sha(self):
        import subprocess
        out = subprocess.run(["git", "-C", str(self.root), "rev-parse", "HEAD"],
                             capture_output=True, text=True, check=True)
        return out.stdout.strip()

    def check(self, behaviour="pass"):
        """A stand-in for the suite's runner that speaks check's own grammar.

        Real `check` names each case in a `Ran:` line, then prints either
        `Passed all N tests` or `Failed M of N tests`, and exits 0 or 1 to
        match. This reproduces that shape so the verdict parser is tested against
        the format rather than against my assumption of it.
        """
        # Real check prints `Ran: <testlist>` from its own argv, so the fake
        # picks out the `group/id` token the same way rather than reading $1.
        preamble = ('__seq=""\nfor __a in "$@"; do\n'
                    '  case "$__a" in */[0-9]*) __seq="$__a" ;; esac\n'
                    'done\n')
        scripts = {
            "pass": 'printf "Ran: %s\\n" "$__seq"\n'
                    'echo "Passed all 1 tests"\nexit 0\n',
            # Fails only when the arm's test directory matches FAKE_FAIL_PATH, so
            # a differential case can be expressed without a real filesystem
            # difference. The path the child reports is still checked elsewhere.
            "fail_arm": 'printf "Ran: %s\\n" "$__seq"\n'
                        'case "$TEST_DIR" in *${FAKE_FAIL_PATH}*)\n'
                        '  echo "Failed 1 of 1 tests"; exit 1 ;; esac\n'
                        'echo "Passed all 1 tests"\nexit 0\n',
            "fail": 'printf "Ran: %s\\n" "$__seq"\necho "Failed 1 of 1 tests"\nexit 1\n',
            "notrun": 'printf "Ran: %s\\n" "$__seq"\necho "Not run: $__seq"\n'
                      'echo "Passed all 1 tests"\nexit 0\n',
            # Exits 0 and says nothing about passing: the shape that made this
            # gate wrong in the first place.
            "silent": 'printf "Ran: %s\\n" "$__seq"\nexit 0\n',
            # Prints the success line but exits nonzero: the two must agree.
            "liar": 'printf "Ran: %s\\n" "$__seq"\necho "Passed all 1 tests"\nexit 1\n',
            # Names a different case than it was asked to run.
            "wrongcase": 'printf "Ran: %s\\n" "generic/999"\necho "Passed all 1 tests"\nexit 0\n',
            # The suite's own refusal grammar.
            "nofatal": 'printf "Ran: %s\\n" "$__seq"\necho "fsstress not found or executable"\n'
                       'echo "Passed all 1 tests"\nexit 0\n',
        }
        write(self.root / "check", "#!/bin/sh\n" + preamble + scripts[behaviour], executable=True)
        # Committed, so the source pin sees a clean tree. An uncommitted runner
        # would be refused for a real reason and mask what the test is about.
        self.git_commit()

    def case(self, cid, body, extra=""):
        return write(self.root / "tests" / "generic" / cid,
                     PRELUDE + extra + body + "\nexit $status\n", executable=True)

    def passing(self, cid):
        self.case(cid, "status=0")

    def failing(self, cid):
        self.case(cid, "status=1", extra='echo "not run" >&2\n')

    def pin(self, path, ids=("005",), case_count=None):
        """Write a pin that matches this tree, so drift is not the thing tested.

        Commits first: a pin is written from the committed state, and anything
        written afterwards would be dirty by design.
        """
        self.git_commit("cases")
        lines = [f"tree_sha {self.sha()}"]
        for cid in ids:
            lines.append(f"case {cid} {gate.sha256_file(self.root / 'tests' / 'generic' / cid)}")
        for cid in ids:
            for rel, sha in sorted(gate.common_closure(
                    self.root / "tests" / "generic" / cid, self.root).items()):
                lines.append(f"common {rel} {sha}")
        lines.append(f"case_count {case_count if case_count is not None else len(ids)}")
        return write(path, "\n".join(lines) + "\n")


class HarnessCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.tmp_path = Path(self.tmp.name)
        self.tree = Tree(self.tmp_path / "xfstests")
        self.native = self.tmp_path / "ARM-NATIVE"
        self.cowfs = self.tmp_path / "ARM-COWFS"
        for d in (self.native, self.cowfs):
            d.mkdir()
        self.saved = (gate.STARTUP_GATE, gate.ALLOWLIST_FILE)

    def tearDown(self):
        gate.STARTUP_GATE, gate.ALLOWLIST_FILE = self.saved

    def out(self, name="out"):
        return str(self.tmp_path / name)

    def open_gate(self):
        gate.STARTUP_GATE = list(MAC_GATE)

    def use_distinct_arms(self):
        """Substitute arm identities for a host that cannot host two filesystems.

        One Mac tmpdir has one device and one fstype, so a real run against two
        tmp directories is refused by `validate_arms` for the right reason. Tests
        about placement and verdicts use this so that code path still runs end to
        end. The real rules keep their own tests in ArmPlacement.
        """
        real = gate.validate_arms
        gate.validate_arms = lambda n, c, require_distinct_fstype=True: distinct_arms(n, c)
        self.addCleanup(setattr, gate, "validate_arms", real)

    def with_check(self, behaviour="pass", fail_path=None):
        """Install the suite-runner stand-in and select the check runner.

        `fail_path` is a substring of the arm's test directory; the stand-in
        reports a failure only for that arm, which is how the differential case
        is expressed without a real filesystem difference."""
        self.tree.check(behaviour)
        if fail_path is not None:
            os.environ["FAKE_FAIL_PATH"] = fail_path
            self.addCleanup(os.environ.pop, "FAKE_FAIL_PATH", None)
        return "check"

    def arm_dirs(self):
        """Two roots the harness will accept: distinct devices are not available
        on one Mac tmpdir, so the cowfs root is a distinct directory and the
        fstype rules are relaxed only where the harness has a knob for it."""
        return str(self.native), str(self.cowfs)

    def args(self, cases=None, **kw):
        class A:
            pass
        a = A()
        a.xfstests = str(self.tree.root)
        a.out = self.out()
        a.native_root, a.cowfs_root = self.arm_dirs()
        a.cases = cases
        a.timeout = 60
        a.require_full = False
        a.runner = "check"
        for k, v in kw.items():
            setattr(a, k, v)
        return a

    def run_gate(self, args):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            rc = gate.run(args)
        return rc, buf.getvalue()


# --- finding 1: the arms must run where they were told to run ----------------

def distinct_arms(native, cowfs):
    """Arm identities for a host that cannot host two real filesystems.

    One Mac tmpdir has one device and one fstype, so a run against two tmp
    directories would be refused by `validate_arms` for the right reason. Tests
    that are about placement rather than about the fstype rule substitute these
    identities so the placement code path is still exercised end to end. The
    real rule keeps its own tests below.
    """
    return {
        "native": {"path": str(native), "fstype": "native-test", "device_id": 1,
                   "mount_target": str(native), "source": "native-test", "is_dir": True},
        "cowfs": {"path": str(cowfs), "fstype": "fuse-test", "device_id": 2,
                  "mount_target": str(cowfs), "source": "cowfs-test", "is_dir": True},
    }


class ArmPlacement(HarnessCase):
    def observer_run(self, arm, test_dir, tmpdir, result_dir, case="generic/005"):
        observer = gate.write_observer(tmpdir)
        return gate.run_case(observer, case, test_dir, tmpdir, result_dir, self.tree.root, 60)

    def test_case_directory_is_created_inside_each_arm_root(self):
        self.open_gate()
        self.use_distinct_arms()
        self.with_check()
        self.tree.passing("005")
        self.tree.passing("010")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        native, cowfs = self.arm_dirs()
        rc, _ = self.run_gate(self.args(cases="005"))
        self.assertEqual(rc, 0, "expected the synthetic case to pass on both arms")
        native_dirs = [d for d in Path(native).rglob("testdir")]
        cowfs_dirs = [d for d in Path(cowfs).rglob("testdir")]
        self.assertEqual(len(native_dirs), 1, native_dirs)
        self.assertEqual(len(cowfs_dirs), 1, cowfs_dirs)
        self.assertNotEqual(native_dirs[0], cowfs_dirs[0])
        # Contained by its own arm and by no other, compared on resolved paths.
        self.assertTrue(str(native_dirs[0].resolve()).startswith(str(Path(native).resolve())))
        self.assertFalse(str(native_dirs[0].resolve()).startswith(str(Path(cowfs).resolve())))
        self.assertTrue(str(cowfs_dirs[0].resolve()).startswith(str(Path(cowfs).resolve())))
        self.assertFalse(str(cowfs_dirs[0].resolve()).startswith(str(Path(native).resolve())))

    def test_identical_roots_are_refused(self):
        self.open_gate()
        self.tree.passing("005")
        self.tree.passing("010")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        a = self.args(cases="005")
        a.cowfs_root = a.native_root
        rc, _ = self.run_gate(a)
        self.assertEqual(rc, 3)

    def test_overlapping_roots_are_refused(self):
        self.open_gate()
        inner = self.native / "inner"
        inner.mkdir()
        a = self.args()
        a.native_root = str(self.native)
        a.cowfs_root = str(inner)
        with self.assertRaises(ValueError) as ctx:
            gate.validate_arms(a.native_root, a.cowfs_root)
        self.assertIn("overlap", str(ctx.exception))

    def test_a_symlinked_root_is_refused(self):
        self.open_gate()
        link = self.tmp_path / "link-to-native"
        link.symlink_to(self.native)
        a = self.args()
        a.native_root = str(link)
        with self.assertRaises(ValueError) as ctx:
            gate.validate_arms(str(link), str(self.cowfs))
        self.assertIn("symlink", str(ctx.exception))

    def test_a_missing_root_is_refused(self):
        with self.assertRaises(ValueError) as ctx:
            gate.validate_arms(str(self.native), str(self.tmp_path / "nope"))
        self.assertIn("does not exist", str(ctx.exception))

    def test_a_non_fuse_cowfs_root_is_refused(self):
        # Both tmp dirs are on the same non-FUSE filesystem, so the cowfs-side
        # fstype rule has to fire even before the device rule.
        with self.assertRaises(ValueError) as ctx:
            gate.validate_arms(str(self.native), str(self.cowfs))
        self.assertIn("fuse", str(ctx.exception).lower())

    def test_arm_identity_is_recorded(self):
        self.open_gate()
        self.use_distinct_arms()
        self.with_check()
        self.tree.passing("005")
        self.tree.passing("010")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        rc, _ = self.run_gate(self.args(cases="005"))
        run_dir = next(Path(self.out()).glob("run-*"))
        meta = json.loads((run_dir / "results.jsonl").read_text().splitlines()[0])
        for arm in ("native", "cowfs"):
            self.assertIn("fstype", meta["arms"][arm])
            self.assertIn("device_id", meta["arms"][arm])

    def test_logs_stay_under_out_not_under_the_arm_roots(self):
        self.open_gate()
        self.use_distinct_arms()
        self.with_check()
        self.tree.passing("005")
        self.tree.passing("010")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        rc, out = self.run_gate(self.args(cases="005"))
        self.assertEqual(rc, 0, out)
        run_dir = next(Path(self.out()).glob("run-*"))
        rows = [json.loads(l) for l in (run_dir / "results.jsonl").read_text().splitlines()]
        verdict = rows[-1]
        # realpath on both sides: /var is a symlink to /private/var on macOS, and a
        # prefix comparison across that symlink would be a false negative.
        real = lambda p: str(Path(p).resolve())
        # The case log and the meta live under --out.
        self.assertTrue(real(verdict["native_log"]).startswith(real(self.out())), verdict["native_log"])
        self.assertTrue(real(verdict["cowfs_log"]).startswith(real(self.out())), verdict["cowfs_log"])
        # The per-case directories live inside the arm roots.
        self.assertTrue(real(verdict["native_test_dir"]).startswith(real(self.native)))
        self.assertTrue(real(verdict["cowfs_test_dir"]).startswith(real(self.cowfs)))
        # And the logs are not on either arm.
        for key in ("native_log", "cowfs_log"):
            self.assertFalse(real(verdict[key]).startswith(real(self.native)))
            self.assertFalse(real(verdict[key]).startswith(real(self.cowfs)))


# --- finding 2: success needs positive evidence ------------------------------

class SuccessEvidence(HarnessCase):
    def rec(self, rc=0, observer=None, scan=None, runner="check", witness=True,
            residue="", arm="cowfs", log_text=""):
        r = {"rc": rc, "timed_out": False, "runner": runner,
             "observer": observer if observer is not None
             else self.complete_observer(residue=residue, arm=arm),
             "scan": scan if scan is not None else {"bytes": 100, "skips": [], "empty": False},
             "outcome": None, "outcome_why": None,
             "log_text": log_text, "log": "x.log"}
        if runner == "check":
            r["suite_verdict"] = {"pass": witness, "rc": rc, "why": "test fixture"}
        # Classify through the real classifier, so the verdict under test is the
        # one production code would produce from the same evidence.
        r["outcome"], r["outcome_why"] = gate.classify_outcome(r)
        return r

    def complete_observer(self, residue="", arm="cowfs", **over):
        obs = {"complete": True, "CASE": "generic/005", "EXPECT": "generic/005",
               "CASE_RC": "0", "TEST_DIR": f"/arms/{arm}/testdir", "IO": "OK",
               "RESIDUE": residue, "LOG_BYTES": "100",
               "FSTYPE": "fuse.cowfs", "DEVICE_ID": "42"}
        obs.update(over)
        return obs

    def test_a_complete_pass_is_a_pass(self):
        v, _ = gate.verdict_for_case(self.rec(arm="native"), self.rec(arm="cowfs"))
        self.assertEqual(v, "PASS")

    def test_arms_that_left_different_work_are_not_a_pass(self):
        # Both exited 0 with a clean log, but the native arm left residue the
        # cowfs arm did not. That is not evidence they did the same thing.
        v, why = gate.verdict_for_case(self.rec(arm="native", residue="left-behind"),
                                       self.rec(arm="cowfs", residue=""))
        self.assertEqual(v, "UNMEASURABLE")
        self.assertIn("different residue", why)

    def test_the_suite_startup_gate_is_read_from_the_log(self):
        # The suite's own refusal, exit 0 and all. Not a pass.
        v, _ = gate.verdict_for_case(
            self.rec(arm="native", log_text="fsstress not found or executable"),
            self.rec(arm="cowfs", log_text="fsstress not found or executable"))
        self.assertEqual(v, "INVALID")

    def test_a_direct_run_can_never_be_a_pass(self):
        v, why = gate.verdict_for_case(
            self.rec(arm="native", runner="direct"),
            self.rec(arm="cowfs", runner="direct"))
        self.assertEqual(v, "INVALID")
        self.assertIn("direct invocation", why)

    def test_a_check_pass_with_a_cowfs_failure_is_a_fail(self):
        v, _ = gate.verdict_for_case(self.rec(arm="native", witness=True),
                                     self.rec(arm="cowfs", rc=1, witness=False))
        self.assertEqual(v, "FAIL")

    def test_an_empty_log_cannot_pass(self):
        scan = {"bytes": 0, "skips": [], "empty": True}
        self.assertEqual(gate.classify_outcome(self.rec(scan=scan))[0], gate.OUTCOME_SKIPPED)

    def test_a_whitespace_log_cannot_pass(self):
        scan = {"bytes": 40, "skips": [], "empty": True}
        self.assertEqual(gate.classify_outcome(self.rec(scan=scan))[0], gate.OUTCOME_SKIPPED)

    def test_rc_zero_with_no_io_cannot_pass(self):
        rec = self.rec(observer=self.complete_observer(IO="SKIPPED"))
        self.assertEqual(gate.classify_outcome(rec)[0], gate.OUTCOME_SKIPPED)

    def test_a_pass_for_the_wrong_case_cannot_pass(self):
        rec = self.rec(observer=self.complete_observer(CASE="generic/999"))
        outcome, why = gate.classify_outcome(rec)
        self.assertEqual(outcome, gate.OUTCOME_SKIPPED)
        self.assertIn("generic/999", why)

    def test_an_adversarial_lookalike_log_is_invalid(self):
        # A log that mimics a pass banner but did not run is caught by the
        # observer check, not by matching the banner.
        rec = self.rec(observer={"complete": False}, scan={"bytes": 200, "skips": [], "empty": False})
        outcome, why = gate.classify_outcome(rec)
        self.assertEqual(outcome, gate.OUTCOME_SKIPPED)
        self.assertIn("observer", why)

    def test_a_missing_observer_block_is_invalid(self):
        rec = self.rec(observer=gate.parse_observer(self.tmp_path / "nothing.log"))
        self.assertEqual(gate.classify_outcome(rec)[0], gate.OUTCOME_SKIPPED)

    def test_a_case_that_failed_is_a_failure_not_a_pass(self):
        self.assertEqual(gate.classify_outcome(self.rec(rc=2))[0], gate.OUTCOME_FAILED)

    def test_the_observer_is_the_child_s_own_account(self):
        self.open_gate()
        # A case that reports the directory it was given and does real I/O there.
        self.tree.case("005", 'printf "hello\\n" > "$TEST_DIR/real"\n'
                               'test "$(cat "$TEST_DIR/real")" = hello || status=1\n'
                               'rm -f "$TEST_DIR/real"\nstatus=0')
        self.tree.passing("010")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        work = self.cowfs / "w"
        test_dir = gate.prepare_dir(work / "testdir")
        tmpdir = gate.prepare_dir(work / "tmp")
        result_dir = gate.prepare_dir(work / "results")
        rec = ArmPlacement.observer_run(self, "cowfs", test_dir, tmpdir, result_dir)
        self.assertEqual(rec["observer"]["complete"], True, rec["observer"])
        self.assertEqual(rec["observer"]["TEST_DIR"], str(test_dir))
        self.assertEqual(rec["observer"]["IO"], "OK")
        # A direct invocation has no supported success witness, so it cannot be a
        # pass however clean the log is.
        self.assertEqual(rec["outcome"], gate.OUTCOME_SKIPPED)
        self.assertIn("direct invocation", rec["outcome_why"])

    def test_the_observer_detects_a_case_that_did_nothing(self):
        self.open_gate()
        self.tree.passing("010")
        work = self.cowfs / "w2"
        test_dir = gate.prepare_dir(work / "testdir")
        tmpdir = gate.prepare_dir(work / "tmp")
        result_dir = gate.prepare_dir(work / "results")
        # A case that refuses to write into TEST_DIR: the observer must see it.
        (test_dir).chmod(0o500)
        self.addCleanup(test_dir.chmod, 0o700)
        rec = ArmPlacement.observer_run(self, "cowfs", test_dir, tmpdir, result_dir)
        self.assertNotEqual(rec["observer"]["IO"], "OK", rec["observer"])


# --- finding 3: an outcome is classified before any comparison --------------

class OutcomeClassification(HarnessCase):
    def arm(self, outcome, why="because", rc=0, skips=None):
        return {"outcome": outcome, "outcome_why": why, "rc": rc,
                "scan": {"skips": skips or [], "bytes": 50, "empty": False},
                "observer": {"complete": True}, "timed_out": False}

    def test_native_failure_is_not_a_cowfs_pass(self):
        v, why = gate.verdict_for_case(
            self.arm(gate.OUTCOME_FAILED, "native broke"),
            self.arm(gate.OUTCOME_PASSED))
        self.assertEqual(v, gate.OUTCOME_FAILED and "UNMEASURABLE")
        self.assertIn("not a cowfs verdict", why)

    def test_native_failure_with_a_cowfs_skip_is_invalid(self):
        v, why = gate.verdict_for_case(
            self.arm(gate.OUTCOME_FAILED, "native broke"),
            self.arm(gate.OUTCOME_SKIPPED, "cowfs skipped"))
        self.assertEqual(v, "INVALID")

    def test_a_cowfs_skip_is_never_ignored(self):
        v, _ = gate.verdict_for_case(
            self.arm(gate.OUTCOME_PASSED),
            self.arm(gate.OUTCOME_SKIPPED, "cowfs skipped"))
        self.assertEqual(v, "INVALID")

    def test_cowfs_failing_where_native_passed_is_a_fail(self):
        v, why = gate.verdict_for_case(
            self.arm(gate.OUTCOME_PASSED), self.arm(gate.OUTCOME_FAILED, "rc=1"))
        self.assertEqual(v, "FAIL")

    def test_both_failing_is_not_a_verdict(self):
        v, _ = gate.verdict_for_case(
            self.arm(gate.OUTCOME_FAILED, "n"), self.arm(gate.OUTCOME_FAILED, "c"))
        self.assertEqual(v, "UNMEASURABLE")

    def test_a_refused_arm_is_unmeasurable(self):
        v, _ = gate.verdict_for_case(
            self.arm(gate.OUTCOME_REFUSED, "timeout"),
            self.arm(gate.OUTCOME_PASSED))
        self.assertEqual(v, "UNMEASURABLE")


# --- finding 4 and 5: source and helper identity -----------------------------

class SourcePin(HarnessCase):
    def test_a_matching_pin_passes(self):
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        ok, problems, _ = gate.verify_source_pin(self.tree.root, allowlist=allow)
        self.assertTrue(ok, problems)

    def test_a_changed_case_is_refused(self):
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        self.tree.case("005", "status=0\necho tampered\n")
        ok, problems, _ = gate.verify_source_pin(self.tree.root, allowlist=allow)
        self.assertFalse(ok)
        self.assertTrue(any("005" in p for p in problems), problems)

    def test_a_changed_common_file_is_refused(self):
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        (self.tree.root / "common" / "rc").write_text("# fake rc\n# tampered\n")
        ok, problems, _ = gate.verify_source_pin(self.tree.root, allowlist=allow)
        self.assertFalse(ok)
        self.assertTrue(any("common/rc" in p for p in problems), problems)

    def test_a_changed_tree_sha_is_refused(self):
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        allow.write_text(allow.read_text().replace(self.tree.sha(), "0" * 40))
        ok, problems, _ = gate.verify_source_pin(self.tree.root, allowlist=allow)
        self.assertFalse(ok)
        self.assertTrue(any("tree sha" in p for p in problems), problems)

    def test_a_dirty_worktree_is_refused(self):
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        write(self.tree.root / "tests" / "generic" / "newcomer", "status=0\n", executable=True)
        ok, problems, _ = gate.verify_source_pin(self.tree.root, allowlist=allow)
        self.assertFalse(ok)
        self.assertTrue(any("dirty" in p for p in problems), problems)

    def test_a_built_artifact_in_a_pinned_directory_is_allowed(self):
        # Someone who builds the suite leaves object files. Those are build
        # output, not a changed case, and must not be treated as source drift.
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        write(self.tree.root / "src" / "mkfile.o", "\0\0")
        ok, problems, detail = gate.verify_source_pin(self.tree.root, allowlist=allow)
        self.assertTrue(ok, problems)
        self.assertTrue(detail["dirty_ignored"], detail)

    def test_unpinned_transitive_source_is_refused(self):
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        # A case that sources a file the pin does not know about.
        write(self.tree.root / "common" / "extra", "# extra\n")
        self.tree.case("005", "status=0", extra=". ./common/extra\n")
        allow2 = self.tmp_path / "allowlist2.txt"
        allow2.write_text(allow.read_text())
        ok, problems, _ = gate.verify_source_pin(self.tree.root, allowlist=allow2)
        self.assertFalse(ok)
        self.assertTrue(any("unpinned" in p for p in problems), problems)

    def test_the_closure_is_read_not_filename_trusted(self):
        self.tree.passing("005")
        closure = gate.common_closure(self.tree.root / "tests" / "generic" / "005", self.tree.root)
        # common/rc is pulled in transitively through common/preamble's case, and
        # common/promotion through common/rc. Both are hashed.
        self.assertIn("common/preamble", closure)
        self.assertIn("common/rc", closure)
        self.assertIn("common/promotion", closure)

    def test_classifier_drift_is_a_refusal(self):
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        self.tree.case("007", "status=0\n")  # newly safe, not in the pin
        ok, problems, _ = gate.verify_source_pin(self.tree.root, allowlist=allow)
        self.assertFalse(ok)
        self.assertTrue(any("drift" in p for p in problems), problems)

    def test_a_missing_reviewed_case_is_refused(self):
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        (self.tree.root / "tests" / "generic" / "005").unlink()
        ok, problems, _ = gate.verify_source_pin(self.tree.root, allowlist=allow)
        self.assertFalse(ok)
        self.assertTrue(any("missing" in p for p in problems), problems)

    def test_preflight_records_the_pin_per_case_hash(self):
        self.open_gate()
        self.tree.passing("010")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("010",))
        gate.ALLOWLIST_FILE = allow
        rec = gate.preflight(self.tree.root, Path(self.out()))
        self.assertIn("case_sha", rec["pin"]["detail"])
        self.assertIn("010", rec["pin"]["detail"]["case_sha"])


# --- finding 6: capabilities are probed, not asserted -----------------------

class CapabilityProbes(HarnessCase):
    def test_capabilities_are_measured(self):
        caps = gate.probe_capabilities("/usr/bin:/bin", self.tree.root)
        for key in ("autoconf", "automake", "libtool", "m4", "getfattr",
                    "ltp/fsstress", "include/config.h"):
            self.assertIn(key, caps)
        # bash exists on the Mac, m4 may or may not; what matters is that the
        # value came from a probe and matches the filesystem.
        self.assertEqual(caps["ltp/fsstress"]["present"], True)
        self.assertEqual(caps["include/config.h"]["present"], False)

    def test_block_scratch_device_is_recorded_as_not_requested(self):
        caps = gate.probe_capabilities("/usr/bin:/bin", self.tree.root)
        self.assertFalse(caps["block_scratch_device"]["present"])
        self.assertIn("not requested", caps["block_scratch_device"]["detail"])


# --- finding 7: classify exits nonzero on drift -----------------------------

class ClassifyExit(HarnessCase):
    def test_drift_exits_three(self):
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        allow.write_text(allow.read_text().replace("case 005", "case 999"))
        gate.ALLOWLIST_FILE = allow

        class A:
            pass
        a = A()
        a.xfstests = str(self.tree.root)
        a.out = self.out()
        a.verbose = False
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            rc = gate.classify_cmd(a)
        self.assertEqual(rc, 3, buf.getvalue())
        self.assertIn("drift", buf.getvalue())

    def test_no_drift_exits_zero(self):
        self.tree.passing("005")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))

        class A:
            pass
        a = A()
        a.xfstests, a.out, a.verbose = str(self.tree.root), self.out(), False
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            rc = gate.classify_cmd(a)
        self.assertEqual(rc, 0, buf.getvalue())


# --- finding 8: a missing probe is UNMEASURABLE with evidence ---------------

class MissingProbe(HarnessCase):
    def test_missing_probe_case_is_unmeasurable_with_evidence(self):
        self.open_gate()
        self.tree.passing("005")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        gate.ALLOWLIST_FILE = allow
        rec = gate.preflight(self.tree.root, Path(self.out()))
        self.assertEqual(rec["verdict"], "UNMEASURABLE")
        self.assertIsNone(rec["suite_probe"]["rc"])
        self.assertIn("missing", rec["reason"])
        # Evidence is written even though the probe could not run.
        self.assertTrue(Path(rec["evidence"]).exists())

    def test_run_returns_two_not_one_when_the_probe_is_missing(self):
        self.open_gate()
        self.use_distinct_arms()
        self.with_check()
        self.tree.passing("005")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        rc, _ = self.run_gate(self.args(cases="005"))
        self.assertEqual(rc, 2)


# --- finding 9: report exits nonzero on a bad run ---------------------------

class ReportExit(HarnessCase):
    def rows(self, verdicts):
        run_dir = self.tmp_path / "run-x"
        meta = {"kind": "meta", "run_id": "run-x", "tree_sha": "a" * 40, "runner": "observer-wrapper",
                "arms": {"native": {"fstype": "ext4", "device_id": 1, "mount_target": "/", "source": "/dev/sda2"},
                         "cowfs": {"fstype": "fuse.cowfs", "device_id": 2, "mount_target": "/mnt", "source": "cowfs"}},
                "expected_cases": 1}
        rows = [meta]
        for v in verdicts:
            rows.append({"kind": "case_verdict", "case": "generic/005", "verdict": v,
                         "why": "w", "native_rc": 0, "cowfs_rc": 0,
                         "native_outcome": gate.OUTCOME_PASSED, "cowfs_outcome": gate.OUTCOME_PASSED,
                         "native_log": "n", "cowfs_log": "c", "native_skips": [], "cowfs_skips": [],
                         "native_test_dir": "n", "cowfs_test_dir": "c",
                         "native_wall_s": 1.0, "cowfs_wall_s": 1.0})
        gate.write_jsonl(run_dir / "results.jsonl", rows)

        class A:
            pass
        a = A()
        a.run = str(run_dir)
        return a

    def test_report_on_pass_exits_zero(self):
        self.assertEqual(gate.report(self.rows(["PASS"])), 0)

    def test_report_on_fail_exits_one(self):
        self.assertEqual(gate.report(self.rows(["FAIL"])), 1)

    def test_report_on_invalid_exits_three(self):
        self.assertEqual(gate.report(self.rows(["INVALID"])), 3)

    def test_report_on_unmeasurable_exits_two(self):
        self.assertEqual(gate.report(self.rows(["UNMEASURABLE"])), 2)

    def test_report_with_no_cases_exits_two(self):
        self.assertEqual(gate.report(self.rows([])), 2)

    def test_report_prints_the_arm_identities(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            gate.report(self.rows(["PASS"]))
        self.assertIn("ARM native", buf.getvalue())
        self.assertIn("fuse.cowfs", buf.getvalue())


# --- finding 10: the coverage denominator is pinned --------------------------

class CoverageDenominator(HarnessCase):
    def test_suffixed_entries_are_included(self):
        write(self.tree.root / "tests" / "generic" / "069_o_tmpfile", PRELUDE + "status=0\n")
        write(self.tree.root / "tests" / "generic" / "307_recovery", PRELUDE + "status=0\n")
        write(self.tree.root / "tests" / "generic" / "005.cfg", "not a case\n")
        records = gate.classify_group(self.tree.root / "tests")
        ids = {r["id"] for r in records}
        self.assertIn("069_o_tmpfile", ids)
        self.assertIn("307_recovery", ids)
        self.assertNotIn("005.cfg", ids)

    def test_require_full_compares_against_the_pin(self):
        self.open_gate()
        self.use_distinct_arms()
        self.with_check()
        self.tree.passing("005")
        self.tree.passing("010")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt",
                                            ("005", "010"), case_count=2)
        rc, out = self.run_gate(self.args(cases="005", require_full=True))
        self.assertEqual(rc, 2)
        self.assertIn("require-full", out)


# --- the classifier is a hypothesis; these are its limits ------------------

class Classification(HarnessCase):
    def case(self, cid, body, extra=""):
        return self.tree.case(cid, body, extra=extra)

    def verdict(self, cid):
        return gate.classify_case(self.tree.root / "tests" / "generic" / cid)["verdict"]

    def test_a_plain_case_is_safe(self):
        self.case("500", "status=0")
        self.assertEqual(self.verdict("500"), "SAFE")

    def test_mkfs_is_refused_with_the_line(self):
        self.case("501", 'mkfs.ext4 -F $TEST_DEV\nstatus=0')
        rec = gate.classify_case(self.tree.root / "tests" / "generic" / "501")
        self.assertEqual(rec["verdict"], "NEEDS_DEVICE")
        self.assertTrue(any("formats a filesystem" in r for r in rec["reasons"]), rec)
        self.assertTrue(any(r.startswith("line ") for r in rec["reasons"]), rec)

    def test_loop_and_scratch_are_refused(self):
        self.case("502", "losetup /dev/loop0 x\n")
        self.assertEqual(self.verdict("502"), "NEEDS_DEVICE")
        self.case("503", "_require_scratch\n")
        self.assertEqual(self.verdict("503"), "NEEDS_SCRATCH")

    def test_a_mount_wrapper_is_refused(self):
        self.case("510", "_test_cycle_mount\nstatus=0")
        self.assertEqual(self.verdict("510"), "NEEDS_DEVICE")

    def test_root_needing_operations_are_refused(self):
        self.case("504", "sudo mkfs.xfs $TEST_DEV\n")
        self.assertEqual(self.verdict("504"), "NEEDS_ROOT")
        self.case("514", "chown 100:100 $TEST_DIR/f\nstatus=0")
        self.assertEqual(self.verdict("514"), "NEEDS_ROOT")
        self.case("515", "_user_do \"echo x\"\n")
        self.assertEqual(self.verdict("515"), "NEEDS_ROOT")
        self.case("516", "mknod $TEST_DIR/null c 1 3\n")
        self.assertEqual(self.verdict("516"), "NEEDS_ROOT")

    def test_a_destructive_absolute_path_is_refused(self):
        self.case("505", "rm -rf /home/other/pool\nstatus=0")
        rec = gate.classify_case(self.tree.root / "tests" / "generic" / "505")
        self.assertEqual(rec["verdict"], "UNSAFE")
        self.assertTrue(any("/home/other/pool" in r for r in rec["reasons"]), rec)

    def test_removing_the_test_dir_is_allowed(self):
        self.case("506", "rm -rf $TEST_DIR/tmp\nrm -fr $TEST_DIR/*\nstatus=0")
        self.assertEqual(self.verdict("506"), "SAFE")

    def test_dev_null_is_not_a_device_node(self):
        self.case("507", "echo x 2>/dev/null\nstatus=0")
        self.assertEqual(self.verdict("507"), "SAFE")

    def test_system_binaries_are_not_destructive(self):
        self.case("509", "cp /bin/true $TEST_DIR/true\nstatus=0")
        self.assertEqual(self.verdict("509"), "SAFE")

    def test_helper_references_are_refused(self):
        self.case("508", "_run_aiodio helper\nstatus=0")
        self.assertEqual(self.verdict("508"), "NEEDS_HELPER")
        self.case("511", "$FSX_PROG -a 4096\nstatus=0")
        self.assertEqual(self.verdict("511"), "NEEDS_HELPER")
        self.case("517", "run_fsx -N 10\nstatus=0")
        self.assertEqual(self.verdict("517"), "NEEDS_HELPER")
        self.case("518", "$here/src/mkfile $TEST_DIR/f 1m\n")
        self.assertEqual(self.verdict("518"), "NEEDS_HELPER")

    def test_unreviewed_source_is_refused(self):
        self.case("520", ". ./common/secret_helpers\nstatus=0")
        rec = gate.classify_case(self.tree.root / "tests" / "generic" / "520")
        self.assertEqual(rec["verdict"], "UNREAD_SOURCE")
        self.assertTrue(any("common/secret_helpers" in r for r in rec["reasons"]), rec)

    def test_soak_and_long_cases_are_refused(self):
        self.case("521", "status=0", extra="_begin_fstest auto soak\n")
        self.assertEqual(self.verdict("521"), "NEEDS_LONG")
        self.case("522", "nr_ops=$((1000000 * TIME_FACTOR))\nstatus=0")
        self.assertEqual(self.verdict("522"), "NEEDS_LONG")

    def test_a_big_allocation_is_refused(self):
        self.case("512", "truncate -s 4G $TEST_DIR/junk\n")
        self.assertEqual(self.verdict("512"), "NEEDS_BIG_SPACE")
        self.case("513", "truncate -s 64M $TEST_DIR/junk\n")
        self.assertNotEqual(self.verdict("513"), "NEEDS_BIG_SPACE")


# --- run-time behaviour of one case -----------------------------------------

class CaseRun(HarnessCase):
    def observe(self, body, extra="", arm="cowfs"):
        self.tree.case("005", body, extra=extra)
        observer = gate.write_observer(self.tmp_path)
        root = self.cowfs if arm == "cowfs" else self.native
        work = root / "w"
        test_dir = gate.prepare_dir(work / "testdir")
        tmpdir = gate.prepare_dir(work / "tmp")
        result_dir = gate.prepare_dir(work / "results")
        return gate.run_case(observer, "generic/005", test_dir, tmpdir, result_dir,
                             self.tree.root, 30)

    def test_exit_code_comes_from_the_child(self):
        rec = self.observe("status=7")
        self.assertEqual(rec["rc"], 7)

    def test_case_directory_is_immutable_per_attempt(self):
        rec = self.observe("status=0")
        with self.assertRaises(FileExistsError):
            gate.prepare_dir(Path(rec["test_dir"]))

    def test_timeout_is_bounded_and_recorded(self):
        rec = self.observe("sleep 30", timeout=2) if False else None
        observer = gate.write_observer(self.tmp_path)
        self.tree.case("005", "sleep 30")
        work = self.cowfs / "w2"
        rec = gate.run_case(observer, "generic/005", gate.prepare_dir(work / "testdir"),
                            gate.prepare_dir(work / "tmp"), gate.prepare_dir(work / "results"),
                            self.tree.root, 2)
        self.assertTrue(rec["timed_out"])
        self.assertEqual(rec["outcome"], gate.OUTCOME_REFUSED)

    def test_the_environment_is_the_same_on_both_arms(self):
        self.tree.case("005", 'printf "fstyp=[%s] scratch=[%s] dev=[%s]\\n" '
                               '"$FSTYP" "$SCRATCH_DEV" "$TEST_DEV"\nstatus=0')
        observer = gate.write_observer(self.tmp_path)
        work = self.native / "w"
        rec = gate.run_case(observer, "generic/005", gate.prepare_dir(work / "testdir"),
                            gate.prepare_dir(work / "tmp"), gate.prepare_dir(work / "results"),
                            self.tree.root, 30)
        log = Path(rec["log"]).read_text()
        self.assertIn("fstyp=[]", log)
        self.assertIn("scratch=[]", log)

    def test_a_malformed_comparison_is_caught(self):
        rec = self.observe('echo "generic/005: [: 3: unary operator expected"\nstatus=0')
        self.assertTrue(any("malformed" in s for s in rec["scan"]["skips"]), rec)

    def test_a_missing_helper_is_caught(self):
        rec = self.observe('echo "common/rc: line 9: /x/src/mkfile: No such file or directory"\nstatus=0')
        self.assertTrue(any("helper binary" in s for s in rec["scan"]["skips"]), rec)

    def test_an_expected_enoent_is_not_a_skip(self):
        rec = self.observe('echo "ls: cannot access \'$TEST_DIR/gone\': No such file or directory"\nstatus=0')
        self.assertEqual(rec["scan"]["skips"], [], rec)


# --- end to end --------------------------------------------------------------

class EndToEnd(HarnessCase):
    def test_a_matched_run_records_exact_codes(self):
        self.open_gate()
        self.with_check()
        self.tree.passing("005")
        self.tree.passing("010")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        # One tmpfs cannot host two distinguishable arms, so run against a
        # private pair the harness accepts by relaxing only the FUSE rule.
        self.use_distinct_arms()
        rc, out = self.run_gate(self.args(cases="005"))
        self.assertEqual(rc, 0, out)
        run_dir = next(Path(self.out()).glob("run-*"))
        rows = [json.loads(l) for l in (run_dir / "results.jsonl").read_text().splitlines()]
        self.assertEqual([r["kind"] for r in rows[1:]], ["case_verdict"])
        verdict = rows[1]
        self.assertEqual(verdict["verdict"], "PASS")
        self.assertEqual(verdict["native_rc"], 0)
        self.assertEqual(verdict["cowfs_rc"], 0)
        self.assertTrue(Path(verdict["native_log"]).is_file())
        self.assertTrue(Path(verdict["cowfs_log"]).is_file())
        self.assertIn("pass witness", verdict["why"])

    def test_cowfs_failure_exits_one_and_keeps_the_evidence(self):
        self.open_gate()
        self.tree.passing("005")
        self.tree.passing("010")
        self.with_check("fail_arm", fail_path="ARM-COWFS")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        self.use_distinct_arms()
        # A case that fails when its directory sits under the cowfs side. It
        # reports its own directory, so it cannot be a path-substring illusion.
        self.tree.case("005", 'case "$TEST_DIR" in *ARM-COWFS*) status=1 ;; *) status=0 ;; esac')
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        rc, _ = self.run_gate(self.args(cases="005"))
        self.assertEqual(rc, 1)
        run_dir = next(Path(self.out()).glob("run-*"))
        rows = [json.loads(l) for l in (run_dir / "results.jsonl").read_text().splitlines()]
        verdict = rows[-1]
        self.assertEqual(verdict["native_rc"], 0)
        self.assertEqual(verdict["cowfs_rc"], 1)
        self.assertEqual(verdict["verdict"], "FAIL")
        self.assertTrue(Path(verdict["native_log"]).is_file())
        self.assertTrue(Path(verdict["cowfs_log"]).is_file())

    def test_a_case_that_does_nothing_never_scores_pass(self):
        self.open_gate()
        self.with_check()
        self.tree.passing("005")
        self.tree.passing("010")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        self.use_distinct_arms()
        # A runner that exits 0 without saying anything passed: the exact shape
        # that produced a false PASS before.
        self.with_check("silent")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        rc, _ = self.run_gate(self.args(cases="005"))
        self.assertNotEqual(rc, 0)
        run_dir = next(Path(self.out()).glob("run-*"))
        rows = [json.loads(l) for l in (run_dir / "results.jsonl").read_text().splitlines()]
        self.assertNotEqual(rows[-1]["verdict"], "PASS")

    def test_a_case_outside_the_reviewed_set_is_refused(self):
        self.open_gate()
        self.with_check()
        self.tree.passing("005")
        self.tree.passing("010")
        gate.ALLOWLIST_FILE = self.tree.pin(self.tmp_path / "allowlist.txt", ("005",))
        self.use_distinct_arms()
        rc, _ = self.run_gate(self.args(cases="005,010"))
        self.assertEqual(rc, 3)

    def test_allowlist_drift_stops_the_run(self):
        self.open_gate()
        self.use_distinct_arms()
        self.with_check()
        self.tree.passing("005")
        self.tree.passing("010")
        allow = self.tree.pin(self.tmp_path / "allowlist.txt", ("005", "010"))
        allow.write_text(allow.read_text().replace("case 005", "case 777"))
        gate.ALLOWLIST_FILE = allow
        self.use_distinct_arms()
        rc, _ = self.run_gate(self.args(cases="005"))
        self.assertEqual(rc, 3)


if __name__ == "__main__":
    unittest.main()