#!/usr/bin/env python3
"""Tests for bench/g5_diff.py. Run: python3 -m unittest discover -s bench -v

No root, no mounts, no suite. The consoles below are captured verbatim from the
cachyos box (kernel 7.2.8); the identities are forged on purpose to show that each
validity rule refuses.
"""

import json
import tempfile
import unittest
from pathlib import Path

import g5_diff as g

HDR_FUSE = "FSTYP         -- fuse\nPLATFORM      -- Linux/x86_64 cachyos-x8664 7.2.8-2-cachyos\n\n"
HDR_EXT4 = HDR_FUSE.replace("fuse", "ext4")


def passed(hdr, n="001"):
    return f"{hdr}generic/{n}         4s\nRan: generic/{n}\nPassed all 1 tests\n"


def notrun(hdr, reason, n="008"):
    return (f"{hdr}generic/{n}        [not run] {reason}\nRan: generic/{n}\n"
            f"Not run: generic/{n}\nPassed all 1 tests\n")


def failed(hdr, n="025"):
    return (f"{hdr}generic/{n}        - output mismatch (see /x/{n}.out.bad)\n"
            f"    --- tests/generic/{n}.out\n"
            f"Ran: generic/{n}\nFailures: generic/{n}\nFailed 1 of 1 tests\n")


class ParseConsole(unittest.TestCase):
    def test_pass(self):
        r = g.parse_console(passed(HDR_FUSE), 0, "generic/001")
        self.assertEqual(r["status"], "PASS")
        self.assertEqual(r["fstyp"], "fuse")

    def test_not_run_is_not_a_pass_even_though_suite_says_passed_all(self):
        r = g.parse_console(notrun(HDR_FUSE, "xfs_io fzero  failed (old kernel/wrong fs?)"),
                            0, "generic/008")
        self.assertEqual(r["status"], "NOT_RUN")
        self.assertIn("fzero", r["reason"])

    def test_fail(self):
        r = g.parse_console(failed(HDR_FUSE), 1, "generic/025")
        self.assertEqual(r["status"], "FAIL")

    def test_exit_status_failure_line(self):
        txt = (HDR_EXT4 + "generic/131        [failed, exit status 1]- output mismatch (see x)\n"
               "Ran: generic/131\nFailures: generic/131\nFailed 1 of 1 tests\n")
        self.assertEqual(g.parse_console(txt, 1, "generic/131")["status"], "FAIL")

    def test_timeout_by_rc(self):
        r = g.parse_console(HDR_FUSE + "generic/074 ", 124, "generic/074")
        self.assertEqual(r["status"], "TIMEOUT")

    def test_nothing_parseable_is_no_result(self):
        self.assertEqual(g.parse_console("check: QA must be run as root\n", 1,
                                         "generic/001")["status"], "NO_RESULT")
        self.assertEqual(g.parse_console("", 0, "generic/001")["status"], "NO_RESULT")

    def test_pass_with_nonzero_rc_is_not_pass(self):
        self.assertEqual(g.parse_console(passed(HDR_FUSE), 1, "generic/001")["status"],
                         "NO_RESULT")

    def test_wrong_case_named_is_no_result(self):
        r = g.parse_console(passed(HDR_FUSE, "002"), 0, "generic/001")
        self.assertEqual(r["status"], "NO_RESULT")

    def test_two_ran_lines_refused(self):
        txt = passed(HDR_FUSE) + "Ran: generic/001\nPassed all 1 tests\n"
        self.assertEqual(g.parse_console(txt, 0, "generic/001")["status"], "NO_RESULT")

    def test_forged_summary_inside_failing_case_does_not_win(self):
        # a case printed a pass banner, then the suite reported the failure last
        txt = (HDR_FUSE + "generic/025        - output mismatch\n"
               "Passed all 1 tests\nRan: generic/025\nFailures: generic/025\nFailed 1 of 1 tests\n")
        self.assertEqual(g.parse_console(txt, 1, "generic/025")["status"], "FAIL")

    def test_bracket_is_what_decides_not_run(self):
        # unknown test ignored: nothing ran
        txt = HDR_FUSE + "generic/999 - unknown test, ignored\nRan: \nPassed all 0 tests\n"
        self.assertEqual(g.parse_console(txt, 0, "generic/999")["status"], "NO_RESULT")


