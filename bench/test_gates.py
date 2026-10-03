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
            self.assertIn("big/b001", str(cm.exception))

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


class CompareRefuses(unittest.TestCase):
    def rep(self, **m):
        return {"kind": "rep", "gate": "g5", "rep": 0, "label": "x", "wall_s": 1, "load1_before": 0, "load1_after": 0, "metrics": m}

    def good(self, size=4 * MIB):
        return self.rep(bytes=size, written_bytes=size, read_bytes=size, read_matches=True)

    def problem(self, row, meta_bytes=None):
        return compare.g5_problem(row, meta_bytes)

    def test_numeric_equality_not_the_flag(self):
        self.assertIsNone(self.problem(self.good()))
        self.assertTrue(self.problem(self.rep(bytes=4 * MIB, written_bytes=5, read_bytes=7, read_matches=True)))
        self.assertTrue(self.problem(self.rep(bytes=4 * MIB, written_bytes=4 * MIB, read_bytes=4 * MIB - 1, read_matches=True)))
        self.assertTrue(self.problem(self.rep(bytes=4 * MIB, written_bytes=4 * MIB - 1, read_bytes=4 * MIB - 1, read_matches=True)))
        self.assertIsNone(self.problem(self.rep(bytes=4 * MIB, written_bytes=4 * MIB, read_bytes=4 * MIB, read_matches=False)))
        self.assertTrue(self.problem(self.rep(bytes=4 * MIB, written_bytes="4194304", read_bytes=4 * MIB, read_matches=True)))
        self.assertTrue(self.problem(self.rep(bytes=4 * MIB, written_bytes=True, read_bytes=True, read_matches=True)))
        self.assertTrue(self.problem(self.rep(bytes=4 * MIB, written_bytes=4 * MIB, read_matches=True)))

    def test_meta_counts_must_agree(self):
        self.assertTrue(self.problem(self.good(4 * MIB), meta_bytes=8 * MIB))
        self.assertIsNone(self.problem(self.good(4 * MIB), meta_bytes=4 * MIB))

    def test_legacy_rows(self):
        self.assertIsNone(self.problem(self.rep(bytes=1 << 30, read_matches=True)))
        self.assertTrue(self.problem(self.rep(bytes=21474836, read_matches=False)))
        self.assertTrue(self.problem(self.rep(bytes=21474836, read_matches=True)))

    def write(self, d, name, *rows):
        f = Path(d) / name
        f.write_text("".join(json.dumps(r) + "\n" for r in rows))
        return str(f)

    def run_compare(self, native, cowfs, noise=None):
        argv = ["compare.py", "--native", *native, "--cowfs", cowfs] + (["--noise-floor", noise] if noise else [])
        with mock.patch.object(sys, "argv", argv), contextlib.redirect_stderr(io.StringIO()) as err, \
                contextlib.redirect_stdout(io.StringIO()):
            return compare.main(), err.getvalue()

    def test_every_input_file_is_validated(self):
        forged = self.rep(bytes=4 * MIB, written_bytes=5, read_bytes=7, read_matches=True)
        short = self.rep(bytes=4 * MIB, written_bytes=4 * MIB, read_bytes=4 * MIB - 1, read_matches=False)
        meta = {"kind": "meta", "counts": {"big_bytes": 4 * MIB}}
        with tempfile.TemporaryDirectory() as d:
            ok = self.write(d, "ok.jsonl", meta, self.good())
            ok2 = self.write(d, "ok2.jsonl", meta, self.good())
            for name, bad_row in (("forged", forged), ("short", short)):
                bad = self.write(d, f"{name}.jsonl", meta, bad_row)
                self.assertEqual(self.run_compare([ok], ok, ok2)[0], 0)
                for placement in ("native", "native2", "cowfs", "noise"):
                    native = [bad] if placement == "native" else [ok, bad] if placement == "native2" else [ok]
                    cow = bad if placement == "cowfs" else ok
                    noise = bad if placement == "noise" else ok2
                    rc, err = self.run_compare(native, cow, noise)
                    self.assertEqual(rc, 3, (name, placement))
                    self.assertIn("INVALID", err)
                    self.assertIn(bad, err)

    def test_pre_fix_baseline_file_is_invalid(self):
        with tempfile.TemporaryDirectory() as d:
            f = self.write(d, "a.jsonl", {"kind": "meta"}, self.rep(bytes=21474836, read_matches=False))
            self.assertEqual(self.run_compare([f], f)[0], 3)


if __name__ == "__main__":
    unittest.main()
