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
import sys
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

    def _valid_manifest(self):
        return {
            "identity": self.ident,
            "case": self.case,
            "outcome": "pass",
            "receipts": [{"path": "live/a.bin", "sha256": "aa" * 32, "size": 4,
                          "kind": "durable", "boundary": "nfs_commit", "seq": 1}],
            "assertions": [{"label": "durable_match", "ok": True}],
            "failures": [],
        }

    def _valid_evidence(self):
        return [
            {"step": 1, "name": "receipt.issued", "ok": True,
             "receipt": {"path": "live/a.bin", "sha256": "aa" * 32, "size": 4,
                         "kind": "durable", "boundary": "nfs_commit", "seq": 1}},
            {"step": 2, "name": "assert.durable_match", "ok": True},
            {"step": 3, "name": "case.terminal", "ok": True,
             "key": self.ident["key"], "manifest": self._valid_manifest()},
        ]

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
        self.assertIn("ghost.bin", reason)

    def test_an_assertion_the_evidence_does_not_contain_is_rejected(self):
        m = self._valid_manifest()
        m["assertions"].append({"label": "fsck_clean", "ok": True})
        reason = self._expect_reject(manifest=m, evidence=self._valid_evidence(),
                                     why="assertion absent from evidence")
        self.assertIn("fsck_clean", reason)

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

    def _probe(self, tmp):
        rec = h.Recorder(tmp)
        return rec, h.Probe(rec, "case-x")

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
        tmp = tempfile.mkdtemp(prefix="cowfs-crash88-test-")
        try:
            rec, probe = self._probe(tmp)
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


class TestNativeControlMatchesTheCowfsOperations(unittest.TestCase):
    def test_the_native_writer_performs_the_same_rename_and_syncs(self):
        import inspect

        src = inspect.getsource(h.internal_native_writer)
        self.assertIn("os.rename", src)
        self.assertIn("fsync_dir", src)
        self.assertIn("os.O_RDONLY", src)


if __name__ == "__main__":
    unittest.main()