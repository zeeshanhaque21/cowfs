"""Mount-namespace canonical path tests for scripts/cowfs-ns-run.sh (issue #17).

Run: python3 -m unittest discover -s bench -v

What is proved, on a host where a namespace can be created:
the command runs with the source directory at the canonical path and in its own mount namespace,
the caller's own mounts are unchanged after a clean exit, a failing exit and a signal, the child's
exit code and signal are forwarded, and no argument is ever reinterpreted.

What is proved on every host, namespace or not:
the helper never falls back to running the command at its raw path. Off Linux, or with the
namespace refused, it reports UNMEASURABLE, exits 77 and leaves the command unrun.

A host that denies namespaces proves the refusal path and nothing else, so the run says which of
the two branches it took instead of counting a refusal as isolation.
"""

import json
import os
import signal
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
HELPER = REPO / "scripts" / "cowfs-ns-run.sh"

EXIT_UNMEASURABLE = 77
EXIT_USAGE = 2

# A child that mounts a tmpfs over its own cwd, then reports how many mounts it can see. The count
# is the positive control: without it, "the caller's mounts did not change" would also hold for a
# child that never managed to mount anything.
MOUNT_TMPFS = """
import os, subprocess, sys
subprocess.run(["mount", "-t", "tmpfs", "tmpfs", os.getcwd()], check=True)
with open(sys.argv[1], "w") as fh:
    fh.write(str(len(open("/proc/self/mountinfo").readlines())))
"""

REPORT_CWD_AND_NS = """
import os, sys
sys.stdout.write(os.getcwd() + "\\n" + os.readlink("/proc/self/ns/mnt") + "\\n")
"""

WRITE_ARGV = """
import json, sys
with open(sys.argv[1], "w") as fh:
    json.dump(sys.argv[2:], fh)
"""

_PROBE = None


def probe_namespace(tmp_root):
    """Run the helper once and report whether a namespace is available here."""
    global _PROBE
    if _PROBE is None:
        src = tmp_root / "probe-src"
        canonical = tmp_root / "probe-canonical"
        src.mkdir()
        canonical.mkdir()
        out = subprocess.run(
            [str(HELPER), "--src", str(src), "--canonical", str(canonical), "--", sys.executable, "-c", "pass"],
            capture_output=True,
            text=True,
            timeout=120,
        )
        _PROBE = (out.returncode == 0, out.stderr.strip())
    return _PROBE


def mountinfo():
    return Path("/proc/self/mountinfo").read_text()


def run(src, canonical, argv, ns_mode=None):
    cmd = [str(HELPER)]
    if ns_mode:
        cmd += ["--ns-mode", ns_mode]
    cmd += ["--src", str(src), "--canonical", str(canonical), "--"] + list(argv)
    return subprocess.run(cmd, capture_output=True, text=True, timeout=300)


class NsCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="cowfs-ns-")
        cls.root = Path(cls.tmp.name)
        root = cls.root
        cls.src = root / "src"
        cls.canonical = root / "canonical"
        cls.src.mkdir()
        cls.canonical.mkdir()
        (cls.src / "a.txt").write_text("hi\n")
        first = _PROBE is None
        cls.namespace, cls.probe_stderr = probe_namespace(root)
        if first:
            if cls.namespace:
                note = "a mount namespace was available, so the isolation matrix ran"
            else:
                note = (
                    "no mount namespace on this host, so only the refusal matrix ran. "
                    f"Reason: {cls.probe_stderr}"
                )
            print(f"cowfs-ns-run: {note}", flush=True)

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def child(self, script, *args):
        return [sys.executable, "-c", script, *args]

    def require_namespace(self):
        if not type(self).namespace:
            self.skipTest("no mount namespace on this host: " + type(self).probe_stderr)


@unittest.skipUnless(os.name == "posix", "the helper under test is a POSIX shell script")
class Refusals(NsCase):
    """What holds on every host, including one that cannot create a namespace."""

    def test_no_namespace_never_falls_back_to_the_raw_path(self):
        # The privileged route needs CAP_SYS_ADMIN, so on an unprivileged host it is refused and
        # the helper must say so instead of running the command anyway. Where the route does work,
        # the child still has to see the canonical path. Either way the raw path is never what a
        # child sees, which is the whole reason to fail closed.
        out = run(
            self.src,
            self.canonical,
            self.child(REPORT_CWD_AND_NS),
            ns_mode="privileged",
        )
        if out.returncode == 0:
            self.assertEqual(out.stdout.splitlines()[0], str(self.canonical))
        else:
            self.assertEqual(out.returncode, EXIT_UNMEASURABLE)
            self.assertIn("UNMEASURABLE", out.stderr)
        self.assertNotIn(str(self.src), out.stdout)
        self.assertEqual(sorted(self.canonical.iterdir()), [])

    @unittest.skipIf(sys.platform.startswith("linux"), "the control for a platform without namespaces")
    def test_off_linux_is_unmeasurable(self):
        out = run(self.src, self.canonical, self.child("print('RAN')"))
        self.assertEqual(out.returncode, EXIT_UNMEASURABLE)
        self.assertIn("UNMEASURABLE", out.stderr)
        self.assertNotIn("RAN", out.stdout)

    def test_missing_canonical_directory_refuses_before_running_the_command(self):
        out = run(self.src, self.root / "absent", self.child("print('RAN')"))
        self.assertEqual(out.returncode, EXIT_USAGE)
        self.assertEqual(out.stdout, "")
        self.assertIn("not an existing directory", out.stderr)

    def test_canonical_inside_source_refuses(self):
        out = run(self.src, self.src, self.child("print('RAN')"))
        self.assertEqual(out.returncode, EXIT_USAGE)
        self.assertEqual(out.stdout, "")

    def test_relative_source_refuses(self):
        out = run("src", self.canonical, self.child("print('RAN')"))
        self.assertEqual(out.returncode, EXIT_USAGE)
        self.assertEqual(out.stdout, "")

    def test_no_command_refuses(self):
        out = subprocess.run(
            [str(HELPER), "--src", str(self.src), "--canonical", str(self.canonical)],
            capture_output=True,
            text=True,
            timeout=120,
        )
        self.assertEqual(out.returncode, EXIT_USAGE)
        self.assertEqual(out.stdout, "")

    def test_unknown_ns_mode_refuses(self):
        out = run(self.src, self.canonical, self.child("print('RAN')"), ns_mode="container")
        self.assertEqual(out.returncode, EXIT_USAGE)
        self.assertEqual(out.stdout, "")


