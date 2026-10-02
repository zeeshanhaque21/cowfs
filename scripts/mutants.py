#!/usr/bin/env python3
"""Mutation run for cowfs-core, isolated per-mutant CARGO_TARGET_DIR.

Each mutant patches one source file in the worktree, runs the test targets in order until one
fails, and restores the file. The worktree source is always restored, including on Ctrl-C.
"""
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "crates/cowfs-core/src"
MUT_ROOT = ROOT / "target/mutants"
OUT = ROOT / "target/mutants.out"

M = {
 "m01_open_drops_before_sync": ("lib.rs",
   "        mopts.before_sync = Some(store_sync_hook(&store));",
   "        mopts.before_sync = None;"),
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
   "            if b < end {\n                if is_hole(&c) {",
   "            if b + 1 < end {\n                if is_hole(&c) {"),
 "m06_dir_rename_no_barrier": ("ns.rs",
   "        self.barrier(sc)?;\n        // The barrier commits, which may evict the nodes behind these names",
   "        // The barrier commits, which may evict the nodes behind these names"),
 "m06b_rename_dir_no_barrier": ("ns.rs",
   "        self.barrier(sc)?;\n        // The barrier commits",
   "        // The barrier commits"),
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
   "        pn.ns_seq.store(seq, Ordering::Release);\n        self.dents.put(parent, name, None, seq);\n        drop(_ns);\n        self.maybe_wake(&sc);\n        Ok(())\n    }\n\n    pub(crate) fn op_rmdir",
   "        pn.ns_seq.store(seq, Ordering::Release);\n        drop(_ns);\n        self.maybe_wake(&sc);\n        Ok(())\n    }\n\n    pub(crate) fn op_rmdir"),
 "m13_content_before_data_flush_skipped_hole_zero": ("file.rs",
   "            prefix = hole_refs(a - total);",
   "            prefix = hole_refs(a - total - 1);"),
 "m14_setattr_grow_no_size": ("io.rs",
   "            attr.size = size;\n            attr.mtime = now;\n            self.queue_content",
   "            attr.size = size.min(cur);\n            attr.mtime = now;\n            self.queue_content"),
 "n02_swap_intent_no_fsync": ("swap.rs",
   "    crate::fsops::sync_file(&f, &tmp).map_err(|e| io(&e.to_string()))?;",
   "    let _ = f.sync_all();"),
 "n04_virt_mark_no_dir_fsync": ("ino.rs",
   "    crate::fsops::sync_dir(root)?;",
   "    let _ = std::fs::File::open(root);"),
 "n09_pinned_blocks_skips_busy": ("lib.rs",
   "            let Some(st) = n.try_read_for(Duration::from_secs(2)) else {\n                return Err(ControlError::Busy);\n            };",
   "            let Ok(st) = n.st.try_read() else { continue };"),
 "n15_staging_name_visible": ("ns.rs",
   ".filter(|(n, id)| **id > cookie && !swap::is_staging(n))",
   ".filter(|(n, id)| **id > cookie && (swap::is_staging(n) || true))"),
 "n16_name_rule_allows_staging": ("snapname.rs",
   "    if name.contains(crate::swap::STAGING) {",
   "    if false {"),
 "b01_fsync_root_noop": ("io.rs",
   "        if ino == ROOT_INO {\n            // the trait defines this as the whole-mount barrier\n            return self.sync_all();\n        }",
   "        if ino == ROOT_INO {\n            return Ok(());\n        }"),
 "b03_no_rollback": ("swap.rs",
   "        if let Err(e) = self.fault(3) {\n            self.rollback(&staged, new);\n            return Err(e);\n        }",
   "        if let Err(e) = self.fault(3) {\n            return Err(e);\n        }"),
 "b05_no_safety_margin": ("ino.rs",
   "            Mark::Missing if has_state => (\n                SAFETY,",
   "            Mark::Missing if has_state => (\n                0,"),
 "b05_zero_mark_trusted": ("ino.rs",
   "            Mark::Value(0) if has_state => (\n                SAFETY,",
   "            Mark::Value(0) if has_state => (\n                0,"),
 "b07_unregister_locked_meta": ("lib.rs",
   "        // `removed` blocks every new operation on this snapshot and its caches are empty, so the\n        // meta commit can run without the namespace and flush locks: it can wait for another\n        // snapshot's store fsync, and holding those locks across that wait is what wedges a mount.\n        self.meta",
   "        let _wedged = sc.ns.lk();\n        let _also = sc.flush.lk();\n        self.meta"),
 "b09_transient_poisons": ("inner.rs",
   "                        let fatal = Node::classify(&e);",
   "                        let fatal = true;"),
 "b10_no_read_cap": ("io.rs",
   "                .min(off.saturating_add(crate::MAX_READ_BYTES));",
   "                ;"),
 "b08_hole_ignores_length": ("file.rs",
   "pub(crate) fn is_hole(c: &ChunkRef) -> bool {\n    c.id == HOLE && u64::from(c.len) <= HOLE_MAX\n}",
   "pub(crate) fn is_hole(c: &ChunkRef) -> bool {\n    c.id == HOLE\n}"),
 # the session contract: an alias lives as long as the inode has a name
 "a01_alias_released_on_commit": ("inner.rs",
   "    /// Drops an orphan nobody holds once its removal is committed.\n    pub(crate) fn try_reclaim(&self, node: &Arc<Node>) {",
   "    /// Drops an orphan nobody holds once its removal is committed.\n    pub(crate) fn try_reclaim(&self, node: &Arc<Node>) {\n        self.aliases.wr().remove(node.ino);"),
 "a02_reclaim_keeps_nlink_zero": ("inner.rs",
   "        if node.st.rd().attr.nlink != 0 {\n            return;\n        }",
   "        if false {\n            return;\n        }"),
 "a03_no_alias_ceiling": ("inner.rs",
   "        let live = self.aliases.rd().len();\n        if live >= self.opts.alias_limit {",
   "        let live = 0usize;\n        if live >= self.opts.alias_limit {"),
 # the mirror of the rule: an unlinked file a handle is open on stays usable, so this mutation is
 # only visible if the pin is ignored the other way
 "a05_pinned_unlinked_is_stale": ("inner.rs",
   "        if !n.pinned() && n.st.rd().attr.nlink == 0 {\n            return Err(Error::Stale);\n        }",
   "        if n.pinned() && n.st.rd().attr.nlink == 0 {\n            return Err(Error::Stale);\n        }"),
 "a04_canon_returns_meta_number": ("ino.rs",
   "    pub(crate) fn canon(&self, snap: u64, m: u64) -> Option<Ino> {\n        if self.rev.is_empty() {\n            return None;\n        }\n        self.rev.get(&pack(snap, m).ok()?).copied()",
   "    pub(crate) fn canon(&self, snap: u64, m: u64) -> Option<Ino> {\n        if self.rev.is_empty() {\n            return None;\n        }\n        let _ = m;\n        None"),
 "b11_load_node_upserts": ("inner.rs",
   "                // a live node for this inode keeps changing; never overwrite it with a state read\n                // from meta, or a dirty file's unflushed extents are lost\n                Err(()) if tries < 64 => {}\n                Err(()) => return Err(Error::Stale),",
   "                Err(()) if tries < 64 => {}\n                Err(()) => {\n                    self.nodes.upsert(ino, node.clone());\n                    return Ok(node);\n                }"),
}

