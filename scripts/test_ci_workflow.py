"""Issue 179: the linux-fuse job must fail when cargo fails, and its Enforce step must reject a bad log.

Runs the real step scripts out of .github/workflows/ci.yml (override with CI_WORKFLOW to test another
copy), with a stub `cargo` on PATH and the shell GitHub Actions would pick for the step.
"""
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = Path(os.environ.get("CI_WORKFLOW", ROOT / ".github/workflows/ci.yml"))
STEPS = {s["name"]: s for s in yaml.safe_load(WORKFLOW.read_text())["jobs"]["linux-fuse"]["steps"] if "name" in s}
CARGO_STEPS = ["Native xattr control", "Native page-cache control", "Native forget control", "Test FUSE mounts and conformance"]
ENFORCE = "Enforce conformance results"
KNOWN = "FAIL cowfs    xattrs         xattr_on_directory_and_symlink                    6.98ms  unexpected error: permission denied (PermissionDenied)"
EXPECTED_SKIPS = {
    "statfs_free_after_unlink": "SKIP cowfs    basic          statfs_free_after_unlink                               -  kernel sends FORGET asynchronously, so reclaim is not observable in the same call",
    "hardlink_limit_reports_too_many_links": "SKIP cowfs    links          hardlink_limit_reports_too_many_links                  -  backend did not declare a small hardlink limit (Options::link_limit)",
    "concurrent_readers_and_writers_of_one_file": "SKIP cowfs    concurrency    concurrent_readers_and_writers_of_one_file             -  not observable through a kernel page cache: native ext4/btrfs/tmpfs tear at this size too",
}


def run_step(name, cwd, env_extra=None):
    """Run a step's `run` the way the runner does: `shell: bash` is bash -eo pipefail, the default is bash -e."""
    step = STEPS[name]
    flags = ["-eo", "pipefail"] if step.get("shell") == "bash" else ["-e"]
    script = step["run"].replace("/dev/shm/", str(cwd) + "/shm-")
    env = {**os.environ, **{k: str(v) for k, v in step.get("env", {}).items()}, **(env_extra or {})}
    env["GITHUB_STEP_SUMMARY"] = str(Path(cwd) / "summary.md")
    return subprocess.run(["bash", "--noprofile", "--norc", *flags, "-c", script], cwd=cwd, env=env, capture_output=True, text=True)


class CargoExitStatus(unittest.TestCase):
    def cargo(self, rc, out):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        bin_dir = Path(tmp.name) / "bin"
        bin_dir.mkdir()
        (bin_dir / "cargo").write_text(f'#!/bin/sh\ncat <<\'EOF\'\n{out}\nEOF\nexit {rc}\n')
        (bin_dir / "cargo").chmod(0o755)
        return tmp.name, {"PATH": f"{bin_dir}:{os.environ['PATH']}"}

    def test_failing_cargo_fails_every_step_except_the_deliberate_one(self):
        for name in CARGO_STEPS:
            with self.subTest(step=name):
                cwd, env = self.cargo(101, "test x ... FAILED\ntest result: FAILED. 0 passed; 1 failed")
                r = run_step(name, cwd, env)
                if name == "Native page-cache control":
                    self.assertEqual(r.returncode, 0, "|| true is deliberate here: a torn read is the known native limit")
                else:
                    self.assertNotEqual(r.returncode, 0, "cargo exited 101 but the step passed (tee swallowed it)")
                self.assertTrue(any(Path(cwd).glob("*.log")), "log capture must survive")

    def test_passing_cargo_passes_and_keeps_the_log(self):
        for name in CARGO_STEPS:
            with self.subTest(step=name):
                cwd, env = self.cargo(0, "test result: ok. 1 passed")
                r = run_step(name, cwd, env)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn("test result: ok", next(Path(cwd).glob("*.log")).read_text())


class Enforce(unittest.TestCase):
    """The Enforce step against logs shaped like the green run on main (known xattr FAIL only)."""

    @staticmethod
    def checks():
        text = (ROOT / "crates/cowfs-vfs-test/src/conformance/list.rs").read_text()
        return len(re.findall(r"^\s+\((\w+), (\w+), (?:Posix|Portable|Cowfs)\),\s*$", text, re.MULTILINE))

    def enforce(self, fuse_extra="", skips=None, known_fail=True):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        d = Path(tmp.name)
        (d / "crates").symlink_to(ROOT / "crates")
        skips = EXPECTED_SKIPS if skips is None else skips
        run = self.checks() - len(skips) - 2
        fuse = "\n".join(
            [f"{run} run, {int(known_fail)} failed, {len(skips)} skipped, 2 heavy not enabled, 0 above the level, 0 leaked threads"]
            + ([KNOWN] if known_fail else []) + list(skips.values())
        ) + "\ntest result: ok. 1 passed; 0 failed\n" + fuse_extra
        (d / "fuse-tests.log").write_text(fuse)
        (d / "native-xattr.log").write_text(KNOWN + "\n1 run, 1 failed, 0 skipped, 0 heavy not enabled, 0 above the level, 0 leaked threads\n")
        (d / "native-page-cache.log").write_text("1 run, 0 failed, 0 skipped, 0 heavy not enabled, 0 above the level, 0 leaked threads\n")
        (d / "native-forget.log").write_text("1 run, 0 failed, 0 skipped, 0 heavy not enabled, 0 above the level, 0 leaked threads\n")
        return run_step(ENFORCE, d)

    def test_main_shape_passes(self):
        r = self.enforce()
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_libtest_failed_line_fails(self):
        r = self.enforce("test mount_x ... FAILED\ntest result: FAILED. 42 passed; 1 failed\n")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("cargo test failed", r.stderr)

    def test_unexpected_conformance_failure_fails(self):
        r = self.enforce("FAIL cowfs    basic          chown_x                    1.0ms  boom\n")
        self.assertNotEqual(r.returncode, 0)

    def test_skip_set_drift_fails(self):
        extra = {**EXPECTED_SKIPS, "new_check": "SKIP cowfs    basic          new_check   -  because"}
        self.assertNotEqual(self.enforce(skips=extra).returncode, 0)


if __name__ == "__main__":
    unittest.main()
