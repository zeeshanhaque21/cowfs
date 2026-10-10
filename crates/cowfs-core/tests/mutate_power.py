#!/usr/bin/env python3
"""Mutation check for the Core power-loss test (tests/power_core.rs), issue 173 slice 5.

Usage: python3 crates/cowfs-core/tests/mutate_power.py [TAG ...]    (no tag: every mutation)

Same method as crates/cowfs-store/tests/mutate.py: copy the repo to target/mut/work, apply ONE
textual mutation to a source file under crates/, run `cargo test -p cowfs-core --test power_core -- power_cut_at_every`
(the sweep only, see MUT_FILTER) with its own CARGO_TARGET_DIR, and record whether it failed. A mutant that leaves the suite
green is a SURVIVOR: a test gap or an equivalent mutation, and each one is explained in
docs/crash-injection-173.md. Results go to target/mut/results.txt.
"""
import os, re, shutil, subprocess, sys

root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
mut = f"{root}/target/mut"
work = f"{mut}/work"
test = os.environ.get("MUT_TEST", "power_core")
# Only the sweep may kill a mutant: the replay test also fails when an op vanishes from the log,
# which says the recorder broke, not that a power cut loses data.
filt = ["--", os.environ.get("MUT_FILTER", "power_cut_at_every")]

HOOK = "        self.run_hook()?;\n        if !has_work {\n            return Ok(None);\n        }\n"
DROP = "        self.drop_intent(target);\n        entry.ok_or(ControlError::NotFound)"


def hook_after_commit(t):
    """Meta runs the store-sync hook AFTER the metadata transaction commits instead of before."""
    c = t.index("    fn commit(\n")
    commit = "            wtx.commit()?;\n"
    i = t.index(commit, c)
    if HOOK not in t[c:i]:
        raise ValueError
    t = t[:c] + t[c:i].replace(HOOK, "        if !has_work {\n            self.run_hook()?;\n            return Ok(None);\n        }\n", 1) + commit + "            self.run_hook()?;\n" + t[i + len(commit):]
    return t


def intent_removed_before_rename(t):
    """finish_swap drops (and syncs the removal of) the intent file before the staging rename."""
    if DROP not in t or "        let mut entry = None;\n" not in t:
        raise ValueError
    t = t.replace(DROP, "        entry.ok_or(ControlError::NotFound)", 1)
    return t.replace("        let mut entry = None;\n", "        self.drop_intent(target);\n        let mut entry = None;\n", 1)


def hook_skipped_for(kind):
    def f(t):
        if HOOK not in t:
            raise ValueError
        return t.replace(HOOK, f"        if !matches!(extra, Extra::{kind} {{ .. }}) {{\n            self.run_hook()?;\n        }}\n        if !has_work {{\n            return Ok(None);\n        }}\n", 1)
    return f