ORDER = [["--lib"], ["--test", "core"], ["--test", "chunks"], ["--test", "names_ino"],
         ["--test", "swap"], ["--test", "poison"], ["--test", "alias"], ["--test", "caches"],
         ["--test", "conformance"], ["--test", "model"], ["--test", "stress"],
         ["--test", "critic2b"], ["--test", "critic"], ["--test", "locks"],
         ["--test", "crash"], ["--test", "kill9"]]
TIMEOUT = int(os.environ.get("COWFS_MUT_TIMEOUT", "1800"))
FOCUSED = {
    "a01_alias_released_on_commit": [
        ["--test", "alias_session", "a_forgotten_directory_keeps_its_number_after_the_commit"],
        ["--test", "alias_session", "a_directory_created_through_the_session_still_takes_children_after_the_commit"],
        ["--test", "alias"],
        ["--test", "core"],
    ],
    "a02_reclaim_keeps_nlink_zero": [
        ["--test", "alias_session", "an_unlinked_inode_goes_stale"],
        # the whole file: reclaiming a named inode drops its node and its alias, so every
        # session number goes stale, and the per-test filters above hide that
        ["--test", "alias_session"],
        ["--test", "core"],
        ["--test", "caches"],
    ],
    "a03_no_alias_ceiling": [
        ["--test", "alias", "a_create_past_the_alias_ceiling_is_refused"],
    ],
    "a05_pinned_unlinked_is_stale": [
        ["--test", "core", "open_unlinked_data_is_pinned_until_released"],
        ["--test", "core"],
    ],
    "a04_canon_returns_meta_number": [
        ["--test", "alias_session"],
        ["--test", "alias"],
        ["--test", "model"],
    ],
    "n02_swap_intent_no_fsync": [
        # the fault test first: the mutant stops calling the seam, so the swap no longer fails
        ["--test", "durability", "a_swap_refuses_when_the_intent_file_cannot_be_made_durable"],
        ["--test", "durability", "the_intent_file_is_durable_before_the_victim_snapshot_is_removed"],
        ["--test", "swap"],
    ],
    "n04_virt_mark_no_dir_fsync": [
        ["--test", "durability", "a_reservation_refuses_when_its_directory_cannot_be_made_durable"],
        ["--test", "durability", "a_new_virtual_reservation_is_durable_before_any_of_its_numbers_is_handed_out"],
        ["--test", "names_ino"],
    ],
    "b11_load_node_upserts": [
        ["--test", "durability", "a_node_load_that_exhausts_its_retry_budget_fails_closed"],
        ["--test", "caches"],
        ["--test", "stress"],
    ],
    "b07_unregister_locked_meta": [
        ["--test", "durability", "removing_a_snapshot_releases_its_locks_before_its_metadata_commit"],
        ["--test", "locks"],
    ],
    "b03_no_rollback": ["--test", "critic2b", "a_step_three_refusal_removes_the_intent_and_staging_snapshot"],
    "b09_transient_poisons": ["--test", "critic2b", "a_single_transient_failure_is_retried_in_the_same_flush"],
    "n15_staging_name_visible": ["--lib", "staging_snapshots_are_hidden_from_mount_root_readdir"],
    "n16_name_rule_allows_staging": ["--test", "critic2b", "staging_names_are_reserved_for_the_swap_protocol"],
}
RECHECK = {
    "m10_fsync_no_meta_sync": [["--test", "flush_boundary"], ["--test", "critic2b"]],
    "n02_swap_intent_no_fsync": [["--test", "swap"], ["--test", "critic2b"]],
    "n04_virt_mark_no_dir_fsync": [["--test", "names_ino"], ["--test", "critic2b"]],
    "n15_staging_name_visible": [["--test", "swap"], ["--test", "critic2b"], ["--test", "names_ino"]],
    "n16_name_rule_allows_staging": [["--test", "names_ino"], ["--test", "critic2b"], ["--test", "swap"]],
    "b11_load_node_upserts": [["--test", "caches"], ["--test", "stress"], ["--test", "critic2b"]],
}


