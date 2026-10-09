#!/usr/bin/env python3
"""Per-PR test selection: which workspace crates do a pull request's changed files reach?

Only a pull_request run narrows anything; push to main and workflow_dispatch always run the full
set. Every changed file is placed in a crate by the longest crate-directory prefix. A file outside
every crate runs the full set unless it is docs/**, *.md or bench/** (bench has its own python
step). Anything the selector cannot place, including a path whose crate directory was renamed or
deleted, forces the full run: the selector may only ever err towards running more.

The nextest filterset is `package(=a) | package(=b)` over the closure: each changed crate plus
everything that depends on it (dev-dependencies included) or runs its binaries (RUNTIME_EDGES). `doc_packages` is the same closure restricted to crates with a lib
target, for `cargo test --doc -p`, which nextest does not run.

Usage: select-tests.py --event NAME --base REV --head REV
Writes key=value lines to $GITHUB_OUTPUT (stdout when unset) and a table to $GITHUB_STEP_SUMMARY
(stderr when unset). Exits non-zero on any error, so a broken selection fails the job and `check`.
"""
import argparse
import json
import os
import subprocess
import sys
from collections import namedtuple

Selection = namedtuple("Selection", "full reason crates tested doc_packages filterset build")

IGNORED_PREFIXES = ("docs/", "bench/")
IGNORED_SUFFIXES = (".md",)
# Runtime edges cargo cannot see: crate -> crates whose BINARIES its tests execute (found next to the
# test binary, so they must be built, and a change to them must rerun the crate's tests).
# scripts/test_select_tests.py scans crates/*/tests and fails when a test crate runs a workspace
# binary that is neither a cargo dependency nor listed here.
RUNTIME_EDGES = {
    "cowfs-treehouse": ("cowfs-cli", "cowfs-daemon"),
    "cowfs-daemon": ("cowfs-cli",),
}
LIB_KINDS = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}


def crate_dir(package, workspace_root):
    manifest = package["manifest_path"].rsplit("/Cargo.toml", 1)[0]
    return manifest.removeprefix(workspace_root.rstrip("/") + "/")


def ignored(path):
    return path.startswith(IGNORED_PREFIXES) or path.endswith(IGNORED_SUFFIXES)


def full(reason):
    return Selection(True, reason, [], [], [], "", [])


def select(files, packages, workspace_root):
    if not files:
        return full("no changed files found: refusing to select nothing")
    dirs = {crate_dir(p, workspace_root): p["name"] for p in packages}
    changed, unplaced = set(), []
    for f in files:
        # Longest directory prefix, so a crate nested in another crate's directory owns its own files.
        owner = max((d for d in dirs if f.startswith(d + "/")), key=len, default=None)
        if owner is not None:
            changed.add(dirs[owner])
        elif not ignored(f):
            unplaced.append(f)
    if unplaced:
        shown = ", ".join(unplaced[:5]) + (f" and {len(unplaced) - 5} more" if len(unplaced) > 5 else "")
        return full(f"changed outside any crate and not ignorable: {shown}")
    workspace = {p["name"] for p in packages}
    dependents = {name: set() for name in workspace}
    for p in packages:
        for dep in p["dependencies"]:  # build, dev and target-specific edges are all listed
            if dep["name"] in workspace:
                dependents[dep["name"]].add(p["name"])
        for needed in RUNTIME_EDGES.get(p["name"], ()):
            if needed in workspace:
                dependents[needed].add(p["name"])
    tested, todo = set(), sorted(changed)
    while todo:
        name = todo.pop()
        if name not in tested:
            tested.add(name)
            todo.extend(dependents[name])
    has_lib = {p["name"] for p in packages if any(set(t["kind"]) & LIB_KINDS for t in p["targets"])}
    crates = sorted(changed)
    # Explicit package() terms over the closure, not rdeps(): nextest cannot see RUNTIME_EDGES.
    filterset = " | ".join(f"package(={t})" for t in sorted(tested)) or "none()"
    # Crates to compile: the tested ones plus the crates whose binaries they run.
    build = set(tested)
    for name in tested:
        build.update(n for n in RUNTIME_EDGES.get(name, ()) if n in workspace)
    return Selection(False, "", crates, sorted(tested), sorted(tested & has_lib), filterset, sorted(build))


def github_output(s):
    return "\n".join(
        [
            f"mode={'full' if s.full else 'filtered'}",
            f"filterset={s.filterset}",
            "doc_args=" + " ".join(f"-p {p}" for p in s.doc_packages),
            # Build the tested crates and the crates whose binaries they run: `--workspace -E` would still compile every test binary.
            "pkg_args=" + " ".join(f"-p {p}" for p in s.build),
        ]
    )


def summary(s, all_crates=()):
    if s.full:
        return f"### Test selection: FULL\n\nReason: {s.reason}\n"
    return (
        "### Test selection: changed crates only\n\n"
        f"- changed crates: {', '.join(s.crates) or 'none'}\n"
        f"- crates tested (changed plus dependents): {', '.join(s.tested) or 'none'}\n"
        f"- crates skipped: {', '.join(sorted(set(all_crates) - set(s.tested))) or 'none'}\n"
        f"- nextest filterset: `{s.filterset}`\n"
        f"- crates compiled (tested plus those whose binaries they run): {', '.join(s.build) or 'none'}\n"
        f"- doctests: {', '.join(s.doc_packages) or 'none'}\n"
    )


def cargo_metadata():
    return json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"], text=True))


def git_ok(*args):
    return subprocess.run(["git", *args], capture_output=True).returncode == 0


def changed_files(base, head):
    """The files a pull request changes, or None when that cannot be established (the caller runs FULL).

    The checkout of a pull_request run is the merge commit: HEAD^1 is the base tip, HEAD^2 the PR head.
    Without a second parent HEAD^1 is the PR's own parent and the diff would shrink to the last commit,
    a silent false skip, so a head that is not a merge is refused.
    """
    if not git_ok("rev-parse", "--verify", "-q", f"{head}^2") or not git_ok("rev-parse", "--verify", "-q", base):
        return None
    # --no-renames lists both the old and the new path of a move, so a renamed crate directory is seen.
    out = subprocess.check_output(["git", "diff", "--name-only", "--no-renames", base, head], text=True)
    return [f for f in out.splitlines() if f]


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--event", required=True)
    ap.add_argument("--base", required=True)
    ap.add_argument("--head", required=True)
    args = ap.parse_args(argv)
    meta = cargo_metadata()
    if args.event != "pull_request":
        s = full(f"{args.event} event always runs the full set")
    else:
        files = changed_files(args.base, args.head)
        if files is None:
            s = full(f"{args.head} is not a merge commit or {args.base} is unresolvable: cannot diff the pull request")
        else:
            s = select(files, meta["packages"], meta["workspace_root"])
    text = summary(s, [p["name"] for p in meta["packages"]])
    with (open(os.environ["GITHUB_OUTPUT"], "a") if "GITHUB_OUTPUT" in os.environ else sys.stdout) as out:
        out.write(github_output(s) + "\n")
    with (open(os.environ["GITHUB_STEP_SUMMARY"], "a") if "GITHUB_STEP_SUMMARY" in os.environ else sys.stderr) as out:
        out.write(text)


if __name__ == "__main__":
    main(sys.argv[1:])
