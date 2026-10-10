//! `PathVfs::mknod` sets the requested mode on the node it made, never on one renamed over it.
//!
//! `cargo test -p cowfs-vfs-path --test mknod_race`

#![cfg(target_os = "linux")]

use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cowfs_vfs::{FileKind, Vfs, ROOT_INO};
use cowfs_vfs_path::PathVfs;

/// A helper thread keeps renaming decoy sockets (mode 0o644, each also linked from a side directory)
/// over the name `mknod` creates. Before the fix, the chmod that followed `mknodat` went by name
/// and could land on a decoy; afterwards every decoy must still have its own mode.
#[test]
fn mknod_mode_never_lands_on_a_node_swapped_in() {
    if !cowfs_vfs_path::mknod_mode_is_atomic() {
        eprintln!(
            "SKIP: unshare(CLONE_FS) is refused here, so mknod falls back to a chmod by name"
        );
        return;
    }
    let root = std::env::temp_dir().join(format!("cowfs-mknod-race-{}", std::process::id()));
    let (dir, side) = (root.join("fs"), root.join("side"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&side).unwrap();
    let fs = PathVfs::new(&dir).expect("open the directory");
    let stop = Arc::new(AtomicBool::new(false));
    let helper = {
        let (dir, side, stop) = (dir.clone(), side.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 0u32;
            while !stop.load(Ordering::Relaxed) && n < 200_000 {
                let keep = side.join(n.to_string());
                // A socket node needs no unsafe code to make; its mode is set afterwards.
                drop(UnixListener::bind(&keep).expect("bind a decoy"));
                std::fs::set_permissions(&keep, std::fs::Permissions::from_mode(0o644)).unwrap();
                let tmp = dir.join(".decoy");
                std::fs::hard_link(&keep, &tmp).unwrap();
                std::fs::rename(&tmp, dir.join("node")).unwrap();
                n += 1;
            }
            n
        })
    };
    for _ in 0..20_000 {
        let _ = fs.unlink(ROOT_INO, b"node");
        if let Ok(a) = fs.mknod(ROOT_INO, b"node", FileKind::Fifo, 0o600, 0) {
            fs.forget(a.ino, 1);
        }
    }
    stop.store(true, Ordering::Relaxed);
    let decoys = helper.join().expect("helper");
    let hit: Vec<_> = std::fs::read_dir(&side)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| std::fs::metadata(p).unwrap().mode() & 0o7777 != 0o644)
        .collect();
    drop(fs);
    cowfs_vfs_path::force_remove_dir_all(&root);
    assert!(
        hit.is_empty(),
        "{} of {decoys} decoys had their mode changed by mknod: {:?}",
        hit.len(),
        &hit[..hit.len().min(5)]
    );
}

/// Gives `dir` a default POSIX ACL (`u::rwx,g::r-x,o::r-x`), so the kernel may change the mode of
/// anything created in it and `mknod` must chmod after `mknodat`. False when `setfacl` is missing
/// or the filesystem has no ACL support.
fn set_default_acl(dir: &std::path::Path) -> bool {
    std::process::Command::new("setfacl")
        .args(["-d", "-m", "u::rwx,g::r-x,o::r-x"])
        .arg(dir)
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs `mknod` in a loop while a helper swaps `node` for a symlink to `victim` (mode 0o644) and
/// for decoy sockets (mode 0o644). Returns the paths whose mode `mknod` changed.
fn race(acl: bool) -> Option<Vec<std::path::PathBuf>> {
    let root = std::env::temp_dir().join(format!(
        "cowfs-mknod-race-{}-{}",
        acl as u8,
        std::process::id()
    ));
    let (dir, side) = (root.join("fs"), root.join("side"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&side).unwrap();
    if acl && !set_default_acl(&dir) {
        eprintln!(
            "SKIP: no setfacl or no default ACL support under {}",
            root.display()
        );
        cowfs_vfs_path::force_remove_dir_all(&root);
        return None;
    }
    let victim = side.join("victim");
    std::fs::write(&victim, b"v").unwrap();
    std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).unwrap();
    let fs = PathVfs::new(&dir).expect("open the directory");
    let stop = Arc::new(AtomicBool::new(false));
    let helper = {
        let (dir, side, victim, stop) = (dir.clone(), side.clone(), victim.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 0u32;
            while !stop.load(Ordering::Relaxed) && n < 200_000 {
                let tmp = dir.join(".decoy");
                if n & 1 == 0 {
                    symlink(&victim, &tmp).unwrap();
                } else {
                    let keep = side.join(n.to_string());
                    drop(UnixListener::bind(&keep).expect("bind a decoy"));
                    std::fs::set_permissions(&keep, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                    std::fs::hard_link(&keep, &tmp).unwrap();
                }
                std::fs::rename(&tmp, dir.join("node")).unwrap();
                n += 1;
            }
        })
    };
    for _ in 0..20_000 {
        let _ = fs.unlink(ROOT_INO, b"node");
        if let Ok(a) = fs.mknod(ROOT_INO, b"node", FileKind::Fifo, 0o600, 0) {
            fs.forget(a.ino, 1);
        }
    }
    stop.store(true, Ordering::Relaxed);
    helper.join().expect("helper");
    let hit = std::fs::read_dir(&side)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| std::fs::metadata(p).unwrap().mode() & 0o7777 != 0o644)
        .collect();
    drop(fs);
    cowfs_vfs_path::force_remove_dir_all(&root);
    Some(hit)
}

/// The symlink swap of #211: a chmod by name after `mknodat` follows a link planted at the name.
#[test]
fn mknod_never_chmods_a_symlink_target() {
    let hit = race(false).unwrap();
    assert!(
        hit.is_empty(),
        "mknod changed the mode of {} nodes it did not make: {:?}",
        hit.len(),
        &hit[..hit.len().min(3)]
    );
}

/// The same race where the kernel may change the mode (default ACL), so `mknod` cannot rely on
/// the creation mode and has to chmod the node it made, not whatever sits at the name.
#[test]
fn mknod_with_default_acl_never_chmods_a_node_swapped_in() {
    if let Some(hit) = race(true) {
        assert!(
            hit.is_empty(),
            "mknod changed the mode of {} nodes it did not make: {:?}",
            hit.len(),
            &hit[..hit.len().min(3)]
        );
    }
}
