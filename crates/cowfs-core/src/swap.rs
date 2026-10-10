//! Crash-safe snapshot replacement: stage a fork, record the intent, then move it into place.
//!
//! This is the replacement path, for promotion. A rename moves a name inside one metadata
//! transaction and does not come here; a promotion replaces the tree under a name, which needs a
//! staged copy first. Two rules make the replacement safe:
//!
//! - The staging snapshot's name carries the reserved suffix `.cowfs-swap<N>`. The name rules
//!   refuse it, so no caller can create or move a snapshot into it, and the synthetic root filters
//!   it out of the listing, so it is never visible.
//! - The old target stays under its name until ONE metadata transaction (`Meta::replace_snapshot`)
//!   gives the staging snapshot that name and removes the old target. So the name is never missing:
//!   at every step exactly one of the old tree and the new tree answers to it. An operation that
//!   returns `Err` leaves the mount exactly as it was, or leaves an intent file that the next
//!   `Core::open` (or the next call for the target) rolls forward.
//!
//! The order, with the point of no return at step 3:
//!
//! 1. fork the source into a staging name (a failure here changes nothing),
//! 2. write and sync the intent file `<root>/swap-<target>`, naming the staging and target snapshots,
//! 3. replace the target: the staging snapshot takes the target name (its id is kept) and the old
//!    target is removed, in one metadata transaction,   <- rollback is no longer possible
//! 4. remove the intent file; a failure is reported and a leftover file is dropped on open.
//!
//! A crash, or a step 3 commit error whose outcome a re-read cannot establish, leaves the intent
//! file, and the next `Core::open` finishes the swap before serving anything. A swap or replacing import of a target with a pending intent
//! finishes that intent first (`Core::recover_target`), so a retry never deletes the only copy of a
//! tree. A crash before step 3 leaves a hidden staging snapshot with no intent file; `Core::open`
//! removes every such orphan once the intents are recovered, before anything can stage a new one,
//! so it cannot take a staging snapshot a live operation owns.
//!
//! Recovery also finishes the images older releases left: the old target already removed (the
//! staging snapshot is then renamed into the name), and a target that is a fork of the staging
//! snapshot (only the staging snapshot goes).

use std::fs;
use std::path::{Path, PathBuf};

use cowfs_vfs::Error;

use super::{control_meta, validate_snapshot_name, ControlError, Core, SnapshotEntry};
use crate::util::MutexExt as _;

const SWAP_PREFIX: &str = "swap-";
/// The intent writer's own temp file. It must not start with [`SWAP_PREFIX`] and must not be
/// distinguished by a suffix: the target is a user name, so `swap-<target>` for the target
/// `base.tmp` is the same file name as a `.tmp`-suffixed temp of `base`.
const TMP_PREFIX: &str = "tmp-swap-";
/// Reserved in snapshot names: see the module docs. The marker and the rule that refuses it live
/// in `cowfs-snapname`, so the control API refuses these names too.
pub(crate) const STAGING: &str = cowfs_snapname::RESERVED;

/// True for a name only the swap may use.
pub(crate) fn is_staging(name: &str) -> bool {
    cowfs_snapname::is_reserved(name)
}

fn tmp_path(root: &Path, target: &str) -> PathBuf {
    root.join(format!("{TMP_PREFIX}{target}"))
}

fn intent_path(root: &Path, target: &str) -> PathBuf {
    root.join(format!("{SWAP_PREFIX}{target}"))
}

