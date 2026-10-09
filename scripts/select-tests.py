#!/usr/bin/env python3
"""Per-PR test selection: which workspace crates do a pull request's changed files reach?

Only a pull_request run narrows anything; push to main and workflow_dispatch always run the full
set. Every changed file is placed in a crate by the longest crate-directory prefix. A file outside
every crate runs the full set unless it is docs/**, *.md or bench/** (bench has its own python
step). Anything the selector cannot place, including a path whose crate directory was renamed or
deleted, forces the full run: the selector may only ever err towards running more.

The nextest filterset is `rdeps(=a) | rdeps(=b)`: each changed crate plus everything that depends on
it, dev-dependencies included. `doc_packages` is the same closure restricted to crates with a lib
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

Selection = namedtuple("Selection", "full reason crates tested doc_packages filterset")

IGNORED_PREFIXES = ("docs/", "bench/")
IGNORED_SUFFIXES = (".md",)
LIB_KINDS = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}


def crate_dir(package, workspace_root):
    manifest = package["manifest_path"].rsplit("/Cargo.toml", 1)[0]
    return manifest.removeprefix(workspace_root.rstrip("/") + "/")


def ignored(path):
    return path.startswith(IGNORED_PREFIXES) or path.endswith(IGNORED_SUFFIXES)


def full(reason):
    return Selection(True, reason, [], [], [], "")


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
    tested, todo = set(), sorted(changed)
    while todo:
        name = todo.pop()
        if name not in tested:
            tested.add(name)
            todo.extend(dependents[name])
    has_lib = {p["name"] for p in packages if any(set(t["kind"]) & LIB_KINDS for t in p["targets"])}
    crates = sorted(changed)
    filterset = " | ".join(f"rdeps(={c})" for c in crates) or "none()"
    return Selection(False, "", crates, sorted(tested), sorted(tested & has_lib), filterset)


def github_output(s):
    return "\n".join(
        [
            f"mode={'full' if s.full else 'filtered'}",
            f"filterset={s.filterset}",
            "doc_args=" + " ".join(f"-p {p}" for p in s.doc_packages),
            # Build only the tested crates: `--workspace -E` would still compile every test binary.
            "pkg_args=" + " ".join(f"-p {p}" for p in s.tested),
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
        f"- doctests: {', '.join(s.doc_packages) or 'none'}\n"
    )


def cargo_metadata():
    return json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"], text=True))


def changed_files(base, head):
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
        s = select(changed_files(args.base, args.head), meta["packages"], meta["workspace_root"])
    text = summary(s, [p["name"] for p in meta["packages"]])
    with (open(os.environ["GITHUB_OUTPUT"], "a") if "GITHUB_OUTPUT" in os.environ else sys.stdout) as out:
        out.write(github_output(s) + "\n")
    with (open(os.environ["GITHUB_STEP_SUMMARY"], "a") if "GITHUB_STEP_SUMMARY" in os.environ else sys.stderr) as out:
        out.write(text)


if __name__ == "__main__":
    main(sys.argv[1:])
