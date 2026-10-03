#!/usr/bin/env python3
"""Focused tests for scripts/measure-live-trial.py. No third-party deps.

Covers the two audit-driven fixes: the clean-target guard must fail closed with or
without `python -O`, and cargo's `Finished ... in` line must parse in both the
seconds and the minutes form.

Run: python3 scripts/test_measure_live_trial.py
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "measure-live-trial.py"
sys.path.insert(0, str(HERE))
import importlib

mlt = importlib.import_module("measure-live-trial")


def tiny_owned_arm(root: Path, arm: str = "nativeA") -> "mlt.Run":
    """A representative tiny run home: real Run, real marker, real arm dir, real target."""
    run = mlt.Run(root.name)
    run.native = root
    if arm == "cowfs":
        run.cow = root / "cow"
        run.cow.mkdir()
    else:
        run.cow = root / "cow"
    run.arm_dir[arm] = root / "arm" / arm
    for base in (run.native, run.cow):
        base.mkdir(parents=True, exist_ok=True)
        (base / mlt.MARKER).write_text(f"{run.id}\n")
    run.arm_dir[arm].mkdir(parents=True)
    return run


class GuardTest(unittest.TestCase):
    def test_owned_target_is_removed(self):
        with tempfile.TemporaryDirectory() as d:
            run = tiny_owned_arm(Path(d))
            t = run.arm_dir["nativeA"] / "corpus-target"
            (t / "debug").mkdir(parents=True)
            (t / "debug" / "bin").write_text("x")
            mlt.clean_target(run, "nativeA")
            self.assertFalse(t.exists())

    def test_missing_target_is_a_noop(self):
        with tempfile.TemporaryDirectory() as d:
            run = tiny_owned_arm(Path(d))
            mlt.clean_target(run, "nativeA")

    def test_foreign_name_is_refused_and_survives(self):
        with tempfile.TemporaryDirectory() as d:
            run = tiny_owned_arm(Path(d))
            foreign = run.arm_dir["nativeA"] / "not-corpus-target"
            foreign.mkdir()
            (foreign / "keep").write_text("keep")
            mlt.clean_target(run, "nativeA")  # only the canonical name is ever touched
            self.assertTrue(foreign.exists())

    def test_unowned_target_is_refused_and_survives(self):
        """The one clause that can fail: no run marker above the target."""
        with tempfile.TemporaryDirectory() as d:
            run = tiny_owned_arm(Path(d))
            outside = Path(d) / "outside"
            outside.mkdir()
            run.arm_dir["nativeA"] = outside
            # the marker in Path(d) would still cover `outside`; drop it for this case
            (run.native / mlt.MARKER).unlink()
            (run.cow / mlt.MARKER).unlink()
            t = outside / "corpus-target"
            t.mkdir()
            (t / "keep").write_text("keep")
            with self.assertRaises(SystemExit):
                mlt.clean_target(run, "nativeA")
            self.assertTrue(t.exists())

    def test_owned_target_under_marker_is_removed_even_if_empty_parent_leaf(self):
        with tempfile.TemporaryDirectory() as d:
            run = tiny_owned_arm(Path(d))
            t = run.arm_dir["nativeA"] / "corpus-target"
            t.mkdir()
            mlt.clean_target(run, "nativeA")
            self.assertFalse(t.exists())

    def test_guard_fails_closed_under_optimized_interpreter(self):
        """The whole point: python -O must not strip the guard."""
        if sys.flags.optimize:
            self.skipTest("already running under -O; covered by the subprocess assertion below")
        with tempfile.TemporaryDirectory() as d:
            run = tiny_owned_arm(Path(d))
            real_parent = run.arm_dir["nativeA"].parent / "elsewhere"
            real_parent.mkdir()
            foreign = real_parent / "corpus-target"
            foreign.mkdir()
            (foreign / "keep").write_text("keep")
            code = f"""
import sys; sys.path.insert(0, {str(HERE)!r})
import importlib; mlt = importlib.import_module('measure-live-trial')
from pathlib import Path
run = mlt.Run("x"); run.native = Path({str(run.arm_dir['nativeA'])!r})
run.cow = Path({str(run.cow)!r}); run.arm_dir["nativeA"] = Path({str(real_parent)!r})
try:
    mlt.clean_target(run, "nativeA"); print("NOGUARD")
except SystemExit: print("GUARDED")
"""
            env = dict(os.environ, PYTHONOPTIMIZE="2")
            out = subprocess.run(
                [sys.executable, "-O", "-c", code], capture_output=True, text=True, env=env
            )
            self.assertEqual(out.stdout.strip(), "GUARDED", out.stderr)
            self.assertTrue((foreign / "keep").exists())


class ParseFinishedTest(unittest.TestCase):
    def test_seconds(self):
        self.assertEqual(mlt.parse_finished("    Finished `dev` profile in 1.23s\n"), 1.23)

    def test_minutes_and_seconds(self):
        self.assertEqual(mlt.parse_finished("    Finished `dev` profile in 2m 03s\n"), 123.0)

    def test_long_form(self):
        self.assertEqual(mlt.parse_finished("Finished `dev` profile [unoptimized] in 12m 34s\n"), 754.0)

    def test_truncated_output_still_parses(self):
        self.assertEqual(mlt.parse_finished("Compiling x\nFinished dev in 0.09s"), 0.09)

    def test_absent_is_none(self):
        self.assertIsNone(mlt.parse_finished("Compiling x\n"))
        self.assertIsNone(mlt.parse_finished("Finished dev profile\n"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
