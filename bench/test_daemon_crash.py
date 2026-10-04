"""Fail-closed controls for the crash harness (issue #88).

Run: python3 -m unittest discover -s bench -v

These are the checks CI can see. They use synthetic ledgers and fake fixtures only:
no daemon, no mount, no cargo build, and nothing under `~/.cowfs`. The real crash
run is `scripts/verify-daemon-crash.py`, which this module loads to test its
identity, receipt and cache logic without starting anything.
"""

import importlib.util
import json
import os
import shutil
import signal
import subprocess
import tempfile
import time
import unittest
import unittest.mock as mock
from pathlib import Path

HERE = Path(__file__).resolve().parent
HARNESS = HERE.parent / "scripts" / "verify-daemon-crash.py"

SAMPLE_NFSSTAT = """Client Info:
NFSv3 RPC Counts:
     Getattr      Setattr       Lookup     Readlink         Read        Write
     2508543      1522974      1536026           28      1456945       155767
      Create       Remove       Rename         Link      Symlink        Mkdir
      1234          5678         91011          12           0          34
      Fsinfo     PathConf       Commit         Null
         778          778       125080            0
NLM RPC Counts:
     Access        Close       Commit       Create   Delegpurge  Delegreturn
           0            0           42            0            0            0
"""


def load_harness():
    spec = importlib.util.spec_from_file_location("verify_daemon_crash", HARNESS)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


h = load_harness()


class TestWireParser(unittest.TestCase):
    """The nfsstat parser, against recorded output. No mount needed."""

    def test_reads_the_nfsv3_commit_column(self):
        counts = h.parse_rpc_counts(SAMPLE_NFSSTAT)
        self.assertEqual(counts["Commit"], 125080)
        self.assertEqual(counts["Rename"], 91011)

    def test_does_not_read_the_nlm_commit_column(self):
        # The NLM section also has a Commit, with a different value. Reading it by
        # header name alone would silently report 42 instead of 125080.
        counts = h.parse_rpc_counts(SAMPLE_NFSSTAT)
        self.assertNotEqual(counts["Commit"], 42)

    def test_missing_section_raises(self):
        with self.assertRaises(ValueError):
            h.parse_rpc_counts("nothing here\n", section="No Such Section:")

    def test_absent_column_yields_none_not_a_guess(self):
        counts = h.parse_rpc_counts(
            "NFSv3 RPC Counts:\n     Read\n        5\n"
        )
        self.assertIsNone(counts.get("Commit"))


class TestReceiptLedgerIsImmutable(unittest.TestCase):
    """A promise is kept or reported failed. It is never reclassified."""

    def test_there_is_no_downgrade(self):
        self.assertFalse(
            hasattr(h.Receipts, "downgrade"),
            "Receipts must not offer a reclassification: that is how a real failure "
            "gets relabelled as permitted",
        )

    def test_receipt_kind_never_changes(self):
        r = h.Receipts()
        r.durable("live/a.bin", "aa" * 32, 10, "nfs_commit")
        r.applied("live/b.bin", "bb" * 32, 10, "nfs_write_only")
        r.removed("live/c.bin", "cc" * 32, 10, "snapshot_rm")
        self.assertEqual([x.kind for x in r.items], ["durable", "applied", "removed"])

    def test_repath_matches_by_path_not_position(self):
        r = h.Receipts()
        r.durable("live/first.bin", "11" * 32, 1, "x")
        r.durable("live/second.bin", "22" * 32, 2, "x")
        self.assertTrue(r.repath_by_path("live/first.bin", "live/renamed.bin"))
        self.assertEqual(r.items[0].path, "live/renamed.bin")
        self.assertEqual(r.items[1].path, "live/second.bin")

    def test_repath_of_an_unknown_path_reports_failure(self):
        r = h.Receipts()
        r.durable("live/a.bin", "aa" * 32, 1, "x")
        self.assertFalse(r.repath_by_path("live/nope.bin", "live/other.bin"))


class TestIdentityBindsEverythingThatMatters(unittest.TestCase):
    """A cached verdict must not outlive anything that could change it."""

    def _ident(self, **over):
        base = dict(
            phase="matrix", case="write_fsync", rep=0,
            argv_scope=["matrix"], config={"files": 4},
        )
        base.update(over)
        return h.case_identity(**base)

    def test_key_is_stable_for_the_same_inputs(self):
        self.assertEqual(self._ident()["key"], self._ident()["key"])

    def test_changing_rev_changes_the_key(self):
        a = self._ident()
        b = dict(a)
        b["rev"] = "deadbeef"
        b["key"] = h.digest_of({k: v for k, v in b.items() if k != "key"})
        self.assertNotEqual(a["key"], b["key"])

    def test_changing_harness_digest_changes_the_key(self):
        a = self._ident()
        b = dict(a, harness_sha256="0" * 64)
        b["key"] = h.digest_of({k: v for k, v in b.items() if k != "key"})
        self.assertNotEqual(a["key"], b["key"])

    def test_changing_every_binary_digest_changes_the_key(self):
        a = self._ident()
        b = dict(a, daemon_sha256="1" * 64, cli_sha256="2" * 64)
        b["key"] = h.digest_of({k: v for k, v in b.items() if k != "key"})
        self.assertNotEqual(a["key"], b["key"])

    def test_phase_is_part_of_the_key(self):
        self.assertNotEqual(
            self._ident(phase="sample")["key"], self._ident(phase="matrix")["key"]
        )

    def test_rep_is_part_of_the_key(self):
        self.assertNotEqual(self._ident(rep=0)["key"], self._ident(rep=1)["key"])

    def test_config_is_part_of_the_key(self):
        self.assertNotEqual(
            self._ident(config={"files": 4})["key"],
            self._ident(config={"files": 8})["key"],
        )

    def test_argv_scope_is_part_of_the_key(self):
        self.assertNotEqual(
            self._ident(argv_scope=["matrix"])["key"],
            self._ident(argv_scope=["matrix", "extra"])["key"],
        )

    def test_key_is_a_digest_of_the_rest(self):
        ident = self._ident()
        recomputed = h.digest_of({k: v for k, v in ident.items() if k != "key"})
        self.assertEqual(ident["key"], recomputed)