# The 28 distinct reasons captured from both arms on the box.
REASONS = {
    "xfs_io fpunch  failed (old kernel/wrong fs?)": "missing_feature",
    "xfs_io falloc  failed (old kernel/wrong fs?)": "missing_feature",
    "xfs_io falloc -k failed (old kernel/wrong fs?)": "missing_feature",
    "xfs_io fzero  failed (old kernel/wrong fs?)": "missing_feature",
    "xfs_io fiemap  failed (old kernel/wrong fs?)": "missing_feature",
    "xfs_io fcollapse  failed (old kernel/wrong fs?)": "missing_feature",
    "xfs_io chattr +ia failed (old kernel/wrong fs?)": "missing_feature",
    "xfs_io chattr +i failed (old kernel/wrong fs?)": "missing_feature",
    "xfs_io exchangerange  not supported on fuse": "missing_feature",
    "file system doesn't support chattr +i": "missing_feature",
    "file system doesn't support any of /usr/bin/chattr +a/+c/+d/+i": "missing_feature",
    "kernel doesn't support renameat2 syscall": "missing_feature",
    "inode creation time not supported by this filesystem": "missing_feature",
    "O_TMPFILE is not supported": "missing_feature",
    "this test requires a valid $SCRATCH_DEV": "harness",
    "/mnt/x/ref/xfstests/src/locktest not built": "harness",
    "dbench not found": "harness",
    "Reflink not supported by test filesystem type: fuse": "by_fstype",
    "Dedupe not supported by test filesystem type: fuse": "by_fstype",
    "ACLs not supported by this filesystem type: fuse": "by_fstype",
    "ext4 does not define maximum ACL count": "by_fstype",
    "require cowfs to be valid block disk": "inherent_fuse",
    "fs block size must be larger than the device block size.  fs block size: 4096, "
    "device block size: 4096": "inherent_fuse",
    "device block size: 4096 greater than 512": "inherent_fuse",
    "Need device logical block size(4096) < fs block size(4096)": "inherent_fuse",
}


class ClassifyReason(unittest.TestCase):
    def test_every_captured_reason(self):
        for reason, want in REASONS.items():
            self.assertEqual(g.classify_reason(reason), want, reason)

    def test_unknown_is_unclassified_never_silently_bucketed(self):
        self.assertEqual(g.classify_reason("something nobody has seen"), "unclassified")


def ident(arm="native", **kw):
    base = {
        "native": dict(arm="native", test_dir="/o/native/mnt", test_dev="/dev/loop3",
                       target_line="ext4|/dev/loop3|/o/native/mnt", source_mounts="1",
                       backing="/o/native/x.img"),
        "cowfs": dict(arm="cowfs", test_dir="/o/cowfs/mnt", test_dev="cowfs",
                      target_line="fuse|cowfs|/o/cowfs/mnt", source_mounts="1", backing="",
                      snapshot="g5-cowfs-001", fsroot="/g5-cowfs-001"),
    }[arm]
    base.update(kw)
    return "".join(f"{k}={v}\n" for k, v in base.items())


class Identity(unittest.TestCase):
    def check(self, arm, **kw):
        return g.check_identity(arm, g.parse_identity(ident(arm, **kw)))

    def test_good(self):
        self.assertEqual(self.check("native"), [])
        self.assertEqual(self.check("cowfs"), [])

    def test_native_that_is_actually_cowfs(self):
        p = self.check("native", test_dev="cowfs", target_line="fuse|cowfs|/o/native/mnt")
        self.assertTrue(p)

    def test_cowfs_that_is_actually_native(self):
        p = self.check("cowfs", test_dev="/dev/loop3",
                       target_line="ext4|/dev/loop3|/o/cowfs/mnt")
        self.assertTrue(p)

    def test_ambiguous_source(self):
        self.assertTrue(self.check("cowfs", source_mounts="2"))
        self.assertTrue(self.check("native", source_mounts="0"))

    def test_target_mismatch(self):
        self.assertTrue(self.check("cowfs", target_line="fuse|cowfs|/elsewhere"))

    def test_cowfs_must_be_on_its_own_fresh_snapshot(self):
        self.assertTrue(self.check("cowfs", fsroot="/"))
        self.assertTrue(self.check("cowfs", fsroot="/some-other-snapshot"))
        self.assertTrue(self.check("cowfs", snapshot=""))

    def test_missing_identity(self):
        self.assertTrue(g.check_identity("native", g.parse_identity("")))

    def test_native_without_backing_file(self):
        self.assertTrue(self.check("native", backing=""))


