"""g5 byte-count regression tests (issue #55). Run: python3 -m unittest discover -s bench"""

import contextlib
import importlib.util
import io
import subprocess
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent


def load(name):
    spec = importlib.util.spec_from_file_location(name, HERE / f"{name}.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


gates = load("gates")
compare = load("compare")
MIB = 1 << 20


def counts_at(scale):
    with mock.patch.dict(os.environ, {"COWFS_BENCH_SCALE": str(scale)}):
        return gates.counts()


class CountsAlignment(unittest.TestCase):
    def test_every_scale_is_a_whole_number_of_units(self):
        for scale in (0.001, 0.1, 1, 2, 3.3, 7, 12.4, 12.5, 12.6, 33, 50, 99.99, 100):
            n = counts_at(scale)
            for key, unit in gates.UNIT.items():
                self.assertEqual(n[key] % unit, 0, (scale, key, n[key]))
                self.assertGreaterEqual(n[key], MIB, (scale, key))
                self.assertLessEqual(n[key], max(unit, gates.FULL[key]), (scale, key))

    def test_below_at_above_one_unit(self):
        # big_bytes: 1 GiB * scale / 100 straddles 1 MiB at scale 0.09765625
        self.assertEqual(counts_at(0.05)["big_bytes"], MIB)
        self.assertEqual(counts_at(0.09765625)["big_bytes"], MIB)
        self.assertEqual(counts_at(0.1)["big_bytes"], MIB)
        self.assertEqual(counts_at(0.19)["big_bytes"], MIB)
        self.assertEqual(counts_at(0.2)["big_bytes"], 2 * MIB)

    def test_issue_55_scale_2(self):
        # 1 GiB * 2% = 21474836.48 used to give 21474836, which no whole-MiB write reaches
        self.assertEqual(counts_at(2)["big_bytes"], 20 * MIB)

    def test_original_one_mib_floor_kept_for_both_byte_keys(self):
        for scale in (0, 1e-9, 0.001, 2):
            self.assertEqual(counts_at(scale)["large_bytes"], MIB, scale)
        self.assertEqual(counts_at(0.001)["big_bytes"], MIB)

    def test_large_bytes_rounds_where_the_old_floor_hid_it(self):
        # 8 MiB * 13% = 1090519.04; the old code asked for 1090519, the writer makes whole 64 KiB chunks
        n = counts_at(13)
        self.assertEqual(n["large_bytes"], 16 * (1 << 16))
        self.assertEqual(n["large_bytes"] % (1 << 16), 0)

    def test_full_scale_unchanged(self):
        n = counts_at(100)
        self.assertEqual(n["big_bytes"], 1 << 30)
        self.assertEqual(n["large_bytes"], 8 << 20)


class G5(unittest.TestCase):
    def run_g5(self, big_bytes, hook=None):
        with tempfile.TemporaryDirectory() as d:
            ctx = gates.Ctx(Path(d), {"big_bytes": big_bytes})
            if hook is None:
                return ctx.g5()
            with hook(Path(d)):
                return ctx.g5()

    def test_positive_at_aligned_sizes(self):
        for size in (MIB, 2 * MIB, 20 * MIB, 20 * MIB + MIB):
            m = self.run_g5(size)
            self.assertTrue(m["read_matches"], size)
            self.assertEqual((m["bytes"], m["written_bytes"], m["read_bytes"]), (size, size, size))

    def test_positive_for_every_scale_that_counts_can_emit(self):
        for scale in (0.05, 0.1, 0.2, 0.3):
            size = counts_at(scale)["big_bytes"]
            m = self.run_g5(size)
            self.assertTrue(m["read_matches"], (scale, size))

    def test_unaligned_count_is_refused_not_recorded(self):
        # the pre-fix bug: expected 21474836, file is 20 MiB. The gate must fail loudly.
        for size in (MIB - 1, 2 * MIB + 1, 21474836):
            with self.assertRaises(SystemExit) as cm:
                self.run_g5(size)
            self.assertIn("counts() says", str(cm.exception))

    def test_negative_control_truncated_file_is_refused(self):
        real_fsync = os.fsync

        @contextlib.contextmanager
        def corrupt(root):
            def fsync_then_truncate(fd):
                real_fsync(fd)
                os.truncate(root / "big" / "seq.bin", 3 * MIB - 1)  # lose bytes after the write is checked
            with mock.patch.object(gates.os, "fsync", fsync_then_truncate):
                yield

        with self.assertRaises(SystemExit):
            # truncated before the size check: caught as a write mismatch
            self.run_g5(4 * MIB, corrupt)

    def short_read_hook(self):
        real_open = open

        class ShortFile:
            def __init__(self, fh):
                self.fh = fh
                self.left = os.fstat(fh.fileno()).st_size - 1

            def __enter__(self):
                return self

            def __exit__(self, *a):
                self.fh.close()

            def read(self, n):
                b = self.fh.read(min(n, self.left))
                self.left -= len(b)
                return b

        def evil_open(path, mode="r", *a, **k):
            fh = real_open(path, mode, *a, **k)
            return ShortFile(fh) if "r" in mode and "b" in mode else fh

        @contextlib.contextmanager
        def short(_root):
            with mock.patch("builtins.open", evil_open):
                yield

        return short

    def test_negative_control_short_read_is_refused_not_published(self):
        with self.assertRaises(SystemExit) as cm:
            self.run_g5(4 * MIB, self.short_read_hook())
        self.assertIn("invalid run", str(cm.exception))

    def test_refusal_removes_only_its_own_file(self):
        hooks = {"short": self.short_read_hook(), "unaligned": None}
        for name, hook in hooks.items():
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                (root / "big").mkdir()
                bystander = root / "big" / "keep.bin"
                bystander.write_bytes(b"x")
                ctx = gates.Ctx(root, {"big_bytes": 4 * MIB if hook else 4 * MIB + 1})
                with self.assertRaises(SystemExit):
                    if hook:
                        with hook(root):
                            ctx.g5()
                    else:
                        ctx.g5()
                self.assertFalse((root / "big" / "seq.bin").exists(), name)
                self.assertTrue(bystander.exists(), name)

    def test_success_removes_its_file(self):
        with tempfile.TemporaryDirectory() as d:
            gates.Ctx(Path(d), {"big_bytes": MIB}).g5()
            self.assertEqual(list((Path(d) / "big").iterdir()), [])

    def test_main_refuses_short_read_and_records_no_rep(self):
        with tempfile.TemporaryDirectory() as d:
            root, out = Path(d) / "root", Path(d) / "out"
            argv = ["gates.py", "--root", str(root), "--label", "t", "--reps", "1", "--gates", "g5", "--no-resume"]
            with mock.patch.object(sys, "argv", argv), mock.patch.dict(os.environ, {"COWFS_BENCH_SCALE": "0.2"}), \
                    mock.patch.object(gates, "OUT", out), contextlib.redirect_stdout(io.StringIO()):
                with self.short_read_hook()(root), self.assertRaises(SystemExit):
                    gates.main()
            rows = [json.loads(line) for f in out.glob("t-*.jsonl") for line in f.read_text().splitlines()]
            self.assertEqual([r for r in rows if r["kind"] == "rep"], [])
            self.assertFalse((root / "big" / "seq.bin").exists())

    def test_main_entrypoint_at_scale_2(self):
        with tempfile.TemporaryDirectory() as d:
            root, out = Path(d) / "root", Path(d) / "out"
            argv = ["gates.py", "--root", str(root), "--label", "t", "--reps", "1", "--gates", "g5", "--no-resume"]
            env = {"COWFS_BENCH_SCALE": "2"}
            with mock.patch.object(sys, "argv", argv), mock.patch.dict(os.environ, env), \
                    mock.patch.object(gates, "OUT", out), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(gates.main(), 0)
            rows = [json.loads(line) for f in out.glob("t-*.jsonl") for line in f.read_text().splitlines()]
            rep = [r for r in rows if r["kind"] == "rep"][0]["metrics"]
            self.assertEqual(rep["bytes"], 20 * MIB)
            self.assertTrue(rep["read_matches"])
            self.assertEqual(rows[0]["counts"]["big_bytes"], 20 * MIB)


class TreeSizes(unittest.TestCase):
    def make(self, d):
        n = {**counts_at(13), "small_files": 256, "large_files": 2}
        self.assertEqual(n["large_bytes"] % (1 << 16), 0)
        ctx = gates.Ctx(Path(d), n)
        ctx.ensure_tree()
        return ctx, n

    def test_large_files_have_nominal_size_where_baseline_rounding_shows(self):
        with tempfile.TemporaryDirectory() as d:
            ctx, n = self.make(d)
            for f in (Path(d) / "tree" / "big").iterdir():
                self.assertEqual(f.stat().st_size, n["large_bytes"])

    def test_marker_hit_accepts_an_intact_tree(self):
        with tempfile.TemporaryDirectory() as d:
            ctx, _ = self.make(d)
            ctx.ensure_tree()

    def test_marker_hit_refuses_truncated_large_file(self):
        with tempfile.TemporaryDirectory() as d:
            ctx, _ = self.make(d)
            os.truncate(Path(d) / "tree" / "big" / "b001", 5)
            with self.assertRaises(SystemExit) as cm:
                ctx.ensure_tree()
            self.assertIn("tree big", str(cm.exception))

    def test_marker_hit_refuses_stray_entries(self):
        for rel, kind in (("tree/big/b099", "file"), ("tree/stray.txt", "file"), ("tree/d300", "dir"),
                          ("tree/d002/extra", "file"), ("tree/d002/sub", "dir"), ("tree/big/sub", "dir")):
            with tempfile.TemporaryDirectory() as d:
                ctx, _ = self.make(d)
                target = Path(d) / rel
                target.mkdir() if kind == "dir" else target.write_bytes(b"x")
                with self.assertRaises(SystemExit, msg=rel):
                    ctx.ensure_tree()

    def test_marker_hit_refuses_missing_large_file_and_missing_dir(self):
        with tempfile.TemporaryDirectory() as d:
            ctx, _ = self.make(d)
            (Path(d) / "tree" / "big" / "b001").unlink()
            with self.assertRaises(SystemExit):
                ctx.ensure_tree()
        with tempfile.TemporaryDirectory() as d:
            ctx, _ = self.make(d)
            for f in (Path(d) / "tree" / "d010").iterdir():
                f.unlink()
            (Path(d) / "tree" / "d010").rmdir()
            with self.assertRaises(SystemExit):
                ctx.ensure_tree()

    def test_marker_hit_refuses_truncated_or_missing_small_file(self):
        with tempfile.TemporaryDirectory() as d:
            ctx, _ = self.make(d)
            victim = next((Path(d) / "tree" / "d007").iterdir())
            os.truncate(victim, 10)
            with self.assertRaises(SystemExit):
                ctx.ensure_tree()
            victim.unlink()
            with self.assertRaises(SystemExit):
                ctx.ensure_tree()


GIB = 1 << 30


def meta(big=4 * MIB, **extra):
    return {"kind": "meta", "counts": {"big_bytes": big}, "corpus_sha": gates.DEFAULT_SHA, **extra}


def rep(size=4 * MIB, gate="g5", **override):
    m = {"bytes": size, "written_bytes": size, "read_bytes": size, "read_matches": True}
    m.update(override)
    return {"kind": "rep", "gate": gate, "rep": 0, "label": "x", "wall_s": 1, "load1_before": 0, "load1_after": 0, "metrics": m}


def drop(row, key):
    del row["metrics"][key]
    return row


def g1(size=GIB, wall=2.0, gate="g1", **override):
    row = {**rep(size), "gate": gate, "wall_s": wall, "metrics": {}}
    row.update(override)
    return row


G1 = [meta(GIB, scale=100), g1()]


# name -> rows that compare.py must refuse in any input slot
INVALID = {
    "legacy boolean only, aligned 1 GiB": [meta(GIB), rep(GIB, written_bytes=None, read_bytes=None)],
    "legacy boolean only without byte fields": [meta(GIB), drop(drop(rep(GIB), "written_bytes"), "read_bytes")],
    "no meta record": [rep()],
    "rep before meta": [rep(), meta()],
    "duplicate meta": [meta(), meta(), rep()],
    "conflicting meta": [meta(4 * MIB), meta(8 * MIB), rep()],
    "meta without counts": [{"kind": "meta"}, rep()],
    "counts without big_bytes": [{"kind": "meta", "counts": {}}, rep()],
    "meta big_bytes bool": [meta(True), rep(1)],
    "meta big_bytes float": [meta(4.0 * MIB), rep()],
    "meta big_bytes zero": [meta(0), rep(0)],
    "meta big_bytes negative": [meta(-MIB), rep(-MIB)],
    "meta big_bytes below 1 MiB": [meta(1 << 16), rep(1 << 16)],
    "meta big_bytes unaligned": [meta(MIB + 1), rep(MIB + 1)],
    "expected is bool": [meta(1), rep(True, written_bytes=True, read_bytes=True)],
    "expected float": [meta(), rep(4.0 * MIB)],
    "expected zero": [meta(), rep(0)],
    "expected negative": [meta(), rep(-4 * MIB)],
    "written missing": [meta(), drop(rep(), "written_bytes")],
    "read missing": [meta(), drop(rep(), "read_bytes")],
    "written string": [meta(), rep(written_bytes=str(4 * MIB))],
    "forged flag, 5 written 7 read": [meta(), rep(written_bytes=5, read_bytes=7)],
    "one byte short read, flag true": [meta(), rep(read_bytes=4 * MIB - 1)],
    "forged aligned 3 MiB under 4 MiB meta": [meta(4 * MIB), rep(3 * MIB)],
    "all counts equal but differ from meta": [meta(4 * MIB), rep(8 * MIB)],
    "3 MiB meta at scale 100": [meta(3 * MIB, scale=100), rep(3 * MIB)],
    "scale is a string": [meta(GIB, scale="100"), rep(GIB)],
    "scale is bool": [meta(GIB, scale=True), rep(GIB)],
    "scale is infinite": [meta(GIB, scale=float("inf")), rep(GIB)],
    "empty file": [],
    "meta only": [meta()],
    "unknown gate g9": [meta(), g1(gate="g9")],
    "gate missing": [meta(), {k: v for k, v in g1().items() if k != "gate"}],
    "rep index missing": [meta(), {k: v for k, v in g1().items() if k != "rep"}],
    "rep index string": [meta(), {**g1(), "rep": "0"}],
    "rep index bool": [meta(), {**g1(), "rep": True}],
    "wall_s bool": [meta(), g1(wall=True)],
    "wall_s string": [meta(), g1(wall="2.0")],
    "wall_s Infinity": [meta(), g1(wall=float("inf"))],
    "wall_s negative": [meta(), g1(wall=-1.0)],
    "wall_s negative small": [meta(), g1(wall=-0.5)],
    "g5 wall_s negative": [meta(), {**rep(), "wall_s": -0.5}],
    "load1_before -Infinity": [meta(), {**g1(), "load1_before": float("-inf")}],
    "load1_before negative": [meta(), {**g1(), "load1_before": -5.0}],
    "load1_after negative": [meta(), {**g1(), "load1_after": -0.5}],
    "load1 untrusted large int overflows float": [meta(), {**g1(), "load1_before": 10 ** 400}],
    "wall_s missing": [meta(), {k: v for k, v in g1().items() if k != "wall_s"}],
    "load1_before missing": [meta(), {k: v for k, v in g1().items() if k != "load1_before"}],
    "load1_before string": [meta(), {**g1(), "load1_before": "0"}],
    "load1_after bool": [meta(), {**g1(), "load1_after": True}],
    "load1_after Infinity": [meta(), {**g1(), "load1_after": float("inf")}],
    "g5 rep with no metrics": [meta(), {"kind": "rep", "gate": "g5", "rep": 0}],
    "scale 1e308 overflow": [meta(4 * MIB, scale=1e308), rep()],
    "json list as a record": [meta(), [1, 2, 3], rep()],
    "rep missing wall_s": [meta(), {k: v for k, v in rep().items() if k != "wall_s"}],
    "g5 rep wall_s not a number": [meta(), {**rep(), "wall_s": "1.0"}],
    "g5 rep wall_s bool": [meta(), {**rep(), "wall_s": True}],
}
VALID = {
    "modern without scale": [meta(), rep()],
    "modern with matching scale 100": [meta(GIB, scale=100), rep(GIB)],
    "modern 3 MiB at scale 0.3": [meta(3 * MIB, scale=0.3), rep(3 * MIB)],
    "modern 20 MiB at scale 2": [meta(20 * MIB, scale=2.0), rep(20 * MIB)],
    "flag false but all numbers agree": [meta(), rep(read_matches=False)],
    "floor 1 MiB at tiny scale": [meta(MIB, scale=0.001), rep(MIB)],
}


class CompareRefuses(unittest.TestCase):
    def write(self, d, name, rows):
        f = Path(d) / name
        f.write_text("".join(json.dumps(r) + "\n" for r in rows))
        return str(f)

    def run_compare(self, native, cowfs, noise=None):
        argv = ["compare.py", "--native", *native, "--cowfs", cowfs] + (["--noise-floor", noise] if noise else [])
        with mock.patch.object(sys, "argv", argv), contextlib.redirect_stderr(io.StringIO()) as err, \
                contextlib.redirect_stdout(io.StringIO()) as out:
            return compare.main(), err.getvalue(), out.getvalue()

    def slots(self, ok, ok2, test):
        return {
            "native": ([test], ok, ok2),
            "second native": ([ok, test], ok, ok2),
            "cowfs": ([ok], test, ok2),
            "noise floor": ([ok], ok, test),
        }

    def test_valid_modern_files_pass_in_every_slot(self):
        with tempfile.TemporaryDirectory() as d:
            for name, rows in VALID.items():
                f = self.write(d, "v.jsonl", rows)
                ok = self.write(d, "ok.jsonl", [meta(), rep()])
                for slot, (nat, cow, noise) in self.slots(ok, ok, f).items():
                    rc, err, _ = self.run_compare(nat, cow, noise)
                    self.assertEqual(rc, 0, (name, slot, err))

    def test_invalid_matrix_is_refused_in_every_slot_with_no_verdict(self):
        with tempfile.TemporaryDirectory() as d:
            ok = self.write(d, "ok.jsonl", [meta(), rep()])
            ok2 = self.write(d, "ok2.jsonl", [meta(), rep()])
            self.assertEqual(self.run_compare([ok], ok, ok2)[0], 0)
            for name, rows in INVALID.items():
                bad = self.write(d, "bad.jsonl", rows)
                for slot, (nat, cow, noise) in self.slots(ok, ok2, bad).items():
                    rc, err, out = self.run_compare(nat, cow, noise)
                    self.assertEqual(rc, 3, (name, slot))
                    self.assertIn("INVALID", err, (name, slot))
                    self.assertIn(bad, err, (name, slot))
                    self.assertNotIn("PASS", out + err, (name, slot))

    def test_not_json_and_missing_file_are_invalid_not_a_traceback(self):
        with tempfile.TemporaryDirectory() as d:
            ok = self.write(d, "ok.jsonl", [meta(), rep()])
            junk = Path(d) / "junk.jsonl"
            junk.write_text("{not json\n" + json.dumps(meta()) + "\n" + json.dumps(rep()) + "\n")
            self.assertEqual(self.run_compare([ok], str(junk))[0], 3)
            self.assertEqual(self.run_compare([ok], str(Path(d) / "absent.jsonl"))[0], 3)

    def test_g1_only_comparison_still_runs(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.write(d, "nat.jsonl", G1)
            cow = self.write(d, "cow.jsonl", [G1[0], {**G1[1], "wall_s": 2.4}])
            rc, err, out = self.run_compare([nat], cow)
            self.assertEqual(rc, 0, err)
            self.assertIn("g1", out)
            tail = out.split("RESULT:")[0].splitlines()
            self.assertEqual([line for line in tail if line.startswith("g5")], ["g5   not run (no input has g5 reps)"])

    def test_g1_g3_only_comparison_still_runs(self):
        g3 = {**rep(GIB), "gate": "g3", "wall_s": 3.0}
        with tempfile.TemporaryDirectory() as d:
            nat = self.write(d, "nat.jsonl", G1 + [g3])
            cow = self.write(d, "cow.jsonl", [G1[0], {**G1[1], "wall_s": 2.4}, {**g3, "wall_s": 3.3}])
            rc, err, out = self.run_compare([nat], cow)
            self.assertEqual(rc, 0, err)
            self.assertIn("g1", out)
            self.assertIn("g3", out)
            tail = out.split("RESULT:")[0].splitlines()
            self.assertEqual([line for line in tail if line.startswith("g5")], ["g5   not run (no input has g5 reps)"])

    def test_g5_in_some_inputs_only_is_refused(self):
        with tempfile.TemporaryDirectory() as d:
            g1f = self.write(d, "g1.jsonl", G1)
            g5f = self.write(d, "g5.jsonl", [meta(), rep()])
            for slot, (nat, cow, noise) in {
                "native lacks g5": ([g1f], g5f, None),
                "cowfs lacks g5": ([g5f], g1f, None),
                "noise floor lacks g5": ([g5f], g5f, g1f),
                "second native lacks g5": ([g5f, g1f], g5f, None),
            }.items():
                rc, err, out = self.run_compare(nat, cow, noise)
                self.assertEqual(rc, 3, (slot, out))
                self.assertIn("no g5 reps while other inputs have g5", err, slot)
                self.assertNotIn("PASS", out + err, slot)

    def test_both_arms_meta_only_is_invalid_not_a_fail(self):
        with tempfile.TemporaryDirectory() as d:
            f = self.write(d, "mo.jsonl", [meta()])
            rc, err, out = self.run_compare([f], f)
            self.assertEqual(rc, 3, out)
            self.assertIn("no rep records", err)

    def test_disjoint_gates_are_invalid_not_a_zero_gate_pass(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.write(d, "nat.jsonl", [meta(), g1()])
            cow = self.write(d, "cow.jsonl", [meta(), g1(gate="g3", wall=2.4)])
            rc, err, out = self.run_compare([nat], cow)
            self.assertEqual(rc, 3, out)
            self.assertIn("no gate is present in both", err)
            self.assertNotIn("PASS", out + err)

    def test_unknown_gate_only_is_invalid_not_a_pass(self):
        with tempfile.TemporaryDirectory() as d:
            f = self.write(d, "g9.jsonl", [meta(), g1(gate="g9")])
            rc, err, out = self.run_compare([f], f)
            self.assertEqual(rc, 3, out)
            self.assertIn("not one of", err)
            self.assertNotIn("PASS", out + err)

    def test_bad_arm_next_to_valid_g5less_peer_is_invalid(self):
        with tempfile.TemporaryDirectory() as d:
            peer = self.write(d, "peer.jsonl", [meta(), g1()])
            for name, rows in (("bad g1", [meta(), g1(wall="x")]), ("meta only", [meta()])):
                bad = self.write(d, "bad.jsonl", rows)
                rc, err, out = self.run_compare([bad], peer)
                self.assertEqual(rc, 3, (name, out))
                self.assertNotIn("PASS", out + err, name)

    def test_empty_and_meta_only_still_fail(self):
        with tempfile.TemporaryDirectory() as d:
            ok = self.write(d, "ok.jsonl", [meta(), rep()])
            for name, rows in (("empty", []), ("meta-only", [meta()])):
                f = self.write(d, "x.jsonl", rows)
                self.assertEqual(self.run_compare([ok], f)[0], 3, name)
                self.assertEqual(self.run_compare([f], ok)[0], 3, name)

    def test_pre_fix_baseline_file_is_invalid(self):
        with tempfile.TemporaryDirectory() as d:
            f = self.write(d, "a.jsonl", [{"kind": "meta", "counts": {"big_bytes": 21474836}, "corpus_sha": gates.DEFAULT_SHA}, rep(21474836, written_bytes=None, read_bytes=None, read_matches=False)])
            self.assertEqual(self.run_compare([f], f)[0], 3)

    def test_nan_load_is_valid_input_but_cannot_support_a_verdict(self):
        # gates.py records NaN when getloadavg fails, so NaN stays valid input and must not
        # become INVALID. It cannot stand in for a load, though: with no finite number recorded
        # the load precondition is unknown, which is unmeasurable rather than a clean pass.
        nan = float("nan")
        with tempfile.TemporaryDirectory() as d:
            ok = self.write(d, "ok.jsonl", [meta(), {**g1(), "load1_before": nan, "load1_after": nan}])
            rc, err, out = self.run_compare([ok], ok)
            self.assertEqual(rc, 2, out)
            self.assertIn("UNMEASURABLE", out)
            self.assertNotIn("PASS", out)
            self.assertNotIn("INVALID", err)
            zero = self.write(d, "zero.jsonl", [meta(), g1(wall=0.0)])
            rc, err, out = self.run_compare([zero], zero)
            self.assertEqual(rc, 2, out)
            self.assertIn("unmeasurable", out)
            self.assertNotIn("PASS", out)

    def test_a_nan_arm_cannot_launder_an_over_ceiling_run(self):
        # max() is not symmetric on NaN: max(NaN, 31.0) is NaN but max(3.5, NaN) is 3.5,
        # so the arms are tested rather than the peak. Both orders must be refused.
        nan = float("nan")
        with tempfile.TemporaryDirectory() as d:
            busy = self.write(d, "busy.jsonl", [meta(), {**g1(), "load1_before": 31.0, "load1_after": 31.0}])
            quiet = self.write(d, "quiet.jsonl", [meta(), {**g1(), "load1_before": 3.5, "load1_after": 3.5}])
            unreadable = self.write(d, "unreadable.jsonl",
                                    [meta(), {**g1(), "load1_before": nan, "load1_after": nan}])
            for native, cowfs, name in ((unreadable, busy, "nan native, busy cowfs"),
                                        (busy, unreadable, "busy native, nan cowfs"),
                                        (quiet, unreadable, "quiet native, nan cowfs"),
                                        (unreadable, quiet, "nan native, quiet cowfs")):
                rc, _, out = self.run_compare([native], cowfs)
                self.assertEqual(rc, 2, (name, out))
                self.assertNotIn("PASS", out, name)

    def test_a_partly_unknown_load_is_not_a_measurement(self):
        # One unreadable observation leaves the arm's load unknown, so the peak over the samples
        # that did land is not a measurement and must not certify quality. NaN stays valid input.
        nan = float("nan")
        with tempfile.TemporaryDirectory() as d:
            quiet = self.write(d, "quiet.jsonl", [meta(), {**g1(), "load1_before": 3.5, "load1_after": 3.5}])
            busy = self.write(d, "busy.jsonl", [meta(), {**g1(), "load1_before": 31.0, "load1_after": 31.0}])
            mixed = {
                "nan before, finite after": (nan, 3.5),
                "finite before, nan after": (3.5, nan),
            }
            for name, (before, after) in mixed.items():
                half = self.write(d, "half.jsonl",
                                  [meta(), {**g1(), "load1_before": before, "load1_after": after}])
                for peer, peer_name in ((quiet, "quiet peer"), (busy, "busy peer")):
                    for arm, native, cowfs in (("native", half, peer), ("cowfs", peer, half)):
                        label = f"native {name} with {peer_name}"
                        if arm == "cowfs":
                            label = f"cowfs {name} with {peer_name}"
                        rc, err, out = self.run_compare([native], cowfs)
                        self.assertEqual(rc, 2, (label, out))
                        self.assertIn("UNMEASURABLE", out, label)
                        self.assertNotIn("PASS", out, label)
                        self.assertNotIn("INVALID", err, label)
            # a multi-rep arm where only one rep is unreadable is the same situation
            rows = [meta()]
            for i, load in enumerate((3.5, 3.5, nan)):
                rows.append({**g1(wall=2.0, rep=i), "load1_before": load, "load1_after": load})
            one_bad_rep = self.write(d, "onerep.jsonl", rows)
            rc, _, out = self.run_compare([one_bad_rep], one_bad_rep)
            self.assertEqual(rc, 2, out)
            self.assertNotIn("PASS", out)

    def test_finite_load_still_decides_the_verdict(self):
        # The do-nothing baseline for the case above: a finite load under the ceiling is scored
        # and one above it is refused, so refusing a non-finite load is not just refusing always.
        with tempfile.TemporaryDirectory() as d:
            low = self.write(d, "low.jsonl", [meta(), {**g1(), "load1_before": 3.5, "load1_after": 3.5}])
            rc, _, out = self.run_compare([low], low)
            self.assertEqual(rc, 0, out)
            self.assertIn("PASS", out)
            high = self.write(d, "high.jsonl", [meta(), {**g1(), "load1_before": 31.0, "load1_after": 31.0}])
            rc, _, out = self.run_compare([high], high)
            self.assertEqual(rc, 2, out)
            self.assertIn("UNMEASURABLE", out)
            # one arm finite and quiet, the other finite and busy, is still a refusal
            skew = self.write(d, "skew.jsonl", [meta(), {**g1(), "load1_before": 20.0, "load1_after": 20.0}])
            rc, _, out = self.run_compare([low], skew)
            self.assertEqual(rc, 2, out)
            self.assertIn("UNMEASURABLE", out)

    def test_genuine_performance_failure_is_exit_1_not_a_pass(self):
        with tempfile.TemporaryDirectory() as d:
            nat = self.write(d, "nat.jsonl", [meta()] + [g1(wall=1.0, rep=i) for i in range(3)])
            cow = self.write(d, "cow.jsonl", [meta()] + [g1(wall=2.0, rep=i) for i in range(3)])
            rc, err, out = self.run_compare([nat], cow)
            self.assertEqual(rc, 1, out)
            self.assertIn("FAIL", out)
            self.assertNotIn("PASS", out)


    def test_g2_pre_fix_file_is_invalid_and_post_fix_file_is_not(self):
        g2 = {"kind": "rep", "gate": "g2", "rep": 0, "wall_s": 1.0, "load1_before": 1.0, "load1_after": 1.0}
        with tempfile.TemporaryDirectory() as d:
            old = self.write(d, "old.jsonl", [meta(), {**g2, "metrics": {}}])
            rc, err, out = self.run_compare([old], old)
            self.assertEqual(rc, 3, out)
            self.assertIn("g2 rep", err)
            new = self.write(d, "new.jsonl", [meta(), {**g2, "metrics": {"rebuilt_count": 5, "bins_relinked": 2}}])
            # Valid, but one 1 s rep is UNMEASURABLE (rc 2), not invalid (rc 3) and not a pass (issue #221).
            self.assertEqual(self.run_compare([new], new)[0], 2)

    def test_gates_writes_scale_into_meta_that_compare_accepts(self):
        with tempfile.TemporaryDirectory() as d:
            root, out = Path(d) / "root", Path(d) / "out"
            argv = ["gates.py", "--root", str(root), "--label", "t", "--reps", "1", "--gates", "g5", "--no-resume"]
            # gates.py records the real load1 and compare.py refuses a ratio above its
            # ceiling, so pin the harness's own documented hook rather than relax that.
            with mock.patch.object(sys, "argv", argv), mock.patch.dict(
                    os.environ, {"COWFS_BENCH_SCALE": "0.3", "COWFS_BENCH_FAKE_LOAD1": "1"}), \
                    mock.patch.object(gates, "OUT", out), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(gates.main(), 0)
            f = str(next(out.glob("t-*.jsonl")))
            first = json.loads(Path(f).read_text().splitlines()[0])
            self.assertEqual((first["scale"], first["counts"]["big_bytes"]), (0.3, 3 * MIB))
            self.assertEqual(self.run_compare([f], f, f)[0], 0)


def artifact(name, kind="lib", fresh=False, exe=None):
    return json.dumps({"reason": "compiler-artifact", "fresh": fresh, "executable": exe,
                       "target": {"name": name, "kind": [kind]}})


# What cargo reported for the g2 edits, measured with `cargo build --message-format=json` on a
# scratch clone of the current pin: the original cookies.rs edit at the old pin (trivial), and
# the cowfs-vfs/src/lib.rs edit at the current pin (13 units, 3 binaries). See
# docs/verification/g1-g2-readiness-20261009.md for the per-edit table.
OLD_EDIT_OUT = "\n".join([artifact("cowfs_vfs", fresh=True), artifact("cowfs_vfs_path")])
NEW_EDIT_OUT = "\n".join([
    artifact("cowfs_vfs"), artifact("cowfs_vfs_path"), artifact("cowfs_vfs_test"),
    artifact("cowfs_fuse"), artifact("cowfs_nfs"), artifact("cowfs_core"), artifact("cowfs_ctl"),
    artifact("cowfs_daemon"), artifact("cowfs_treehouse"), artifact("cowfs_cli"),
    artifact("cowfs-daemon", "bin", exe="/t/debug/cowfs-daemon"),
    artifact("cowfs-treehouse", "bin", exe="/t/debug/cowfs-treehouse"),
    artifact("cowfs", "bin", exe="/t/debug/cowfs"),
    json.dumps({"reason": "build-finished", "success": True}),
])


class CorpusPinShape(unittest.TestCase):
    def test_pin_is_a_full_sha_other_than_the_retired_one(self):
        self.assertRegex(gates.DEFAULT_SHA, r"^[0-9a-f]{40}$")
        self.assertNotEqual(gates.DEFAULT_SHA, "c1619ec16df3a6b11dd5a1e08e8a512b4fedd240")

    def test_meta_records_the_pin_compare_checks(self):
        self.assertEqual(gates.meta(Path("/r"), "l", 1, ["g1"], {})["corpus_sha"], gates.DEFAULT_SHA)


class G2RebuildValidity(unittest.TestCase):
    """g2 must rebuild and relink something real, or the rep is refused (review of PR 202)."""

    def test_old_edit_shape_is_refused(self):
        units = gates.rebuilt_units(OLD_EDIT_OUT)
        self.assertEqual([u["name"] for u in units], ["cowfs_vfs_path"])
        self.assertIn("rebuilt", gates.g2_rebuild_problem(units))

    def test_new_edit_shape_is_accepted(self):
        units = gates.rebuilt_units(NEW_EDIT_OUT)
        self.assertEqual(len(units), 13)
        self.assertIsNone(gates.g2_rebuild_problem(units))

    def test_noop_and_missing_edited_crate_and_no_relink_are_each_refused(self):
        self.assertTrue(gates.g2_rebuild_problem(gates.rebuilt_units("")))
        no_edit = "\n".join([artifact("a"), artifact("b"), artifact("c", "bin", exe="/x")])
        self.assertIn(gates.EDIT_CRATE, gates.g2_rebuild_problem(gates.rebuilt_units(no_edit)))
        no_bin = "\n".join([artifact(gates.EDIT_CRATE), artifact("b"), artifact("c")])
        self.assertIn("relink", gates.g2_rebuild_problem(gates.rebuilt_units(no_bin)))

    def test_build_scripts_and_garbage_lines_do_not_count(self):
        out = "\n".join(["not json", artifact("x", "custom-build"), artifact("y", "custom-build"), artifact(gates.EDIT_CRATE)])
        self.assertEqual([u["name"] for u in gates.rebuilt_units(out)], [gates.EDIT_CRATE])

    def test_edit_target_lives_in_the_checked_crate(self):
        self.assertEqual(gates.EDIT_TARGET.split("/")[1].replace("-", "_"), gates.EDIT_CRATE)

    def run_g2(self, stdout):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            ctx = gates.Ctx(root, {})
            target = ctx.corpus / gates.EDIT_TARGET
            target.parent.mkdir(parents=True)
            target.write_text("// lib\n")
            done = mock.Mock(stdout=stdout, stderr="", returncode=0)
            with mock.patch.object(gates, "checked", return_value=done) as ck:
                try:
                    ctx.g2_prep()
                    return ctx.g2(), ck
                finally:
                    self.assertIn("--message-format=json", ck.call_args[0][0])
                    self.assertIn("// cowfs bench g2 edit", target.read_text())

    def test_g2_appends_one_comment_per_rep_never_accumulating(self):
        with tempfile.TemporaryDirectory() as d:
            ctx = gates.Ctx(Path(d), {})
            ctx.corpus.mkdir()
            git = lambda *a: subprocess.run(["git", "-C", str(ctx.corpus), *a], check=True, capture_output=True)
            git("init", "-q")
            target = ctx.corpus / gates.EDIT_TARGET
            target.parent.mkdir(parents=True)
            target.write_text("// lib\n")
            git("add", "-A")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "x")
            done = mock.Mock(stdout=NEW_EDIT_OUT, stderr="", returncode=0)
            real = gates.checked
            fake = lambda cmd, **kw: real(cmd, **kw) if cmd[0] == "git" else done
            with mock.patch.object(gates, "checked", side_effect=fake):
                for _ in range(3):
                    ctx.g2_prep()
                    ctx.g2()
            self.assertEqual(target.read_text().count("// cowfs bench g2 edit"), 1)

    def test_the_reset_and_the_edit_are_outside_the_timed_region(self):
        """Issue #221: the git reset ran inside the timed g2 region and added a process to every rep."""
        events = []
        ctx = mock.Mock()
        ctx.g2_prep.side_effect = lambda: events.append("prep")
        ctx.g2.side_effect = lambda: events.append("g2") or {}
        clock = iter([100.0, 107.5])
        with mock.patch.object(gates.time, "monotonic", side_effect=lambda: events.append("clock") or next(clock)):
            wall, _ = gates.timed_rep(ctx, "g2")
        self.assertEqual(events, ["prep", "clock", "g2", "clock"])
        self.assertEqual(wall, 7.5)

    def test_g2_itself_never_runs_git_or_writes_the_edit(self):
        with tempfile.TemporaryDirectory() as d:
            ctx = gates.Ctx(Path(d), {})
            target = ctx.corpus / gates.EDIT_TARGET
            target.parent.mkdir(parents=True)
            target.write_text("// lib\n")
            done = mock.Mock(stdout=NEW_EDIT_OUT, stderr="", returncode=0)
            with mock.patch.object(gates, "checked", return_value=done) as ck:
                ctx.g2()
            self.assertEqual([c[0][0][0] for c in ck.call_args_list], ["cargo"])
            self.assertEqual(target.read_text(), "// lib\n")

    def test_a_gate_without_a_prep_step_is_just_timed(self):
        ctx = mock.Mock(spec=["g1"])
        ctx.g1.return_value = {}
        self.assertEqual(gates.timed_rep(ctx, "g1")[1], {})

    def test_g2_rebuilds_the_test_executables_too(self):
        """Issue #221: plain `cargo build` after the vfs edit is about 2.6 s of native work, `--tests` is about 23 s."""
        _, ck = self.run_g2(NEW_EDIT_OUT)
        self.assertIn("--tests", ck.call_args[0][0])

    def test_g2_test_targets_are_built_untimed_before_the_first_rep(self):
        """Otherwise rep 0 would carry a cold build of every test target and its dev-dependencies."""
        self.assertIn("warm_tests", gates.SETUP["g2"])
        self.assertLess(gates.SETUP["g2"].index("warm_target"), gates.SETUP["g2"].index("warm_tests"))
        with tempfile.TemporaryDirectory() as d:
            ctx = gates.Ctx(Path(d), {})
            done = mock.Mock(stdout="", stderr="", returncode=0)
            with mock.patch.object(gates, "checked", return_value=done) as ck:
                ctx.warm_tests()
            self.assertEqual(ck.call_args[0][0][:3], ["cargo", "build", "--tests"])
            self.assertIn("--offline", ck.call_args[0][0])

    def test_g2_records_the_rebuilt_units(self):
        m, _ = self.run_g2(NEW_EDIT_OUT)
        self.assertEqual((m["rebuilt_count"], m["bins_relinked"]), (13, 3))
        self.assertIn(gates.EDIT_CRATE, m["rebuilt_units"])

    def test_g2_refuses_the_trivial_workload_and_records_no_rep(self):
        with self.assertRaises(SystemExit) as cm:
            self.run_g2(OLD_EDIT_OUT)
        self.assertIn("g2", str(cm.exception))

    def test_compare_refuses_a_g2_rep_without_the_rebuild_evidence(self):
        row = {"gate": "g2", "rep": 0, "metrics": {}}
        self.assertTrue(compare.g2_problem(row))
        self.assertTrue(compare.g2_problem({**row, "metrics": {"rebuilt_count": 1, "bins_relinked": 0}}))
        self.assertIsNone(compare.g2_problem({**row, "metrics": {"rebuilt_count": 5, "bins_relinked": 2}}))
        self.assertTrue(compare.g2_problem({**row, "metrics": {"rebuilt_count": True, "bins_relinked": 1}}))
        self.assertEqual(compare.G2_MIN_UNITS, gates.G2_MIN_UNITS)


if __name__ == "__main__":
    unittest.main()
