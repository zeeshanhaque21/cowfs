//! Crash-safe snapshot replacement: stage a fork, record the intent, then move it into place.
//!
//! This is the replacement path, for promotion. A rename moves a name inside one metadata
//! transaction and does not come here; a promotion has to destroy the target it replaces, and
//! that cannot be one transaction. Two rules make the replacement safe without one:
//!
//! - The staging snapshot's name carries the reserved suffix `.cowfs-swap<N>`. The name rules
//!   refuse it, so no caller can create or move a snapshot into it, and the synthetic root filters
//!   it out of the listing, so it is never visible.
//! - An operation that returns `Err` leaves the mount exactly as it was, unless the old target was
//!   already removed, in which case the swap is rolled forward and the call returns `Ok`. There is
//!   no third state: a name is never missing from the live mount without an intent file that
//!   explains it.
//!
//! The order, with the point of no return at step 3:
//!
//! 1. fork the source into a staging name (a failure here changes nothing),
//! 2. write and sync the intent file `<root>/swap-<target>`, naming the staging and target snapshots,
//! 3. remove the old target if there is one,          <- rollback is no longer possible
//! 4. fork the staging snapshot into the target name,
//! 5. remove the staging snapshot,
//! 6. remove the intent file.
//!
//! A crash or error from step 3 on leaves the intent file, and the next `Core::open` finishes
//! steps 4 to 6 before serving anything. A swap or replacing import of a target with a pending
//! intent finishes that intent first (`Core::recover_target`), so a retry never deletes the only
//! copy of a tree. A crash before step 3 leaves a hidden staging snapshot with no intent file;
//! `Core::open` removes every such orphan once the intents are recovered, before anything can
//! stage a new one, so it cannot take a staging snapshot a live operation owns.

use std::fs;
use std::io::Write as _;
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

/// The staging name for `target`; deterministic, so recovery can clean it up without the intent
/// file. An import stages into the same name: its crash leaves nothing a caller can see either.
pub(crate) fn staging_name(target: &str) -> String {
    let base: String = target.chars().take(200).collect();
    format!("{base}{STAGING}0")
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
    let mut f = fs::File::create(&tmp).map_err(|e| io(&e.to_string()))?;
    f.write_all(format!("{staged}\n{target}\n").as_bytes())
        .map_err(|e| io(&e.to_string()))?;
    // the record must be on the medium before the rename, and the rename on the medium before
    // anything destructive reads the intent file
    crate::fsops::sync_file(&f, &tmp).map_err(|e| io(&e.to_string()))?;
    drop(f);
    fs::rename(&tmp, &p).map_err(|e| io(&e.to_string()))?;
    crate::fsops::note("intent_renamed");
    crate::fsops::sync_dir(root).map_err(|e| io(&e.to_string()))?;
    Ok(())
}

/// The staging and target names of an intent file, or `None` if it is unreadable or torn.
fn read_intent(p: &Path) -> Option<(String, String)> {
    let s = fs::read_to_string(p).ok()?;
    let mut it = s.lines();
    let staged = it.next()?.to_string();
    let target = it.next()?.to_string();
    (!staged.is_empty() && !target.is_empty() && is_staging(&staged)).then_some((staged, target))
}

/// Intent files left by an interrupted swap. Every `swap-*` file is one: the writer's temp files
/// have their own prefix. A `swap-<X>.tmp` left by the older temp naming is dropped by
/// `recover_intent`, which finds its name disagrees with the target it records.
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
/// A torn intent file names nothing, so the staging name its target implies is removed with it.
fn recover_intent(core: &Core, p: &Path) -> Result<(), ControlError> {
    let Some((staged, target)) = read_intent(p) else {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix(SWAP_PREFIX))
            .map(str::to_string)
            .unwrap_or_default();
        let staged = staging_name(&name);
        if let Ok(sc) = core.inner.snap_by_name_raw(&staged) {
            let _ = core.inner.unregister(&sc);
        }
        let _ = fs::remove_file(p);
        *core.inner.last_error.lk() = Some(format!(
            "swap recovery: {p:?} is unreadable, removed any staging snapshot named {staged}"
        ));
        return Ok(());
    };
    if intent_path(&core.inner.root, &target) != p {
        // an older release's temp file (`swap-<target>.tmp`): its swap had not started
        let _ = fs::remove_file(p);
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
    if fs::remove_file(p).is_ok() {
        sync_dir(&core.inner.root);
    }
    Ok(())
}