#[cfg(test)]
thread_local! {
    /// Test seam: makes `finish_swap` fail, the one step `set_swap_fault` cannot reach at open.
    static FAIL_FINISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Test seam: the re-read after a failed replace commit cannot be read.
    static FAIL_REREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Test seam: forces every staging name's hash, to build the collision a real name pair needs
    /// a 2^32 search for.
    static HASH_OVERRIDE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// The staging name for `target`; deterministic, so a duplicate is found and cleaned up without the
/// intent file. An import stages into the same name: its crash leaves nothing a caller can see.
///
/// A readable prefix of the target plus a hash of the whole target, so two targets never share a
/// staging snapshot however long their common prefix is. The result is at most 200 + 17 + 12 bytes,
/// inside `NAME_MAX`. Intents record their staging name, so one written under the older naming
/// (200 characters of the target, no hash) still recovers; nothing compares a recorded name with
/// this function.
pub(crate) fn staging_name(target: &str) -> String {
    #[cfg(test)]
    if let Some(h) = HASH_OVERRIDE.with(std::cell::Cell::get) {
        return format!("{}~{h:016x}{STAGING}0", &target[..target.len().min(200)]);
    }
    let mut end = target.len().min(200);
    while !target.is_char_boundary(end) {
        end -= 1;
    }
    // FNV-1a 64 of the full target; a collision needs two valid names with equal prefix and hash
    let h = target.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{}~{h:016x}{STAGING}0", &target[..end])
}

fn sync_dir(dir: &Path) {
    let _ = crate::fsops::sync_dir(dir);
}

fn io(msg: &str) -> ControlError {
    ControlError::Fs(Error::Io(msg.to_string()))
}

fn write_intent(root: &Path, staged: &str, target: &str) -> Result<(), ControlError> {
    let p = intent_path(root, target);
    let tmp = tmp_path(root, target);
    let f = crate::fsops::create_with(&tmp, format!("{staged}\n{target}\n").as_bytes())
        .map_err(|e| io(&e.to_string()))?;
    // the record must be on the medium before the rename, and the rename on the medium before
    // anything destructive reads the intent file
    crate::fsops::sync_file(&f, &tmp).map_err(|e| io(&e.to_string()))?;
    drop(f);
    crate::fsops::rename(&tmp, &p).map_err(|e| io(&e.to_string()))?;
    crate::fsops::note("intent_renamed");
    crate::fsops::sync_dir(root).map_err(|e| io(&e.to_string()))?;
    Ok(())
}

/// The staging and target names of an intent file, or `None` if it is unreadable or torn.
fn read_intent(p: &Path) -> Option<(String, String)> {
    let s = fs::read_to_string(p).ok()?;
    // a record is written whole and renamed into place; one without its final newline is cut off
    if !s.ends_with('\n') {
        return None;
    }
    let mut it = s.lines();
    let staged = it.next()?.to_string();
    let target = it.next()?.to_string();
    (!staged.is_empty() && !target.is_empty() && is_staging(&staged)).then_some((staged, target))
}

/// Intent files left by an interrupted swap. Every `swap-*` file is one: the writer's temp files
/// have their own prefix. A `swap-<X>.tmp` left by the older temp naming is dropped by
/// `recover_intent`, which finds the name is exactly `swap-<recorded target>.tmp`.
fn intents(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(SWAP_PREFIX))
        })
        .collect();
    v.sort();
    v
}

/// Completes one swap a crash or an error left half done, from its intent file `p`.
///
/// A torn intent file names nothing, so the staging name its file name implies is used: kept and
/// rolled forward when the target is gone, removed with the file when the target still exists.
/// The writer is temp, sync, rename, directory sync, so this needs media corruption to happen.
fn recover_intent(core: &Core, p: &Path) -> Result<(), ControlError> {
    let Some((staged, target)) = read_intent(p) else {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix(SWAP_PREFIX))
            .map(str::to_string)
            .unwrap_or_default();
        let staged = staging_name(&name);
        // The file name carries the target, so a torn record can still be rolled forward when the
        // old target is already gone: the staging snapshot is then the only copy of the new tree.
        let kept = validate_snapshot_name(&name).is_ok()
            && core.inner.snap_by_name(&name).is_err()
            && core.inner.snap_by_name_raw(&staged).is_ok();
        if kept {
            // Same as the readable path: a failed roll-forward keeps the staging tree and the
            // intent file, the only copy of the new tree, and the next open tries again.
            core.finish_swap(&staged, &name)?;
        } else if let Ok(sc) = core.inner.snap_by_name_raw(&staged) {
            let _ = core.inner.unregister(&sc);
        }
        let gone = crate::fsops::remove_file(p);
        sync_dir(&core.inner.root);
        *core.inner.last_error.lk() = Some(format!(
            "swap recovery: {p:?} is unreadable, {} staging snapshot {staged}{}",
            if kept {
                "rolled forward"
            } else {
                "removed any"
            },
            match gone {
                Ok(()) => String::new(),
                Err(e) => format!("; could not remove it: {e}, the next open retries"),
            }
        ));
        return Ok(());
    };
    if p.file_name().and_then(|n| n.to_str()) == Some(&format!("{SWAP_PREFIX}{target}.tmp")) {
        // an older release's temp file (`swap-<target>.tmp`): its swap had not started
        if let Err(e) = crate::fsops::remove_file(p) {
            *core.inner.last_error.lk() = Some(format!(
                "swap recovery: could not remove {p:?}: {e}; the next open retries"
            ));
        }
        return Ok(());
    }
    match core.finish_swap(&staged, &target) {
        // `NotFound` is also what a swap whose staged tree is gone returns (a store the old
        // issue 177 already damaged); the intent still has to go or the name stays blocked
        Ok(_) | Err(ControlError::NotFound) => {}
        Err(e) => return Err(e),
    }
    if core.inner.snap_by_name(&target).is_err() {
        *core.inner.last_error.lk() = Some(format!(
            "swap recovery: neither {target} nor its staged tree {staged} exists, intent removed"
        ));
    }
    match crate::fsops::remove_file(p) {
        Ok(()) => sync_dir(&core.inner.root),
        // `finish_swap` already dropped it
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            *core.inner.last_error.lk() = Some(format!(
                "swap recovery: could not remove {p:?}: {e}; the next open drops it"
            ));
        }
    }
    Ok(())
}

