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
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
sys.path.insert(0, str(HERE))
import gates as gates_mod  # noqa: E402

COMPARE = HERE / "compare.py"
GATES = HERE / "gates.py"
MIB = 1 << 20
GIB = 1 << 30
ALL = ["g1", "g2", "g3", "g4", "g5", "g6"]


OLD_PIN = "c1619ec16df3a6b11dd5a1e08e8a512b4fedd240"


def meta(gates, big_bytes=GIB, label="arm", scale=100, sha=None):
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
        "platform": "Darwin",
        "python": "3.12.2",
        "started": 0.0,
    }
    if scale is not None:
        row["scale"] = scale
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
        "metrics": {"rebuilt_count": 5, "bins_relinked": 2} if gate == "g2" else {},
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
        rows = {"g1": [rep("g1", 0, 8.0)], "g2": [rep("g2", 0, 4.0)], "g3": [rep("g3", 0, 2.0)]}
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


if __name__ == "__main__":
    unittest.main()
