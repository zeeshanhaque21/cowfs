//! `mount_snapshot` and `unmount_snapshot`: a snapshot at a path the client chooses.
//!
//! A client-chosen path is a mount primitive, so it is a capability and the server keeps it.
//! Every rule in the table in `docs/v1-treehouse.md` ("What the daemon must refuse for
//! `mount_snapshot`") is enforced here, and each has a test named after it. A control-socket
//! client is not the treehouse companion, so the daemon re-checks what the companion checks.
//!
//! Atomicity and the crash rule: one operation holds the registry lock across the whole
//! export, so no caller ever sees a half-built export, and a snapshot that was already
//! exported keeps its export until the new one is fully mounted. A daemon killed mid-export
//! leaves a mount the next start sweeps, which is the crash rule every cowfs operation has.

use crate::backend::Backend;
use crate::daemon::uid;
use crate::holders;
use crate::mounts::Mounted;
use cowfs_ctl::{
    validate_abs_path, validate_snapshot_name, CtlError, CtlResult, ErrorCode, MountSnapshot,
    UnmountSnapshot,
};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// How many components below an export root a path must sit, so it is at least
/// `{root}/{pool}/{slot}/{repo}`.
pub const MIN_COMPONENTS_BELOW_ROOT: usize = 3;

fn invalid(why: impl Into<String>) -> CtlError {
    CtlError::new(ErrorCode::InvalidParams, why.into())
}

/// `busy` carries the holders that caused it, so a caller can name them the way
/// `snapshot_rm` does.
fn busy(what: &str, holders: &[cowfs_ctl::ProcessInfo]) -> CtlError {
    #[derive(serde::Serialize)]
    struct Details<'a> {
        holders: &'a [cowfs_ctl::ProcessInfo],
    }
    CtlError::new(ErrorCode::Busy, format!("{what} is held"))
        .with_details(serde_json::to_value(Details { holders }).unwrap_or(serde_json::Value::Null))
}

#[derive(Debug)]
struct Export {
    name: String,
    /// `None` only in tests, which fill the registry without a real mount.
    mount: Option<Mounted>,
}

/// The daemon's live exports, and the rules that decide whether one may be created.
#[derive(Clone, Debug)]
pub struct Exports {
    backend: Arc<dyn Backend>,
    roots: Arc<Vec<PathBuf>>,
    forbidden: Arc<Vec<PathBuf>>,
    /// Where the default mount is. A holder is a process inside the snapshot a client sees,
    /// which is under here. On the core backend the store holds no per-snapshot directory at
    /// all, so scanning the store would report no holder for any snapshot.
    default_mount: PathBuf,
    live: Arc<Mutex<HashMap<PathBuf, Export>>>,
}

impl Exports {
    /// Builds the registry. `roots` is the set of directories a client may export inside,
    /// `forbidden` is the default mount point and the store, which are never targets, and
    /// `default_mount` is the default mount point itself.
    pub fn new(
        backend: Arc<dyn Backend>,
        roots: Vec<PathBuf>,
        forbidden: Vec<PathBuf>,
        default_mount: PathBuf,
    ) -> Exports {
        let resolve = |paths: Vec<PathBuf>| -> Vec<PathBuf> {
            let mut out: Vec<PathBuf> = paths
                .into_iter()
                .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
                .collect();
            out.sort();
            out.dedup();
            out
        };
        Exports {
            backend,
            roots: Arc::new(resolve(roots)),
            forbidden: Arc::new(resolve(forbidden)),
            default_mount,
            live: Arc::default(),
        }
    }

