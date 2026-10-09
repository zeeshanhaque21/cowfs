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
WORKFLOW_YAML = yaml.safe_load(WORKFLOW.read_text())
STEPS = {s["name"]: s for s in WORKFLOW_YAML["jobs"]["linux-fuse"]["steps"] if "name" in s}
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


class EveryTeeHasPipefail(unittest.TestCase):
    def test_no_step_pipes_through_tee_without_pipefail(self):
        for job, spec in WORKFLOW_YAML["jobs"].items():
            for step in spec.get("steps", []):
                run = step.get("run", "")
                if re.search(r"\|\s*tee\b", run):
                    with self.subTest(job=job, step=step.get("name", run[:40])):
                        self.assertTrue(
                            step.get("shell") == "bash" or "pipefail" in run,
                            "a pipe into tee takes tee's exit status unless pipefail is on",
                        )


class CheckAggregate(unittest.TestCase):
    JOBS = ["lint", "fault-seam", "test", "linux-fuse", "linux-namespaces"]
    CHECK = WORKFLOW_YAML["jobs"]["check"]

    def run_check(self, results):
        script = self.CHECK["steps"][0]["run"]
        script = re.sub(r"\$\{\{ needs\.([\w-]+)\.result \}\}", lambda m: results.get(m.group(1), "<unset>"), script)
        return subprocess.run(["bash", "--noprofile", "--norc", "-e", "-c", script], capture_output=True, text=True)

    def test_needs_and_cancelled_semantics(self):
        self.assertTrue(set(self.JOBS) <= set(self.CHECK["needs"]))
        self.assertEqual(self.CHECK["if"].replace(" ", ""), "${{!cancelled()}}")

    def test_all_green_passes_and_any_red_fails(self):
        green = {j: "success" for j in self.JOBS}
        self.assertEqual(self.run_check(green).returncode, 0)
        for job in self.JOBS:
            with self.subTest(red=job):
                self.assertNotEqual(self.run_check({**green, job: "failure"}).returncode, 0)


TEST_STEPS = {s["name"]: s for s in WORKFLOW_YAML["jobs"]["test"]["steps"] if "name" in s}
LINT_STEPS = {s["name"]: s for s in WORKFLOW_YAML["jobs"]["lint"]["steps"] if "name" in s}


