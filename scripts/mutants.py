#!/usr/bin/env python3
"""Mutation run for cowfs-core, per-mutant CARGO_TARGET_DIR (APFS clone of a warm base target).

Each mutant patches one source file in the worktree, runs the test targets in order until one
fails, and restores the file. The worktree source is always restored, including on Ctrl-C.
"""
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path("/Users/zeeshanhaque/.treehouse/cowfs-7c1bf8/9/cowfs")
SRC = ROOT / "crates/cowfs-core/src"
BASE_TARGET = Path(os.environ.get("COWFS_BASE_TARGET", str(ROOT / "target")))
MUT_ROOT = ROOT / "target/mutants"
OUT = ROOT / "target/mutants.out"
C = "crates/cowfs-core/src/"

M = {
 "m01_open_drops_before_sync": ("lib.rs",
   "opts.meta.before_sync = Some(store_sync_hook(&store));",
   "opts.meta.before_sync = None;"),
 "m02_rename_keeps_old_dentry": ("ns.rs",
   "        self.dents.put(parent, name, None, seq);\n        self.dents\n            .put(new_parent, new_name, Some((src, skind)), seq);",
   "        self.dents\n            .put(new_parent, new_name, Some((src, skind)), seq);"),
 "m03_forget_noop": ("io.rs",
   "        let Some(n) = self.nodes.get(&ino) else {\n            return;\n        };\n        let mut under",
   "        let Some(n) = self.nodes.get(&ino) else {\n            return;\n        };\n        if true { return; }\n        let mut under"),
 "m04_shrink_skips_truncate": ("io.rs",
   "                f.truncate(&self.blocks, size)?;",
   "                let _ = size;"),
 "m05_rmw_tail_off_by_one": ("file.rs",
   "            if b < end {\n                if c.id == HOLE {",
   "            if b + 1 < end {\n                if c.id == HOLE {"),
 "m06_dir_rename_no_barrier": ("ns.rs",
   "        self.barrier(sc)?;\n        let src = sn.ino;",
   "        let src = sn.ino;"),
 "m07_notempty_mapped_exists": ("error.rs",
   "M::NotEmpty => Error::NotEmpty,",
   "M::NotEmpty => Error::Exists,"),
 "m08_flusher_timer_off": ("inner.rs",
   "|| q.age().is_none_or(|a| a >= self.opts.flush_interval))",
   "|| false)"),
 "m09_open_accepts_corrupt_store": ("lib.rs",
   "if rec.has_corruption() {",
   "if false && rec.has_corruption() {"),
 "m10_fsync_no_meta_sync": ("inner.rs",
   "        self.flush_snapshot(sc)?;\n        self.meta.sync().map_err(from_meta)?;\n        *self.unsynced.lk() = None;",
   "        self.flush_snapshot(sc)?;"),
 "m11_fork_no_flush": ("lib.rs",
   "        self.inner.check_new_name(name)?;\n        self.inner.flush_snapshot(&sc)?;\n        let snap = sc.snap.fork",
   "        self.inner.check_new_name(name)?;\n        let snap = sc.snap.fork"),
 "m12_unlink_keeps_dentry": ("ns.rs",
   "        pn.ns_seq.store(seq, Ordering::Release);\n        self.dents.put(parent, name, None, seq);\n        drop(_ns);",
   "        pn.ns_seq.store(seq, Ordering::Release);\n        drop(_ns);"),
 "m13_content_before_data_flush_skipped_hole_zero": ("file.rs",
   "            prefix = hole_refs(a - total);",
   "            prefix = hole_refs(a - total - 1);"),
 "m14_setattr_grow_no_size": ("io.rs",
   "            attr.size = size;\n            attr.mtime = now;\n            self.queue_content",
   "            attr.size = size.min(cur);\n            attr.mtime = now;\n            self.queue_content"),
}

ORDER = [["--lib"], ["--test", "core"], ["--test", "chunks"], ["--test", "names_ino"],
         ["--test", "swap"], ["--test", "poison"], ["--test", "alias"], ["--test", "caches"],
         ["--test", "conformance"], ["--test", "model"], ["--test", "stress"],
         ["--test", "crash"], ["--test", "kill9"]]
TIMEOUT = int(os.environ.get("COWFS_MUT_TIMEOUT", "1800"))


def seed_target(name):
    tgt = MUT_ROOT / name / "target"
    if tgt.exists():
        shutil.rmtree(tgt, ignore_errors=True)
    tgt.parent.mkdir(parents=True, exist_ok=True)
    # APFS clone: instant, copy-on-write, so each mutant has its own CARGO_TARGET_DIR
    subprocess.check_call(["cp", "-cRc", str(BASE_TARGET), str(tgt)])


def run(name):
    f, old, new = M[name]
    p = SRC / f
    orig = p.read_text()
    if orig.count(old) != 1:
        return name, f"PATCH-FAILED count={orig.count(old)}", ""
    tgt = MUT_ROOT / name / "target"
    log_path = MUT_ROOT / name / "run.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    killed = None
    t0 = time.time()
    try:
        p.write_text(orig.replace(old, new))
        env = dict(os.environ, CARGO_TARGET_DIR=str(tgt))
        with open(log_path, "w") as log:
            for t in ORDER:
                pr = subprocess.Popen(
                    ["cargo", "test", "-j4", "-p", "cowfs-core"] + t,
                    cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT,
                    start_new_session=True)
                try:
                    rc = pr.wait(timeout=TIMEOUT)
                except subprocess.TimeoutExpired:
                    os.killpg(pr.pid, 9)
                    rc = "TIMEOUT"
                if rc != 0:
                    killed = f"{' '.join(t)} rc={rc}"
                    break
    finally:
        p.write_text(orig)
    return name, ("KILLED by " + killed) if killed else "SURVIVED", f"{time.time() - t0:.0f}s"


if __name__ == "__main__":
    names = sys.argv[1:] or list(M)
    for n in names:
        seed_target(n)
        line = " | ".join(run(n))
        print(line, flush=True)
        with open(OUT, "a") as f:
            f.write(line + "\n")
        shutil.rmtree(MUT_ROOT / n, ignore_errors=True)