    /// The export roots, as resolved.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Snapshot name to export path, for what is live right now.
    pub fn live(&self) -> HashMap<PathBuf, String> {
        self.lock()
            .iter()
            .map(|(p, e)| (p.clone(), e.name.clone()))
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<PathBuf, Export>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The whole rule table, then the mount. One lock is held across the whole operation, so
    /// the holder check and the export cannot be split apart and two exports of one snapshot
    /// cannot interleave.
    pub fn mount_snapshot(&self, req: &MountSnapshot) -> CtlResult<()> {
        // The lock is held across the whole operation, so the holder check and the export
        // cannot be split apart and two exports of one snapshot cannot interleave.
        let mut live = self.lock();
        self.check(req, &live)?;
        if req.expect_no_holders {
            let held = holders::scan(&self.snapshot_dir(&req.name));
            if !held.is_empty() {
                return Err(busy(&format!("snapshot {:?}", req.name), &held));
            }
        }
        let path = PathBuf::from(&req.path);
        let vfs = self
            .backend
            .snapshot(&req.name)
            .map_err(|e| self.not_found_or_io(e, &req.name))?;
        let mount = Mounted::mount(vfs, &path).map_err(|e| {
            invalid(format!(
                "cannot export {:?} at {}: {e}",
                req.name,
                path.display()
            ))
        })?;
        live.insert(
            path,
            Export {
                name: req.name.clone(),
                mount: Some(mount),
            },
        );
        Ok(())
    }

    /// Removes one export. `busy` while anything holds it, and nothing changed then.
    pub fn unmount_snapshot(&self, req: &UnmountSnapshot) -> CtlResult<()> {
        validate_abs_path("path", &req.path)?;
        let path = PathBuf::from(&req.path);
        let mut live = self.lock();
        if !live.contains_key(&path) {
            return Err(CtlError::not_found(format!(
                "{} is not an export of this daemon",
                path.display()
            )));
        }
        let held = holders::scan(&path);
        if !held.is_empty() {
            return Err(busy(&path.display().to_string(), &held));
        }
        // Dropping the `Mounted` unmounts and stops its server, synchronously.
        drop(live.remove(&path));
        Ok(())
    }

    /// Unmounts every export, for shutdown. Returns what could not be unmounted, so a caller
    /// can report a mount that outlived the daemon instead of leaving it silent.
    pub fn unmount_all(&self) -> Vec<String> {
        let mut live = self.lock();
        let paths: Vec<PathBuf> = live.keys().cloned().collect();
        let mut stuck = Vec::new();
        for p in paths {
            let Some(Export { name, mount }) = live.remove(&p) else {
                continue;
            };
            if let Some(mount) = mount {
                if let Err(e) = mount.unmount() {
                    stuck.push(format!("{name} at {}: {e}", p.display()));
                }
            }
        }
        stuck
    }

    /// The whole rule table. Every refusal is `invalid_params` except a missing snapshot
    /// (`not_found`), so a client can tell "you asked wrong" from "there is nothing there".
    fn check(&self, req: &MountSnapshot, live: &HashMap<PathBuf, Export>) -> CtlResult<()> {
        // Rule: `name` must pass `validate_snapshot_name` and the snapshot must exist. This is
        // also what keeps `.nfs*` and AppleDouble out of an export name.
        validate_snapshot_name(&req.name)?;
        if !self
            .backend
            .snapshots()
            .list()
            .unwrap_or_default()
            .contains(&req.name)
        {
            return Err(CtlError::not_found(format!(
                "snapshot {:?} does not exist",
                req.name
            )));
        }
        // Rule: `path` must be absolute, at most 4096 bytes and free of control characters.
        validate_abs_path("path", &req.path)?;
        let path = PathBuf::from(&req.path);
        // Rule: `path` must not contain `..`, and must not resolve to an existing symlink.
        // `Path::components` silently drops a `.` in the middle, so the components are taken
        // from the string as the client wrote it.
        if req.path.split('/').any(|c| c == "." || c == "..") {
            return Err(invalid(format!(
                "{} must not contain `.` or `..`",
                path.display()
            )));
        }
        // Rule: `path` must not be the mount point, an ancestor of the mount point, or the
        // store directory. The ancestors matter too, so an export cannot land inside the store.
        // This is checked before containment, so the refusal names the real reason when a
        // caller aims at the store from inside a pool.
        for denied in self.forbidden.iter() {
            if path == *denied || denied.starts_with(&path) || path.starts_with(denied) {
                return Err(invalid(format!(
                    "{} may not be the mount point, an ancestor of it, or the store",
                    path.display()
                )));
            }
        }
        // Rule: `path` must be inside a configured export root, at least three components
        // below it: `{root}/{pool}/{slot}/{repo}`.
        let root = self.root_for(&path)?;
        let below = path
            .strip_prefix(&root)
            .unwrap_or(path.as_path())
            .to_owned();
        let depth = below.components().count();
        if depth < MIN_COMPONENTS_BELOW_ROOT {
            return Err(invalid(format!(
                "{} must be at least {MIN_COMPONENTS_BELOW_ROOT} components below the export root {}",
                path.display(),
                root.display()
            )));
        }
        // Rule: no component of `path` may be a symlink, resolved from the export root down,
        // and every existing component up to the root must be a directory this uid owns.
        self.check_components(&root, &below)?;
        // Rule: the export root must not be group or other writable: a world-writable parent
        // lets another user aim the mount.
        self.check_root(&root)?;
        // Rule: `path` must be absent, or an empty directory that contains nothing but `.` and
        // `..`. Mounting over a non-empty directory hides real data, which is what stops a bad
        // path from silently shadowing a checkout.
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(invalid(format!("cannot read {}: {e}", path.display()))),
            Ok(md) if md.file_type().is_symlink() => {
                return Err(invalid(format!("{} is a symlink", path.display())))
            }
            Ok(md) if !md.is_dir() => {
                return Err(invalid(format!("{} is not a directory", path.display())))
            }
            Ok(_) => {
                let mut entries = std::fs::read_dir(&path)
                    .map_err(|e| invalid(format!("cannot read {}: {e}", path.display())))?;
                if entries.next().is_some() {
                    return Err(invalid(format!(
                        "{} is not empty, so mounting over it would hide real data",
                        path.display()
                    )));
                }
            }
        }
        // Rule: `name` must not already be exported at a different `path`: one snapshot, one
        // export, or two live mountpoints for one writable tree.
        if let Some(elsewhere) = live
            .iter()
            .find(|(_, e)| e.name == req.name)
            .map(|(p, _)| p.clone())
        {
            return Err(invalid(format!(
                "snapshot {:?} is already exported at {}",
                req.name,
                elsewhere.display()
            )));
        }
        if live.contains_key(&path) {
            return Err(invalid(format!(
                "{} is already an export of this daemon",
                path.display()
            )));
        }
        Ok(())
    }