/// Completes every swap a crash or an error left half done, then removes the staging snapshots no
/// intent names, when the store is opened.
///
/// A failure is reported through `Core::last_flush_error` and the intent file stays for the next
/// open, so a swap is never silently dropped.
pub(crate) fn recover(core: &Core) {
    // a temp file left by a crash before its rename never carries authority; the store flock means
    // no writer is mid-way through one
    for e in fs::read_dir(&core.inner.root)
        .into_iter()
        .flatten()
        .flatten()
    {
        if e.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            // best-effort on purpose: the file is inert, and the next open retries
            let _ = crate::fsops::remove_file(e.path());
        }
    }
    for p in intents(&core.inner.root) {
        if let Err(e) = recover_intent(core, &p) {
            *core.inner.last_error.lk() = Some(format!("swap recovery: {e}"));
        }
    }
    sweep_orphans(core);
}

/// Removes every staging snapshot that no intent file names (issue 176): a crash during the staging
/// write leaves one, and nothing else would ever remove a name that is not ingested again.
///
/// Runs only from `recover`, inside `Core::open`, when no operation can own a staging snapshot yet:
/// the store's `LOCK` file is held by flock for the life of the `Core`, so no other process or `Core`
/// can be staging into this store while it opens.
/// An intent that could not be recovered still protects its staging snapshot.
fn sweep_orphans(core: &Core) {
    let named: std::collections::HashSet<String> = intents(&core.inner.root)
        .iter()
        .filter_map(|p| read_intent(p).map(|(staged, _)| staged))
        .collect();
    let Ok(all) = core.inner.meta.snapshots() else {
        return;
    };
    for info in all {
        if is_staging(&info.name) && !named.contains(&info.name) {
            if let Ok(sc) = core.inner.snap_by_name_raw(&info.name) {
                let _ = core.inner.unregister(&sc);
            }
        }
    }
}

/// Refuses a replacement of `target` that could not write its intent file: the writer's temp file
/// `tmp-swap-<target>` is 9 bytes longer than `swap-<target>`. Checked before anything is staged,
/// so the first call fails the way every later one would.
pub(crate) fn check_target_len(target: &str) -> Result<(), ControlError> {
    if TMP_PREFIX.len() + target.len() > cowfs_snapname::NAME_MAX {
        return Err(ControlError::InvalidName(
            "too long to replace: the swap's intent file name must fit in 255 bytes",
        ));
    }
    Ok(())
}

impl Core {
    /// Removes the leftover staging snapshot `staged` of `target`, unless an intent for another
    /// target names it (a staging-hash collision): that snapshot is the only copy of that swap's
    /// new tree, so the call is refused before it changes anything.
    pub(crate) fn clear_leftover(&self, staged: &str, target: &str) -> Result<(), ControlError> {
        let taken = intents(&self.inner.root)
            .iter()
            .filter_map(|p| read_intent(p))
            .any(|(s, t)| s == staged && t != target);
        if taken {
            return Err(ControlError::InvalidName(
                "its staging name is held by another pending swap",
            ));
        }
        if let Ok(leftover) = self.inner.snap_by_name_raw(staged) {
            let _ = self.inner.unregister(&leftover);
        }
        Ok(())
    }

    /// Replaces snapshot `new` with a clone of `src`, for the promotion path. A rename moves a name
    /// rather than replacing one and does not come here: it has no victim to remove and needs no
    /// staging, so `Core::rename_snapshot` commits the name directly.
    /// Returns `Ok` only when the new name is in place. Returns `Err` with the mount unchanged,
    /// except when the metadata commit that replaces the target failed and a re-read of the file
    /// cannot show it did not land: the old target then stays live with the intent file pending,
    /// and the next open or call for the target installs the new tree (anything written to the old
    /// target in between is discarded).
    /// The one fork gives the new snapshot a new id, which it keeps under the target name, so every
    /// inode number in it differs from the old target's.
    pub(crate) fn swap_snapshot(
        &self,
        src: &str,
        new: &str,
    ) -> Result<SnapshotEntry, ControlError> {
        validate_snapshot_name(new)?;
        check_target_len(new)?;
        if src == new {
            return Err(ControlError::InvalidName(
                "source and target are the same snapshot",
            ));
        }
        self.recover_target(new)?;
        let src_sc = self.inner.snap_by_name(src)?;
        // the snapshot whose name goes away: the existing target, when there is one
        let victim: Option<&str> = if self.inner.snap_by_name(new).is_ok() {
            Some(new)
        } else {
            None
        };
        self.inner.check_new_name_except(new, victim)?;
        let staged = staging_name(new);
        // A pending intent for this target was finished above, so a leftover staging snapshot is
        // garbage from a crash before the intent.
        self.clear_leftover(&staged, new)?;
        self.inner.flush_snapshot(&src_sc)?;
        self.stage_and_intent(&src_sc, &staged, new)?;
        if let Err(e) = self.fault(1).and_then(|()| self.fault(2)) {
            self.rollback(&staged, new);
            return Err(e);
        }
        // the last point that can roll back: an old target with an open handle cannot be replaced
        if let Err(e) = self.fault(3).and_then(|()| self.check_target_idle(new)) {
            self.rollback(&staged, new);
            return Err(e);
        }
        // Past this point an error cannot be reported as "nothing happened", so the swap is rolled
        // forward instead and the call succeeds. The only exception is a failure of the roll
        // forward itself (an I/O error), which returns `Err` with the intent file on disk: the next
        // `Core::open` completes it. The old target is still visible until `finish_swap` replaces
        // it in the one metadata commit.
        if let Err(e) = self.fault(4) {
            *self.inner.last_error.lk() = Some(format!("swap: {e}, rolled forward"));
        }
        let done = self.finish_live(&staged, new);
        if let Err(e) = self.fault(5) {
            *self.inner.last_error.lk() = Some(format!("swap: {e} after the swap completed"));
        }
        done
    }