/// Completes every swap a crash or an error left half done, then removes the staging snapshots no
/// intent names, when the store is opened.
///
/// A failure is reported through `Core::last_flush_error` and the intent file stays for the next
/// open, so a swap is never silently dropped.
pub(crate) fn recover(core: &Core) {
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

impl Core {
    /// Replaces snapshot `new` with a clone of `src`, for the promotion path. A rename moves a name
    /// rather than replacing one and does not come here: it has no victim to remove and needs no
    /// staging, so `Core::rename_snapshot` commits the name directly.
    /// Returns `Ok` only when the new name is in place. Returns `Err` with the mount unchanged,
    /// except after the old target was removed, where the swap is rolled forward instead.
    /// The two forks change the snapshot id, so every inode number in the new snapshot differs
    /// from the old one's.
    pub(crate) fn swap_snapshot(
        &self,
        src: &str,
        new: &str,
    ) -> Result<SnapshotEntry, ControlError> {
        validate_snapshot_name(new)?;
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
        if let Ok(leftover) = self.inner.snap_by_name_raw(&staged) {
            let _ = self.inner.unregister(&leftover);
        }
        self.inner.flush_snapshot(&src_sc)?;
        self.stage_and_intent(&src_sc, &staged, new)?;
        if let Err(e) = self.fault(1).and_then(|()| self.fault(2)) {
            self.rollback(&staged, new);
            return Err(e);
        }
        if let Err(e) = self.fault(3) {
            self.rollback(&staged, new);
            return Err(e);
        }
        // point of no return: the old target is about to go
        if let Some(v) = victim {
            let sc = self.inner.snap_by_name(v)?;
            if let Err(e) = self.inner.unregister(&sc) {
                self.rollback(&staged, new);
                return Err(e);
            }
            crate::fsops::note("victim_removed");
        }
        // Past this point an error cannot be reported as "nothing happened", so the swap is rolled
        // forward instead and the call succeeds. The only exception is a failure of the roll
        // forward itself (an I/O error), which returns `Err` with the intent file on disk: the next
        // `Core::open` completes it.
        if let Err(e) = self.fault(4) {
            *self.inner.last_error.lk() = Some(format!("swap: {e}, rolled forward"));
        }
        let done = self.finish_swap(&staged, new);
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
        if let Ok(old) = self.inner.snap_by_name(target) {
            if let Err(e) = self.inner.unregister(&old) {
                self.rollback(staged, target);
                return Err(e);
            }
        }
        // Past this point the old target is gone and only `staged` holds the new tree, so an error
        // is returned as it is and nothing is cleaned up: the intent file makes `Core::open`, or the
        // next call for this target, finish. Fault 4 is that error without running `finish_swap`.
        self.fault(4)?;
        self.finish_swap(staged, target)
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
    fn rollback(&self, staged: &str, target: &str) {
        if let Ok(sc) = self.inner.snap_by_name_raw(staged) {
            let _ = self.inner.unregister(&sc);
        }
        let _ = fs::remove_file(intent_path(&self.inner.root, target));
        let _ = fs::remove_file(tmp_path(&self.inner.root, target));
        sync_dir(&self.inner.root);
    }

    /// Steps 4 to 6: fork the staging snapshot into the target name, remove the staging snapshot,
    /// remove the intent file.
    pub(crate) fn finish_swap(
        &self,
        staged: &str,
        target: &str,
    ) -> Result<SnapshotEntry, ControlError> {
        let mut entry = None;
        if self.inner.snap_by_name(target).is_err() {
            let sc = self.inner.snap_by_name_raw(staged)?;
            self.inner.flush_snapshot(&sc)?;
            let fork = sc.snap.fork(target).map_err(control_meta)?;
            entry = Some(self.inner.register(fork)?);
        }
        if let Ok(st) = self.inner.snap_by_name_raw(staged) {
            let _ = self.inner.unregister(&st);
        }
        if fs::remove_file(intent_path(&self.inner.root, target)).is_ok() {
            sync_dir(&self.inner.root);
        }
        entry.ok_or(ControlError::NotFound)
    }

    /// Test seam: make the swap fail at `step` (1 to 5). 0 disables it.
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
}