    fn root_for(&self, path: &Path) -> CtlResult<PathBuf> {
        self.roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .cloned()
            .ok_or_else(|| {
                invalid(format!(
                    "{} is not inside a configured export root ({})",
                    path.display(),
                    self.roots
                        .iter()
                        .map(|r| r.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
    }

    /// Component by component from the export root down, never following a link. A single
    /// `realpath` at the end checks the wrong thing: it resolves the links and then reports
    /// success, which is exactly the bypass this rule exists to close. A component that does
    /// not exist yet is fine: the slot treehouse is about to create has not been created.
    fn check_components(&self, root: &Path, below: &Path) -> CtlResult<()> {
        use std::os::unix::fs::MetadataExt;
        let mut at = root.to_owned();
        for component in below.components() {
            at.push(component);
            match std::fs::symlink_metadata(&at) {
                Ok(md) if md.file_type().is_symlink() => {
                    return Err(invalid(format!(
                        "{} is a symlink, so a mount aimed here could leave the export root",
                        at.display()
                    )))
                }
                Ok(md) if !md.is_dir() => {
                    return Err(invalid(format!("{} is not a directory", at.display())))
                }
                Ok(md) if md.uid() != uid() => {
                    return Err(invalid(format!(
                        "{} is owned by another user",
                        at.display()
                    )))
                }
                Err(e) if e.kind() == ErrorKind::NotFound => break,
                Err(e) => return Err(invalid(format!("cannot read {}: {e}", at.display()))),
                _ => {}
            }
        }
        Ok(())
    }

    /// The daemon's uid must own the export root, and the root must not be group or other
    /// writable. The same rule the socket directory obeys.
    fn check_root(&self, root: &Path) -> CtlResult<()> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let md = std::fs::metadata(root).map_err(|e| {
            invalid(format!(
                "cannot read the export root {}: {e}",
                root.display()
            ))
        })?;
        if md.uid() != uid() {
            return Err(invalid(format!(
                "the export root {} is owned by another user",
                root.display()
            )));
        }
        if md.permissions().mode() & 0o077 != 0 {
            return Err(invalid(format!(
                "the export root {} is group or other writable",
                root.display()
            )));
        }
        Ok(())
    }

    /// Where a client sees snapshot `name`: a directory under the default mount. This is also what
    /// the handler's `ps` scans, so the two agree on who is holding what.
    fn snapshot_dir(&self, name: &str) -> PathBuf {
        self.default_mount.join(name)
    }

    fn not_found_or_io(&self, e: std::io::Error, name: &str) -> CtlError {
        match e.kind() {
            ErrorKind::NotFound => CtlError::not_found(format!("snapshot {name:?} does not exist")),
            _ => CtlError::new(ErrorCode::IoError, e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::PathBackend;
    use crate::mounts;

    /// Rule: `path` must be absolute, at most 4096 bytes and free of control characters.
    #[test]
    fn a_bad_path_is_refused() {
        let f = Fixture::new();
        for bad in [
            "relative/path".to_owned(),
            String::new(),
            format!("/{}", "x".repeat(4096)),
            "/a\nb".to_owned(),
        ] {
            let mut req = f.request("1");
            req.path = bad.clone();
            let e = f.reject(&req);
            assert_eq!(e.code, ErrorCode::InvalidParams, "{bad:?}: {e}");
        }
    }

    /// Rule: `path` must be inside a configured export root, at least three components below it.
    #[test]
    fn a_path_outside_every_root_or_too_shallow_is_refused() {
        let f = Fixture::new();
        let other = f.root.parent().unwrap().join("elsewhere");
        std::fs::create_dir_all(other.join("p").join("1").join("repo")).unwrap();
        let mut outside = f.request("1");
        outside.path = other.join("p").join("1").join("repo").display().to_string();
        let e = f.reject(&outside);
        assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
        assert!(e.message.contains("export root"), "{e}");

        for shallow in [
            f.root.join("repo-abc123").display().to_string(),
            f.pool.join("1").display().to_string(),
        ] {
            let mut req = f.request("1");
            req.path = shallow;
            let e = f.reject(&req);
            assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
            assert!(e.message.contains("components below"), "{e}");
        }
    }

    /// Rule: no component of `path` may be a symlink, resolved from the export root down.
    #[test]
    fn a_symlinked_component_is_refused() {
        let f = Fixture::new();
        let link = f.pool.join("linked");
        std::os::unix::fs::symlink("/tmp", &link).unwrap();
        let mut req = f.request("1");
        req.path = link.join("x").join("y").join("z").display().to_string();
        let e = f.reject(&req);
        assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
        assert!(e.message.contains("symlink"), "{e}");

        // The target itself being a symlink is refused too, with nothing above it linked.
        std::fs::create_dir_all(f.pool.join("2").join("repo")).unwrap();
        std::os::unix::fs::symlink(f.pool.join("2"), f.pool.join("3")).unwrap();
        let mut req = f.request("3");
        req.path = f.pool.join("3").join("repo").display().to_string();
        let e = f.reject(&req);
        assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
        assert!(e.message.contains("symlink"), "{e}");
    }

    /// Rule: `path` must not contain `..`, and must not resolve to an existing symlink.
    #[test]
    fn a_dot_component_is_refused() {
        let f = Fixture::new();
        for bad in [
            format!("{}/../2/repo", f.slot("1")),
            format!("{}/./repo", f.pool.join("1").display()),
        ] {
            let mut req = f.request("1");
            req.path = bad.clone();
            let e = f.reject(&req);
            assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
            assert!(e.message.contains("`..`"), "{e}");
        }
    }

    /// Rule: `path` must be absent, or an empty directory.
    #[test]
    fn a_non_empty_or_non_directory_target_is_refused() {
        let f = Fixture::new();
        let occupied = f.pool.join("1").join("repo");
        std::fs::write(occupied.join("Cargo.toml"), b"x").unwrap();
        let e = f.reject(&f.request("1"));
        assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
        assert!(e.message.contains("not empty"), "{e}");

        let file = f.pool.join("4").join("repo");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        let mut req = f.request("4");
        req.path = file.display().to_string();
        let e = f.reject(&req);
        assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
        assert!(e.message.contains("not a directory"), "{e}");
    }

    /// Rule: `path` must not be the mount point, an ancestor of the mount point, or the store.
    #[test]
    fn the_mount_point_the_store_and_their_ancestors_are_refused() {
        let f = Fixture::new();
        let store = f.backend.store_path().to_owned();
        let mut targets = vec![store.clone(), f.mount.clone()];
        targets.push(store.parent().unwrap().to_owned());
        targets.push(f.mount.parent().unwrap().to_owned());
        for denied in targets {
            let mut req = f.request("1");
            req.path = denied.display().to_string();
            let e = f.reject(&req);
            assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
            assert!(e.message.contains("mount point"), "{e}");
        }
    }

    /// Rule: the daemon's uid must own `path` and every component up to the export root, and
    /// the export root must not be group or other writable.
    #[test]
    fn a_group_writable_export_root_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new();
        std::fs::set_permissions(&f.root, std::fs::Permissions::from_mode(0o707)).unwrap();
        let e = f.reject(&f.request("1"));
        assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
        assert!(e.message.contains("writable"), "{e}");
    }

    /// Rule: `name` must pass `validate_snapshot_name`, and the snapshot must exist.
    #[test]
    fn a_bad_or_missing_snapshot_name_is_refused() {
        let f = Fixture::new();
        for bad in ["../escape", ".hidden", "a/b", ""] {
            let mut req = f.request("1");
            req.name = bad.to_owned();
            let e = f.reject(&req);
            assert_eq!(e.code, ErrorCode::InvalidParams, "{bad:?}: {e}");
        }
        let mut missing = f.request("1");
        missing.name = "nosuch".into();
        assert_eq!(f.reject(&missing).code, ErrorCode::NotFound);
    }

    /// Rule: `name` must not already be exported at a different `path`.
    #[test]
    fn one_snapshot_is_exported_once() {
        let f = Fixture::new();
        let first = f.request("1");
        f.exports
            .record_for_test(PathBuf::from(&first.path), "base");
        let e = f.reject(&f.request("2"));
        assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
        assert!(e.message.contains("already exported"), "{e}");

        // The same path under a different name is refused too: one path, one export.
        let mut other_name = f.request("1");
        other_name.name = "base2".into();
        let e = f.reject(&other_name);
        assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
        assert!(e.message.contains("already an export"), "{e}");
    }

    /// Rule: `expect_no_holders` defaults to true and is evaluated under the same lock as the
    /// export, so a holder makes it `busy` and nothing changes. Then a free snapshot exports.
    #[test]
    fn a_holder_makes_the_export_busy_and_changes_nothing() {
        let f = Fixture::new();
        let held = f.mount.join("base");
        std::fs::write(held.join("marker"), b"x").unwrap();
        let keep = crate::holders::Holder::holding(&held.join("marker"));
        let req = f.request("1");
        let e = f
            .exports
            .mount_snapshot(&req)
            .expect_err("a holder must be refused");
        assert_eq!(e.code, ErrorCode::Busy, "{e}");
        assert!(f.exports.live().is_empty(), "nothing was exported");
        drop(keep);

        assert!(f.exports.mount_snapshot(&req).is_ok());
        assert_eq!(f.exports.live().len(), 1);
        assert!(f.exports.unmount_all().is_empty());
    }

    /// Rule: the whole operation is atomic, and a refused request leaves no trace, so a later
    /// attempt with a good path succeeds. The cheapest proof of the whole table.
    #[test]
    fn a_refused_export_leaves_no_trace_and_a_good_one_then_works() {
        let f = Fixture::new();
        let occupied = f.pool.join("1").join("repo");
        let mut bad = f.request("1");
        bad.path = occupied.display().to_string();
        std::fs::write(occupied.join("keep"), b"x").unwrap();
        assert!(
            f.reject(&bad).message.contains("not empty"),
            "a real checkout is refused"
        );
        assert!(f.exports.live().is_empty());
        assert_eq!(
            std::fs::read_dir(&occupied).unwrap().count(),
            1,
            "the refused path is exactly as it was"
        );
        assert!(mounts::available(), "this platform can mount");
        std::fs::remove_file(occupied.join("keep")).unwrap();
        let good = f.request("1");
        f.exports
            .mount_snapshot(&good)
            .expect("the good path exports");
        assert_eq!(
            f.exports.live(),
            HashMap::from([(PathBuf::from(&good.path), "base".to_owned())])
        );
        // `unmount_snapshot` mirrors it and only touches a path the daemon exported.
        let e = f
            .exports
            .unmount_snapshot(&UnmountSnapshot { path: f.slot("9") })
            .expect_err("never exported");
        assert_eq!(e.code, ErrorCode::NotFound, "{e}");
        f.exports
            .unmount_snapshot(&UnmountSnapshot { path: good.path })
            .expect("unmounting our own export");
        assert!(f.exports.live().is_empty());
    }

    /// Rule: `unmount_snapshot` is `busy`, with nothing changed, while a holder is inside it.
    #[test]
    fn unmount_is_busy_while_a_holder_is_inside_the_export() {
        let f = Fixture::new();
        let req = f.request("1");
        f.exports.mount_snapshot(&req).unwrap();
        let held = f.exports.live().keys().next().unwrap().clone();
        assert!(mounts::available());
        std::fs::write(held.join("marker"), b"x").unwrap();
        let keep = crate::holders::Holder::holding(&held.join("marker"));
        let e = f
            .exports
            .unmount_snapshot(&UnmountSnapshot {
                path: held.display().to_string(),
            })
            .expect_err("a holder must refuse the unmount");
        assert_eq!(e.code, ErrorCode::Busy, "{e}");
        assert_eq!(f.exports.live().len(), 1, "nothing changed");
        drop(keep);
        f.exports
            .unmount_snapshot(&UnmountSnapshot {
                path: held.display().to_string(),
            })
            .expect("unmounting once free");
        assert!(f.exports.live().is_empty());
    }

    /// Rule: the daemon owns the lifetime. An export is unmounted by `unmount_snapshot` or at
    /// shutdown, never by the client, so nothing a client does leaves a live mountpoint.
    #[test]
    fn unmount_all_removes_everything_the_daemon_exported() {
        let f = Fixture::new();
        assert!(mounts::available());
        for (slot, name) in [("1", "base"), ("2", "base2")] {
            let mut req = f.request(slot);
            req.name = name.to_owned();
            f.exports
                .mount_snapshot(&req)
                .unwrap_or_else(|e| panic!("slot {slot}: {e}"));
        }
        assert_eq!(f.exports.live().len(), 2);
        for path in f.exports.live().keys() {
            assert!(
                mounts::is_mounted(path),
                "{} is not mounted",
                path.display()
            );
        }
        assert!(f.exports.unmount_all().is_empty());
        assert!(f.exports.live().is_empty());
        for path in [f.slot("1"), f.slot("2")] {
            assert!(
                !mounts::is_mounted(Path::new(&path)),
                "{path} still mounted"
            );
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        backend: Arc<dyn Backend>,
        exports: Exports,
        root: PathBuf,
        pool: PathBuf,
        mount: PathBuf,
    }

    fn private() -> std::fs::Permissions {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(0o700)
    }

    impl Fixture {
        fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            // The temp dir is a symlink on macOS (`/var` -> `/private/var`), and the export
            // rules compare against resolved roots, so the fixture works in real paths.
            let real = std::fs::canonicalize(dir.path()).unwrap();
            let store = real.join("store");
            let backend: Arc<dyn Backend> = Arc::new(PathBackend::open(&store).unwrap());
            backend.snapshots().create("base", None).unwrap();
            backend.snapshots().create("base2", None).unwrap();
            let root = real.join("th").join(".treehouse");
            let pool = root.join("repo-abc123");
            std::fs::create_dir_all(pool.join("1").join("repo")).unwrap();
            // The root rule is the socket directory's rule, so the fixture obeys it.
            std::fs::set_permissions(&root, private()).unwrap();
            let mount = real.join("mnt");
            // The default mount shows each snapshot as a directory under it, and the holder
            // check looks there: a holder is a process inside what a client sees. A real mount
            // creates these; without an adapter the fixture does it by hand.
            for name in ["base", "base2"] {
                std::fs::create_dir_all(mount.join(name)).unwrap();
            }
            Fixture {
                exports: Exports::new(
                    Arc::clone(&backend),
                    vec![root.clone()],
                    vec![store, mount.clone()],
                    mount.clone(),
                ),
                backend,
                root,
                pool,
                mount,
                _dir: dir,
            }
        }

        /// A valid slot path: `{root}/{pool}/{slot}/{repo}`.
        fn slot(&self, slot: &str) -> String {
            self.pool.join(slot).join("repo").display().to_string()
        }

        fn request(&self, slot: &str) -> MountSnapshot {
            MountSnapshot {
                name: "base".into(),
                path: self.slot(slot),
                expect_no_holders: true,
            }
        }

        fn reject(&self, req: &MountSnapshot) -> CtlError {
            self.exports
                .mount_snapshot(req)
                .expect_err("this request must be refused")
        }
    }

    impl Exports {
        /// Fills the registry without a real mount, so the rules that are about the registry
        /// can be tested on a machine whose adapter is unusable.
        #[cfg(test)]
        fn record_for_test(&self, path: PathBuf, name: &str) {
            self.lock().insert(
                path,
                Export {
                    name: name.to_owned(),
                    mount: None,
                },
            );
        }
    }
}