    /// Finishes the swap a failed earlier call left pending for `target`, before a new call of the
    /// same name removes its staging snapshot: after a failed `finish_swap` that snapshot is the
    /// only copy of the new tree (issue 177). An error leaves the intent in place and is returned,
    /// so the caller does not go on to destroy what the intent still needs.
    pub(crate) fn recover_target(&self, target: &str) -> Result<(), ControlError> {
        let p = intent_path(&self.inner.root, target);
        if p.exists() {
            recover_intent(self, &p)?;
        }
        Ok(())
    }

    /// Replaces `target` with the already verified snapshot `staged`, for an import that replaces.
    /// The intent record goes down before the old target is removed, so a crash after that point is
    /// rolled forward by `Core::open`; an error before it leaves the old target in place.
    pub(crate) fn replace_with_staged(
        &self,
        staged: &str,
        target: &str,
    ) -> Result<SnapshotEntry, ControlError> {
        if let Err(e) = self
            .fault(2)
            .and_then(|()| write_intent(&self.inner.root, staged, target))
        {
            self.rollback(staged, target);
            return Err(e);
        }
        if let Err(e) = self.fault(3) {
            self.rollback(staged, target);
            return Err(e);
        }
        // the last point that can roll back: an old target with an open handle cannot be replaced
        if let Err(e) = self.check_target_idle(target) {
            self.rollback(staged, target);
            return Err(e);
        }
        // Past this point an error is returned as it is and nothing is cleaned up: the intent file
        // makes `Core::open`, or the next call for this target, finish. The old target is still
        // visible until `finish_swap` replaces it in the one metadata commit. Fault 4 is that error
        // without running `finish_swap`.
        self.fault(4)?;
        self.finish_live(staged, target)
    }

    /// `Busy` when the target a swap replaces has an open handle: checked before the commit, while a
    /// rollback is still possible.
    fn check_target_idle(&self, target: &str) -> Result<(), ControlError> {
        match self.inner.snap_by_name(target) {
            Ok(v) if v.open_handles.load(std::sync::atomic::Ordering::Acquire) > 0 => {
                Err(ControlError::Busy)
            }
            _ => Ok(()),
        }
    }

    /// Steps 1 and 2. A failure in either leaves nothing behind.
    fn stage_and_intent(
        &self,
        src_sc: &crate::queue::SnapCtx,
        staged: &str,
        new: &str,
    ) -> Result<(), ControlError> {
        let r = (|| {
            let fork = src_sc.snap.fork(staged).map_err(control_meta)?;
            self.inner.register(fork)?;
            write_intent(&self.inner.root, staged, new)
        })();
        if r.is_err() {
            self.rollback(staged, new);
        }
        r
    }

    /// Undo steps 1 and 2: no target was removed, so the mount goes back to exactly what it was.
    ///
    /// Removal errors are ignored on purpose: the caller is already returning the error that got
    /// us here, and a leftover intent or temp file is handled by the next call for this target or
    /// the next open (`recover_target`, `recover`). That includes a failed `unregister` of the
    /// staging snapshot (a poisoned database): with the intent gone, the orphan sweep in
    /// `Core::open` removes the leftover staging row.
    fn rollback(&self, staged: &str, target: &str) {
        if let Ok(sc) = self.inner.snap_by_name_raw(staged) {
            let _ = self.inner.unregister(&sc);
        }
        let _ = crate::fsops::remove_file(intent_path(&self.inner.root, target));
        let _ = crate::fsops::remove_file(tmp_path(&self.inner.root, target));
        sync_dir(&self.inner.root);
    }

    /// Steps 3 and 4 for recovery: install the staging snapshot under the target name, remove the
    /// intent file. A failure leaves the intent pending, whatever the step.
    pub(crate) fn finish_swap(
        &self,
        staged: &str,
        target: &str,
    ) -> Result<SnapshotEntry, ControlError> {
        self.finish(staged, target, false)
    }

