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
                self.assertGreaterEqual(n[key], unit, (scale, key))
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

    def test_negative_control_read_back_shorter_than_written(self):
        real_open = open

        class ShortFile:
            def __init__(self, fh):
                self.fh = fh
                self.left = None

            def __enter__(self):
                return self

            def __exit__(self, *a):
                self.fh.close()

            def read(self, n):
                if self.left is None:
                    self.left = 4 * MIB - 1
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

        m = self.run_g5(4 * MIB, short)
        self.assertFalse(m["read_matches"])
        self.assertEqual((m["bytes"], m["written_bytes"], m["read_bytes"]), (4 * MIB, 4 * MIB, 4 * MIB - 1))

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
    def test_large_files_have_nominal_size(self):
        with tempfile.TemporaryDirectory() as d:
            n = {"small_files": 256, "large_files": 2, "large_bytes": counts_at(3.3)["large_bytes"]}
            self.assertEqual(n["large_bytes"] % (1 << 16), 0)
            ctx = gates.Ctx(Path(d), n)
            ctx.ensure_tree()
            for f in (Path(d) / "tree" / "big").iterdir():
                self.assertEqual(f.stat().st_size, n["large_bytes"])


class CompareRefuses(unittest.TestCase):
    def rep(self, **m):
        return {"kind": "rep", "gate": "g5", "rep": 0, "label": "x", "metrics": m}

    def test_short_read_rep_is_invalid(self):
        self.assertEqual(len(compare.short_reads([self.rep(read_matches=False)])), 1)
        self.assertEqual(len(compare.short_reads([self.rep()])), 1)
        self.assertEqual(len(compare.short_reads([self.rep(read_matches=True)])), 0)

    def test_compare_exits_3_on_pre_fix_file(self):
        with tempfile.TemporaryDirectory() as d:
            f = Path(d) / "a.jsonl"
            f.write_text(json.dumps({"kind": "meta"}) + "\n" + json.dumps(
                {**self.rep(bytes=21474836, read_matches=False), "wall_s": 1, "load1_before": 0, "load1_after": 0}) + "\n")
            argv = ["compare.py", "--native", str(f), "--cowfs", str(f)]
            with mock.patch.object(sys, "argv", argv), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(compare.main(), 3)


if __name__ == "__main__":
    unittest.main()
