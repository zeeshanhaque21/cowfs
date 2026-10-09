"""Gate coverage reporting through the real comparator CLI (issue #80).

Run: python3 -m unittest discover -s bench

Every case here drives `python3 bench/compare.py` as a subprocess, because the
defect being pinned was silent output from the real entrypoint, not a return value:
in-process `compare.main()` cannot see the exit code or the stderr split that a
caller sees. One case builds its arms with the real `gates.py` CLI, so the gate list
the coverage report reads is the one the harness actually writes.
"""

import json
import os
import random
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))
import compare  # noqa: E402
import gates as gates_mod  # noqa: E402

COMPARE = HERE / "compare.py"
GATES = HERE / "gates.py"
MIB = 1 << 20
GIB = 1 << 30
ALL = ["g1", "g2", "g3", "g4", "g5", "g6"]


OLD_PIN = "c1619ec16df3a6b11dd5a1e08e8a512b4fedd240"


def meta(gates, big_bytes=GIB, label="arm", scale=100, sha=None, platform="Darwin", measured_on=None):
    row = {
        "kind": "meta",
        "label": label,
        "root": "/private/bench",
        "reps": 1,
        "gates": gates,
        "counts": {"big_bytes": big_bytes},
        "corpus_sha": sha or gates_mod.DEFAULT_SHA,
        "cargo_home": "/private/cargo-home",
        "cargo_jobs": "4",
        "host": "unit-test",
        "platform": platform,
        "python": "3.12.2",
        "started": 0.0,
    }
    if scale is not None:
        row["scale"] = scale
    if platform is None:
        del row["platform"]
    if measured_on is not None:
        row["measured_on"] = measured_on
    return row


def rep(gate, index, wall, label="arm", load=1.0):
    return {
        "kind": "rep",
        "label": label,
        "root": "/private/bench",
        "gate": gate,
        "rep": index,
        "wall_s": wall,
        "load1_before": load,
        "load1_after": load,
        "metrics": {"rebuilt_count": 127, "bins_relinked": 3} if gate == "g2" else {},
        "ts": 0.0,
    }


def g5_rep(index, label="arm", wall=1.0):
    row = rep("g5", index, wall, label)
    row["metrics"] = {"bytes": MIB, "written_bytes": MIB, "read_bytes": MIB, "read_matches": True}
    return row


def coverage_line(out):
    """The machine-readable coverage line, parsed, and nothing else survives."""
    lines = [line for line in out.splitlines() if line.startswith("coverage ")]
    if len(lines) != 1:
        raise AssertionError(f"want exactly one coverage line, got {len(lines)}")
    return json.loads(lines[0][len("coverage "):])


def gap(text, gate):
    """The one human line describing why a gate produced no comparison."""
    found = [line for line in text.splitlines() if line.strip().startswith(f"{gate}  not compared:")]
    return found[0] if len(found) == 1 else None


class CliCase(unittest.TestCase):
    def arm(self, directory, name, label, gates, rows, **kw):
        path = Path(directory) / name
        path.write_text("".join(json.dumps(r) + "\n" for r in [meta(gates, label=label, **kw), *rows]))
        return str(path)

    def cli(self, native, cowfs, noise=None):
        argv = [sys.executable, str(COMPARE), "--native", *native, "--cowfs", cowfs]
        if noise:
            argv += ["--noise-floor", noise]
        p = subprocess.run(argv, capture_output=True, text=True, cwd=str(REPO))
        return p.returncode, p.stdout, p.stderr