    /// [`Core::finish_swap`] for a live call. With `undo`, a failure that is certain to have
    /// changed nothing, because it happens before the metadata commit (the target is `Busy`, or the
    /// staged tree cannot be flushed), is rolled back like any earlier step: the old target is
    /// still live and writable, so leaving an intent that a restart would act on later would
    /// discard whatever is written to it in between. An error from the commit itself is rolled back only
    /// when a re-read shows the replace did not land; otherwise the outcome is unknown and it stays pending.
    pub(crate) fn finish_live(
        &self,
        staged: &str,
        target: &str,
    ) -> Result<SnapshotEntry, ControlError> {
        self.finish(staged, target, true)
    }

    fn finish(
        &self,
        staged: &str,
        target: &str,
        undo: bool,
    ) -> Result<SnapshotEntry, ControlError> {
        #[cfg(test)]
        if FAIL_FINISH.with(std::cell::Cell::get) {
            return Err(io("injected finish_swap failure"));
        }
        let mut entry = None;
        match self.inner.snap_by_name(target) {
            Err(_) => {
                // no target: a first install, or an older release's swap that removed it first
                let sc = self.inner.snap_by_name_raw(staged)?;
                self.inner.flush_snapshot(&sc)?;
                entry = Some(self.move_name(&sc, staged, target, None)?);
            }
            Ok(old) => {
                // The old target still stands: replace it by name in one metadata commit. The
                // exception is an older release's swap that forked `staged` into the target and
                // crashed before dropping the staging snapshot; that target is a fork of `staged`
                // (its parent), already the new tree, and only the staging snapshot goes below.
                if let Ok(sc) = self.inner.snap_by_name_raw(staged) {
                    if self.parent_of(&old) != Some(sc.id) {
                        // A flush failure and `Busy` happen before the commit and change nothing; an
                        // error from the commit itself is handled below.
                        let unwind = |e: ControlError| {
                            if undo
                                && self
                                    .inner
                                    .snap_by_name(target)
                                    .is_ok_and(|t| t.id == old.id)
                            {
                                self.rollback(staged, target);
                            }
                            e
                        };
                        self.inner
                            .flush_snapshot(&sc)
                            .map_err(|e| unwind(e.into()))?;
                        // the intent file and its directory entry are durable: the old tree may go
                        crate::fsops::note("victim_removed");
                        let moved = self.move_name(&sc, staged, target, Some(&old));
                        entry = Some(match moved {
                            Err(ControlError::Busy) => return Err(unwind(ControlError::Busy)),
                            // A commit error: roll back only when the file provably still has the
                            // old target under its name and the staged tree under the staging
                            // name. When that cannot be read, or says the replace landed, the
                            // outcome is unknown and the intent stays for the next open.
                            Err(e) => {
                                if self.replace_did_not_land(old.id, sc.id, staged, target) {
                                    return Err(unwind(e));
                                }
                                return Err(e);
                            }
                            Ok(r) => r,
                        });
                    }
                }
            }
        }
        // Only a target that is a fork of `staged` (a crash of an older release) leaves a staging
        // snapshot here; after the rename above there is none.
        if let Ok(st) = self.inner.snap_by_name_raw(staged) {
            let _ = self.inner.unregister(&st);
        }
        // fault 6 is a crash here: the rename is done, the intent file is still on disk
        self.fault(6)?;
        self.drop_intent(target);
        entry.ok_or(ControlError::NotFound)
    }

    /// True when the metadata file, re-read now, still has snapshot `old` under `target` and
    /// snapshot `new` under `staged`: a failed replace commit changed nothing.
    fn replace_did_not_land(&self, old: u64, new: u64, staged: &str, target: &str) -> bool {
        #[cfg(test)]
        if FAIL_REREAD.with(std::cell::Cell::get) {
            return false;
        }
        self.inner.meta.durable_snapshots().is_ok_and(|rows| {
            let name_of = |id: u64| rows.iter().find(|r| r.id.0 == id).map(|r| r.name.as_str());
            name_of(old) == Some(target) && name_of(new) == Some(staged)
        })
    }

    /// The recorded parent id of `sc`.
    fn parent_of(&self, sc: &crate::queue::SnapCtx) -> Option<u64> {
        let snap = self
            .inner
            .meta
            .snapshot_by_id(cowfs_meta::SnapshotId(sc.id))
            .ok()?;
        snap.info().ok()?.parent.map(|p| p.0)
    }

    /// Step 6: removes the intent file of `target`. The new name is already in place, so a failure
    /// is reported through `last_flush_error` and never fails the swap: a leftover intent finds its
    /// target present and no staging snapshot, and the next `Core::open` drops it.
    fn drop_intent(&self, target: &str) {
        let p = intent_path(&self.inner.root, target);
        match self
            .fault(7)
            .and_then(|()| crate::fsops::remove_file(&p).map_err(|e| io(&e.to_string())))
        {
            Ok(()) => sync_dir(&self.inner.root),
            // nothing to remove: a recovery that already dropped it, or a swap with no intent
            Err(_) if !p.exists() => {}
            Err(e) => {
                *self.inner.last_error.lk() = Some(format!(
                    "swap: could not remove {p:?} after the swap completed: {e}; the next open drops it"
                ));
            }
        }
    }