def seed_target(name):
    tgt = MUT_ROOT / name / "target"
    tgt.mkdir(parents=True, exist_ok=True)


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
            plan = FOCUSED.get(name) or RECHECK.get(name) or ORDER
            # a FOCUSED entry may be one command as a flat list of strings
            plan = [plan] if plan and isinstance(plan[0], str) else plan
            for t in plan:
                pr = subprocess.Popen(
                    ["rtk", "cargo", "test", "-j4", "-p", "cowfs-core"] + t,
                    cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT,
                    start_new_session=True)
                try:
                    rc = pr.wait(timeout=max(1, TIMEOUT - (time.time() - t0)))
                except subprocess.TimeoutExpired:
                    subprocess.run(["ps", "-p", str(pr.pid), "-o", "pid=,args="], check=True)
                    os.killpg(pr.pid, 9)
                    pr.wait()
                    rc = "TIMEOUT"
                if rc != 0:
                    killed = f"{' '.join(t)} rc={rc}"
                    break
    finally:
        p.write_text(orig)
    text = log_path.read_text()
    if killed:
        if "TIMEOUT" in killed:
            result = "TIMEOUT " + killed
        elif "could not compile" in text or "error[E" in text or "unclosed delimiter" in text:
            result = "COMPILE-FAILED " + killed
        elif "test result: FAILED" in text:
            result = "KILLED by " + killed
        else:
            result = "INCONCLUSIVE " + killed
    else:
        result = "SURVIVED"
    return name, result, f"{time.time() - t0:.0f}s"


if __name__ == "__main__":
    names = sys.argv[1:] or list(M)
    for n in names:
        seed_target(n)
        line = " | ".join(run(n))
        print(line, flush=True)
        with open(OUT, "a") as f:
            f.write(line + "\n")