class OneSidedGates(CliCase):
    """The reported defect: a gate with data in one arm only was dropped in silence."""

    def test_gate_missing_from_the_cowfs_arm_names_that_arm(self):
        with tempfile.TemporaryDirectory() as d:
            nat1 = self.arm(d, "n1.jsonl", "native1", ["g1", "g3"], [rep("g1", 0, 8.0), rep("g3", 0, 2.0)])
            nat2 = self.arm(d, "n2.jsonl", "native2", ["g1", "g3"], [rep("g1", 0, 8.2), rep("g3", 0, 2.1)])
            cow = self.arm(d, "c.jsonl", "cowfs1", ["g1", "g3"], [rep("g1", 0, 9.6)])
            rc, out, err = self.cli([nat1, nat2], cow, noise=nat2)
            self.assertEqual(rc, 0, err)
            line = gap(out, "g3")
            self.assertIsNotNone(line, out)
            self.assertIn("the cowfs arm has no g3 data", line)
            self.assertIn("native has 2 reps", line)
            self.assertIn("cowfs1 requested it and recorded 0 reps", line)
            cov = coverage_line(out)
            self.assertEqual(cov["compared"], ["g1"])
            self.assertEqual(cov["gates_known"], 6)
            g3 = next(g for g in cov["not_compared"] if g["gate"] == "g3")
            self.assertEqual(g3["missing_in"], "cowfs")
            self.assertEqual(g3["reps"], {"native": 2, "cowfs": 0})
            self.assertEqual(g3["requested_by"], {"native1": 1, "native2": 1, "cowfs1": 0})
            self.assertIn("RESULT: PASS  scope: compared 1 of 6 (g1), not compared g2 g3 g4 g5 g6", out)
            self.assertNotIn("g3 ", out.split("gate coverage")[0].split("gate  n_nat")[1])

    def test_two_native_files_sharing_a_label_report_their_combined_reps(self):
        """gates.py stamps the file name, so --no-resume twice yields one label, two files."""
        with tempfile.TemporaryDirectory() as d:
            nat1 = self.arm(d, "n1.jsonl", "native1", ["g1", "g3"],
                            [rep("g1", 0, 8.0), rep("g3", 0, 2.0), rep("g3", 1, 2.2)])
            nat2 = self.arm(d, "n2.jsonl", "native1", ["g1", "g3"], [rep("g1", 0, 8.2), rep("g3", 0, 2.1)])
            cow = self.arm(d, "c.jsonl", "cowfs1", ["g1", "g3"], [rep("g1", 0, 9.6)])
            rc, out, err = self.cli([nat1, nat2], cow)
            self.assertEqual(rc, 0, err)
            line = gap(out, "g3")
            self.assertIsNotNone(line, out)
            self.assertIn("native has 3 reps", line)
            self.assertIn("native1 requested it and recorded 3 reps", line)
            g3 = next(g for g in coverage_line(out)["not_compared"] if g["gate"] == "g3")
            self.assertEqual(g3["reps"], {"native": 3, "cowfs": 0})
            self.assertEqual(g3["requested_by"], {"native1": 3, "cowfs1": 0})

    def test_gate_missing_from_the_native_arm_names_that_arm(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "native1", ["g1"], [rep("g1", 0, 8.0)])
            cow = self.arm(d, "c.jsonl", "cowfs1", ["g1", "g3"], [rep("g1", 0, 9.0), rep("g3", 0, 2.2)])
            rc, out, err = self.cli([nat], cow)
            self.assertEqual(rc, 0, err)
            line = gap(out, "g3")
            self.assertIsNotNone(line, out)
            self.assertIn("the native arm has no g3 data", line)
            self.assertIn("cowfs has 1 reps", line)
            self.assertEqual(coverage_line(out)["compared"], ["g1"])

    def test_every_one_sided_direction_of_a_three_gate_pair(self):
        """Each arm's own extra gate is reported, and the matched one still runs."""
        rows = {"g1": [rep("g1", 0, 8.0)], "g2": [rep("g2", i, 4.0 + i / 10) for i in range(3)], "g3": [rep("g3", 0, 2.0)]}
        with tempfile.TemporaryDirectory() as d:
            for native_gates, cowfs_gates, expect_rc in (
                (["g1"], ["g1", "g2"], 0),
                (["g1", "g2"], ["g1"], 0),
                (["g1", "g2"], ["g1", "g2"], 0),
            ):
                nat = self.arm(d, f"n-{'-'.join(native_gates)}.jsonl", "nat", native_gates,
                               sum((rows[g] for g in native_gates), []))
                cow = self.arm(d, f"c-{'-'.join(cowfs_gates)}.jsonl", "cow", cowfs_gates,
                               sum((rows[g] for g in cowfs_gates), []))
                rc, out, err = self.cli([nat], cow)
                self.assertEqual(rc, expect_rc, err)
                compared = coverage_line(out)["compared"]
                self.assertEqual(compared, sorted(set(native_gates) & set(cowfs_gates)))
                missing = [g for g in ("g2", "g3") if g in set(native_gates) ^ set(cowfs_gates)]
                self.assertEqual([g for g in missing if gap(out, g)], missing, out)

    def test_partial_verdict_still_exits_zero_and_names_the_uncompared_gates(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "native1", ["g1", "g3"], [rep("g1", 0, 8.0), rep("g3", 0, 2.0)])
            cow = self.arm(d, "c.jsonl", "cowfs1", ["g1", "g3"], [rep("g1", 0, 9.0)])
            rc, out, err = self.cli([nat], cow)
            self.assertEqual(rc, 0, err)
            table = out.split("gate coverage")[0].splitlines()
            self.assertEqual([line.split()[0] for line in table if line[:2] in ("g1", "g2", "g3", "g4", "g5", "g6")], ["g1"])
            self.assertIn("RESULT: PASS  scope: compared 1 of 6 (g1), not compared g2 g3 g4 g5 g6", out)

    def test_fail_and_unmeasurable_verdicts_carry_the_scope_too(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "native1", ["g1", "g3"], [rep("g1", 0, 8.0), rep("g3", 0, 2.0)])
            slow = self.arm(d, "slow.jsonl", "cowfs1", ["g1", "g3"], [rep("g1", 0, 20.0)])
            rc, out, err = self.cli([nat], slow)
            self.assertEqual(rc, 1, err)
            self.assertIn("RESULT: FAIL (1)  scope: compared 1 of 6 (g1), not compared g2 g3 g4 g5 g6", out)
            # A zero wall on the native side cannot form a ratio, so the gate is
            # unmeasurable; on the cowfs side it is a real 0x ratio and still judged.
            flat = self.arm(d, "flat.jsonl", "native1", ["g1", "g3"], [rep("g1", 0, 0.0), rep("g3", 0, 2.0)])
            rc, out, err = self.cli([flat], self.arm(d, "ok.jsonl", "cowfs1", ["g1", "g3"],
                                                     [rep("g1", 0, 9.0), rep("g3", 0, 2.2)]))
            self.assertEqual(rc, 2, err)
            self.assertIn("unmeasurable", out)
            self.assertIn("RESULT: 1 gate(s) unmeasurable, 0 failed  scope: compared 2 of 6 (g1 g3), "
                          "not compared g2 g4 g5 g6", out)
            self.assertNotIn("PASS", out.splitlines()[-1])