    /// Test seam: make the swap fail at `step` (1 to 7). 0 disables it.
    #[doc(hidden)]
    pub fn set_swap_fault(&self, step: u8) {
        self.inner
            .swap_fault
            .store(step, std::sync::atomic::Ordering::Relaxed);
    }

    fn fault(&self, step: u8) -> Result<(), ControlError> {
        if self
            .inner
            .swap_fault
            .load(std::sync::atomic::Ordering::Relaxed)
            == step
        {
            Err(io("injected swap fault"))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowfs_vfs::{Vfs, ROOT_INO};

    #[test]
    fn staging_snapshots_are_hidden_from_mount_root_readdir() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(
            dir.path(),
            crate::Options {
                background: false,
                ..Default::default()
            },
        )
        .unwrap();
        c.create_snapshot("src").unwrap();
        let sc = c.inner.snap_by_name("src").unwrap();
        let staged = staging_name("new");
        c.stage_and_intent(&sc, &staged, "new").unwrap();
        assert!(c
            .meta()
            .snapshots()
            .unwrap()
            .iter()
            .any(|s| s.name == staged));
        let listing = c.readdir(ROOT_INO, 0, 100).unwrap();
        assert!(listing.entries.iter().all(|e| e.name != staged.as_bytes()));
        c.rollback(&staged, "new");
    }

    #[test]
    fn sweep_keeps_a_staging_snapshot_an_intent_names_and_drops_an_orphan() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(
            dir.path(),
            crate::Options {
                background: false,
                ..Default::default()
            },
        )
        .unwrap();
        c.create_snapshot("src").unwrap();
        let sc = c.inner.snap_by_name("src").unwrap();
        let staged = staging_name("new");
        c.stage_and_intent(&sc, &staged, "new").unwrap();
        sweep_orphans(&c);
        assert!(c.inner.snap_by_name_raw(&staged).is_ok(), "intent names it");
        fs::remove_file(intent_path(&c.inner.root, "new")).unwrap();
        sweep_orphans(&c);
        assert!(c.inner.snap_by_name_raw(&staged).is_err(), "orphan stays");
    }