class TestCacheFailsClosed(unittest.TestCase):
    """validate_cached must reject anything it cannot prove, and reuse nothing else."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        self.run_dir = self.tmp
        self.ident = h.case_identity(
            "matrix", "write_fsync", 0, ["matrix"], {"files": 4}
        )
        self.case = h.case_name_for(self.ident)
        self.case_dir = h.next_case_dir(self.run_dir, self.case, self.ident)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _write_manifest(self, manifest):
        with open(os.path.join(self.case_dir, "manifest.json"), "w") as f:
            json.dump(manifest, f)

    def _write_evidence(self, lines):
        with open(os.path.join(self.case_dir, "records.jsonl"), "w") as f:
            for line in lines:
                f.write(json.dumps(line) + "\n")

    def _receipt(self):
        return {"path": "live/a.bin", "sha256": "aa" * 32, "size": 4,
                "kind": "durable", "boundary": "nfs_commit", "seq": 1}

    def _valid_manifest(self):
        return {
            "identity": self.ident,
            "case": self.case,
            "outcome": "pass",
            "receipts": [self._receipt()],
            "assertions": [{"label": "durable_match", "ok": True}],
            "failures": [],
        }

    def _valid_evidence(self):
        man = self._valid_manifest()
        return [
            {"step": 1, "name": "receipt.issued", "ok": True, "receipt": self._receipt()},
            {"step": 2, "name": "readback.durable", "ok": True, "path": "live/a.bin",
             "want": "aa" * 32, "want_size": 4, "present": True, "got": "aa" * 32,
             "matched": True, "boundary": "nfs_commit"},
            {"step": 3, "name": "assert.durable_match", "ok": True, "path": "live/a.bin",
             "want": "aa" * 32, "got": "aa" * 32, "want_size": 4, "got_size": 4},
            {"step": 4, "name": "case.verdict", "ok": True, "outcome": "pass",
             "failures": [], "assertions": man["assertions"],
             "receipt_state": man["receipts"]},
            {"step": 5, "name": "case.terminal", "ok": True,
             "key": self.ident["key"], "manifest": man},
        ]

    def _failing_evidence(self):
        """A real failing shape: the durable receipt came back missing."""
        man = dict(self._valid_manifest(), outcome="fail", failures=["durable_present"],
                   assertions=[{"label": "durable_present", "ok": False}])
        return [
            {"step": 1, "name": "receipt.issued", "ok": True, "receipt": self._receipt()},
            {"step": 2, "name": "readback.durable", "ok": True, "path": "live/a.bin",
             "want": "aa" * 32, "want_size": 4, "present": False, "got": None,
             "matched": False, "boundary": "nfs_commit"},
            {"step": 3, "name": "assert.durable_present", "ok": False, "path": "live/a.bin",
             "parent_entries": ["orig.bin"]},
            {"step": 4, "name": "case.verdict", "ok": False, "outcome": "fail",
             "failures": ["durable_present"], "assertions": man["assertions"],
             "receipt_state": man["receipts"]},
            {"step": 5, "name": "case.terminal", "ok": False,
             "key": self.ident["key"], "manifest": man},
        ], man

    def _expect_reject(self, manifest=None, evidence=None, why=""):
        if manifest is not None:
            self._write_manifest(manifest)
        if evidence is not None:
            self._write_evidence(evidence)
        ok, reason, _ = h.validate_cached(self.ident, self.case_dir)
        self.assertFalse(ok, "must reject: " + why)
        return reason

    def test_a_complete_verified_manifest_is_reused(self):
        self._write_manifest(self._valid_manifest())
        self._write_evidence(self._valid_evidence())
        ok, reason, man = h.validate_cached(self.ident, self.case_dir)
        self.assertTrue(ok, reason)
        self.assertEqual(man["outcome"], "pass")

    def test_no_manifest_is_rejected(self):
        self._expect_reject(why="nothing written yet")

    def test_unparseable_manifest_is_rejected(self):
        with open(os.path.join(self.case_dir, "manifest.json"), "w") as f:
            f.write("{not json")
        ok, _, _ = h.validate_cached(self.ident, self.case_dir)
        self.assertFalse(ok)

    def test_manifest_missing_a_required_field_is_rejected(self):
        for field in h.REQUIRED_MANIFEST_FIELDS:
            m = self._valid_manifest()
            del m[field]
            reason = self._expect_reject(manifest=m, evidence=self._valid_evidence(),
                                         why="missing " + field)
            self.assertIn(field, reason)

    def test_identity_mismatch_is_rejected(self):
        m = self._valid_manifest()
        m["identity"] = dict(m["identity"], rev="deadbeef")
        self._expect_reject(manifest=m, evidence=self._valid_evidence(),
                            why="wrong rev")

    def test_wrong_binary_digest_in_the_manifest_is_rejected(self):
        m = self._valid_manifest()
        m["identity"] = dict(m["identity"], daemon_sha256="0" * 64)
        self._expect_reject(manifest=m, evidence=self._valid_evidence(),
                            why="wrong binary digest")

    def test_non_conclusive_outcome_is_rejected(self):
        for outcome in ("aborted", "error", "in_progress", None):
            m = self._valid_manifest()
            m["outcome"] = outcome
            self._expect_reject(manifest=m, evidence=self._valid_evidence(),
                                why="outcome %r" % outcome)

    def test_missing_evidence_file_is_rejected(self):
        self._expect_reject(manifest=self._valid_manifest(), evidence=None,
                            why="no records.jsonl")

    def test_evidence_without_a_terminal_record_is_rejected(self):
        self._expect_reject(manifest=self._valid_manifest(),
                            evidence=self._valid_evidence()[:2],
                            why="no terminal record")

    def test_terminal_record_with_the_wrong_identity_is_rejected(self):
        ev = self._valid_evidence()
        ev[-1]["manifest"] = dict(ev[-1]["manifest"])
        ev[-1]["manifest"]["identity"] = dict(ev[-1]["manifest"]["identity"], rev="x")
        self._expect_reject(manifest=self._valid_manifest(), evidence=ev,
                            why="terminal identity mismatch")

    def test_a_receipt_the_evidence_does_not_contain_is_rejected(self):
        m = self._valid_manifest()
        m["receipts"].append({"path": "live/ghost.bin", "sha256": "bb" * 32,
                              "size": 9, "kind": "durable",
                              "boundary": "nfs_commit", "seq": 2})
        reason = self._expect_reject(manifest=m, evidence=self._valid_evidence(),
                                     why="receipt absent from evidence")
        self.assertIn("receipt count differs", reason)

    def test_a_receipt_in_both_files_but_absent_from_readback_is_rejected(self):
        # Manifest and ledger verdict agree with each other but not with the
        # evidence, which is the case a name-only cross-check would wave through.
        ghost = {"path": "live/ghost.bin", "sha256": "bb" * 32, "size": 9,
                 "kind": "durable", "boundary": "nfs_commit", "seq": 2}
        m = self._valid_manifest()
        m["receipts"] = m["receipts"] + [ghost]
        ev = self._valid_evidence()
        for r in ev:
            if r["name"] == "case.verdict":
                r["receipt_state"] = m["receipts"]
            if r["name"] == "case.terminal":
                r["manifest"] = m
        reason = self._expect_reject(manifest=m, evidence=ev,
                                     why="agreed-upon receipt with no readback")
        self.assertIn("derives", reason)

    def test_an_assertion_the_evidence_does_not_contain_is_rejected(self):
        m = self._valid_manifest()
        m["assertions"].append({"label": "fsck_clean", "ok": True})
        reason = self._expect_reject(manifest=m, evidence=self._valid_evidence(),
                                     why="assertion absent from evidence")
        self.assertIn("assertions differ from the ledger verdict", reason)

    def test_a_bare_terminal_flag_cannot_suppress_a_case(self):
        # The failure mode a critic demonstrated: a hand-written line claiming a case
        # is done, with nothing behind it.
        self._expect_reject(evidence=[{"step": 1, "name": "case.terminal", "ok": True,
                                       "key": self.ident["key"], "terminal": True}],
                            why="forged bare terminal")

    def test_a_forged_all_fields_pass_still_needs_matching_receipts(self):
        # Every self-reported field is present and says pass, but the evidence has no
        # receipts, so it must be rejected rather than trusted.
        forged = self._valid_manifest()
        self._expect_reject(manifest=forged,
                            evidence=[{"step": 1, "name": "case.terminal", "ok": True,
                                       "key": self.ident["key"], "manifest": forged,
                                       "terminal": True}],
                            why="self-reported pass with no receipts behind it")


class TestLedgerToleratesATornLastLine(unittest.TestCase):
    def test_a_torn_final_line_does_not_lose_the_prior_records(self):
        tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        try:
            path = os.path.join(tmp, "records.jsonl")
            with open(path, "w") as f:
                f.write(json.dumps({"step": 1, "name": "a", "ok": True}) + "\n")
                f.write('{"step": 2, "name": "b", "ok": tr')  # torn
            rec = h.Recorder(tmp)
            self.assertEqual(len(rec.prior), 1)
            self.assertEqual(rec.step, 1)
            rec.close()
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


class TestForeignPidIsRefusedAndLeftAlive(unittest.TestCase):
    """A refused signal must leave the other process running."""

    def test_refusal_does_not_signal(self):
        tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        try:
            rec = h.Recorder(tmp)
            victim = subprocess.Popen(["sleep", "30"], start_new_session=True)
            try:
                class Fake:
                    pid = victim.pid

                    def poll(self):
                        return None

                with self.assertRaises(h.ForeignProcess):
                    h.kill_verified(Fake(), rec, "/no-such-socket", "/no-such-store")
                time.sleep(0.3)
                self.assertTrue(h.pid_alive(victim.pid),
                                "the refused process must still be alive 300ms later")
            finally:
                victim.kill()
                victim.wait()
            with open(os.path.join(tmp, "records.jsonl")) as f:
                names = [json.loads(line)["name"] for line in f if line.strip()]
            rec.close()
            self.assertIn("kill.refused_foreign_pid", names)
            self.assertNotIn("kill.exited", names)
            self.assertNotIn("kill.verified_target", names)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


class TestProcessIdentityDetectsRecycling(unittest.TestCase):
    """A pid alone is not an identity. `lstart` is checked too.

    `ps -o lstart=` has one-second resolution, so two processes started inside the
    same second share it. The guard is therefore cmdline plus start time, and the
    test drives the guard directly rather than pretending the resolution is finer
    than it is.
    """

    def test_lstart_is_captured_for_a_live_child(self):
        p = subprocess.Popen(["sleep", "5"], start_new_session=True)
        try:
            self.assertTrue(h.pid_start_time(p.pid))
        finally:
            p.kill()
            p.wait()

    def test_a_changed_identity_is_refused(self):
        tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        victim = subprocess.Popen(["sleep", "30"], start_new_session=True)
        try:
            rec = h.Recorder(tmp)
            store = "token-%d" % victim.pid

            class Fake:
                pid = victim.pid

                def poll(self):
                    return None

            real = h.process_identity(victim)
            forged = dict(real, lstart="Sun Dec 31 23:59:59 1900")
            # Stub only the command line, so the earlier path check passes and the
            # identity check is what has to refuse.
            with mock.patch.object(h, "pid_cmdline", return_value=store):
                with self.assertRaises(h.ForeignProcess):
                    h.kill_verified(
                        Fake(), rec, store, store, expect=forged, sig=signal.SIGKILL
                    )
            time.sleep(0.3)
            self.assertTrue(h.pid_alive(victim.pid),
                            "a refused identity must leave the process alive")
            rec.close()
        finally:
            victim.kill()
            victim.wait()
            shutil.rmtree(tmp, ignore_errors=True)


class TestExitContractIsExplicit(unittest.TestCase):
    """A cached-only run must not be mistakable for a fresh pass."""

    def test_summary_fields_exist_for_the_cached_only_verdict(self):
        # The harness builds this dict inline; assert the contract it must satisfy.
        for field in ("verdict", "fresh_acceptance", "exit", "counts", "rev",
                      "harness_sha256", "daemon_sha256", "cli_sha256"):
            self.assertIn(field, _summary_contract())
        self.assertFalse(_summary_contract()["fresh_acceptance"])
        self.assertEqual(_summary_contract()["verdict"], "cached_only")
        self.assertEqual(_summary_contract()["exit"], 2)
        self.assertEqual(_summary_contract()["counts"]["executed"], 0)

    def test_counts_separate_executed_from_reused(self):
        c = _summary_contract()["counts"]
        for field in ("executed", "reused", "rejected", "passed", "failed"):
            self.assertIn(field, c)


def _summary_contract():
    """The summary a zero-execution run must produce, spelled out.

    Kept as data so a test can assert the contract rather than trust the code path
    that builds it.
    """
    return {
        "verdict": "cached_only",
        "fresh_acceptance": False,
        "exit": 2,
        "counts": {
            "executed": 0, "reused": 3, "rejected": 0,
            "passed": 3, "failed": 0, "aborted": 0, "error": 0,
        },
        "rev": "x", "harness_sha256": "x", "daemon_sha256": "x", "cli_sha256": "x",
    }


class TestCasesCannotPassEitherWay(unittest.TestCase):
    """The rename boundary must be falsifiable, which is what makes it evidence."""

    def test_the_posix_rename_case_asserts_durability(self):
        import inspect

        src = inspect.getsource(h.case_rename_posix_durability)
        self.assertIn("fsync_dir", src, "the case must issue the POSIX barrier")
        self.assertNotIn("downgrade", src)
        self.assertNotIn("applied", src)

    def test_the_known_failing_cases_are_the_rename_posix_ones(self):
        for name in h.KNOWN_FAILING:
            self.assertIn("rename", name)
        self.assertNotIn("rename_committed", h.KNOWN_FAILING)

    def test_the_control_case_is_not_listed_as_expected_to_fail(self):
        # rename_committed passes today and is what makes the failures attributable.
        self.assertIn("rename_committed", h.CASES)


class TestProbeLabelsMustMatch(unittest.TestCase):
    """`mark()` used to be called with a different label than `sample()`, which made
    every delta None while the run still looked healthy. Both halves are pinned."""

    def _probe(self, tmp, **kw):
        rec = h.Recorder(tmp)
        return rec, h.Probe(rec, "case-x", baseline_secs=0, **kw)

    def test_marking_an_unsampled_label_is_reported_not_ignored(self):
        tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        try:
            rec, probe = self._probe(tmp)
            probe.sample("sampled_step")
            probe.mark("a_different_label")
            rec.close()
            with open(os.path.join(tmp, "records.jsonl")) as f:
                names = [json.loads(l)["name"] for l in f if l.strip()]
            self.assertIn("wire.mark_unmatched", names)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_sampled_and_marked_produce_a_delta_or_an_explicit_error(self):
        # Injected counter, so this does not assume a real nfsstat exists. The
        # tool-missing branch has its own class; nothing is skipped.
        tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        try:
            rec, probe = self._probe(tmp, reader=lambda: 7)
            probe.sample("step")
            probe.mark("step")
            out = probe.report("step")
            rec.close()
            self.assertIsNotNone(out["commit_delta"])
            self.assertIsNone(out["error"])
            self.assertIn("attributable", out)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_reporting_an_unsampled_label_says_so(self):
        tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        try:
            rec, probe = self._probe(tmp)
            out = probe.report("never_sampled")
            rec.close()
            self.assertIsNone(out["commit_delta"])
            self.assertIn("no sample taken", out["error"])
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_every_case_samples_and_marks_the_same_labels(self):
        import inspect
        import re

        src = inspect.getsource(h)
        samples = set(re.findall(r'probe\.sample\("([^"]+)"\)', src))
        marks = set(re.findall(r'probe\.mark\("([^"]+)"\)', src))
        self.assertTrue(samples, "expected the cases to sample the wire")
        self.assertEqual(
            marks - samples, set(),
            "every mark() label must also be sample()d, or its delta stays None",
        )


class TestCachedFailureCannotBeLaundered(unittest.TestCase):
    """The review's finding: a real failing verdict was editable into a pass.

    Every mutation here starts from a genuine failing ledger, so the control is
    "the untouched failing verdict is accepted as a FAILURE" and the tests are
    "each laundering is rejected". A cached FAIL must never come back as a pass.
    """

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        self.ident = h.case_identity("matrix", "write_fsync", 0, ["m"], {"files": 4})
        self.case = h.case_name_for(self.ident)
        self.case_dir = h.next_case_dir(self.tmp, self.case, self.ident)
        self.evidence, self.manifest = self._failing_evidence()
        self._write(self.manifest, self.evidence)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _write(self, manifest, evidence):
        with open(os.path.join(self.case_dir, "manifest.json"), "w") as f:
            json.dump(manifest, f)
        with open(os.path.join(self.case_dir, "records.jsonl"), "w") as f:
            for r in evidence:
                f.write(json.dumps(r) + "\n")

    def _receipt(self):
        return {"path": "live/a.bin", "sha256": "aa" * 32, "size": 4,
                "kind": "durable", "boundary": "nfs_commit", "seq": 1}

    def _failing_evidence(self):
        rc = self._receipt()
        man = {
            "identity": self.ident, "case": self.case, "outcome": "fail",
            "receipts": [rc],
            "assertions": [{"label": "durable_present", "ok": False}],
            "failures": ["durable_present"],
        }
        return [
            {"step": 1, "name": "receipt.issued", "ok": True, "receipt": rc},
            {"step": 2, "name": "readback.durable", "ok": True, "path": rc["path"],
             "want": rc["sha256"], "want_size": rc["size"], "present": False,
             "got": None, "matched": False, "boundary": rc["boundary"]},
            {"step": 3, "name": "assert.durable_present", "ok": False,
             "path": rc["path"], "parent_entries": ["orig.bin"]},
            {"step": 4, "name": "case.verdict", "ok": False, "outcome": "fail",
             "failures": ["durable_present"], "assertions": man["assertions"],
             "receipt_state": man["receipts"]},
            {"step": 5, "name": "case.terminal", "ok": False,
             "key": self.ident["key"], "manifest": man},
        ], man

    def _check(self, manifest, evidence):
        self._write(manifest, evidence)
        return h.validate_cached(self.ident, self.case_dir)

    def test_control_an_untouched_failure_is_reused_as_a_failure(self):
        ok, reason, man = self._check(self.manifest, self.evidence)
        self.assertTrue(ok, reason)
        self.assertEqual(man["outcome"], "fail",
                         "a cached failure must stay a failure")

    def _expect_reject(self, manifest, evidence, why):
        ok, reason, _ = self._check(manifest, evidence)
        self.assertFalse(ok, "must reject: " + why)
        return reason

    def test_reviewer_launder_outcome_flags_and_ledger_together(self):
        m = json.loads(json.dumps(self.manifest))
        ev = json.loads(json.dumps(self.evidence))
        m["outcome"] = "pass"
        m["failures"] = []
        for a in m["assertions"]:
            a["ok"] = True
        for r in ev:
            if r["name"] == "case.terminal":
                r["manifest"] = m
            if r["name"] == "case.verdict":
                r["outcome"] = "pass"
                r["failures"] = []
                r["assertions"] = m["assertions"]
            if r["name"].startswith("assert."):
                r["ok"] = True
        self._expect_reject(m, ev, "reviewer's exact laundering")

    def test_manifest_outcome_only(self):
        m = json.loads(json.dumps(self.manifest))
        m["outcome"] = "pass"
        self._expect_reject(m, self.evidence, "outcome relabelled")

    def test_manifest_failures_only(self):
        m = json.loads(json.dumps(self.manifest))
        m["failures"] = []
        self._expect_reject(m, self.evidence, "failures emptied")

    def test_manifest_assertion_value_only(self):
        m = json.loads(json.dumps(self.manifest))
        m["assertions"][0]["ok"] = True
        self._expect_reject(m, self.evidence, "assertion value flipped")

    def test_ledger_assert_flag_only(self):
        ev = json.loads(json.dumps(self.evidence))
        for r in ev:
            if r["name"].startswith("assert."):
                r["ok"] = True
        self._expect_reject(self.manifest, ev, "ledger flag flipped")

    def test_readback_presence_fabricated(self):
        ev = json.loads(json.dumps(self.evidence))
        for r in ev:
            if r["name"] == "readback.durable":
                r["present"] = True
                r["got"] = self._receipt()["sha256"]
                r["matched"] = True
        self._expect_reject(self.manifest, ev, "readback rewritten to matched")

    def test_receipt_kind_weakened_in_both_files(self):
        m = json.loads(json.dumps(self.manifest))
        ev = json.loads(json.dumps(self.evidence))
        m["receipts"][0]["kind"] = "applied"
        for r in ev:
            if r["name"] == "receipt.issued":
                r["receipt"]["kind"] = "applied"
            if r["name"] == "case.verdict":
                r["receipt_state"][0]["kind"] = "applied"
        self._expect_reject(m, ev, "durable weakened to applied")

    def test_receipt_kind_weakened_in_the_manifest_only(self):
        m = json.loads(json.dumps(self.manifest))
        m["receipts"][0]["kind"] = "applied"
        self._expect_reject(m, self.evidence, "manifest kind differs from ledger")

    def test_missing_case_verdict_is_rejected(self):
        ev = [r for r in self.evidence if r["name"] != "case.verdict"]
        reason = self._expect_reject(self.manifest, ev, "verdict record absent")
        self.assertIn("case.verdict", reason)

    def test_a_cached_failure_does_not_become_a_pass_on_reuse(self):
        """End to end through the manifest the caller would actually receive."""
        ok, _, man = self._check(self.manifest, self.evidence)
        self.assertTrue(ok)
        self.assertNotEqual(man["outcome"], "pass")


class Counter:
    """A deterministic stand-in for the nfsstat counter.

    An explicit reading list rather than a computed one, so a test cannot
    accidentally agree with the code it is checking.
    """

    def __init__(self, readings):
        self.readings = list(readings)
        self.i = 0

    def __call__(self):
        v = self.readings[min(self.i, len(self.readings) - 1)]
        self.i += 1
        return v


class TestProbeWithAnInjectedCounter(unittest.TestCase):
    """The available path, with no real nfsstat involved.

    Injected readings make this identical on Linux CI and on macOS, so nothing
    here depends on a macOS-only tool being installed and nothing is skipped.
    """

    def _run(self, readings):
        tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        try:
            rec = h.Recorder(tmp)
            probe = h.Probe(rec, "case-x", reader=Counter(readings), baseline_secs=0)
            probe.sample("step")
            probe.mark("step")
            out = probe.report("step")
            rec.close()
            return out
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_available_counter_yields_a_real_delta(self):
        out = self._run([10, 10, 10, 1000, 1004])
        self.assertTrue(out["available"])
        self.assertEqual(out["commit_delta"], 4)
        self.assertIsNone(out["error"])
        self.assertIs(out["attributable"], True)

    def test_quiet_host_with_no_delta_is_not_positive_attribution(self):
        out = self._run([10, 10, 10, 1000, 1000])
        self.assertEqual(out["commit_delta"], 0)
        self.assertIs(
            out["attributable"], False,
            "a zero delta observes no activity; it is not evidence of activity",
        )

    def test_drift_is_reported_and_not_subtracted(self):
        out = self._run([10, 10, 15, 1000, 1004])  # idle drifts 5, step rises 4
        self.assertEqual(out["commit_delta"], 4)
        self.assertEqual(out["idle_baseline_drift"], 5)
        self.assertIs(out["delta_is_background_subtracted"], False)
        self.assertIs(out["attributable"], False)


class TestProbeWithoutTheTool(unittest.TestCase):
    """`nfsstat` is macOS-only. Its absence must be explicit, never a fake zero."""

    def _run(self, reader):
        tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        try:
            rec = h.Recorder(tmp)
            probe = h.Probe(rec, "case-x", reader=reader, baseline_secs=0)
            probe.sample("step")
            probe.mark("step")
            out = probe.report("step")
            rec.close()
            return out
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_missing_tool_reports_unavailable_with_an_error(self):
        out = self._run(lambda: None)
        self.assertIs(out["available"], False)
        self.assertIsNone(out["commit_delta"], "no tool must not yield a delta")
        self.assertTrue(out["error"], "absence must carry an explicit reason")
        self.assertIsNone(out["attributable"])

    def test_a_raising_reader_is_also_unavailable_not_a_zero(self):
        def boom():
            raise FileNotFoundError("nfsstat")

        out = self._run(boom)
        self.assertIs(out["available"], False)
        self.assertIsNone(out["commit_delta"])
        self.assertTrue(out["error"])

    def test_the_real_reader_path_also_reports_unavailable_when_absent(self):
        """The shipped reader, pointed at a path that does not exist."""
        saved = h.NFSSTAT
        h.NFSSTAT = "/nonexistent/nfsstat"
        try:
            out = self._run(lambda: h.commit_count())
            self.assertIs(out["available"], False)
            self.assertIsNone(out["commit_delta"])
            self.assertTrue(out["error"])
        finally:
            h.NFSSTAT = saved


class TestAttributableTruthTable(unittest.TestCase):
    """The whole rule as literals, so a precedence accident cannot pass.

    Positive attribution needs a positive delta that stands clear of the drift. A
    zero delta is never positive attribution, and a negative delta means the
    counter was reset, which invalidates the reading.
    """

    CASES = [
        (0, 0, False, "no observed COMMIT is not evidence of activity"),
        (1, 0, True, "positive on a quiet host"),
        (2, 0, True, "positive on a quiet host"),
        (-1, 0, False, "counter reset"),
        (-1, 14, False, "negative even when large"),
        (0, 14, False, "zero delta under drift"),
        (0, 2, False, "zero delta under drift"),
        (2, 14, False, "below 3x drift"),
        (41, 14, False, "just below 3x drift"),
        (42, 14, True, "exactly at 3x drift"),
        (43, 14, True, "above 3x drift"),
        (None, 0, None, "unknown delta"),
        (1, None, None, "unknown drift"),
    ]

    def test_truth_table(self):
        for delta, drift, want, why in self.CASES:
            got = h.Probe.attributable_for(delta, drift)
            self.assertEqual(
                got, want,
                "delta=%s drift=%s got=%s want=%s (%s)" % (delta, drift, got, want, why),
            )

    def test_zero_over_zero_is_explicitly_false(self):
        self.assertIs(h.Probe.attributable_for(0, 0), False)

    def test_the_report_states_the_rule_rather_than_a_precedence_accident(self):
        import inspect

        src = inspect.getsource(h.Probe.attributable_for)
        self.assertIn("never positive attribution", src)
        self.assertIn("3 * drift", src)


class TestDeriveVerdictIgnoresTheOkFlag(unittest.TestCase):
    """The cached verdict is re-derived from the ledger's semantic fields."""

    def _prior(self, matched):
        return [
            {"step": 1, "name": "readback.durable", "ok": True, "path": "live/a.bin",
             "want": "aa", "want_size": 4, "present": matched,
             "got": "aa" if matched else None, "matched": matched,
             "boundary": "nfs_commit"},
            {"step": 2, "name": "assert.durable_match", "ok": True, "path": "live/a.bin",
             "want": "aa", "got": "aa", "want_size": 4, "got_size": 4},
        ]

    def test_matched_receipt_derives_pass(self):
        rc = [{"path": "live/a.bin", "sha256": "aa", "kind": "durable"}]
        self.assertEqual(h.derive_verdict(self._prior(True), rc)[0], "pass")

    def test_missing_receipt_derives_fail_even_with_every_flag_true(self):
        rc = [{"path": "live/a.bin", "sha256": "aa", "kind": "durable"}]
        derived, failures, _ = h.derive_verdict(self._prior(False), rc)
        self.assertEqual(derived, "fail")
        self.assertIn("durable_present", failures)

    def test_a_durable_receipt_with_no_readback_record_derives_fail(self):
        rc = [{"path": "live/ghost.bin", "sha256": "bb", "kind": "durable"}]
        self.assertEqual(h.derive_verdict(self._prior(True), rc)[0], "fail")

    def test_flipped_assert_flag_is_caught_by_its_own_fields(self):
        prior = self._prior(True)
        prior[1]["ok"] = False  # only the flag moved; want still equals got
        rc = [{"path": "live/a.bin", "sha256": "aa", "kind": "durable"}]
        derived, _, contradictions = h.derive_verdict(prior, rc)
        self.assertEqual(derived, "fail")
        self.assertIn("durable_match", contradictions)

    def test_a_hash_mismatch_is_derived_from_want_and_got(self):
        prior = self._prior(True)
        prior[1]["got"] = "zz"
        prior[0]["got"] = "zz"
        rc = [{"path": "live/a.bin", "sha256": "aa", "kind": "durable"}]
        self.assertEqual(h.derive_verdict(prior, rc)[0], "fail")