M = [
    # name, file under crates/, pattern (str) or function, replacement
    ("E1 Core's store sync hook does nothing", "cowfs-core/src/lib.rs",
     "        store.sync().map_err(|e| match e {\n            cowfs_store::Error::Io(e) => e,\n            e => std::io::Error::other(e.to_string()),\n        })\n",
     "        let _ = &store;\n        Ok(())\n"),
    ("E2 store sync runs AFTER the metadata commit", "cowfs-meta/src/db.rs", hook_after_commit, None),
    ("E3 snapshot-add commit (fork_snapshot, the swap staging fork) skips the store sync", "cowfs-meta/src/db.rs", hook_skipped_for("Add"), None),
    ("E4 snapshot-rename commit (rename_snapshot, the swap rename) skips the store sync", "cowfs-meta/src/db.rs", hook_skipped_for("Rename"), None),
    ("E5 snapshot-remove commit (remove_snapshot, the swap victim) skips the store sync", "cowfs-meta/src/db.rs", hook_skipped_for("Remove"), None),
    ("I1 intent file: no directory fsync after the rename", "cowfs-core/src/swap.rs",
     "    crate::fsops::sync_dir(root).map_err(|e| io(&e.to_string()))?;\n    Ok(())\n}\n\n/// The staging and target names",
     "    Ok(())\n}\n\n/// The staging and target names"),
    ("I2 intent file: no fsync of its data before the rename", "cowfs-core/src/swap.rs",
     "    crate::fsops::sync_file(&f, &tmp).map_err(|e| io(&e.to_string()))?;\n", ""),
    ("I3 intent removed before the staging rename", "cowfs-core/src/swap.rs", intent_removed_before_rename, None),
    ("I4 intent removal not followed by a directory fsync", "cowfs-core/src/swap.rs",
     "            Ok(()) => sync_dir(&self.inner.root),\n            // nothing to remove: a recovery that already dropped it",
     "            Ok(()) => {}\n            // nothing to remove: a recovery that already dropped it"),
    # the store's own mutation 'W1 watermark before pack fsync' (tests/mutate.py), seen from Core
    ("W1 watermark raised before the pack fsync", "cowfs-store/src/store.rs",
     "            self.io.sync_file(&file, &pack::pack_path(&self.dir, id))?;\n            self.wm\n                .lock()\n                .unwrap_or_else(PoisonError::into_inner)\n                .advance(Mark { pack: id, len })?;",
     "            self.wm\n                .lock()\n                .unwrap_or_else(PoisonError::into_inner)\n                .advance(Mark { pack: id, len })?;\n            self.io.sync_file(&file, &pack::pack_path(&self.dir, id))?;"),
]

only = sys.argv[1:]
os.makedirs(mut, exist_ok=True)
subprocess.run(["rsync", "-a", "--delete", "--exclude", "target", "--exclude", ".git", f"{root}/", work + "/"], check=True)
base = f"{mut}/tm_base"
if not os.path.exists(base):
    env = dict(os.environ, CARGO_TARGET_DIR=base)
    subprocess.run(["cargo", "test", "-j4", "--no-run", "-p", "cowfs-core", "--test", test], cwd=work, env=env, check=True, capture_output=True)
out = open(f"{mut}/results.txt", "a")
for name, f, a, b in M:
    tag = name.split()[0]
    if only and tag not in only:
        continue
    target = f"{work}/crates/{f}"
    shutil.copy(f"{root}/crates/{f}", target)
    t = open(target).read()
    if callable(a):
        try:
            mutated = a(t)
        except ValueError:
            out.write(f"{name}: PATTERN NOT FOUND\n"); out.flush(); continue
    elif a not in t:
        out.write(f"{name}: PATTERN NOT FOUND\n"); out.flush(); continue
    else:
        mutated = t.replace(a, b, 1)
    open(target, "w").write(mutated)
    tgt = f"{mut}/tm_{tag}"
    shutil.rmtree(tgt, ignore_errors=True)
    subprocess.run(["cp", "-a", base, tgt], check=True)
    env = dict(os.environ, CARGO_TARGET_DIR=tgt)
    try:
        r = subprocess.run(["cargo", "test", "-j4", "--no-fail-fast", "-p", "cowfs-core", "--test", test, *filt], cwd=work, env=env, capture_output=True, text=True, timeout=1200)
        txt = r.stdout + r.stderr
        failed = sorted(set(re.findall(r"^test (\S+) \.\.\. FAILED", txt, re.M)))
        if "could not compile" in txt:
            v = "COMPILE ERROR"
        elif failed:
            first = re.findall(r"FAIL (k=\S+ seed=\d+ op=[^:]*: .{0,160})", txt)
            v = "KILLED by " + ", ".join(failed[:4]) + (f"; first image: {first[0]}" if first else "")
        else:
            v = "SURVIVED"
    except subprocess.TimeoutExpired:
        v = "KILLED (suite timed out after 1200 s)"
    out.write(f"{name}: {v}\n"); out.flush()
    shutil.rmtree(tgt, ignore_errors=True)
    shutil.copy(f"{root}/crates/{f}", target)
out.write("DONE\n"); out.close()
