#!/usr/bin/env python3
"""Mutation controls for the fsx gate: inputs that must not be accepted as an arm.

    mutations.py --runner PATH --config PATH --work DIR [--fsx PATH]
                 --native-root DIR --cowfs-root DIR [--second-arm DIR] [--report FILE]

Every control is a way of lying to the runner. The point of the list is that each one either
changes the arm's identity, changes what fsx is allowed to excuse, or fakes work that was never
done. A control that comes back PASS is a defect in the gate, not a control that failed.

None of this mounts anything or needs root: the controls that would need a real mount are marked
as such and are run by hand against a private one, recorded in the report.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys


def run(argv):
    proc = subprocess.run(argv, capture_output=True, text=True, timeout=1800)
    return proc.returncode, proc.stdout, proc.stderr


def synthetic_child(path, ops=200, ops_text=None, write_bytes=0, write_stream=False):
    """A program that looks like fsx from the outside: it prints an A-OK line and exits 0."""
    stream = "".join("write 0x%x 0x10\\n" % (i * 16) for i in range(ops)) if write_stream else ""
    body = "#!/usr/bin/env python3\nimport sys, os\n"
    body += "d = sys.argv[sys.argv.index('-P') + 1]\n"
    body += "f = sys.argv[-1]\n"
    body += "os.makedirs(d, exist_ok=True)\n"
    if write_bytes:
        body += ("with open(f, 'wb') as h:\n"
                 "    h.write(bytes((i * 7) %% 256 for i in range(%d)))\n" % write_bytes)
    else:
        body += "open(f, 'a').close()\n"
    if write_stream:
        body += "open(os.path.join(d, 'fsx.dat.fsxops'), 'w').write(%r)\n" % stream
    body += "print('All %d operations completed A-OK!')\n" % ops
    body += "print('LOG DUMP (%d total operations):')\n" % ops
    body += "sys.exit(0)\n"
    with open(path, "w") as f:
        f.write(body)
    os.chmod(path, 0o755)
    return path


def control_second_arm_is_not_cowfs(args, work):
    """5d: the cowfs arm pointed at a tmpfs that is not the filesystem under test."""
    arm = os.path.join(work, "not-cowfs")
    os.makedirs(arm, exist_ok=True)
    code, out, err = run([sys.executable, args.runner, "--native-root", args.native_root,
                          "--cowfs-root", arm, "--fsx-bin", args.fsx, "--config", args.config,
                          "--out", os.path.join(work, "out-5d"), "--mode", "full",
                          "--label", "control-5d-tmpfs-as-cowfs"])
    return {"control": "5d", "name": "a tmpfs labelled cowfs", "exit": code,
            "expect": "refused before any case",
            "status": "ok" if code == 3 and "cowfs" in out else "LEAK",
            "output": out.strip().splitlines()[:6] + err.strip().splitlines()[:2]}


def control_native_arm_is_the_mount(args, work):
    """5f: the control pointed at the filesystem under test is not a control."""
    code, out, err = run([sys.executable, args.runner, "--native-root", args.cowfs_root,
                          "--cowfs-root", args.cowfs_root, "--fsx-bin", args.fsx,
                          "--config", args.config, "--out", os.path.join(work, "out-5f"),
                          "--mode", "smoke",
                          "--daemon-pid-file", args.daemon_pid_file or "",
                          "--label", "control-5f-native-is-the-mount"])
    return {"control": "5f", "name": "the native arm pointed at the cowfs mount", "exit": code,
            "expect": "refused: the control is the thing under test",
            "status": "ok" if code == 3 and "the thing under test" in out else "LEAK",
            "output": out.strip().splitlines()[:6] + err.strip().splitlines()[:2]}


def control_synthetic_child(args, work):
    """16: an executable that prints the A-OK line is not the pinned tool."""
    if not args.fsx:
        return {"control": "16", "name": "synthetic child as the arm", "skipped": "no --fsx given"}
    child = synthetic_child(os.path.join(work, "fake-fsx"), ops=200, write_bytes=4096,
                            write_stream=True)
    code, out, err = run([sys.executable, args.runner, "--native-root", args.native_root,
                          "--cowfs-root", args.cowfs_root, "--fsx-bin", child,
                          "--config", args.config, "--out", os.path.join(work, "out-16"),
                          "--mode", "smoke",
                          "--daemon-pid-file", args.daemon_pid_file or "",
                          "--label", "control-16-synthetic-child"])
    return {"control": "16", "name": "a synthetic child instead of the pinned fsx", "exit": code,
            "expect": "refused: the binary is not the manifest's",
            "status": "ok" if code == 3 and "manifest" in out else "LEAK",
            "output": out.strip().splitlines()[:6] + err.strip().splitlines()[:2]}


def control_reused_attempt_dir(args, work):
    """19: stale bytes in a case directory must not satisfy a new invocation.

    The fake arm is the real cowfs mount, so the arm attestation passes and the refusal under test
    is the fresh attempt directory rather than the filesystem type.
    """
    if not args.fsx:
        return {"control": "19", "name": "reused attempt directory", "skipped": "no --fsx given"}
    # The stale directory lives inside the real mount, so only the attempt-directory guard can
    # refuse this run.
    stale = os.path.join(args.cowfs_root, ".fsx-gate", "stale-case")
    os.makedirs(stale, exist_ok=True)
    with open(os.path.join(stale, "fsx.dat"), "wb") as f:
        f.write(b"stale bytes from an earlier attempt" * 100)
    with open(os.path.join(stale, "fsx.dat.fsxops"), "w") as f:
        f.write("".join("write 0x%x 0x10\n" % (i * 16) for i in range(200)))
    before = sorted(os.listdir(os.path.join(args.cowfs_root, ".fsx-gate")))
    child = synthetic_child(os.path.join(work, "empty-child"), ops=200, write_bytes=0)
    code, out, err = run([sys.executable, args.runner, "--native-root", args.native_root,
                          "--cowfs-root", args.cowfs_root, "--fsx-bin", child,
                          "--config", args.config, "--out", os.path.join(work, "out-19"),
                          "--mode", "smoke", "--allow-unpinned-fsx",
                          "--daemon-pid-file", args.daemon_pid_file or "",
                          "--label", "control-19-reused-attempt"])
    after = sorted(os.listdir(os.path.join(args.cowfs_root, ".fsx-gate")))
    # The child writes no bytes, so its data file is empty and the gate must refuse the empty
    # result. Nothing is deleted, so the stale directory is still there and still unread.
    refused_empty = code == 1 and "empty" in out
    # And the fresh attempt must be a new directory, not the stale one.
    fresh_dirs = [d for d in after if d not in before]
    return {"control": "19", "name": "a child that writes nothing over stale bytes", "exit": code,
            "expect": "an empty result is refused and the stale directory is not reused",
            "status": "ok" if refused_empty and "stale-case" not in fresh_dirs else "LEAK",
            "attempt_dirs_before": before, "attempt_dirs_after": after,
            "stale_preserved": "stale-case" in after,
            "output": out.strip().splitlines()[:8] + err.strip().splitlines()[:2]}


def control_restart_noop(args, work):
    """17: a restart hook that does nothing is not a restart."""
    if not args.fsx:
        return {"control": "17", "name": "a no-op restart hook", "skipped": "no --fsx given"}
    code, out, err = run([sys.executable, args.runner, "--native-root", args.native_root,
                          "--cowfs-root", args.cowfs_root, "--fsx-bin", args.fsx,
                          "--config", args.config, "--out", os.path.join(work, "out-17"),
                          "--mode", "smoke", "--label", "control-17-noop-restart",
                          "--daemon-pid-file", args.daemon_pid_file or "",
                          "--restart-cmd", "/bin/true"])
    leaked = code == 0
    return {"control": "17", "name": "a restart hook that does nothing", "exit": code,
            "expect": "failed: pid and start time are unchanged",
            "status": "LEAK" if leaked else "ok",
            "output": out.strip().splitlines()[-6:] + err.strip().splitlines()[:2]}


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--runner", required=True)
    p.add_argument("--config", required=True)
    p.add_argument("--work", required=True)
    p.add_argument("--fsx")
    p.add_argument("--native-root", required=True)
    p.add_argument("--cowfs-root", required=True)
    p.add_argument("--daemon-pid-file")
    p.add_argument("--second-arm", help="a directory that is a mount but not a cowfs one")
    p.add_argument("--report")
    args = p.parse_args(argv)
    work = os.path.abspath(args.work)
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work, exist_ok=True)

    results = []
    results.append(control_second_arm_is_not_cowfs(args, work))
    if args.fsx:
        results.append(control_native_arm_is_the_mount(args, work))
    results.append(control_synthetic_child(args, work))
    results.append(control_reused_attempt_dir(args, work))
    if args.daemon_pid_file and os.path.exists(args.daemon_pid_file):
        results.append(control_restart_noop(args, work))
    else:
        results.append({"control": "17", "name": "a no-op restart hook",
                        "skipped": "no --daemon-pid-file that exists"})

    leaks = [r for r in results if r.get("status") == "LEAK"]
    report = {"controls": results, "leaks": len(leaks)}
    if args.report:
        with open(args.report, "w") as f:
            json.dump(report, f, indent=2, sort_keys=True)
    for r in results:
        if "skipped" in r:
            print("control %s SKIPPED: %s (%s)" % (r["control"], r["name"], r["skipped"]))
            continue
        print("control %s exit %s: %s" % (r["control"], r["exit"],
                                          "ok" if r["status"] == "ok" else "LEAK"))
        for line in r.get("output", []):
            print("    %s" % line)
    print("%d leak(s) of %d controls" % (len(leaks), len(results)))
    return 1 if leaks else 0


if __name__ == "__main__":
    sys.exit(main())
