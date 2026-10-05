#!/usr/bin/env python3
"""Direct command-line exit codes for every verdict kind, measured rather than asserted.

    exit-taxonomy.py --runner PATH --config PATH --work DIR --fsx PATH
                     --native-root DIR --cowfs-root DIR [--daemon-pid-file PATH]
                     [--report FILE]

Nine invocations whose exit codes are read from the process, not from a predicate inside the
runner. A contract nobody has run is a comment.

Nothing here signals, restarts or unmounts anything. The mount is used as it stands.

    exit 0  PASS           a real matched pair on both arms
    exit 1  FAIL           a divergence, an over-budget run, or a plan the cap cannot hold
    exit 2  UNMEASURABLE   an operation the filesystem does not have
    exit 3  INVALID        the tool pin, an arm that is not a cowfs mount, a malformed invocation
"""

import argparse
import json
import os
import subprocess
import sys


def child(path, ops, streams, sizes):
    """A program that looks like fsx from the outside.

    streams maps an arm to the op names it records, so a control can put a fabricated divergence
    at a chosen operation. sizes maps an arm to the bytes it writes, so a control can make one arm
    breach a budget the other fits inside.
    """
    # The runner probes the binary once with no arguments to read its usage text and confirm it
    # behaves like fsx. A child that treated its own path as its data file would write over itself
    # during that probe and then be unexecutable, which is how the first version of this control
    # produced an Exec format error instead of the divergence it was meant to produce.
    body = ["#!/usr/bin/env python3", "import os, sys",
            "if len(sys.argv) < 4:",
            "    print('usage: fake-fsx [-S seed -N ops] [-l max] [-o maxop] [-P dir] file')",
            "    sys.exit(90)",
            "data = sys.argv[-1]",
            "# The attempt directory name carries the arm, so one child can record a different",
            "# stream per arm, which is what a divergence control needs.",
            "arm = 'cowfs' if 'cowfs' in data.split('/')[-2] else 'native'",
            "arm = os.environ.get('FSX_GATE_ARM', arm)",
            "sizes = %r" % sizes, "ops = %r" % ops, "streams = %r" % streams,
            "size = sizes.get(arm, sizes.get('*', 0))",
            "stream = streams.get(arm, streams.get('*', []))",
            "with open(data, 'wb') as h:",
            "    h.write(bytes((i * 7) % 256 for i in range(size)))",
            "with open(data + '.fsxops', 'w') as h:",
            "    h.write(''.join('%s 0x%x 0x10\\n' % (name, i * 16) for i, name in enumerate(stream)))",
            "print('All %d operations completed A-OK!' % ops)",
            "print('LOG DUMP (%d total operations):' % ops)",
            "sys.exit(0)"]
    with open(path, "w") as f:
        f.write("\n".join(body) + "\n")
    os.chmod(path, 0o755)
    return path


def invoke(runner, config, out, native, cowfs, fsx, extra, env_arm=None):
    env = dict(os.environ)
    if env_arm:
        env["FSX_GATE_ARM"] = env_arm
    argv = [sys.executable, runner, "--native-root", native, "--cowfs-root", cowfs,
            "--fsx-bin", fsx, "--config", config, "--out", out] + list(extra)
    proc = subprocess.run(argv, capture_output=True, text=True, timeout=3600, env=env)
    return proc