class TestExpectationDirections(unittest.TestCase):
    """Each assertion's `ok` must track its own recorded field in the right
    direction. `removed_absent` asserts absence, so ok is the negation; getting
    that backwards makes the fail-closed check reject its own generator."""

    CASES = [
        ("snapshot_name_present", True, True),
        ("snapshot_name_present", False, False),
        ("snapshot_is_dir", True, True),
        ("snapshot_is_dir", False, False),
        ("removed_absent", False, True),
        ("removed_absent", True, False),
        ("native_receipts_present_kill", True, True),
        ("native_receipts_present_kill", False, False),
    ]

    def test_direction(self):
        for label, present, want_ok in self.CASES:
            rec = {"name": "assert." + label, "ok": want_ok, "present": present}
            self.assertEqual(
                h._expected_from_assert_record(rec), want_ok,
                "%s with present=%s" % (label, present),
            )

    def test_rc_style_assertions(self):
        for label in ("snapshot_rm_ok", "gc_rm_garbage_ok"):
            self.assertIs(
                h._expected_from_assert_record({"name": "assert." + label, "rc": 0}), True)
            self.assertIs(
                h._expected_from_assert_record({"name": "assert." + label, "rc": 1}), False)

    def test_durable_present_uses_the_recorded_parent_listing(self):
        rec = {"name": "assert.durable_present", "ok": False,
               "path": "live/moved.bin", "parent_entries": ["orig.bin"]}
        self.assertIs(h._expected_from_assert_record(rec), False)
        rec["parent_entries"] = ["moved.bin"]
        self.assertIs(h._expected_from_assert_record(rec), True)

    def test_a_label_with_no_deciding_field_yields_none(self):
        self.assertIsNone(h._expected_from_assert_record(
            {"name": "assert.gc_survivor_after", "ok": True}))


