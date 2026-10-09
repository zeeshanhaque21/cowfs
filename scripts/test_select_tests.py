"""Unit tests for scripts/select-tests.py (per-PR changed-crate test selection).

The selector maps a PR's changed files to workspace crates and says which tests must run. It must
only ever err towards running MORE: any file it cannot place forces the full run.
"""
import importlib.util
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("select_tests", ROOT / "scripts/select-tests.py")
sel = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sel)


def pkg(name, dir, deps=(), dev=(), lib=True):
    """A `cargo metadata --no-deps` package record (only the fields the selector reads)."""
    dependencies = [{"name": d, "kind": None} for d in deps] + [{"name": d, "kind": "dev"} for d in dev]
    kinds = [["lib"]] if lib else [["bin"]]
    return {
        "name": name,
        "manifest_path": f"/ws/{dir}/Cargo.toml",
        "dependencies": dependencies,
        "targets": [{"kind": k} for k in kinds],
    }


# core <- vfs <- fuse, vfs <- nfs ; core <- cli (bin only) ; vfs-test is a dev-dependency of fuse.
WORKSPACE = [
    pkg("cowfs-core", "crates/cowfs-core"),
    pkg("cowfs-vfs", "crates/cowfs-vfs", deps=["cowfs-core"]),
    pkg("cowfs-vfs-test", "crates/cowfs-vfs-test", deps=["cowfs-vfs"]),
    pkg("cowfs-fuse", "crates/cowfs-fuse", deps=["cowfs-vfs"], dev=["cowfs-vfs-test"]),
    pkg("cowfs-nfs", "crates/cowfs-nfs", deps=["cowfs-vfs", "serde"]),
    pkg("cowfs-cli", "crates/cowfs-cli", deps=["cowfs-core"], lib=False),
]


def select(files, packages=WORKSPACE):
    return sel.select(files, packages, WORKSPACE_ROOT)


WORKSPACE_ROOT = "/ws"


class Selection(unittest.TestCase):
    def test_edge_crate_selects_only_itself(self):
        r = select(["crates/cowfs-nfs/src/lib.rs"])
        self.assertFalse(r.full)
        self.assertEqual(r.crates, ["cowfs-nfs"])
        self.assertEqual(r.filterset, "rdeps(=cowfs-nfs)")

    def test_core_crate_selects_every_dependent_including_dev_edges(self):
        r = select(["crates/cowfs-core/src/lib.rs"])
        self.assertFalse(r.full)
        self.assertEqual(r.crates, ["cowfs-core"])
        self.assertEqual(
            r.tested, ["cowfs-cli", "cowfs-core", "cowfs-fuse", "cowfs-nfs", "cowfs-vfs", "cowfs-vfs-test"]
        )

    def test_dev_dependency_edge_is_followed(self):
        r = select(["crates/cowfs-vfs-test/src/lib.rs"])
        self.assertEqual(r.tested, ["cowfs-fuse", "cowfs-vfs-test"])

    def test_two_crates_or_their_filtersets(self):
        r = select(["crates/cowfs-nfs/a.rs", "crates/cowfs-cli/b.rs", "crates/cowfs-nfs/c.rs"])
        self.assertEqual(r.crates, ["cowfs-cli", "cowfs-nfs"])
        self.assertEqual(r.filterset, "rdeps(=cowfs-cli) | rdeps(=cowfs-nfs)")

    def test_doc_packages_are_lib_crates_of_the_tested_set_only(self):
        r = select(["crates/cowfs-core/src/lib.rs"])
        self.assertNotIn("cowfs-cli", r.doc_packages)  # bin-only: `cargo test --doc -p` would error
        self.assertEqual(r.doc_packages, ["cowfs-core", "cowfs-fuse", "cowfs-nfs", "cowfs-vfs", "cowfs-vfs-test"])

    def test_crate_with_longest_prefix_wins(self):
        packages = WORKSPACE + [pkg("inner", "crates/cowfs-core/inner")]
        r = select(["crates/cowfs-core/inner/src/lib.rs"], packages)
        self.assertEqual(r.crates, ["inner"])

    def test_prefix_must_be_a_whole_directory(self):
        # crates/cowfs-vfs-test/... must not be claimed by crates/cowfs-vfs
        r = select(["crates/cowfs-vfs-test/src/lib.rs"])
        self.assertEqual(r.crates, ["cowfs-vfs-test"])

    def test_markdown_inside_a_crate_still_selects_the_crate(self):
        # a README can be include_str!'d into a doctest
        r = select(["crates/cowfs-nfs/README.md"])
        self.assertEqual(r.crates, ["cowfs-nfs"])


