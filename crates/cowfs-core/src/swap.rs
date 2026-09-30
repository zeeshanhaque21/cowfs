//! Crash-safe snapshot swap: stage a fork, record the intent, then move it into place.
//!
//! `cowfs-meta` has no atomic rename of a snapshot, so `Core` cannot replace a name in one
//! transaction.
//! Instead every replace goes through this order, and the intent file lets a later `Core::open`
//! finish a swap that a crash or an error left half done:
//!
//! 1. fork the source into a staging name (nothing is lost if this fails),
//! 2. write and sync the intent file `<root>/swap-<target>`, naming the staging and target snapshots,
//! 3. remove the old target (if any),
//! 4. fork the staging snapshot into the target name,
//! 5. remove the staging snapshot,
//! 6. remove the intent file.
//!
//! Any failure before step 3 leaves the old target untouched.
//! Any failure or crash from step 3 on leaves the intent file, and the next open completes steps
//! 4 to 6.
//! So a snapshot name never disappears without a record that explains it, and a failed swap never
//! costs the old base.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use cowfs_vfs::Error;

use super::{control_meta, validate_snapshot_name, ControlError, Core, SnapshotEntry};

const SWAP_PREFIX: &str = "swap-";

fn intent_path(root: &Path, target: &str) -> PathBuf {
    root.join(format!("{SWAP_PREFIX}{target}"))
}

fn staging_name(target: &str, n: u32) -> String {
    let base: String = target.chars().take(200).collect();
    format!("{base}.cowfs-swap{n}")
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
    let mut f = fs::File::create(&p).map_err(|e| io(&e.to_string()))?;
    f.write_all(format!("{staged}\n{target}\n").as_bytes())
        .map_err(|e| io(&e.to_string()))?;
    f.sync_all().map_err(|e| io(&e.to_string()))?;
    let d = fs::File::open(root).map_err(|e| io(&e.to_string()))?;
    d.sync_all().map_err(|e| io(&e.to_string()))
}

fn read_intent(p: &Path) -> Option<(String, String)> {
    let s = fs::read_to_string(p).ok()?;
    let mut it = s.lines();
    let staged = it.next()?.to_string();
    let target = it.next()?.to_string();
    (!staged.is_empty() && !target.is_empty()).then_some((staged, target))
}

/// Intent files left by an interrupted swap.
pub(crate) fn intents(root: &Path) -> Vec<PathBuf> {
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

/// Completes every swap a crash or an error left half done when the store is opened.
/// A failure is reported through `Core::last_flush_error` and the intent file stays for the next
/// open, so a swap is never silently dropped.
pub(crate) fn recover(core: &Core) {
    for p in intents(&core.inner.root) {
        let Some((staged, target)) = read_intent(&p) else {
            continue;
        };
        if let Err(e) = core.finish_swap(&staged, &target) {
            *crate::util::MutexExt::lk(&core.inner.last_error) =
                Some(format!("swap recovery: {e}"));
            continue;
        }
        if fs::remove_file(&p).is_ok() {
            sync_dir(&core.inner.root);
        }
    }
}

impl Core {
    /// Replaces snapshot `new` with a clone of `src`, or renames `old` when `old == src`.
    ///
    /// `replace` allows an existing `new` to be overwritten (a promote); without it an existing
    /// `new` is `ControlError::Exists`.
    /// The staging and final forks change the snapshot id, so every inode number in the new
    /// snapshot differs from the old one's (see `docs/v1-core.md`).
    pub(crate) fn swap_snapshot(
        &self,
        src: &str,
        old: Option<&str>,
        new: &str,
        replace: bool,
    ) -> Result<SnapshotEntry, ControlError> {
        validate_snapshot_name(new)?;
        if src == new || Some(new) == old {
            return Err(ControlError::InvalidName(
                "source and target are the same snapshot",
            ));
        }
        let src_sc = self.inner.snap_by_name(src)?;
        if self.inner.snap_by_name(new).is_ok() && !replace {
            return Err(ControlError::Exists);
        }
        let mut staged = String::new();
        for n in 0..1000u32 {
            let cand = staging_name(new, n);
            if self.inner.snap_by_name(&cand).is_err() {
                staged = cand;
                break;
            }
        }
        if staged.is_empty() {
            return Err(io("no free staging name"));
        }
        self.inner.flush_snapshot(&src_sc)?;
        self.fault(1)?;
        let fork = src_sc.snap.fork(&staged).map_err(control_meta)?;
        self.inner.register(fork)?;
        if let Err(e) = self
            .fault(2)
            .and_then(|()| write_intent(&self.inner.root, &staged, new))
        {
            // nothing observable was changed, so leave no staging snapshot behind
            if let Ok(st) = self.inner.snap_by_name(&staged) {
                let _ = self.inner.unregister(&st);
            }
            return Err(e);
        }
        self.fault(3)?;
        let victim = old
            .or(Some(new))
            .filter(|v| self.inner.snap_by_name(v).is_ok());
        if let Some(v) = victim {
            let sc = self.inner.snap_by_name(v)?;
            self.inner.unregister(&sc)?;
        }
        self.fault(4)?;
        self.finish_swap(&staged, new)
    }

    /// Steps 4 to 6 of a swap.
    fn finish_swap(&self, staged: &str, target: &str) -> Result<SnapshotEntry, ControlError> {
        let mut entry = None;
        if self.inner.snap_by_name(target).is_err() {
            let sc = self.inner.snap_by_name(staged)?;
            self.inner.flush_snapshot(&sc)?;
            let fork = sc.snap.fork(target).map_err(control_meta)?;
            entry = Some(self.inner.register(fork)?);
        }
        if let Ok(st) = self.inner.snap_by_name(staged) {
            let _ = self.inner.unregister(&st);
        }
        if fs::remove_file(intent_path(&self.inner.root, target)).is_ok() {
            sync_dir(&self.inner.root);
        }
        entry.ok_or(ControlError::NotFound)
    }

    /// Test seam: make the swap fail after `step` (1 to 4). 0 disables it.
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
