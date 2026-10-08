#!/usr/bin/env python3
"""CI discovery bridge for the fsx gate's tests.

`python3 -m unittest discover -s bench` in `.github/workflows/ci.yml` finds `test_*.py` directly
under `bench/`, and `bench/fsx-gate/` has a hyphen in its name, so the gate's tests were never
discovered. This module runs them without duplicating any assertion.

The CI command is not editable here, and the test file is not moved out of the directory that owns
it, so this is the smallest thing that makes the guards visible to every run.

    python3 -m unittest discover -s bench -v
    python3 -m unittest bench.test_fsx_gate -v
    python3 -m unittest bench.test_fsx_gate -v -k RestartLeg
"""

import importlib.util
import os
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
GATE_DIR = os.path.join(HERE, "fsx-gate")
RUNNER = os.path.join(GATE_DIR, "run-fsx-gate.py")
GATE_TESTS = os.path.join(GATE_DIR, "test_run_fsx_gate.py")


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def gate_suite():
    """The gate's own test module, loaded by path because its directory is not a package."""
    if not os.path.exists(RUNNER) or not os.path.exists(GATE_TESTS):
        raise unittest.SkipTest("bench/fsx-gate is not present in this checkout")
    load("cowfs_fsx_gate_runner", RUNNER)
    module = load("cowfs_fsx_gate_tests", GATE_TESTS)
    return unittest.defaultTestLoader.loadTestsFromModule(module)


def load_tests(loader, tests, pattern):  # noqa: ARG001 - the unittest hook signature
    """unittest calls this; it is what makes discover -s bench pick the gate up."""
    suite = unittest.TestSuite()
    suite.addTests(gate_suite())
    return suite


if __name__ == "__main__":
    result = unittest.TextTestRunner(verbosity=2).run(gate_suite())
    sys.exit(0 if result.wasSuccessful() else 1)