def first_line(text):
    """The gate's own verdict line: the last line that names a status, or the first non-empty."""
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    for line in reversed(lines):
        if line.startswith(("PASS", "FAIL", "UNMEASURABLE", "INVALID")):
            return line
    return lines[0] if lines else ""


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--runner", required=True)
    p.add_argument("--config", required=True)
    p.add_argument("--work", required=True)
    p.add_argument("--fsx", required=True)
    p.add_argument("--native-root", required=True)
    p.add_argument("--cowfs-root", required=True)
    p.add_argument("--daemon-pid-file")
    p.add_argument("--report")
    args = p.parse_args(argv)
    work = os.path.abspath(args.work)
    os.makedirs(work, exist_ok=True)

    common = ["--daemon-pid-file", args.daemon_pid_file or "",
              "--expect-backend", "core", "--expect-native-fstype", "ext4"]
    rows = []

    def run(name, expect, note, out_name, extra, fsx=None, env_arm=None, config=None):
        out = os.path.join(work, out_name)
        proc = invoke(args.runner, config or args.config, out, args.native_root, args.cowfs_root,
                      fsx or args.fsx, extra, env_arm)
        ok = proc.returncode == expect
        rows.append({"case": name, "expected_exit": expect, "exit": proc.returncode,
                     "match": ok, "first_line": first_line(proc.stdout or proc.stderr),
                     "note": note,
                     "argv": [sys.executable, args.runner] + extra})
        print("%-34s expected %s got %s  %s  %s"
              % (name, expect, proc.returncode, "OK" if ok else "MISMATCH",
                 first_line(proc.stdout or proc.stderr)[:110]))
        return proc

    # 0: a real matched pair, both arms, the pinned tool.
    run("pass, matched pair", 0, "smoke seed 1, 200 ops, real pinned fsx, real mount",
        "out-pass", common + ["--mode", "smoke", "--seeds", "1"])
    # 2: a capability the filesystem does not have, so the arms did different work.
    run("unmeasurable, capability", 2,
        "full seed 1, the mount has no fallocate, so the two arms' streams part company at it",
        "out-unmeasurable", common + ["--mode", "full", "--seeds", "1"])
    # 3: the tool is not the one the manifest names.
    run("invalid, tool pin", 3, "a binary that is not the approved fsx",
        "out-pin", common + ["--mode", "smoke"], fsx="/bin/sh")
    # 3: the arm is not a cowfs mount.
    notcowfs = os.path.join(work, "not-cowfs")
    os.makedirs(notcowfs, exist_ok=True)
    run("invalid, arm not a cowfs mount", 3, "the cowfs arm pointed at a plain directory",
        "out-arm", common + ["--mode", "smoke", "--cowfs-root", notcowfs, "--fsx-bin", "/bin/sh",
                             "--allow-unpinned-fsx"])
    # 3: a malformed invocation.
    run("invalid, undeclared mode", 3, "--mode names a mode the config does not declare",
        "out-usage", common + ["--mode", "no-such-mode"])
    # 1: a divergence, with a child whose recorded streams part company at an operation nothing
    # says the filesystem lacks.
    mix = ["write", "read", "mapread", "mapwrite", "truncate"]
    divergent = child(os.path.join(work, "divergent-fsx"), 200,
                      {"native": list(mix), "cowfs": mix[:1] + ["write"]},
                      {"*": 4096})
    run("fail, divergence at a read", 1,
        "identical for op 0, then the cowfs arm records write where native records read, and "
        "nothing says this filesystem lacks write",
        "out-divergence", common + ["--mode", "smoke", "--allow-unpinned-fsx"], fsx=divergent)
    # 1: the divergence rule itself, in a mode that permits a capability difference. Both arms
    # record every operation the mode requires, so nothing here is a missing-op report: the two
    # streams part company at operation 3, where native records mapwrite and cowfs records write,
    # and nothing says this filesystem lacks write.
    swapped = child(os.path.join(work, "swapped-fsx"), 200,
                    {"native": list(mix),
                     "cowfs": ["write", "read", "mapread", "write", "truncate"]},
                    {"*": 4096})
    run("fail, divergence in a capability mode", 1,
        "full mode, both arms record all five required ops, first divergence is mapwrite against "
        "write and no capability explains it",
        "out-divergence-full", common + ["--mode", "full", "--seeds", "1", "--allow-unpinned-fsx"],
        fsx=swapped)

    # 1: a plan the per-arm total cannot hold, refused before any child exists.
    reduced = os.path.join(work, "reduced.json")
    with open(args.config) as f:
        gate_json = json.load(f)
    gate_json["caps"]["max_bytes_written_per_arm"] = 1000
    with open(reduced, "w") as f:
        json.dump(gate_json, f)
    run("fail, plan over the cap", 1,
        "the declared batch cannot fit a per-arm total of 1000, so it is refused before a child",
        "out-plan", common + ["--mode", "smoke"], config=reduced)

    # 1 at run time: an arm that breaches a budget the plan fits inside. A child that ignores its
    # own -l is the only way to reach it, which is exactly why the guard exists.
    over = 400000
    tight = os.path.join(work, "tight.json")
    gate_json["caps"]["max_bytes_written_per_arm"] = 262144
    with open(tight, "w") as f:
        json.dump(gate_json, f)
    # One child shape, three byte profiles, so each arm can be over on its own.
    for name, sizes in (("native only", {"native": over, "cowfs": 1024}),
                        ("cowfs only", {"native": 1024, "cowfs": over}),
                        ("both arms", {"native": over, "cowfs": over})):
        path = child(os.path.join(work, "over-fsx"), 200, {"*": mix}, sizes)
        run("fail, over budget, " + name, 1,
            "the per-arm total of 262144 fits a plan of one file at the per-case maximum; this arm "
            "writes %d, which the child only reaches by ignoring its own -l" % over,
            "out-over-" + name.replace(" ", "-"),
            common + ["--mode", "smoke", "--seeds", "1", "--allow-unpinned-fsx"],
            fsx=path, config=tight)

    mismatches = [r for r in rows if not r["match"]]
    report = {"cases": rows, "mismatches": len(mismatches),
              "distinct_exits": sorted({r["exit"] for r in rows})}
    if args.report:
        with open(args.report, "w") as f:
            json.dump(report, f, indent=2, sort_keys=True)
    print("%d case(s), %d mismatch(es), exits seen %s"
          % (len(rows), len(mismatches), report["distinct_exits"]))
    return 1 if mismatches else 0


if __name__ == "__main__":
    sys.exit(main())