    fn src_dir(root: &Path, tag: &str, body: &str) -> PathBuf {
        let d = root.join(tag);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("f"), body).unwrap();
        d
    }

    fn put(c: &Core, from: &Path, name: &str, replace: bool) -> Result<(), crate::ImportError> {
        let mut hooks = crate::Hooks {
            progress: &mut |_, _| true,
        };
        if replace {
            crate::ingest_replacing(c, from, name, &mut hooks).map(|_| ())
        } else {
            crate::ingest(c, from, name, &mut hooks).map(|_| ())
        }
    }

    /// N1 (PR 220 round 3): two targets that collide under the staging hash. A pending swap of `a`
    /// owns the staging snapshot; any call for `b` is refused before it touches that snapshot.
    #[test]
    fn a_staging_hash_collision_with_a_pending_swap_is_refused_and_loses_nothing() {
        HASH_OVERRIDE.with(|h| h.set(Some(0xdead_beef)));
        // the same 200-byte prefix, so only the (forced) hash could tell the staging names apart
        let (a, b) = (
            &format!("{}1", "p".repeat(200)),
            &format!("{}2", "p".repeat(200)),
        );
        assert_eq!(staging_name(a), staging_name(b));
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let v1 = src_dir(scratch.path(), "v1", "old");
        let va = src_dir(scratch.path(), "va", "content of A");
        let vb = src_dir(scratch.path(), "vb", "content of B");
        let opts = || crate::Options {
            background: false,
            ..Default::default()
        };
        {
            let c = Core::open(dir.path(), opts()).unwrap();
            put(&c, &v1, a, true).unwrap();
            c.create_snapshot("src").unwrap();
            c.set_swap_fault(4);
            assert!(put(&c, &va, a, true).is_err());
            c.set_swap_fault(0);
            let staged = staging_name(a);
            assert!(c.inner.snap_by_name_raw(&staged).is_ok());
            // a plain ingest, a replacing ingest and a promote of `b` are all refused
            for r in [
                put(&c, &vb, b, false).is_err(),
                put(&c, &vb, b, true).is_err(),
                c.swap_snapshot("src", b).is_err(),
            ] {
                assert!(r, "a colliding call must be refused");
            }
            assert!(c.inner.snap_by_name_raw(&staged).is_ok(), "A's tree kept");
            assert!(intent_path(&c.inner.root, a).exists(), "A's intent kept");
            assert!(c.inner.snap_by_name(b).is_err(), "B created nothing");
        }
        let c = Core::open(dir.path(), opts()).unwrap();
        let v = c.snapshot_view(a).unwrap();
        let ino = v.lookup(ROOT_INO, b"f").unwrap().ino;
        assert_eq!(v.read(ino, 0, 64).unwrap(), b"content of A");
        HASH_OVERRIDE.with(|h| h.set(None));
    }

    /// A torn intent whose roll-forward fails keeps the staging tree and the intent file, and a
    /// later recovery still lands the new tree (here from the image of an older release, where the
    /// old target was already removed: the torn record only rolls forward when the target is gone).
    #[test]
    fn a_failed_roll_forward_of_a_torn_intent_keeps_the_only_copy() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let v1 = src_dir(scratch.path(), "v1", "old");
        let v2 = src_dir(scratch.path(), "v2", "new");
        let c = Core::open(
            dir.path(),
            crate::Options {
                background: false,
                ..Default::default()
            },
        )
        .unwrap();
        put(&c, &v1, "abc", true).unwrap();
        c.set_swap_fault(4);
        assert!(put(&c, &v2, "abc", true).is_err());
        c.set_swap_fault(0);
        // the image an older release left: the old target removed before the rename
        let old = c.inner.snap_by_name("abc").unwrap();
        c.inner.unregister(&old).unwrap();
        let p = intent_path(&c.inner.root, "abc");
        fs::write(&p, "torn").unwrap();
        FAIL_FINISH.with(|f| f.set(true));
        assert!(recover_intent(&c, &p).is_err());
        FAIL_FINISH.with(|f| f.set(false));
        assert!(p.exists(), "intent kept");
        assert!(c.inner.snap_by_name_raw(&staging_name("abc")).is_ok());
        recover_intent(&c, &p).unwrap();
        let v = c.snapshot_view("abc").unwrap();
        let ino = v.lookup(ROOT_INO, b"f").unwrap().ino;
        assert_eq!(v.read(ino, 0, 64).unwrap(), b"new");
    }

    /// N3: replacing an existing target whose temp intent name would not fit fails on the first
    /// call, before anything is staged. A fresh name needs no intent, so it is not refused.
    #[test]
    fn a_replacing_target_too_long_for_its_intent_is_refused_on_the_first_call() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let v = src_dir(scratch.path(), "v", "x");
        let c = Core::open(
            dir.path(),
            crate::Options {
                background: false,
                ..Default::default()
            },
        )
        .unwrap();
        let long = "n".repeat(250);
        // no victim: installed without an intent file, as on main
        put(&c, &v, &long, true).unwrap();
        let before = c.meta().snapshots().unwrap().len();
        // a victim exists: refused up front, nothing staged
        assert!(matches!(
            put(&c, &v, &long, true),
            Err(crate::ImportError::Core(ControlError::InvalidName(_)))
        ));
        assert_eq!(
            c.meta().snapshots().unwrap().len(),
            before,
            "nothing staged"
        );
        put(&c, &v, &"n".repeat(246), true).unwrap();
        put(&c, &v, &"n".repeat(246), true).unwrap();
    }

    /// An older release forked the staging snapshot into the target name, then crashed before it
    /// dropped the staging snapshot: the target is a fork of `staged` and already the new tree.
    /// Recovery keeps it and drops the staging snapshot, instead of replacing it with its parent.
    #[test]
    fn a_target_forked_from_the_staging_snapshot_is_kept_and_the_staging_snapshot_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let v1 = src_dir(scratch.path(), "v1", "old");
        let v2 = src_dir(scratch.path(), "v2", "new");
        let c = Core::open(
            dir.path(),
            crate::Options {
                background: false,
                ..Default::default()
            },
        )
        .unwrap();
        put(&c, &v1, "abc", true).unwrap();
        c.set_swap_fault(4);
        assert!(put(&c, &v2, "abc", true).is_err());
        c.set_swap_fault(0);
        let staged = staging_name("abc");
        let old = c.inner.snap_by_name("abc").unwrap();
        c.inner.unregister(&old).unwrap();
        let st = c.inner.snap_by_name_raw(&staged).unwrap();
        c.inner.register(st.snap.fork("abc").unwrap()).unwrap();
        let p = intent_path(&c.inner.root, "abc");
        assert!(p.exists());
        recover_intent(&c, &p).unwrap();
        assert!(!p.exists());
        assert!(
            c.inner.snap_by_name_raw(&staged).is_err(),
            "staging dropped"
        );
        let v = c.snapshot_view("abc").unwrap();
        let ino = v.lookup(ROOT_INO, b"f").unwrap().ino;
        assert_eq!(v.read(ino, 0, 64).unwrap(), b"new");
    }

    /// A target that turns `Busy` between the up-front check and the commit (a handle opened in
    /// that window) is rolled back like any earlier step: the old target stays live and writable,
    /// so no intent may stay behind for a restart to act on.
    #[test]
    fn a_target_that_turns_busy_at_the_commit_is_rolled_back_not_left_pending() {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(
            dir.path(),
            crate::Options {
                background: false,
                ..Default::default()
            },
        )
        .unwrap();
        c.create_snapshot("src").unwrap();
        c.create_snapshot("abc").unwrap();
        let root = c.lookup(ROOT_INO, b"abc").unwrap().ino;
        let f = c.create(root, b"f", 0o644).unwrap().ino;
        let h = c.open(f).unwrap();
        let staged = staging_name("abc");
        let src = c.inner.snap_by_name("src").unwrap();
        c.stage_and_intent(&src, &staged, "abc").unwrap();
        assert_eq!(
            c.finish_live(&staged, "abc").err(),
            Some(ControlError::Busy)
        );
        assert!(
            !intent_path(&c.inner.root, "abc").exists(),
            "no intent left"
        );
        assert!(
            c.inner.snap_by_name_raw(&staged).is_err(),
            "staging dropped"
        );
        c.write(f, 0, b"still writable").unwrap();
        c.release(h).unwrap();
    }

    fn open_core(dir: &Path) -> Core {
        Core::open(
            dir,
            crate::Options {
                background: false,
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// Stages `src` (holding file `f`) over `abc` and runs the live finish with the meta commit
    /// fault `fault` and, optionally, an unreadable re-read. Returns the call's result.
    fn finish_with(c: &Core, fault: u8, reread_fails: bool) -> Result<SnapshotEntry, ControlError> {
        let root = c.lookup(ROOT_INO, b"src").unwrap().ino;
        c.create(root, b"f", 0o644).unwrap();
        let staged = staging_name("abc");
        let src = c.inner.snap_by_name("src").unwrap();
        c.inner.flush_snapshot(&src).unwrap();
        c.stage_and_intent(&src, &staged, "abc").unwrap();
        FAIL_REREAD.with(|f| f.set(reread_fails));
        c.inner.meta.set_commit_fault(fault);
        let r = c.finish_live(&staged, "abc");
        c.inner.meta.set_commit_fault(0);
        FAIL_REREAD.with(|f| f.set(false));
        r
    }

    fn has_f(c: &Core, snap: &[u8]) -> bool {
        let root = c.lookup(ROOT_INO, snap).unwrap().ino;
        c.lookup(root, b"f").is_ok()
    }

    /// A replace commit that fails before anything is written (through `move_name`, so the
    /// victim's `removed` flag is set and undone) provably changed nothing: rolled back like a
    /// failure before it, the old target live and writable, no intent for a restart.
    #[test]
    fn a_commit_error_that_did_not_land_is_rolled_back_not_left_pending() {
        let dir = tempfile::tempdir().unwrap();
        let c = open_core(dir.path());
        c.create_snapshot("src").unwrap();
        c.create_snapshot("abc").unwrap();
        assert!(finish_with(&c, 1, false).is_err());
        let staged = staging_name("abc");
        assert!(
            !intent_path(&c.inner.root, "abc").exists(),
            "no intent left"
        );
        assert!(
            c.inner.snap_by_name_raw(&staged).is_err(),
            "staging dropped"
        );
        assert!(
            !has_f(&c, b"abc"),
            "the old target is still the one answering"
        );
        let root = c.lookup(ROOT_INO, b"abc").unwrap().ino;
        c.create(root, b"g", 0o644).unwrap();
    }

    /// The data-loss direction: a commit that landed but returned `Err` must keep the intent and
    /// the staging snapshot (a rollback would delete the only copy of the new tree's name), and
    /// the next open installs the new tree.
    #[test]
    fn a_commit_that_landed_but_returned_an_error_keeps_the_intent_and_rolls_forward() {
        let dir = tempfile::tempdir().unwrap();
        {
            let c = open_core(dir.path());
            c.create_snapshot("src").unwrap();
            c.create_snapshot("abc").unwrap();
            assert!(finish_with(&c, 2, false).is_err());
            assert!(intent_path(&c.inner.root, "abc").exists(), "intent kept");
            assert!(
                c.inner
                    .meta
                    .durable_snapshots()
                    .unwrap()
                    .iter()
                    .any(|r| r.name == staging_name("abc") || r.name == "abc"),
                "the new tree is still named in the file"
            );
        }
        let c = open_core(dir.path());
        assert!(has_f(&c, b"abc"), "the new tree is installed");
        assert!(!intent_path(&c.inner.root, "abc").exists());
        assert!(c.inner.snap_by_name_raw(&staging_name("abc")).is_err());
    }

    /// An unreadable re-read is an unknown outcome: the intent stays even though nothing landed,
    /// and the next open rolls forward.
    #[test]
    fn an_unreadable_reread_after_a_commit_error_keeps_the_intent() {
        let dir = tempfile::tempdir().unwrap();
        {
            let c = open_core(dir.path());
            c.create_snapshot("src").unwrap();
            c.create_snapshot("abc").unwrap();
            assert!(finish_with(&c, 1, true).is_err());
            assert!(intent_path(&c.inner.root, "abc").exists(), "intent kept");
            assert!(
                c.inner.snap_by_name_raw(&staging_name("abc")).is_ok(),
                "staging kept"
            );
        }
        let c = open_core(dir.path());
        assert!(has_f(&c, b"abc"), "the new tree is installed");
    }
}