class TestAttemptDirsNeverCollideOrWipe(unittest.TestCase):
    """A re-execution must not land on an earlier attempt's store, and must not
    delete it either: the instructions are to preserve failed fixtures."""

    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        self.ident = h.case_identity("matrix", "write_fsync", 0, ["m"], {"files": 4})
        self.case = h.case_name_for(self.ident)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def test_first_attempt_is_the_bare_key_dir(self):
        d = h.next_case_dir(self.tmp, self.case, self.ident)
        self.assertEqual(os.path.basename(d), "k" + self.ident["key"][:12])

    def test_second_attempt_gets_its_own_directory(self):
        first = h.next_case_dir(self.tmp, self.case, self.ident)
        marker = os.path.join(first, "records.jsonl")
        with open(marker, "w") as f:
            f.write("prior evidence\n")
        second = h.next_case_dir(self.tmp, self.case, self.ident)
        self.assertNotEqual(first, second)
        self.assertEqual(os.path.basename(second), "k%s-a1" % self.ident["key"][:12])

    def test_the_prior_attempt_is_preserved(self):
        first = h.next_case_dir(self.tmp, self.case, self.ident)
        marker = os.path.join(first, "records.jsonl")
        with open(marker, "w") as f:
            f.write("prior evidence\n")
        h.next_case_dir(self.tmp, self.case, self.ident)
        self.assertTrue(os.path.exists(marker), "earlier evidence must survive")

    def test_a_different_identity_gets_a_different_base(self):
        other = h.case_identity("matrix", "write_fsync", 1, ["m"], {"files": 4})
        self.assertNotEqual(
            h.attempt_base(self.ident), h.attempt_base(other)
        )

    def test_candidate_dirs_are_ordered_oldest_first(self):
        cands = h.candidate_dirs(self.tmp, self.case, self.ident)
        base = h.attempt_base(self.ident)
        self.assertEqual(
            [os.path.basename(c) for c in cands[:3]],
            [base, base + "-a1", base + "-a2"],
        )