class ScopedComparisonsPreserved(CliCase):
    def test_matched_g1_only_and_g1_g3_only_still_pass(self):
        with tempfile.TemporaryDirectory() as d:
            cases = {
                "g1": (["g1"], [rep("g1", 0, 8.0)], [rep("g1", 0, 9.0)], ["g1"]),
                "g1+g3": (["g1", "g3"], [rep("g1", 0, 8.0), rep("g3", 0, 2.0)],
                          [rep("g1", 0, 9.0), rep("g3", 0, 2.2)], ["g1", "g3"]),
            }
            for name, (wanted, native_rows, cowfs_rows, compared) in cases.items():
                nat = self.arm(d, f"n-{name}.jsonl", "nat", wanted, native_rows)
                cow = self.arm(d, f"c-{name}.jsonl", "cow", wanted, cowfs_rows)
                rc, out, err = self.cli([nat], cow)
                self.assertEqual(rc, 0, (name, err))
                self.assertIn(f"gate coverage  {len(compared)} of 6 compared ({' '.join(compared)})", out)
                self.assertEqual(coverage_line(out)["compared"], compared)
                self.assertIsNone(gap(out, compared[0]))
                tail = out.split("RESULT:")[0].splitlines()
                self.assertEqual([line for line in tail if line.startswith("g5")],
                                 ["g5   not run (no input has g5 reps)"], name)
                self.assertNotIn("not compared:", "\n".join(out.splitlines()[:12]))

    def test_a_meta_with_no_gate_list_is_not_reported_as_requesting_anything(self):
        with tempfile.TemporaryDirectory() as d:
            # The native arm ran g3 but its meta records no gate list, so the report
            # may say the native arm has the data, never that it asked for it.
            nat = Path(d) / "n.jsonl"
            nat.write_text(json.dumps({"kind": "meta", "counts": {"big_bytes": GIB}, "corpus_sha": gates_mod.DEFAULT_SHA}) + "\n"
                           + json.dumps(rep("g1", 0, 8.0)) + "\n" + json.dumps(rep("g3", 0, 2.0)) + "\n")
            cow = self.arm(d, "c.jsonl", "cowfs1", ["g1", "g3"], [rep("g1", 0, 9.0)])
            rc, out, err = self.cli([str(nat)], cow)
            self.assertEqual(rc, 0, err)
            line = gap(out, "g3")
            self.assertIsNotNone(line, out)
            self.assertIn("the cowfs arm has no g3 data", line)
            self.assertIn("native has 1 reps", line)
            self.assertIn("cowfs1 requested it and recorded 0 reps", line)
            g3 = next(g for g in coverage_line(out)["not_compared"] if g["gate"] == "g3")
            self.assertEqual(g3["requested_by"], {"cowfs1": 0})

    def test_an_arm_with_no_gate_list_records_a_gap_with_nobody_named_as_asking(self):
        with tempfile.TemporaryDirectory() as d:
            nat = Path(d) / "n.jsonl"
            nat.write_text(json.dumps({"kind": "meta", "counts": {"big_bytes": GIB}, "corpus_sha": gates_mod.DEFAULT_SHA}) + "\n"
                           + json.dumps(rep("g1", 0, 8.0)) + "\n")
            cow = self.arm(d, "c.jsonl", "cowfs1", ["g1", "g3"], [rep("g1", 0, 9.0)])
            rc, out, err = self.cli([str(nat)], cow)
            self.assertEqual(rc, 0, err)
            line = gap(out, "g3")
            self.assertIsNotNone(line, out)
            self.assertIn("no data in any input", line)
            self.assertIn("cowfs1 requested it and recorded 0 reps", line)
            self.assertIsNone(gap(out, "g2"))
            g3 = next(g for g in coverage_line(out)["not_compared"] if g["gate"] == "g3")
            self.assertEqual(g3["missing_in"], "any input")

    def test_gates_neither_arm_requested_are_not_reported_as_gaps(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g1"], [rep("g1", 0, 8.0)])
            cow = self.arm(d, "c.jsonl", "cow", ["g1"], [rep("g1", 0, 9.0)])
            rc, out, err = self.cli([nat], cow)
            self.assertEqual(rc, 0, err)
            self.assertEqual([line for line in out.splitlines() if "  not compared:" in line], [])


class RefusalsUnchanged(CliCase):
    def test_g5_all_or_nothing_is_still_refused_with_no_coverage_printed(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g5", "g6"], [g5_rep(0), rep("g6", 0, 3.0)],
                           big_bytes=MIB, scale=None)
            cow = self.arm(d, "c.jsonl", "cow", ["g6"], [rep("g6", 0, 3.2)], big_bytes=MIB, scale=None)
            rc, out, err = self.cli([nat], cow)
            self.assertEqual(rc, 3, out)
            self.assertIn("no g5 reps while other inputs have g5", err)
            self.assertNotIn("PASS", out + err)
            self.assertNotIn("gate coverage", out)

    def test_disjoint_gates_are_still_refused_and_print_no_coverage(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g1"], [rep("g1", 0, 8.0)])
            cow = self.arm(d, "c.jsonl", "cow", ["g3"], [rep("g3", 0, 2.0)])
            rc, out, err = self.cli([nat], cow)
            self.assertEqual(rc, 3, out)
            self.assertIn("no gate is present in both", err)
            self.assertNotIn("gate coverage", out + err)

    def test_malformed_input_is_still_refused_and_names_the_file(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g1"], [rep("g1", 0, 8.0)])
            bad = Path(d) / "bad.jsonl"
            bad.write_text("{not json\n" + json.dumps(meta(["g1"])) + "\n" + json.dumps(rep("g1", 0, 8.0)) + "\n")
            rc, out, err = self.cli([str(bad)], nat)
            self.assertEqual(rc, 3, out)
            self.assertIn("INVALID", err)
            self.assertIn("not JSON", err)
            self.assertNotIn("gate coverage", out)
            rc, out, err = self.cli([nat], str(Path(d) / "absent.jsonl"))
            self.assertEqual(rc, 3, out)
            self.assertIn("unreadable", err)


class NoiseFloorCoverage(CliCase):
    def test_a_noise_floor_missing_a_gate_the_native_ran_says_so(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g1", "g3"], [rep("g1", 0, 8.0), rep("g3", 0, 2.0)])
            noise = self.arm(d, "noise.jsonl", "native2", ["g1"], [rep("g1", 0, 8.2)])
            cow = self.arm(d, "c.jsonl", "cow", ["g1", "g3"], [rep("g1", 0, 9.0), rep("g3", 0, 2.2)])
            rc, out, err = self.cli([nat], cow, noise=noise)
            self.assertEqual(rc, 0, err)
            self.assertIn("noise floor has no g3 data, which the native arm ran", out)
            self.assertIsNone(gap(out, "g3"))

    def test_a_noise_floor_covering_every_compared_gate_says_nothing(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g1", "g3"], [rep("g1", 0, 8.0), rep("g3", 0, 2.0)])
            noise = self.arm(d, "noise.jsonl", "native2", ["g1", "g3"], [rep("g1", 0, 8.2), rep("g3", 0, 2.1)])
            cow = self.arm(d, "c.jsonl", "cow", ["g1", "g3"], [rep("g1", 0, 9.0), rep("g3", 0, 2.2)])
            rc, out, err = self.cli([nat], cow, noise=noise)
            self.assertEqual(rc, 0, err)
            self.assertNotIn("noise floor has no", out)


class RealHarnessOutput(CliCase):
    """Arms written by the real gates.py CLI, so the gate list read is the one written."""

    def test_real_gates_output_reports_a_gate_the_cowfs_arm_never_ran(self):
        with tempfile.TemporaryDirectory() as d:
            # gates.py records real load1 and the comparator refuses a ratio above its
            # ceiling, so an ambient load over 30 turns this into an exit-2 run. Pin the
            # harness's own documented hook instead of relaxing the production policy.
            env = dict(os.environ, COWFS_BENCH_SCALE="0.001", COWFS_BENCH_CARGO_HOME=str(Path(d) / "cargo-home"),
                       COWFS_BENCH_FAKE_LOAD1="1")
            arms = {}
            for label, gates in (("native1", "g5,g6"), ("native2", "g5,g6"), ("cowfs1", "g5")):
                root = Path(d) / label
                p = subprocess.run(
                    [sys.executable, str(GATES), "--root", str(root), "--label", f"cov80test-{label}",
                     "--reps", "1", "--gates", gates, "--no-resume"],
                    capture_output=True, text=True, cwd=str(REPO), env=env,
                )
                self.assertEqual(p.returncode, 0, p.stderr[-2000:])
                self.addCleanup(lambda f=Path(p.stdout.strip().splitlines()[-1]): f.unlink(missing_ok=True))
                arms[label] = p.stdout.strip().splitlines()[-1]
            first = [json.loads(line) for line in Path(arms["native1"]).read_text().splitlines()]
            self.assertEqual(first[0]["gates"], ["g5", "g6"])
            rc, out, err = self.cli([arms["native1"], arms["native2"]], arms["cowfs1"], noise=arms["native2"])
            self.assertEqual(rc, 0, err)
            line = gap(out, "g6")
            self.assertIsNotNone(line, out)
            self.assertIn("the cowfs arm has no g6 data", line)
            self.assertIn("cov80test-native1 requested it and recorded 1 reps", line)
            self.assertEqual(coverage_line(out)["compared"], ["g5"])
            self.assertIn("scope: compared 1 of 6 (g5)", out)


class CorpusPin(CliCase):
    """Data from another corpus pin is refused, never compared silently (the g1/g2 re-pin)."""

    def arms(self, d, native_sha=None, cowfs_sha=None, drop_sha=False):
        nat = self.arm(d, "n.jsonl", "nat", ["g1"], [rep("g1", 0, 8.0)], sha=native_sha)
        cow = self.arm(d, "c.jsonl", "cow", ["g1"], [rep("g1", 0, 9.0)], sha=cowfs_sha)
        if drop_sha:
            rows = [json.loads(line) for line in Path(nat).read_text().splitlines()]
            del rows[0]["corpus_sha"]
            Path(nat).write_text("".join(json.dumps(r) + "\n" for r in rows))
        return nat, cow

    def test_current_pin_on_both_arms_compares(self):
        with tempfile.TemporaryDirectory() as d:
            nat, cow = self.arms(d)
            rc, out, err = self.cli([nat], cow)
            self.assertEqual(rc, 0, err)

    def test_old_pin_is_invalid_in_either_arm_and_names_both_shas(self):
        self.assertNotEqual(OLD_PIN, gates_mod.DEFAULT_SHA)
        for kw in ({"native_sha": OLD_PIN}, {"cowfs_sha": OLD_PIN}, {"native_sha": OLD_PIN, "cowfs_sha": OLD_PIN}):
            with self.subTest(kw=kw), tempfile.TemporaryDirectory() as d:
                nat, cow = self.arms(d, **kw)
                rc, out, err = self.cli([nat], cow)
                self.assertEqual(rc, 3, out)
                self.assertIn("INVALID", err)
                self.assertIn(OLD_PIN, err)
                self.assertIn(gates_mod.DEFAULT_SHA, err)
                self.assertNotIn("RESULT: PASS", out)

    def test_meta_without_a_corpus_sha_is_invalid(self):
        with tempfile.TemporaryDirectory() as d:
            nat, cow = self.arms(d, drop_sha=True)
            rc, out, err = self.cli([nat], cow)
            self.assertEqual(rc, 3, out)
            self.assertIn("corpus_sha", err)


class G2Unmeasurable(CliCase):
    """Issue #221: g2 is never a PASS or FAIL when it is too short, too noisy or too thin to test its bar."""

    def run_g2(self, native, cowfs):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g2"], [rep("g2", i, w) for i, w in enumerate(native)])
            cow = self.arm(d, "c.jsonl", "cow", ["g2"], [rep("g2", i, w) for i, w in enumerate(cowfs)])
            return self.cli([nat], cow)

    def assertUnmeasurable(self, native, cowfs, needle):
        rc, out, err = self.run_g2(native, cowfs)
        self.assertEqual(rc, 2, out + err)
        self.assertIn("UNMEASURABLE", out)
        self.assertIn(needle, out)
        self.assertIn("quiet host", out)
        self.assertIn("heavier edit", out)
        self.assertNotIn("PASS (", out)
        self.assertNotIn("FAIL (", out)

    def test_short_native_work_is_never_a_pass(self):
        # The 1.4 s regime of PR 216: an equal cowfs arm would otherwise PASS.
        self.assertUnmeasurable([1.4, 1.5, 1.4], [1.5, 1.5, 1.6], "floor")

    def test_short_native_work_is_never_a_fail_either(self):
        self.assertUnmeasurable([1.0, 1.1, 1.0], [9.0, 9.5, 9.2], "floor")

    def test_wide_native_spread_is_unmeasurable(self):
        self.assertUnmeasurable([4.0, 4.1, 9.0, 4.2, 4.0], [4.2, 4.3, 4.2], "native reps span")

    def test_wide_cowfs_spread_is_unmeasurable(self):
        self.assertUnmeasurable([4.0, 4.1, 4.2], [4.0, 9.5, 4.3], "cowfs reps span")

    def test_too_few_reps_is_unmeasurable(self):
        self.assertUnmeasurable([5.0, 5.1], [5.0, 5.1], "need 3")

    def test_long_steady_work_is_still_judged(self):
        # Ranges well inside the budget on both platforms (issue #232): every median in them gives the same answer.
        rc, out, err = self.run_g2([5.0, 5.1, 5.2], [5.1, 5.2, 5.3])
        self.assertEqual(rc, 0, out + err)
        self.assertIn("PASS (", out)
        rc, out, err = self.run_g2([4.4, 4.9, 5.2], [12.0, 12.1, 12.2])
        self.assertEqual(rc, 1, out + err)
        self.assertIn("FAIL (", out)

    def test_ranges_that_straddle_the_bar_are_unmeasurable_not_judged_on_medians(self):
        # Medians 5.1 and 5.5 look fine on both platforms, but the ranges allow a 3.0 s add (macOS) and 8.0 vs 7.5 s (Linux).
        self.assertUnmeasurable([5.0, 5.1, 5.2], [4.8, 5.5, 8.0], "could")

    def test_other_gates_are_not_subject_to_the_g2_rule(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g1"], [rep("g1", 0, 0.5)])
            cow = self.arm(d, "c.jsonl", "cow", ["g1"], [rep("g1", 0, 0.6)])
            rc, out, err = self.cli([nat], cow)
            self.assertEqual(rc, 0, out + err)

    def test_parameters_are_the_stated_ones(self):
        self.assertEqual((compare.G2_NATIVE_FLOOR_S, compare.G2_SPREAD_MAX, compare.G2_MIN_REPS), (3.0, 2.0, 3))


class G2NoFlip(unittest.TestCase):
    """Issue #232: a g2 verdict needs every median inside the observed [min, max] ranges to agree."""

    def d(self, nat, cow, mac):
        rows = lambda ws: [{"wall_s": w} for w in ws]
        return compare.g2_decision(rows(nat), rows(cow), mac, 1.0)

    def test_macos_budget(self):
        self.assertEqual(self.d([5.0, 5.1, 5.2], [5.1, 5.2, 5.3], True), "PASS")
        self.assertEqual(self.d([5.0, 5.1, 5.2], [6.3, 6.4, 6.5], True), "FAIL")   # min_cow - max_nat = 1.1 >= 1
        self.assertIsNone(self.d([5.0, 5.1, 5.2], [5.5, 6.0, 6.5], True))         # 0.3 < 1 but 1.5 >= 1
        self.assertEqual(self.d([5.0, 5.0, 5.0], [6.0, 6.0, 6.0], True), "FAIL")  # exactly the budget fails, as verdict() did
        self.assertEqual(self.d([5.0, 5.0, 5.0], [5.9, 5.9, 5.9], True), "PASS")

    def test_linux_ratio(self):
        self.assertEqual(self.d([5.0, 5.1, 5.2], [5.1, 5.2, 5.3], False), "PASS")
        self.assertEqual(self.d([5.0, 5.1, 5.2], [10.0, 10.1, 10.2], False), "FAIL")  # 10.0 > 1.5 * 5.2
        self.assertIsNone(self.d([5.0, 5.1, 5.2], [7.0, 7.9, 8.0], False))           # 8.0 > 7.5 but 7.0 <= 7.8
        self.assertEqual(self.d([5.0, 5.0, 5.0], [7.5, 7.5, 7.5], False), "PASS")     # 1.5x exactly passes
        self.assertEqual(self.d([5.0, 5.0, 5.0], [7.6, 7.6, 7.6], False), "FAIL")

    def test_a_twelve_second_range_on_a_23_second_workload_only_decides_far_results(self):
        nat = [17.0, 23.0, 29.0]
        self.assertIsNone(self.d(nat, [18.0, 24.0, 30.0], True))
        self.assertIsNone(self.d(nat, [18.0, 24.0, 30.0], False))
        self.assertEqual(self.d(nat, [31.0, 32.0, 33.0], True), "FAIL")
        self.assertEqual(self.d(nat, [45.0, 46.0, 47.0], False), "FAIL")


class G2PinnedToVerdict(unittest.TestCase):
    """g2_decision restates the two verdict() formulas; verdict() is monotone in each median, so the four min/max corners decide."""

    @staticmethod
    def corners(nat, cow, mac, budget):
        (nlo, nhi), (clo, chi) = compare.spread(nat), compare.spread(cow)
        seen = {compare.verdict("g2", n, c, 0, 0, mac, budget)[0] for n in (nlo, nhi) for c in (clo, chi)}
        return seen.pop() if len(seen) == 1 else None

    def test_decision_equals_the_four_corners_seeded_fuzz(self):
        rnd = random.Random(265)
        decided = 0
        for _ in range(20000):
            base, budget, mac = rnd.choice([3.0, 5.0, 23.0, 40.0]), rnd.choice([0.5, 1.0, 2.0]), rnd.random() < 0.5
            nat = [{"wall_s": base * rnd.uniform(0.8, 1.6)} for _ in range(rnd.randint(1, 6))]
            cow = [{"wall_s": base * rnd.uniform(0.8, 3.0) + rnd.choice([0.0, 0.5, 1.0])} for _ in range(rnd.randint(1, 6))]
            got = compare.g2_decision(nat, cow, mac, budget)
            self.assertEqual(got, self.corners(nat, cow, mac, budget), (nat, cow, mac, budget))
            decided += got is not None
            for _ in range(3):  # and a median anywhere inside the ranges gives the decided answer
                if got:
                    n, c = rnd.uniform(*compare.spread(nat)), rnd.uniform(*compare.spread(cow))
                    self.assertEqual(compare.verdict("g2", n, c, 0, 0, mac, budget)[0], got)
        self.assertTrue(2000 < decided < 18000, decided)  # the fuzz exercises both outcomes

    def test_the_boundaries_are_the_same_inequalities(self):
        r = lambda *w: [{"wall_s": x} for x in w]
        for mac, cow, want in ((True, 6.0, "FAIL"), (True, 5.99, "PASS"), (False, 7.5, "PASS"), (False, 7.51, "FAIL")):
            self.assertEqual(compare.g2_decision(r(5.0, 5.0), r(cow, cow), mac, 1.0), want)
            self.assertEqual(compare.verdict("g2", 5.0, cow, 0, 0, mac, 1.0)[0], want)

    def test_exact_equality_on_the_range_edges(self):
        r = lambda *w: [{"wall_s": x} for x in w]
        # macOS: max_cow - min_nat == budget exactly is not a PASS (PASS is strictly under); min_nat 4.0, max_cow 5.0
        self.assertIsNone(compare.g2_decision(r(4.0, 5.0), r(4.5, 5.0), True, 1.0))
        # Linux: min_cow == 1.5 * max_nat exactly is not a FAIL (FAIL is strictly over); max_nat 4.0, min_cow 6.0
        self.assertIsNone(compare.g2_decision(r(2.0, 4.0), r(6.0, 9.0), False, 1.0))


class G2MeasuringPlatform(CliCase):
    """The g2 rule follows where the reps were measured, not where compare.py runs."""

    NAT, COW = [5.0, 5.1, 5.2], [6.3, 6.4, 6.5]  # add 1.1 s: FAIL under the macOS budget, PASS under the 1.5x Linux ratio

    def run_g2(self, **kw):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g2"], [rep("g2", i, w) for i, w in enumerate(self.NAT)], **kw)
            cow = self.arm(d, "c.jsonl", "cow", ["g2"], [rep("g2", i, w) for i, w in enumerate(self.COW)], **kw)
            return self.cli([nat], cow)

    def test_macos_data_gets_the_budget_on_any_host(self):
        rc, out, err = self.run_g2(platform="macOS-14.5-arm64", measured_on="macos")
        self.assertEqual(rc, 1, out + err)
        self.assertIn("measured on   macos", out)

    def test_linux_data_gets_the_ratio_on_any_host(self):
        rc, out, err = self.run_g2(platform="Linux-6.8", measured_on="linux")
        self.assertEqual(rc, 0, out + err)
        self.assertIn("measured on   linux", out)

    def test_measured_on_disagreeing_with_the_platform_string_is_invalid(self):
        for kw in ({"platform": "Darwin", "measured_on": "linux"}, {"platform": "Linux-6.8", "measured_on": "macos"}):
            rc, out, err = self.run_g2(**kw)
            self.assertEqual(rc, 3, (kw, out, err))
            self.assertIn("disagrees", err)
        self.assertEqual(self.run_g2(platform="Plan9", measured_on="linux")[0], 0)  # an unreadable legacy string cannot contradict

    def test_data_without_measured_on_falls_back_to_the_platform_string(self):
        self.assertEqual(self.run_g2(platform="Darwin")[0], 1)
        self.assertEqual(self.run_g2(platform="macOS-14.5-arm64")[0], 1)
        self.assertEqual(self.run_g2(platform="Linux-6.8.0-x86_64")[0], 0)

    def test_unknown_platform_is_invalid_not_guessed(self):
        for kw in ({"platform": None}, {"platform": "Plan9"}, {"platform": "Darwin", "measured_on": "windows"}):
            rc, out, err = self.run_g2(**kw)
            self.assertEqual(rc, 3, (kw, out, err))
            self.assertIn("measuring platform", err)

    def test_arms_measured_on_different_platforms_are_invalid(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g2"], [rep("g2", i, w) for i, w in enumerate(self.NAT)], platform="Darwin", measured_on="macos")
            cow = self.arm(d, "c.jsonl", "cow", ["g2"], [rep("g2", i, w) for i, w in enumerate(self.COW)], platform="Linux-6.8", measured_on="linux")
            rc, out, err = self.cli([nat], cow)
        self.assertEqual(rc, 3, out + err)
        self.assertIn("different platforms", err)

    def test_gates_without_g2_need_no_platform(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g1"], [rep("g1", 0, 1.0)], platform=None)
            cow = self.arm(d, "c.jsonl", "cow", ["g1"], [rep("g1", 0, 1.1)], platform=None)
            self.assertEqual(self.cli([nat], cow)[0], 0)

    def test_gates_py_writes_measured_on(self):
        want = {"darwin": "macos", "linux": "linux"}.get(sys.platform, sys.platform)
        with tempfile.TemporaryDirectory() as d:
            m = gates_mod.meta(Path(d), "x", 1, ["g1"], {"big_bytes": GIB})
        self.assertEqual(m["measured_on"], want)


class ExitPrecedence(CliCase):
    """Issue #232: INVALID(3) before any verdict, then FAIL(1) > UNMEASURABLE(2) > PASS(0)."""

    def two_gates(self, g1_cow, g2_native, g2_cow):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g1", "g2"],
                           [rep("g1", i, 1.0) for i in range(3)] + [rep("g2", i, w) for i, w in enumerate(g2_native)])
            cow = self.arm(d, "c.jsonl", "cow", ["g1", "g2"],
                           [rep("g1", i, g1_cow) for i in range(3)] + [rep("g2", i, w) for i, w in enumerate(g2_cow)])
            return self.cli([nat], cow)

    def test_fail_beats_unmeasurable_and_the_result_line_shows_both(self):
        rc, out, err = self.two_gates(5.0, [1.0, 1.1, 1.0], [1.0, 1.1, 1.0])  # g1 5x FAIL, g2 under the floor
        self.assertEqual(rc, 1, out + err)
        self.assertIn("RESULT: FAIL (1), 1 unmeasurable", out)

    def test_unmeasurable_alone_still_exits_two(self):
        rc, out, err = self.two_gates(1.0, [1.0, 1.1, 1.0], [1.0, 1.1, 1.0])
        self.assertEqual(rc, 2, out + err)
        self.assertIn("RESULT: 1 gate(s) unmeasurable, 0 failed", out)

    def test_invalid_beats_a_fail(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.arm(d, "n.jsonl", "nat", ["g1"], [rep("g1", i, 1.0) for i in range(3)])
            cow = self.arm(d, "c.jsonl", "cow", ["g1", "g2"],
                           [rep("g1", i, 5.0) for i in range(3)] + [{**rep("g2", 0, 9.0), "metrics": {"rebuilt_count": 100, "bins_relinked": 3}}])
            rc, out, err = self.cli([nat], cow)
            self.assertEqual(rc, 3, out + err)
            self.assertIn("rebuilt", err)
            self.assertNotIn("RESULT: FAIL", out)


if __name__ == "__main__":
    unittest.main()