class ForcedFull(unittest.TestCase):
    def assertFull(self, files, why=None):
        r = select(files)
        self.assertTrue(r.full, files)
        self.assertTrue(r.reason)
        if why:
            self.assertIn(why, r.reason)

    def test_workflow_and_nextest_config(self):
        self.assertFull([".github/workflows/ci.yml"], ".github/workflows/ci.yml")
        self.assertFull([".config/nextest.toml"])

    def test_lockfile_root_manifest_and_scripts(self):
        self.assertFull(["Cargo.lock"])
        self.assertFull(["Cargo.toml"])
        self.assertFull(["scripts/verify-core.sh"])

    def test_unknown_path_forces_full(self):
        self.assertFull(["import-e2e.sh"])
        self.assertFull(["spikes/x/src/main.rs"])

    def test_one_unplaceable_file_among_crate_files_forces_full(self):
        self.assertFull(["crates/cowfs-nfs/src/lib.rs", "Cargo.lock"], "Cargo.lock")

    def test_empty_diff_forces_full(self):
        self.assertFull([], "no changed files")

    def test_under_crates_but_not_a_crate_forces_full(self):
        self.assertFull(["crates/brand-new-dir/src/lib.rs"])

    def test_crate_directory_rename_forces_full(self):
        # git diff --name-only --no-renames lists the old path too, and the old directory is no longer a
        # crate in the metadata of the new tree, so the move is not placed and the run goes full.
        renamed = [p for p in WORKSPACE if p["name"] != "cowfs-nfs"] + [pkg("cowfs-nfs", "crates/cowfs-nfs2", deps=["cowfs-vfs"])]
        r = select(["crates/cowfs-nfs/src/lib.rs", "crates/cowfs-nfs2/src/lib.rs"], renamed)
        self.assertTrue(r.full)
        self.assertIn("crates/cowfs-nfs/src/lib.rs", r.reason)


class Ignored(unittest.TestCase):
    def test_docs_only_selects_nothing(self):
        r = select(["docs/design.md", "docs/reviews/x.md", "README.md", "notes/todo.md"])
        self.assertFalse(r.full)
        self.assertEqual(r.crates, [])
        self.assertEqual(r.filterset, "none()")
        self.assertEqual(r.doc_packages, [])

    def test_bench_only_selects_nothing(self):
        r = select(["bench/harness.py", "bench/test_namespaces.py"])
        self.assertFalse(r.full)
        self.assertEqual(r.crates, [])

    def test_ignored_files_do_not_widen_a_crate_selection(self):
        r = select(["docs/a.md", "bench/b.py", "crates/cowfs-nfs/src/lib.rs"])
        self.assertEqual(r.crates, ["cowfs-nfs"])


class Outputs(unittest.TestCase):
    def test_github_output_lines(self):
        full = sel.github_output(select(["Cargo.lock"]))
        self.assertIn("mode=full", full)
        self.assertIn("filterset=", full)
        part = sel.github_output(select(["crates/cowfs-nfs/a.rs"]))
        self.assertIn("mode=filtered", part)
        self.assertIn("filterset=rdeps(=cowfs-nfs)", part)
        self.assertIn("doc_args=-p cowfs-nfs", part)

    def test_summary_names_crates_filterset_and_reason(self):
        text = sel.summary(select(["crates/cowfs-core/a.rs"]))
        for needle in ["cowfs-core", "rdeps(=cowfs-core)", "cowfs-fuse"]:
            self.assertIn(needle, text)
        self.assertIn("Cargo.lock", sel.summary(select(["Cargo.lock"])))


class Cli(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run(["python3", str(ROOT / "scripts/select-tests.py"), *args], capture_output=True, text=True, cwd=ROOT)

    def test_non_pull_request_events_are_always_full(self):
        for event in ["push", "workflow_dispatch", "schedule"]:
            with self.subTest(event=event):
                r = self.run_cli("--event", event, "--base", "HEAD", "--head", "HEAD", "--print")
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertIn("mode=full", r.stdout)
                self.assertIn(event, r.stdout + r.stderr)

    def test_unresolvable_base_is_an_error_not_a_silent_full(self):
        r = self.run_cli("--event", "pull_request", "--base", "no-such-ref", "--head", "HEAD", "--print")
        self.assertNotEqual(r.returncode, 0)

    def test_real_workspace_places_every_crate_directory(self):
        meta = sel.cargo_metadata()
        for p in meta["packages"]:
            d = sel.crate_dir(p, meta["workspace_root"])
            with self.subTest(crate=p["name"]):
                self.assertEqual(sel.select([d + "/src/lib.rs"], meta["packages"], meta["workspace_root"]).crates, [p["name"]])


if __name__ == "__main__":
    unittest.main()