WRAP = "op=umount moved=1 caller=/bin/bash ./check generic/001  args=cowfs\n"
CASE_UM = "op=umount moved=1 caller=/bin/bash ./tests/generic/001  args=cowfs\n"
CASE_MT = "op=mount moved=1 caller=/bin/bash ./tests/generic/001  args=-t fuse cowfs /m\n"


class Cycles(unittest.TestCase):
    def test_count(self):
        self.assertEqual(g.count_cycles(""), 0)
        # check's own wrap-up unmounts TEST_DEV after the case: not a cycle
        self.assertEqual(g.count_cycles(WRAP), 0)
        # the case cycles the mount, then the wrap-up
        self.assertEqual(g.count_cycles(CASE_UM + CASE_MT + WRAP), 2)

    def test_case_unmount_without_restore_is_counted(self):
        # the wrap-up then finds nothing mounted and is logged moved=0 by the shim
        nomove = "op=umount moved=0 caller=/bin/bash ./check generic/001  args=cowfs\n"
        self.assertEqual(g.count_cycles(CASE_UM + nomove), 1)

    def test_unparseable_line_counts_conservatively(self):
        self.assertEqual(g.count_cycles("umount cowfs\n"), 1)


class Record(unittest.TestCase):
    def test_emulated_cycle_is_not_a_clean_pass(self):
        r = g.make_record("cowfs", "generic/001", passed(HDR_FUSE), 0, ident("cowfs"),
                          CASE_UM + CASE_MT + WRAP)
        self.assertEqual(r["status"], "PASS_EMULATED")

    def test_wrapup_umount_alone_is_a_clean_pass(self):
        r = g.make_record("cowfs", "generic/001", passed(HDR_FUSE), 0, ident("cowfs"),
                          WRAP)
        self.assertEqual(r["status"], "PASS")

    def test_residue_in_bare_directory_poisons_the_record(self):
        for residue in ("tmp.abc\n", "\nfile\n"):
            r = g.make_record("cowfs", "generic/001", passed(HDR_FUSE), 0, ident("cowfs"), "",
                              residue)
            self.assertTrue([p for p in r["problems"] if "bare" in p], residue)
        ok = g.make_record("cowfs", "generic/001", passed(HDR_FUSE), 0, ident("cowfs"), "", "")
        self.assertEqual(ok["problems"], [])

    def test_native_cycle_is_a_real_pass(self):
        r = g.make_record("native", "generic/001", passed(HDR_EXT4), 0, ident("native"), "")
        self.assertEqual(r["status"], "PASS")

    def test_header_must_match_arm(self):
        r = g.make_record("native", "generic/001", passed(HDR_FUSE), 0, ident("native"), "")
        self.assertTrue(r["problems"])
        r = g.make_record("cowfs", "generic/001", passed(HDR_EXT4), 0, ident("cowfs"), "")
        self.assertTrue(r["problems"])

    def test_forged_identity_poisons_the_record(self):
        r = g.make_record("native", "generic/001", passed(HDR_EXT4), 0,
                          ident("native", test_dev="cowfs", target_line="fuse|cowfs|/o"), "")
        self.assertTrue(r["problems"])


def rec(arm, case, status, cls=None, cycles=0, problems=()):
    return {"arm": arm, "case": case, "status": status, "reason": "r" if cls else None,
            "class": cls, "cycles": cycles, "problems": list(problems)}


class Pair(unittest.TestCase):
    def test_labels(self):
        P = g.pair_label
        self.assertEqual(P(rec("n", "a", "PASS"), rec("c", "a", "PASS")), "both_pass")
        self.assertEqual(P(rec("n", "a", "PASS"), rec("c", "a", "FAIL")), "worse")
        self.assertEqual(P(rec("n", "a", "PASS"), rec("c", "a", "TIMEOUT")), "worse")
        self.assertEqual(P(rec("n", "a", "PASS"), rec("c", "a", "NOT_RUN", "missing_feature")), "gap")
        self.assertEqual(P(rec("n", "a", "PASS"), rec("c", "a", "PASS_EMULATED")), "emulated")
        self.assertEqual(P(rec("n", "a", "FAIL"), rec("c", "a", "PASS")), "better")
        self.assertEqual(P(rec("n", "a", "NOT_RUN", "by_fstype"), rec("c", "a", "NOT_RUN", "by_fstype")),
                         "both_not_run")
        self.assertEqual(P(rec("n", "a", "FAIL"), rec("c", "a", "FAIL")), "both_fail")
        self.assertEqual(P(rec("n", "a", "NOT_RUN", "by_fstype"), rec("c", "a", "PASS")), "better")


