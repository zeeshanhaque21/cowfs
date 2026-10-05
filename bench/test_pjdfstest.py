"""Checks for bench/pjdfstest.py.

Every record here is marked synthetic. The verdict refuses synthetic records, so a fixture can
never be scored as conformance evidence; these tests prove the refusal and the pairing rules
directly instead.

The pairing tests come first on purpose: an identity that survives a shifted, added, missing or
duplicated result is the property the whole gate rests on.
"""

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

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


SANITISED_SCOPE = "sanitised-reference"
EXPORT_RE = re.compile(r"cowfs-[0-9a-f]{32}")
MOUNTED_BY_RE = re.compile(r"mounted by \S+")


def scrub(value, prefixes):
    """Replace host facts by rule: the longest prefix first, then the export and user tokens."""
    if not isinstance(value, str):
        return value
    for base, replacement in sorted(prefixes, key=lambda pair: len(pair[0]), reverse=True):
        if value == base:
            value = replacement or Path(base).name
        elif value.startswith(base + "/"):
            value = value[len(base) + 1:]
            if replacement:
                value = f"{replacement}/{value}"
        elif base in value:
            value = value.replace(base + "/", f"{replacement}/" if replacement else "")
    return MOUNTED_BY_RE.sub("mounted by <user>", EXPORT_RE.sub("<private-export>", value))


def json_paths(node, path=""):
    if isinstance(node, dict):
        for key, value in node.items():
            yield from json_paths(value, f"{path}.{key}" if path else key)
    elif isinstance(node, list):
        for index, value in enumerate(node):
            yield from json_paths(value, f"{path}[{index}]")
    else:
        yield path, node


FIXTURE = Path(__file__).resolve().parents[1] / "bench" / "pjdfstest-fixture"
ESTABLISHED = 25
UNPAIRABLE = 26


def stage_fixture(root: Path, rewrite_raw: bool = True) -> tuple[Path, Path]:
    """Copy the tracked fixture into `root`, optionally re-pointing raw streams at the copy.

    The committed records already name their streams relatively. The rewrite exists to exercise the
    absolute form, which the historical record sets use, and it keeps every raw_sha256 verifiable
    because the transcript bytes are identical either way.
    """
    run, tool = root / "run", root / "tool"
    shutil.copytree(FIXTURE / "run", run)
    shutil.copytree(FIXTURE / "tool", tool)
    if rewrite_raw:
        rows = [json.loads(line) for line in (run / "cases.jsonl").read_text().splitlines()]
        for row in rows:
            row["raw"] = str((run / row["raw"]).resolve())
        (run / "cases.jsonl").write_text("".join(json.dumps(r, sort_keys=True) + "\n" for r in rows))
    return run, tool


def digests(root: Path) -> dict:
    return {str(q.relative_to(root)): hashlib.sha256(q.read_bytes()).hexdigest()
            for q in sorted(root.rglob("*")) if q.is_file()}


def run_cli(script: Path, run: Path, *args: str, repo: Path | None = None,
            cwd: Path | None = None) -> subprocess.CompletedProcess:
    """The published CLI as a child process, which is how a reader meets it."""
    command = [sys.executable, str(script), "--reconcile", str(run)]
    if repo is not None:
        command += ["--repo", str(repo)]
    return subprocess.run(command + list(args), capture_output=True, text=True, timeout=600,
                          cwd=None if cwd is None else str(cwd), check=False)