@unittest.skipUnless(os.name == "posix", "the helper under test is a POSIX shell script")
class Isolation(NsCase):
    """What holds where a namespace can be created."""

    def test_command_runs_in_the_canonical_path_in_its_own_namespace(self):
        self.require_namespace()
        out = run(self.src, self.canonical, self.child(REPORT_CWD_AND_NS))
        self.assertEqual(out.returncode, 0, out.stderr)
        cwd, child_ns = out.stdout.splitlines()
        self.assertEqual(cwd, str(self.canonical))
        # Read from the child's own namespace, so the isolation claim is measured and not inferred.
        self.assertNotEqual(child_ns, os.readlink("/proc/self/ns/mnt"))

    def test_source_is_visible_through_the_canonical_path_only(self):
        self.require_namespace()
        out = run(self.src, self.canonical, self.child("print(open('a.txt').read(), end='')"))
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout, "hi\n")
        # Outside the namespace the canonical directory is untouched, so several agents can share
        # one canonical path without any of them seeing another's tree.
        self.assertEqual(sorted(self.canonical.iterdir()), [])
        self.assertEqual(sorted(p.name for p in self.src.iterdir()), ["a.txt"])

    def test_exit_code_is_forwarded(self):
        self.require_namespace()
        out = run(self.src, self.canonical, self.child("import sys; sys.exit(42)"))
        self.assertEqual(out.returncode, 42)

    def test_signal_is_forwarded(self):
        self.require_namespace()
        out = run(
            self.src,
            self.canonical,
            self.child("import os, signal; os.kill(os.getpid(), signal.SIGTERM)"),
        )
        self.assertEqual(out.returncode, -signal.SIGTERM)

    def test_arguments_are_never_reinterpreted(self):
        self.require_namespace()
        tricky = ["a b", "*", "$HOME", "; rm -rf /", "--src=/etc", "quote's", "--", ""]
        recorded = self.root / "argv.json"
        out = run(self.src, self.canonical, self.child(WRITE_ARGV, str(recorded), *tricky))
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(json.loads(recorded.read_text()), tricky)

    def test_caller_mounts_are_unchanged_after_a_clean_exit(self):
        self.require_namespace()
        before = mountinfo()
        marker = self.root / "clean.marker"
        out = run(self.src, self.canonical, self.child(MOUNT_TMPFS, str(marker)))
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertGreater(int(marker.read_text()), len(before.splitlines()))
        self.assertEqual(mountinfo(), before)

    def test_caller_mounts_are_unchanged_after_a_signal(self):
        self.require_namespace()
        before = mountinfo()
        marker = self.root / "signal.marker"
        out = run(
            self.src,
            self.canonical,
            self.child(
                MOUNT_TMPFS + "import os, signal\nos.kill(os.getpid(), signal.SIGKILL)\n",
                str(marker),
            ),
        )
        self.assertEqual(out.returncode, -signal.SIGKILL)
        self.assertGreater(int(marker.read_text()), len(before.splitlines()))
        self.assertEqual(mountinfo(), before)

    def test_caller_mounts_are_unchanged_after_a_failing_exit(self):
        self.require_namespace()
        before = mountinfo()
        marker = self.root / "fail.marker"
        out = run(
            self.src,
            self.canonical,
            self.child(MOUNT_TMPFS + "import sys\nsys.exit(3)\n", str(marker)),
        )
        self.assertEqual(out.returncode, 3)
        self.assertGreater(int(marker.read_text()), len(before.splitlines()))
        self.assertEqual(mountinfo(), before)

    @unittest.skipIf(os.geteuid() == 0, "a root caller cannot tell a namespace uid from its own")
    def test_writes_inside_the_namespace_land_as_the_real_user(self):
        # The user namespace maps the caller to uid 0 inside. A file created through the namespace
        # must still belong to the real uid outside, or the namespace would fill the store with
        # files the owning agent could not rewrite.
        self.require_namespace()
        out = run(self.src, self.canonical, self.child("open('made.txt', 'w').close()"))
        self.assertEqual(out.returncode, 0, out.stderr)
        made = self.src / "made.txt"
        self.assertTrue(made.exists(), "the write did not land in the source tree")
        self.assertEqual(made.stat().st_uid, os.getuid())
        made.unlink()


if __name__ == "__main__":
    unittest.main()