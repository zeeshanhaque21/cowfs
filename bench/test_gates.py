"""g5 byte-count regression tests (issue #55). Run: python3 -m unittest discover -s bench"""

import contextlib
import importlib.util
import io
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
    return {"kind": "meta", "counts": {"big_bytes": big}, **extra}


def rep(size=4 * MIB, gate="g5", **override):
    m = {"bytes": size, "written_bytes": size, "read_bytes": size, "read_matches": True}
    m.update(override)
    return {"kind": "rep", "gate": gate, "rep": 0, "label": "x", "wall_s": 1, "load1_before": 0, "load1_after": 0, "metrics": m}


def drop(row, key):
    del row["metrics"][key]
    return row


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
    "no g5 reps": [meta(), rep(gate="g1")],
    "g5 rep with no metrics": [meta(), {"kind": "rep", "gate": "g5", "rep": 0}],
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

    def test_pre_fix_baseline_file_is_invalid(self):
        with tempfile.TemporaryDirectory() as d:
            f = self.write(d, "a.jsonl", [{"kind": "meta", "counts": {"big_bytes": 21474836}}, rep(21474836, written_bytes=None, read_bytes=None, read_matches=False)])
            self.assertEqual(self.run_compare([f], f)[0], 3)

    def test_gates_writes_scale_into_meta_that_compare_accepts(self):
        with tempfile.TemporaryDirectory() as d:
            root, out = Path(d) / "root", Path(d) / "out"
            argv = ["gates.py", "--root", str(root), "--label", "t", "--reps", "1", "--gates", "g5", "--no-resume"]
            with mock.patch.object(sys, "argv", argv), mock.patch.dict(os.environ, {"COWFS_BENCH_SCALE": "0.3"}), \
                    mock.patch.object(gates, "OUT", out), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(gates.main(), 0)
            f = str(next(out.glob("t-*.jsonl")))
            first = json.loads(Path(f).read_text().splitlines()[0])
            self.assertEqual((first["scale"], first["counts"]["big_bytes"]), (0.3, 3 * MIB))
            self.assertEqual(self.run_compare([f], f, f)[0], 0)


if __name__ == "__main__":
    unittest.main()
