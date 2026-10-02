//! Crash-safe snapshot replacement: stage a fork, record the intent, then move it into place.
//!
//! `cowfs-meta` has no atomic rename of a snapshot, so `Core` cannot replace a name in one
//! transaction. Two rules make the operation safe without one:
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
//! steps 4 to 6 before serving anything. A crash before step 3 leaves a staging snapshot with no
//! intent file, which `Core::open` removes because the name is deterministic.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use cowfs_vfs::Error;

use super::{control_meta, validate_snapshot_name, ControlError, Core, SnapshotEntry};
use crate::util::MutexExt as _;

const SWAP_PREFIX: &str = "swap-";
/// Reserved in snapshot names: see the module docs.
pub(crate) const STAGING: &str = ".cowfs-swap";

/// True for a name only the swap may use.
pub(crate) fn is_staging(name: &str) -> bool {
    name.contains(STAGING)
}

fn intent_path(root: &Path, target: &str) -> PathBuf {
    root.join(format!("{SWAP_PREFIX}{target}"))
}

/// The staging name for `target`; deterministic, so recovery can clean it up without the intent
/// file.
fn staging_name(target: &str) -> String {
    let base: String = target.chars().take(200).collect();
    format!("{base}{STAGING}0")
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

fn io(msg: &str) -> ControlError {
    ControlError::Fs(Error::Io(msg.to_string()))
}

fn write_intent(root: &Path, staged: &str, target: &str) -> Result<(), ControlError> {
    let p = intent_path(root, target);
    let tmp = root.join(format!("{SWAP_PREFIX}{target}.tmp"));
    let mut f = fs::File::create(&tmp).map_err(|e| io(&e.to_string()))?;
    f.write_all(format!("{staged}\n{target}\n").as_bytes())
        .map_err(|e| io(&e.to_string()))?;
    f.sync_all().map_err(|e| io(&e.to_string()))?;
    drop(f);
    fs::rename(&tmp, &p).map_err(|e| io(&e.to_string()))?;
    sync_dir(root);
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

/// Intent files left by an interrupted swap.
fn intents(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(SWAP_PREFIX) && !n.ends_with(".tmp"))
        })
        .collect();
    v.sort();
    v
}

/// Completes every swap a crash or an error left half done when the store is opened.
///
/// A failure is reported through `Core::last_flush_error` and the intent file stays for the next
/// open, so a swap is never silently dropped.
pub(crate) fn recover(core: &Core) {
    for p in intents(&core.inner.root) {
        let Some((staged, target)) = read_intent(&p) else {
            // a torn intent: the staging name is deterministic, so remove it and say so
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.trim_start_matches(SWAP_PREFIX).to_string())
                .unwrap_or_default();
            let staged = staging_name(&name);
            if let Ok(sc) = core.inner.snap_by_name_raw(&staged) {
                let _ = core.inner.unregister(&sc);
            }
            let _ = fs::remove_file(&p);
            *core.inner.last_error.lk() = Some(format!(
                "swap recovery: {p:?} is unreadable, removed any staging snapshot named {staged}"
            ));
            continue;
        };
        if let Err(e) = core.finish_swap(&staged, &target) {
            *core.inner.last_error.lk() = Some(format!("swap recovery: {e}"));
            continue;
        }
        if fs::remove_file(&p).is_ok() {
            sync_dir(&core.inner.root);
        }
    }
}

impl Core {
    /// Replaces snapshot `new` with a clone of `src`.
    ///
    /// `rename_from` is `Some(old)` for a rename, where `old == src` and `old != new`; `None` for a
    /// promote, where the existing `new` is the victim.
    /// Returns `Ok` only when the new name is in place. Returns `Err` with the mount unchanged,
    /// except after the old target was removed, where the swap is rolled forward instead.
    /// The two forks change the snapshot id, so every inode number in the new snapshot differs
    /// from the old one's.
    pub(crate) fn swap_snapshot(
        &self,
        src: &str,
        rename_from: Option<&str>,
        new: &str,
    ) -> Result<SnapshotEntry, ControlError> {
        validate_snapshot_name(new)?;
        if src == new {
            return Err(ControlError::InvalidName(
                "source and target are the same snapshot",
            ));
        }
        let src_sc = self.inner.snap_by_name(src)?;
        // the snapshot whose name goes away: the renamed one, or an existing target of a promote
        let victim: Option<&str> = match rename_from {
            Some(o) => Some(o),
            None if self.inner.snap_by_name(new).is_ok() => Some(new),
            None => None,
        };
        self.inner.check_new_name_except(new, victim)?;
        let staged = staging_name(new);
        // a leftover staging snapshot from an earlier crash: the intent file is gone, so this is
        // garbage. Removing it is what `Core::open` does; do the same here.
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
        let _ = fs::remove_file(self.inner.root.join(format!("{SWAP_PREFIX}{target}.tmp")));
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
}
