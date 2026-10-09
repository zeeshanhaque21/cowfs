//! `PathVfs::mknod` sets the requested mode on the node it made, never on one renamed over it.
//!
//! `cargo test -p cowfs-vfs-path --test mknod_race`

#![cfg(target_os = "linux")]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
