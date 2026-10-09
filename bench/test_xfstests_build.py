#!/usr/bin/env python3
"""Tests for bench/xfstests_build.sh (issue 101). Run: python3 -m unittest discover -s bench -v

No network, no make: every case here must be refused before the build starts.
"""

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "xfstests_build.sh"


def git(cwd, *args):
    env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t", GIT_COMMITTER_NAME="t",
               GIT_COMMITTER_EMAIL="t@t")
    subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True, env=env)


def run(xfs, **env):
    return subprocess.run(["bash", str(SCRIPT), str(xfs)], capture_output=True, text=True,
                          env=dict(os.environ, **env), timeout=60)


class Pins(unittest.TestCase):
    def test_commit_pin_is_the_allowlist_tree_sha(self):
        allow = (HERE / "xfstests-allowlist.txt").read_text()
        self.assertRegex(allow, r"(?m)^tree_sha [0-9a-f]{40}$")
        src = SCRIPT.read_text()
        self.assertIn("xfstests-allowlist.txt", src)
        self.assertRegex(src, r"(?m)^TREE=3683cb11c7dde850a567042e89b4e35e5d082b8e\b")

    def test_suite_flags_are_not_altered(self):
        # issue 101 declined hand-written config.h; the build must be the suite's own make.
        src = SCRIPT.read_text()
        self.assertNotRegex(src, r"CFLAGS=|file-prefix-map|config\.h\s*>")

    def test_g5_meta_records_the_helper_digests_under_the_same_keys(self):
        root = (HERE / "g5_root.sh").read_text()
        build = SCRIPT.read_text()
        for key in ("fsstress_sha256", "fsx_sha256"):
            self.assertIn(f'echo "{key}=$(sha256sum "$XFS/ltp/{key[:-7]}"', root)
            self.assertIn(f'echo "{key}=$(sha256sum "$XFS/ltp/{key[:-7]}"', build)


class Refusals(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.xfs = Path(self.tmp.name) / "xfstests"

    def tearDown(self):
        self.tmp.cleanup()

    def seed(self):
        self.xfs.mkdir()
        git(self.xfs, "init", "-q")
        (self.xfs / "Makefile").write_text("default:\n\ttouch MAKE_RAN\n")
        git(self.xfs, "add", "Makefile")
        git(self.xfs, "commit", "-q", "-m", "not the pin")

    def test_present_tree_at_another_commit_is_refused_before_make(self):
        self.seed()
        r = run(self.xfs)
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertRegex(r.stderr, r"refusing: .* is at [0-9a-f]{40}, the pin is 3e1ee800")
        self.assertFalse((self.xfs / "MAKE_RAN").exists())

    def test_unverifiable_fetch_is_refused(self):
        # an upstream that does not have the pinned commit: the fetch fails, nothing builds
        r = run(self.xfs, XFSTESTS_UPSTREAM=str(Path(self.tmp.name) / "nowhere"))
        self.assertNotEqual(r.returncode, 0)
        self.assertFalse((self.xfs / "MAKE_RAN").exists())
        self.assertFalse(Path(str(self.xfs) + ".identity.txt").exists())


if __name__ == "__main__":
    unittest.main()