def sanitise_run(source_run: Path, tool_root: Path, lease_root: Path, dest: Path) -> dict:
    """Write a host-free, portable copy of a run's records into `dest`, and describe the transform.

    Replacements are by rule rather than field by field, so the map is mechanical and checkable: the
    run directory and the remaining lease prefix become relative to the copy, the tool checkout
    becomes `tool`, and the capture host's user and private export name become declared tokens. The
    ten transcripts and the five case scripts are copied byte for byte.

    The identity copy this writes is a sanitised reference, not a live receipt. It declares its own
    scope and records the sha256 of the receipt it came from, because the fields it keeps say which
    filesystems that run saw and say nothing about any filesystem now.

    The source is only ever read.
    """
    prefixes = ((str(tool_root), "tool"), (str(source_run), ""), (str(lease_root), ""))
    (dest / "run" / "raw").mkdir(parents=True)
    for rel in p.CURATED_CASES:
        (dest / "tool" / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(tool_root / rel, dest / "tool" / rel)
    shutil.copy(tool_root / "COPYING", dest / "tool" / "COPYING")
    for stream in sorted((source_run / "raw").iterdir()):
        shutil.copy(stream, dest / "run" / "raw" / stream.name)

    rows = [json.loads(line) for line in (source_run / "cases.jsonl").read_text().splitlines()]
    rewritten = []
    for row in rows:
        row["raw"] = f"raw/{Path(row['raw']).name}"
        rewritten.append({key: scrub(value, prefixes)
                          for key, value in row.items()})
    (dest / "run" / "cases.jsonl").write_text(
        "".join(json.dumps(r, sort_keys=True) + "\n" for r in rewritten))

    identity = json.loads((source_run / "identity.json").read_text())
    before = dict(json_paths(identity))
    cleaned = json.loads(json.dumps(identity))
    cleaned = _replace_strings(cleaned, lambda v: scrub(v, prefixes))
    cleaned["runtime_identity"]["declared_scope"] = SANITISED_SCOPE
    cleaned["runtime_identity"]["sanitisation"] = {
        "kind": "host paths, the capture host's user and the private export name replaced by tokens",
        "source_run": source_run.name,
        "source_identity_sha256": hashlib.sha256((source_run / "identity.json").read_bytes())
                                  .hexdigest(),
        "rules": ["the tool checkout prefix becomes tool", "the run directory prefix becomes relative",
                  "the remaining lease prefix becomes repo-relative",
                  "the private export name becomes <private-export>", "mounted by <user>"],
        "kept": ["st_dev", "fstype", "mountpoint", "plan", "ok", "not_ok", "raw_sha256"],
        "meaning": "provenance of the capture, not an attestation of the filesystem it ran on",
    }
    (dest / "run" / "identity.json").write_text(json.dumps(cleaned, indent=1, sort_keys=True) + "\n")
    changed = sorted(path for path, value in json_paths(cleaned)
                     if before.get(path) != value and path in before)

    provenance = {
        "what_this_is": "a host-free derivative of one receipted run, kept so the checks have a "
                        "fixed self-contained input",
        "source_run": source_run.name,
        "source": {name: hashlib.sha256((source_run / name).read_bytes()).hexdigest()
                   for name in ("cases.jsonl", "identity.json")},
        "fixture": {str(path.relative_to(dest)): hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in sorted(dest.rglob("*")) if path.is_file()},
        "identity_fields_rewritten": changed,
        "identity_fields_kept": sorted(set(before) - set(changed)),
        "declared_scope": SANITISED_SCOPE,
        "not_a_live_receipt": "the fixture's identity.json is a sanitised reference. The live receipt "
                              "is the run directory it came from, and only that receipt attests a "
                              "filesystem.",
    }
    (dest / "PROVENANCE.json").write_text(json.dumps(provenance, indent=2, sort_keys=True) + "\n")
    return provenance


def _replace_strings(node, transform):
    if isinstance(node, dict):
        return {key: _replace_strings(value, transform) for key, value in node.items()}
    if isinstance(node, list):
        return [_replace_strings(value, transform) for value in node]
    return transform(node)


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
    def pinned_context(self):
        """The committed curated closure, verified against its pins.

        A verdict that can say FAIL or UNMEASURABLE needs a verified source: without one the
        classification prerequisite refuses, and a refusal cannot be read as either of those. The
        integrity properties below need no such context, and are exercised without one on purpose.
        """
        verified = p.verify_tool(FIXTURE / "tool")
        self.assertEqual(verified["problems"], [], "the committed closure must verify")
        return verified, FIXTURE / "tool" / "tests"

    def run_verdict(self, native_cases, cowfs_cases, identity=None, extra=None, test="x.t",
                    context=False, cowfs_plan=None):
        tool, tests_root = self.pinned_context() if context else (None, None)
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp)
            lines = []
            for arm, cases, plan in (("native", native_cases, None),
                                     ("cowfs", cowfs_cases, cowfs_plan)):
                record_ = record(arm, test, cases, plan=plan)
                record_.pop("synthetic")
                record_.update({"bail_out": 0, "malformed": []})
                if extra:
                    record_.update(extra)
                lines.append(json.dumps(record_, sort_keys=True))
            (run / "cases.jsonl").write_text("\n".join(lines) + "\n")
            return p.verdict(run, tool, tests_root,
                             identity if identity is not None else GOOD_IDENTITY)

    def test_established_regression_is_fail(self):
        # open/17.t makes three assertions in a fixed order and prints no operation text, which is
        # what the real transcript looks like, so the pairs come from the pinned script's slots.
        native = [case(True, n=1), case(True, n=2), case(True, n=3)]
        cowfs = [case(True, n=1), case(False, n=2), case(True, n=3)]
        report = self.run_verdict(native, cowfs, test="open/17.t", context=True)
        self.assertEqual(len(report["comparison"]["established_regressions"]), 1)
        self.assertEqual((report["state"], report["exit_status"]), (p.FAIL, 1))

    def test_a_pass_with_nothing_pairable_is_disclosed_as_coverage(self):
        """Nothing pairable is a limit, and the settled taxonomy discloses it without moving the exit.

        This method was written before the coverage rule was settled and expected UNMEASURABLE. The
        settled rule discloses coverage and lets no other kind change the exit, so with a verified
        source and nothing pairable the verdict is PASS and the disclosure carries the limit. Whether
        an entirely empty scope should read as a pass is a question for the gate's owner, recorded
        here rather than settled by a test, and it is the same shape as the false pass this harness
        already refuses elsewhere: evidence that could not be read.
        """
        # The cowfs arm emitted fewer assertions than the pinned script makes, so nothing pairs.
        native = [case(True, n=1), case(True, n=2), case(True, n=3)]
        cowfs = [case(True, n=1), case(True, n=2)]
        report = self.run_verdict(native, cowfs, test="open/17.t", context=True, cowfs_plan=2)
        self.assertEqual(report["comparison"]["established_regressions"], [])
        self.assertTrue(report["comparison"]["unpairable"])
        self.assertEqual((report["state"], report["exit_status"]), (p.PASS, 0))
        self.assertTrue(any("cannot be paired" in r["message"] for r in report["reasons"]))
        without_source = self.run_verdict(native, cowfs, test="open/17.t", cowfs_plan=2)
        self.assertEqual((without_source["state"], without_source["exit_status"]), (p.INVALID, 3))

    def test_synthetic_records_are_refused_by_the_verdict_itself(self):
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

    def test_malformed_json_is_invalid_input_not_a_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = Path(tmp)
            (run / "cases.jsonl").write_text("{not json\n")
            self.assertEqual(p.verdict(run, identity=GOOD_IDENTITY)["exit_status"], 3)

    def test_mixed_raw_formats_are_refused(self):
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
    """An analysis reads evidence; it never writes into it, and it never skips.

    Every check here builds its own copy of the tracked fixture under `bench/pjdfstest-fixture`
    inside a private temporary directory, so no check depends on the ignored tool cache, on another
    check having run first, or on a file it does not own. Nothing here touches a preserved run.
    """

    ESTABLISHED = ESTABLISHED
    UNPAIRABLE = UNPAIRABLE
    SENTINEL = "this file is evidence and must not change\n"
    # What the pinned closure makes of the fixture's records: the receipted run 20261005T025742Z
    # scored 25 established regressions and 26 unpairable assertions on exactly these bytes.

    def setUp(self):
        self.work = Path(tempfile.mkdtemp(prefix="pjdfstest-reconcile-"))
        self.addCleanup(shutil.rmtree, self.work, ignore_errors=True)
        (self.work / "analysis").mkdir()
        self.run, self.tool = stage_fixture(self.work)

    digests = staticmethod(digests)

    def cli(self, *args: str, repo: Path | None = None,
            script: Path | None = None, cwd: Path | None = None) -> subprocess.CompletedProcess:
        return run_cli(script or Path(p.__file__), self.run, *args, repo=repo, cwd=cwd)

    def git_repo(self, name: str, tracked: Path) -> Path:
        """A private git repo holding one tracked file, so provenance can be exercised at all."""
        root = self.work / name
        root.mkdir()
        target = root / tracked
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(Path(p.__file__), target)
        subprocess.run(["git", "init", "--quiet", str(root)], check=True, timeout=120)
        subprocess.run(["git", "-C", str(root), "-c", "user.name=t", "-c", "user.email=t@x",
                        "add", str(target.relative_to(root))], check=True, timeout=120)
        subprocess.run(["git", "-C", str(root), "-c", "user.name=t", "-c", "user.email=t@x",
                        "commit", "--quiet", "-m", "track"], check=True, timeout=120)
        return root

    def bare_repo(self) -> Path:
        """A checkout with no tool cache, which is what a fresh clone looks like."""
        bare = self.work / "fresh-clone"
        (bare / "bench").mkdir(parents=True)
        shutil.copy(Path(p.__file__), bare / "bench" / "pjdfstest.py")
        return bare

    def test_the_pinned_closure_classifies_the_record_set_and_fails(self):
        out = self.work / "analysis" / "fresh.json"
        before = self.digests(self.run)
        done = self.cli("--tool", str(self.tool), "--output", str(out))
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("state FAIL exit 1", done.stdout)
        payload = json.loads(out.read_text())
        self.assertEqual(payload["state"], p.FAIL)
        self.assertEqual(len(payload["comparison"]["established_regressions"]), self.ESTABLISHED)
        self.assertEqual(len(payload["comparison"]["unpairable"]), self.UNPAIRABLE)
        self.assertEqual(sorted(r["kind"] for r in payload["reasons"] if r["kind"] != "COVERAGE"),
                         ["DIVERGENCE"])
        self.assertEqual(self.digests(self.run), before, "the analysis touched its own input")
        self.assertEqual(payload["analysis"]["inputs"]["cases.jsonl"],
                         hashlib.sha256((self.run / "cases.jsonl").read_bytes()).hexdigest())
        self.assertIn("fresh reading", payload["analysis"]["note"])

    def test_a_reconciliation_without_the_pinned_scripts_is_invalid_not_pass(self):
        done = self.cli("--output", str(self.work / "analysis" / "none.json"), repo=self.bare_repo())
        self.assertNotEqual(done.returncode, 0, "a gate that never ran must not read as a pass")
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("state INVALID exit 3", done.stdout)
        self.assertIn("not a pjdfstest checkout", done.stdout)
        self.assertIn("nothing was classified and nothing was written", done.stdout)

    def test_verdict_without_the_pinned_scripts_refuses_before_classifying(self):
        identity = json.loads((self.run / "identity.json").read_text())["runtime_identity"]
        report = p.verdict(self.run, None, None, identity)
        self.assertEqual(report["state"], p.INVALID)
        self.assertEqual(report["exit_status"], 3)
        self.assertIn("so no assertion can be classified",
                      " ".join(r["message"] for r in report["reasons"]))

    def test_an_altered_curated_closure_is_refused(self):
        script = self.tool / "tests" / "open" / "17.t"
        script.write_text(script.read_text() + "# tampered\n")
        done = self.cli("--tool", str(self.tool), "--output", str(self.work / "analysis" / "bad.json"))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("is not the pinned blob", done.stdout)

    def test_an_existing_default_receipt_is_refused_and_not_one_byte_changes(self):
        sentinel = self.run / "reconciliation.json"
        sentinel.write_text(self.SENTINEL)
        before = self.digests(self.run)
        done = self.cli("--tool", str(self.tool))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("already exists", done.stdout)
        self.assertEqual(self.digests(self.run), before, "a byte of the run changed")

    def test_an_output_inside_the_run_is_refused(self):
        before = hashlib.sha256((self.run / "cases.jsonl").read_bytes()).hexdigest()
        done = self.cli("--tool", str(self.tool), "--output", str(self.run / "cases.jsonl"))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("inside the run directory", done.stdout)
        self.assertEqual(hashlib.sha256((self.run / "cases.jsonl").read_bytes()).hexdigest(), before)

    def test_an_output_named_like_a_raw_stream_is_refused(self):
        raw = min((self.run / "raw").iterdir(), key=lambda q: q.name)
        before = hashlib.sha256(raw.read_bytes()).hexdigest()
        done = self.cli("--tool", str(self.tool), "--output", str(raw))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertEqual(hashlib.sha256(raw.read_bytes()).hexdigest(), before)

    def test_a_symlinked_output_is_refused_rather_than_followed(self):
        foreign = self.work / "foreign.json"
        foreign.write_text(self.SENTINEL)
        for name, target in (("link.json", foreign), ("dead.json", self.work / "nowhere.json")):
            link = self.work / name
            link.symlink_to(target)
            done = self.cli("--tool", str(self.tool), "--output", str(link))
            self.assertEqual(done.returncode, 3, f"{name}: " + done.stdout + done.stderr)
            self.assertTrue(link.is_symlink(), f"{name} was replaced rather than refused")
        self.assertEqual(foreign.read_text(), self.SENTINEL)
        self.assertFalse((self.work / "nowhere.json").exists())

    def test_a_missing_parent_directory_is_a_typed_refusal(self):
        done = self.cli("--tool", str(self.tool),
                        "--output", str(self.work / "absent" / "deep" / "fresh.json"))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("could not be staged", done.stdout)
        self.assertNotIn("Traceback", done.stdout + done.stderr)

    def test_a_parent_that_is_a_file_is_a_typed_refusal(self):
        parent = self.work / "not-a-directory"
        parent.write_text("a file where a directory was wanted\n")
        done = self.cli("--tool", str(self.tool), "--output", str(parent / "fresh.json"))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("could not be staged", done.stdout)
        self.assertNotIn("Traceback", done.stdout + done.stderr)
        self.assertEqual(parent.read_text(), "a file where a directory was wanted\n")

    @unittest.skipIf(os.geteuid() == 0, "root ignores directory write permissions")
    def test_an_unwritable_directory_is_a_typed_refusal(self):
        locked = self.work / "locked"
        locked.mkdir()
        self.addCleanup(os.chmod, locked, 0o700)
        os.chmod(locked, 0o500)
        done = self.cli("--tool", str(self.tool), "--output", str(locked / "fresh.json"))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("could not be staged", done.stdout)
        self.assertNotIn("Traceback", done.stdout + done.stderr)
        self.assertEqual(list(locked.iterdir()), [])

    def test_a_link_failure_keeps_the_staged_evidence_and_says_so(self):
        out = self.work / "staged.json"
        with mock.patch.object(p.os, "link", side_effect=PermissionError(13, "Permission denied")):
            written, message = p.write_exclusive(out, "{}\n")
        self.assertFalse(written)
        self.assertIn("Permission denied", message)
        staged = list(out.parent.glob("staged.json.staged-*"))
        self.assertEqual(len(staged), 1, "a failed publication must leave its evidence behind")
        for path in staged:
            path.unlink()

    def test_an_existing_explicit_output_is_refused(self):
        out = self.work / "taken.json"
        out.write_text(self.SENTINEL)
        done = self.cli("--tool", str(self.tool), "--output", str(out))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertEqual(out.read_text(), self.SENTINEL)

    def test_a_run_without_an_identity_receipt_is_invalid_three(self):
        (self.run / "identity.json").unlink()
        done = self.cli("--tool", str(self.tool), "--output", str(self.work / "analysis" / "x.json"))
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("no runtime identity was supplied", done.stdout)

    def test_write_exclusive_never_replaces_and_leaves_no_staging_file(self):
        out = self.work / "once.json"
        written, message = p.write_exclusive(out, "{}\n")
        self.assertTrue(written, message)
        self.assertEqual(out.read_text(), "{}\n")
        self.assertEqual(list(self.work.glob("once.json.staged-*")), [])
        self.assertFalse(p.write_exclusive(out, "second\n")[0])
        self.assertEqual(out.read_text(), "{}\n", "the second write changed the file")
        self.assertEqual(list(self.work.glob("once.json.staged-*")), [])

    def test_the_revision_is_unknown_in_a_checkout_that_does_not_track_the_script(self):
        elsewhere = self.git_repo("unrelated", Path("tools/pjdfstest.py"))
        subprocess.run(["git", "-C", str(elsewhere), "rm", "--quiet", "-r", "--cached", "tools"],
                       capture_output=True, text=True, timeout=120, check=False)
        subprocess.run(["git", "-C", str(elsewhere), "-c", "user.name=t", "-c", "user.email=t@x",
                        "commit", "--quiet", "--allow-empty", "-m", "untrack"], check=True,
                       timeout=120)
        script = elsewhere / "tools" / "pjdfstest.py"
        done = self.cli("--tool", str(self.tool), "--output", str(self.work / "analysis" / "v.json"),
                        repo=elsewhere, script=script)
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        analysis = json.loads((self.work / "analysis" / "v.json").read_text())["analysis"]
        self.assertEqual(analysis["analyser_revision"], "UNKNOWN")
        self.assertEqual(analysis["analyser_sha256"],
                         hashlib.sha256(script.read_bytes()).hexdigest(),
                         "the analysis must bind the bytes that actually ran")
        self.assertIn("not the origin of this analysis",
                      analysis["ambient_checkout"]["relevance"])
        self.assertNotIn("source_head", analysis, "a checkout HEAD is not the analyser's origin")

    def test_the_revision_is_the_head_that_tracks_these_exact_bytes(self):
        elsewhere = self.git_repo("elsewhere", Path("bench/pjdfstest.py"))
        script = elsewhere / "bench" / "pjdfstest.py"
        head = subprocess.run(["git", "-C", str(elsewhere), "rev-parse", "HEAD"], capture_output=True,
                              text=True, timeout=120, check=True).stdout.strip()
        done = self.cli("--tool", str(self.tool), "--output", str(self.work / "analysis" / "t.json"),
                        repo=elsewhere, script=script)
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        analysis = json.loads((self.work / "analysis" / "t.json").read_text())["analysis"]
        self.assertEqual(analysis["analyser_revision"], head)
        self.assertIn("is the script on disk", analysis["analyser_revision_evidence"])
        self.assertEqual(analysis["ambient_checkout"]["head"], head)
        script.write_text(script.read_text() + "\n# edited after the commit\n")
        again = self.cli("--tool", str(self.tool),
                         "--output", str(self.work / "analysis" / "u.json"), repo=elsewhere,
                        script=script)
        self.assertEqual(again.returncode, 1, again.stdout + again.stderr)
        self.assertEqual(
            json.loads((self.work / "analysis" / "u.json").read_text())["analysis"]
            ["analyser_revision"], "UNKNOWN", "an edited script must not claim a revision")