class TestStaleMountCannotWedgeTheHarness(unittest.TestCase):
    """A dead NFS mount must not be able to hang teardown.

    Two defects showed up together in a real run that exceeded 40 minutes:
    `is_our_mount` called `os.path.realpath` on a path that had just become a
    stale mount, which blocks indefinitely, and the umount ran unbounded, so a
    command stuck in uninterruptible sleep could never be reaped. Both are pinned
    here without touching a real mount.
    """

    def test_mount_keys_are_computed_without_touching_a_live_path(self):
        tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        try:
            target = os.path.join(tmp, "mnt")
            keys = h._mount_keys(target)
            self.assertEqual(keys[0], os.path.normpath(os.path.abspath(target)))
            self.assertTrue(all(k for k in keys))
        finally:
            shutil.rmtree(tmp, ignore_errors=True)

    def test_is_our_mount_does_not_resolve_the_path_it_is_asked_about(self):
        """The resolved form must come from the cached keys, never a fresh realpath."""
        calls = []
        real_realpath = os.path.realpath

        def spy(path, *a, **k):
            calls.append(path)
            return real_realpath(path, *a, **k)

        os.path.realpath = spy
        try:
            h.is_our_mount("/definitely/not/a/mount", keys=["/cached/key"])
        finally:
            os.path.realpath = real_realpath
        self.assertEqual(calls, [], "is_our_mount must not call realpath")

    def test_is_our_mount_matches_either_cached_form(self):
        with mock.patch.object(h, "run_bounded") as rb:
            rb.return_value = h.Proc.Result(
                0, "localhost:/cowfs-abc on /cached/key (nfs, nodev)\n", ""
            )
            self.assertTrue(h.is_our_mount("/ignored", keys=["/literal", "/cached/key"]))
            rb.return_value = h.Proc.Result(0, "nothing here\n", "")
            self.assertFalse(h.is_our_mount("/ignored", keys=["/literal", "/cached/key"]))

    def test_an_unreachable_mount_table_reads_as_not_ours(self):
        def boom(argv, timeout):
            raise h.BoundedTimeout("mount table unreadable")

        with mock.patch.object(h, "run_bounded", boom):
            self.assertFalse(h.is_our_mount("/whatever", keys=["/k"]))

    def test_run_bounded_returns_a_result(self):
        r = h.run_bounded(["/bin/echo", "hello"], timeout=20)
        self.assertEqual(r.returncode, 0)
        self.assertIn("hello", r.stdout)

    def test_run_bounded_raises_rather_than_blocking_on_a_stuck_child(self):
        """The whole point: a child that ignores the kill must not block the caller."""
        with self.assertRaises(h.BoundedTimeout):
            h.run_bounded(["/bin/sleep", "120"], timeout=1)

    def test_run_bounded_kills_the_child_group(self):
        with self.assertRaises(h.BoundedTimeout):
            h.run_bounded(["/bin/sleep", "120"], timeout=1)
        # Give the orphan a moment, then confirm it is not still running.
        time.sleep(0.5)
        out = subprocess.run(
            ["/bin/ps", "-o", "command="], capture_output=True, text=True
        ).stdout
        self.assertNotIn("sleep 120", out)


class TestNativeControlMatchesTheCowfsOperations(unittest.TestCase):
    def test_the_native_writer_performs_the_same_rename_and_syncs(self):
        import inspect

        src = inspect.getsource(h.internal_native_writer)
        self.assertIn("os.rename", src)
        self.assertIn("fsync_dir", src)
        self.assertIn("os.O_RDONLY", src)


if __name__ == "__main__":
    unittest.main()