IDS6 = ["generic/005", "generic/236", "generic/245", "generic/309", "generic/360", "generic/755"]


def meta(**kw):
    m = dict(tree_head="T" * 40, tree_porcelain="", check_sha256="C" * 64,
             cowfs_bin="/w/target/release/cowfs-daemon", cowfs_profile="release",
             cowfs_bin_sha256="b" * 64)
    m.update({f"case_sha.{i.split('/')[1]}": "s" for i in IDS6})
    m.update(kw)
    return m


PIN = {"tree_sha": "T" * 40, "runner": {"check": "C" * 64},
       "cases": {i.split("/")[1]: "s" for i in IDS6}}


def good_records(cases=IDS6, cow="PASS"):
    n = [rec("native", c, "PASS") for c in cases]
    c = [rec("cowfs", x, cow) for x in cases]
    ctl = rec("control", "generic/005", "FAIL")
    return n, c, ctl


class Verdict(unittest.TestCase):
    def build(self, n, c, ctl, mode="acceptance", m=None, requested=IDS6, pin=PIN):
        return g.build_receipt(n, c, ctl, meta() if m is None else m, pin, mode, requested)

    def test_acceptance_pass(self):
        r = self.build(*good_records())
        self.assertEqual((r["verdict"], r["exit"]), ("PASS", 0))

    def test_worse_is_fail(self):
        n, c, ctl = good_records()
        c[1]["status"] = "FAIL"
        r = self.build(n, c, ctl)
        self.assertEqual((r["verdict"], r["exit"]), ("FAIL", 1))
        self.assertEqual(r["worse"], ["generic/236"])

    def test_all_not_run_cowfs_is_unmeasurable_not_diagnostic_zero(self):
        n, c, ctl = good_records()
        for x in c:
            x["status"], x["class"] = "NOT_RUN", "missing_feature"
        for mode in ("acceptance", "diagnostic"):
            r = self.build(n, c, ctl, mode=mode)
            self.assertEqual((r["verdict"], r["exit"]), ("UNMEASURABLE", 2), mode)

    def test_all_not_run_native_is_unmeasurable(self):
        n, c, ctl = good_records()
        for x in n:
            x["status"], x["class"] = "NOT_RUN", "harness"
        self.assertEqual(self.build(n, c, ctl, mode="diagnostic")["exit"], 2)

    def test_acceptance_with_emulated_cycle_is_not_pass(self):
        n, c, ctl = good_records()
        c[0]["status"] = "PASS_EMULATED"
        r = self.build(n, c, ctl)
        self.assertNotEqual(r["verdict"], "PASS")
        self.assertEqual(r["exit"], 2)

    def test_acceptance_gap_is_not_pass(self):
        n, c, ctl = good_records()
        c[0]["status"], c[0]["class"] = "NOT_RUN", "missing_feature"
        r = self.build(n, c, ctl)
        self.assertEqual((r["verdict"], r["exit"]), ("UNMEASURABLE", 2))

    def test_acceptance_only_on_reviewed_set(self):
        n, c, ctl = good_records(IDS6[:5])
        r = self.build(n, c, ctl, requested=IDS6[:5])
        self.assertEqual((r["verdict"], r["exit"]), ("INVALID", 3))

    def test_missing_control_failure_is_invalid(self):
        n, c, ctl = good_records()
        ctl["status"] = "PASS"
        self.assertEqual(self.build(n, c, ctl)["verdict"], "INVALID")
        r = g.build_receipt(n, c, None, meta(), PIN, "acceptance", IDS6)
        self.assertEqual(r["verdict"], "INVALID")

    def test_record_problem_is_invalid(self):
        n, c, ctl = good_records()
        c[2]["problems"] = ["identity: cowfs arm is ext4"]
        self.assertEqual(self.build(n, c, ctl)["verdict"], "INVALID")

    def test_no_result_is_invalid(self):
        n, c, ctl = good_records()
        c[2]["status"] = "NO_RESULT"
        self.assertEqual(self.build(n, c, ctl)["verdict"], "INVALID")

    def test_arms_must_cover_the_same_ids(self):
        n, c, ctl = good_records()
        self.assertEqual(self.build(n, c[:-1], ctl)["verdict"], "INVALID")
        self.assertEqual(self.build(n + [n[0]], c, ctl)["verdict"], "INVALID")

    def test_tree_sha_must_be_pinned(self):
        self.assertEqual(self.build(*good_records(), m=meta(tree_head="X" * 40))["verdict"], "INVALID")

    def test_dirty_tree_invalid(self):
        self.assertEqual(self.build(*good_records(), m=meta(tree_porcelain="?? tmp.1"))["verdict"],
                         "INVALID")

    def test_check_bytes_must_be_pinned(self):
        self.assertEqual(self.build(*good_records(), m=meta(check_sha256="D" * 64))["verdict"],
                         "INVALID")

    def test_reviewed_case_bytes_must_be_pinned(self):
        r = self.build(*good_records(), m=meta(**{"case_sha.236": "other"}))
        self.assertEqual(r["verdict"], "INVALID")

    def test_missing_meta_invalid(self):
        self.assertEqual(self.build(*good_records(), m={})["verdict"], "INVALID")

    def test_debug_daemon_not_acceptable(self):
        r = self.build(*good_records(), m=meta(cowfs_profile="debug"))
        self.assertEqual(r["verdict"], "INVALID")

    def test_diagnostic_never_says_pass(self):
        ids = IDS6 + ["generic/008"]
        n, c, ctl = good_records(ids)
        c[-1]["status"], c[-1]["class"] = "NOT_RUN", "missing_feature"
        r = self.build(n, c, ctl, mode="diagnostic", requested=ids)
        self.assertEqual((r["verdict"], r["exit"]), ("DIAGNOSTIC", 0))
        self.assertEqual(r["gap"], {"missing_feature": ["generic/008"]})

    def test_gap_requires_native_pass_symmetric_skips_are_not_gaps(self):
        ids = IDS6 + ["generic/110"]
        n, c, ctl = good_records(ids)
        n[-1]["status"], n[-1]["class"] = "NOT_RUN", "by_fstype"
        c[-1]["status"], c[-1]["class"] = "NOT_RUN", "by_fstype"
        r = self.build(n, c, ctl, mode="diagnostic", requested=ids)
        self.assertEqual(r["gap"], {})
        self.assertEqual(r["counts"]["both_not_run"], 1)

    def test_counts_by_class(self):
        ids = IDS6 + ["generic/008", "generic/114"]
        n, c, ctl = good_records(ids)
        c[-2]["status"], c[-2]["class"] = "NOT_RUN", "missing_feature"
        c[-1]["status"], c[-1]["class"] = "NOT_RUN", "inherent_fuse"
        r = self.build(n, c, ctl, mode="diagnostic", requested=ids)
        self.assertEqual(r["cowfs_not_run_by_class"], {"missing_feature": 1, "inherent_fuse": 1})


class LoadRun(unittest.TestCase):
    def test_round_trip_from_directory(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            (d / "meta.txt").write_text("".join(f"{k}={v}\n" for k, v in meta().items()))
            (d / "cases.txt").write_text("\n".join(IDS6[:1]) + "\n")
            for arm, hdr, idt in (("native", HDR_EXT4, ident("native")),
                                  ("cowfs", HDR_FUSE, ident("cowfs"))):
                cd = d / arm / "005"
                cd.mkdir(parents=True)
                (cd / "console.txt").write_text(passed(hdr, "005"))
                (cd / "rc").write_text("0\n")
                (cd / "identity.txt").write_text(idt)
            cd = d / "control" / "005"
            cd.mkdir(parents=True)
            (cd / "console.txt").write_text(failed(HDR_FUSE, "005"))
            (cd / "rc").write_text("1\n")
            (cd / "identity.txt").write_text(ident("cowfs"))
            run = g.load_run(d)
            self.assertEqual(run["requested"], ["generic/005"])
            self.assertEqual(run["native"][0]["status"], "PASS")
            self.assertEqual(run["control"]["status"], "FAIL")
            self.assertEqual(run["meta"]["cowfs_profile"], "release")


if __name__ == "__main__":
    unittest.main()