class FixtureIsPortableAndDeclared(unittest.TestCase):
    """The committed fixture is host-free, self-describing, and scores the same from anywhere.

    The fixture exists so the checks have a fixed input. These checks are about that input's
    integrity, and about the two ways a reader can be misled by a record set: one that only works
    from one directory, and one whose missing evidence reads as a clean bill of health.
    """

    HOST_NEEDLES = ("/Users/", "zeeshanhaque", ".treehouse", "/tmp/")
    ORIGINAL_CASES = "eb2ff1245baa4aa3"
    ORIGINAL_IDENTITY = "31d92ea0d2bd89d7"

    def setUp(self):
        self.work = Path(tempfile.mkdtemp(prefix="pjdfstest-fixture-"))
        self.addCleanup(shutil.rmtree, self.work, ignore_errors=True)
        (self.work / "analysis").mkdir()
        self.run, self.tool = stage_fixture(self.work, rewrite_raw=False)
        self.harness = Path(p.__file__)

    def rows(self) -> list[dict]:
        return [json.loads(line) for line in (self.run / "cases.jsonl").read_text().splitlines()]

    def write_rows(self, rows: list[dict]) -> None:
        (self.run / "cases.jsonl").write_text(
            "".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))

    def reconcile(self, *args: str, run: Path | None = None, cwd: Path | None = None,
                  repo: Path | None = None) -> tuple[subprocess.CompletedProcess, Path]:
        out = self.work / "out" / f"analysis-{len(list((self.work / 'out').glob('*.json')))}.json"
        out.parent.mkdir(exist_ok=True)
        done = run_cli(self.harness, run or self.run, "--tool", str(self.tool), "--output", str(out),
                       cwd=cwd, repo=repo)
        return done, out

    def test_the_same_records_score_the_same_from_three_working_directories(self):
        payloads = []
        for name, cwd in (("inside", self.run.parent), ("one-up", self.run.parent.parent),
                          ("unrelated", self.work)):
            done, out = self.reconcile(cwd=cwd)
            self.assertEqual(done.returncode, 1, f"{name}: " + done.stdout + done.stderr)
            self.assertIn("state FAIL exit 1", done.stdout, name)
            payloads.append(out.read_bytes())
            out.unlink()
        self.assertEqual(len(set(payloads)), 1, "the working directory changed the verdict")
        payload = json.loads(payloads[0])
        self.assertEqual(len(payload["comparison"]["established_regressions"]), ESTABLISHED)
        self.assertEqual(len(payload["comparison"]["unpairable"]), UNPAIRABLE)

    def test_a_relative_raw_path_cannot_climb_out_of_the_run(self):
        outside = self.work / "outside"
        outside.mkdir()
        shutil.copy(self.run / "raw" / "native_open_17.t.tap", outside / "borrowed.tap")
        rows = self.rows()
        for row in rows:
            if (row["arm"], row["test"]) == ("native", "open/17.t"):
                row["raw"] = "../outside/borrowed.tap"
        self.write_rows(rows)
        done, _ = self.reconcile()
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("outside the run directory", done.stdout)

    def test_a_relative_raw_path_through_a_symlink_out_of_the_run_is_refused(self):
        outside = self.work / "outside"
        outside.mkdir()
        target = outside / "elsewhere.tap"
        shutil.copy(self.run / "raw" / "native_open_17.t.tap", target)
        link = self.run / "raw" / "link.tap"
        link.symlink_to(target)
        rows = self.rows()
        for row in rows:
            if (row["arm"], row["test"]) == ("native", "open/17.t"):
                row["raw"] = "raw/link.tap"
        self.write_rows(rows)
        done, _ = self.reconcile()
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("outside the run directory", done.stdout)

    def test_an_absolute_raw_path_is_taken_as_written(self):
        elsewhere = self.work / "historical"
        shutil.copytree(self.run / "raw", elsewhere / "raw")
        rows = self.rows()
        for row in rows:
            row["raw"] = str((elsewhere / row["raw"]).resolve())
        self.write_rows(rows)
        done, out = self.reconcile()
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertNotIn("outside the run directory", done.stdout)
        self.assertEqual(len(json.loads(out.read_text())["comparison"]["established_regressions"]),
                         ESTABLISHED)

    def test_a_record_set_with_no_readable_raw_stream_is_invalid_not_pass(self):
        for stream in (self.run / "raw").iterdir():
            stream.unlink()
        done, _ = self.reconcile()
        self.assertNotEqual(done.returncode, 0, "missing evidence must never read as a pass")
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("raw stream is missing", done.stdout)
        self.assertIn("INTEGRITY", done.stdout)

    def test_a_run_wider_than_the_closure_is_refused(self):
        rows = self.rows()
        for row in rows:
            if row["test"] == "unlink/14.t":
                row["test"] = "unlink/15.t"
        self.write_rows(rows)
        done, _ = self.reconcile()
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("have no script profile", done.stdout)

    def test_a_closure_with_an_extra_script_is_refused(self):
        extra = self.tool / "tests" / "open" / "18.t"
        extra.write_text("#!/bin/sh\nexpect 0 unlink ${n0}\n")
        done, _ = self.reconcile()
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("carries only the pinned cases", done.stdout)

    def test_a_closure_with_a_file_that_is_not_metadata_is_refused(self):
        (self.tool / "helper.sh").write_text("#!/bin/sh\necho not a case\n")
        done, _ = self.reconcile()
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("carries no helper.sh", done.stdout)

    def test_an_altered_upstream_notice_is_refused(self):
        notice = self.tool / "COPYING"
        notice.write_text(notice.read_text() + "extra\n")
        done, _ = self.reconcile()
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("COPYING sha256", done.stdout)

    def test_the_fixture_carries_no_host_identifiers_and_hashes_as_declared(self):
        provenance = json.loads((FIXTURE / "PROVENANCE.json").read_text())
        for rel, want in provenance["fixture"].items():
            self.assertEqual(hashlib.sha256((FIXTURE / rel).read_bytes()).hexdigest(), want, rel)
        self.assertTrue(provenance["source"]["cases.jsonl"].startswith(self.ORIGINAL_CASES))
        self.assertTrue(provenance["source"]["identity.json"].startswith(self.ORIGINAL_IDENTITY))
        for path in sorted(FIXTURE.rglob("*")):
            if not path.is_file():
                continue
            text = path.read_text(errors="replace")
            for needle in self.HOST_NEEDLES:
                self.assertNotIn(needle, text, f"{path} carries {needle}")
        for rel, want in p.CURATED_CASES.items():
            self.assertEqual(hashlib.sha256((FIXTURE / "tool" / rel).read_bytes()).hexdigest(), want,
                             rel)
        self.assertEqual(hashlib.sha256((FIXTURE / "tool/COPYING").read_bytes()).hexdigest(),
                         p.CURATED_METADATA["COPYING"])

    def test_the_transform_replaces_host_facts_by_rule(self):
        lease = self.work / "Users" / "someone" / "repo"
        source, tool = lease / "run", lease / "tool"
        (source / "raw").mkdir(parents=True)
        shutil.copytree(FIXTURE / "tool", tool)
        rows = self.rows()
        for index, row in enumerate(rows):
            stream = source / "raw" / Path(row["raw"]).name
            shutil.copy(FIXTURE / "run" / row["raw"], stream)
            row["raw"] = str(stream)
            row["case_dir"] = f"{source}/case-{index}"
            row["script_path"] = f"{tool}/{row['script_path']}"
        source.joinpath("cases.jsonl").write_text(
            "".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))
        document = json.loads((FIXTURE / "run/identity.json").read_text())
        document["daemon"]["store"] = f"{source}/store"
        document["daemon"]["socket"] = f"{lease}/rt/c.sock"
        document["daemon"]["argv"] = [f"{lease}/target/release/cowfs-daemon", "--store",
                                      f"{source}/store"]
        document["runtime_identity"]["native"]["path"] = f"{source}/native"
        document["runtime_identity"]["cowfs"]["path"] = f"{source}/mnt/pjd"
        document["runtime_identity"]["cowfs"]["mountpoint"] = f"{source}/mnt"
        source.joinpath("identity.json").write_text(json.dumps(document, indent=1) + "\n")
        before = digests(source)
        dest = self.work / "derived"
        provenance = sanitise_run(source, tool, lease, dest)
        self.assertEqual(digests(source), before, "the transform wrote to its source")
        for path in sorted(dest.rglob("*")):
            if path.is_file():
                text = path.read_text(errors="replace")
                self.assertNotIn(f"{lease}/", text, f"{path} kept a host path")
        self.assertIn("runtime_identity.native.path", provenance["identity_fields_rewritten"])
        self.assertIn("daemon.socket", provenance["identity_fields_rewritten"])
        self.assertEqual(provenance["declared_scope"], SANITISED_SCOPE)
        identity = json.loads((dest / "run" / "identity.json").read_text())
        self.assertEqual(identity["runtime_identity"]["declared_scope"], SANITISED_SCOPE)
        self.assertEqual(identity["runtime_identity"]["sanitisation"]["source_identity_sha256"],
                         hashlib.sha256((source / "identity.json").read_bytes()).hexdigest())
        derived = [json.loads(line) for line in
                   (dest / "run" / "cases.jsonl").read_text().splitlines()]
        for row in derived:
            self.assertEqual(hashlib.sha256((dest / "run" / row["raw"]).read_bytes()).hexdigest(),
                             row["raw_sha256"], row["test"])

    def test_the_verdict_repeats_the_receipts_declared_scope(self):
        done, out = self.reconcile()
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn(f"declares scope {SANITISED_SCOPE!r}", done.stdout)
        analysis = json.loads(out.read_text())["analysis"]
        self.assertEqual(analysis["identity_receipt"]["declared_scope"], SANITISED_SCOPE)
        self.assertTrue(analysis["identity_receipt"]["sanitisation"]["source_identity_sha256"]
                        .startswith(self.ORIGINAL_IDENTITY))
        document = json.loads((self.run / "identity.json").read_text())
        del document["runtime_identity"]["declared_scope"]
        del document["runtime_identity"]["sanitisation"]
        (self.run / "identity.json").write_text(json.dumps(document, indent=1, sort_keys=True) + "\n")
        again, _ = self.reconcile()
        self.assertEqual(again.returncode, 1, again.stdout + again.stderr)
        self.assertNotIn("declares scope", again.stdout,
                         "the disclosure must follow the receipt, not this fixture")



if __name__ == "__main__":
    unittest.main()