def run_test_step(name, cwd, env_extra, shard=1, of=2):
    """Run a step of the `test` job with a recording stub `cargo` and the matrix expressions filled in."""
    step = TEST_STEPS[name]
    script = step["run"].replace("${{ matrix.shard }}", str(shard)).replace("${{ matrix.of }}", str(of))
    bin_dir = Path(cwd) / "bin"
    bin_dir.mkdir(exist_ok=True)
    (bin_dir / "cargo").write_text(
        f'#!/bin/sh\necho "$@" >> "{cwd}/cargo.calls"\n'
        'case "$1 $2" in "nextest list") printf "%s" "$STUB_LIST";; esac\n'
    )
    (bin_dir / "cargo").chmod(0o755)
    env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", **{k: v for k, v in step.get("env", {}).items() if "${{" not in v}, **env_extra}
    r = subprocess.run(["bash", "--noprofile", "--norc", "-e", "-c", script], cwd=cwd, env=env, capture_output=True, text=True)
    calls = Path(cwd, "cargo.calls")
    return r, calls.read_text().splitlines() if calls.exists() else []


class ChangedCrateSelection(unittest.TestCase):
    """The test job narrows to the PR's changed crates; every other path runs what ran before."""

    def tmp(self):
        t = tempfile.TemporaryDirectory()
        self.addCleanup(t.cleanup)
        return t.name

    def test_selection_runs_before_nextest_and_cannot_be_ignored(self):
        steps = WORKFLOW_YAML["jobs"]["test"]["steps"]
        names = [s.get("name") for s in steps]
        self.assertLess(names.index("Select tests"), names.index("Nextest"))
        select = TEST_STEPS["Select tests"]
        self.assertEqual(select["id"], "select")
        self.assertNotIn("continue-on-error", select)
        self.assertIn("scripts/select-tests.py", select["run"])
        self.assertEqual(select["env"]["EVENT"], "${{ github.event_name }}")
        checkout = next(s for s in steps if s.get("uses", "").startswith("actions/checkout"))
        self.assertGreaterEqual(checkout["with"]["fetch-depth"], 2, "HEAD^1 must exist for the PR diff")

    def test_selector_error_fails_the_step(self):
        cwd = self.tmp()  # no workspace, no git history: the selector cannot work, and must say so
        (Path(cwd) / "scripts").symlink_to(ROOT / "scripts")
        r = subprocess.run(
            ["bash", "--noprofile", "--norc", "-e", "-c", TEST_STEPS["Select tests"]["run"]],
            cwd=cwd, env={**os.environ, "EVENT": "pull_request"}, capture_output=True, text=True,
        )
        self.assertNotEqual(r.returncode, 0)

    def test_check_aggregate_still_requires_the_test_job(self):
        self.assertIn("test", WORKFLOW_YAML["jobs"]["check"]["needs"])

    def test_full_mode_runs_the_whole_workspace_sharded(self):
        r, calls = run_test_step("Nextest", self.tmp(), {"MODE": "full", "FILTERSET": ""}, shard=2)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(calls, ["nextest run --workspace --profile ci --partition hash:2/2"])

    def test_filtered_mode_builds_only_selected_crates_keeps_sharding_and_passes_the_filterset_as_one_argument(self):
        filterset = "package(=cowfs-core) | package(=cowfs-nfs)"
        env = {"MODE": "filtered", "FILTERSET": filterset, "PKG_ARGS": "-p cowfs-core -p cowfs-nfs", "STUB_LIST": "a b\nc d\n"}
        r, calls = run_test_step("Nextest", self.tmp(), env, shard=1)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(len(calls), 2, calls)
        self.assertTrue(calls[0].startswith("nextest list -p cowfs-core -p cowfs-nfs"), calls[0])
        run = calls[1]
        self.assertNotIn("--workspace", run, "--workspace would compile every test binary")
        self.assertIn("-p cowfs-core -p cowfs-nfs", run)
        self.assertIn("--partition hash:1/2", run)
        self.assertIn("--no-tests=pass", run, "a shard left with no test is not a failure")
        self.assertTrue(run.endswith(f"-E {filterset}"), run)

    def test_non_empty_selection_that_selects_no_test_fails(self):
        env = {"MODE": "filtered", "FILTERSET": "package(=cowfs-core)", "PKG_ARGS": "-p cowfs-core", "STUB_LIST": ""}
        r, calls = run_test_step("Nextest", self.tmp(), env)
        self.assertNotEqual(r.returncode, 0)
        self.assertEqual(len(calls), 1, "must not go on to run with --no-tests=pass")

    def test_unknown_or_empty_mode_fails_closed(self):
        for mode in ["", "partial", "FULL"]:
            for step in ["Nextest", "Doctests"]:
                with self.subTest(mode=mode, step=step):
                    r, calls = run_test_step(step, self.tmp(), {"MODE": mode, "FILTERSET": "", "PKG_ARGS": "", "DOC_ARGS": ""})
                    self.assertNotEqual(r.returncode, 0)
                    self.assertEqual(calls, [])

    def test_filtered_without_crates_needs_the_explicit_empty_filterset(self):
        r, calls = run_test_step("Nextest", self.tmp(), {"MODE": "filtered", "FILTERSET": "", "PKG_ARGS": ""})
        self.assertNotEqual(r.returncode, 0)

    def test_empty_selection_runs_no_cargo_at_all(self):
        r, calls = run_test_step("Nextest", self.tmp(), {"MODE": "filtered", "FILTERSET": "none()", "PKG_ARGS": ""})
        self.assertEqual((r.returncode, calls), (0, []))

    def test_doctests_full_filtered_and_empty(self):
        r, calls = run_test_step("Doctests", self.tmp(), {"MODE": "full", "DOC_ARGS": ""})
        self.assertEqual(calls, ["test --doc --workspace"])
        r, calls = run_test_step("Doctests", self.tmp(), {"MODE": "filtered", "DOC_ARGS": "-p cowfs-core -p cowfs-nfs"})
        self.assertEqual(calls, ["test --doc -p cowfs-core -p cowfs-nfs"])
        r, calls = run_test_step("Doctests", self.tmp(), {"MODE": "filtered", "DOC_ARGS": ""})
        self.assertEqual((r.returncode, calls), (0, []))

    def test_the_selector_tests_run_in_lint(self):
        self.assertIn("test_select_tests.py", LINT_STEPS["Test selector unit tests"]["run"])

    def test_untouched_selections_stay_untouched(self):
        # linux-fuse, linux-namespaces, fault-seam and lint are not narrowed by the selector.
        for job in ["lint", "fault-seam", "linux-fuse", "linux-namespaces"]:
            self.assertNotIn("select-tests", yaml.safe_dump(WORKFLOW_YAML["jobs"][job]))


if __name__ == "__main__":
    unittest.main()
