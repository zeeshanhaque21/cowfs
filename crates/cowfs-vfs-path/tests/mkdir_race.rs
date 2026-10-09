//! `PathVfs::mkdir` sets the requested mode on the directory it made, never on one renamed over it.
//!
//! `cargo test -p cowfs-vfs-path --test mkdir_race`

#![cfg(target_os = "linux")]

use std::collections::VecDeque;
use std::fs::File;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cowfs_vfs::{Vfs, ROOT_INO};
use cowfs_vfs_path::PathVfs;

/// A helper thread keeps renaming decoy directories (mode 0o755, each held open so its mode can
/// be read after it is gone) over the name `mkdir` creates. Before the fix, the fchmod that
/// followed `mkdirat` went through a descriptor opened by name and could land on a decoy.
#[test]
fn mkdir_mode_never_lands_on_a_directory_swapped_in() {
    if !cowfs_vfs_path::mknod_mode_is_atomic() {
        eprintln!("SKIP: unshare(CLONE_FS) is refused here, so mkdir falls back to a chmod");
        return;
    }
    let root = std::env::temp_dir().join(format!("cowfs-mkdir-race-{}", std::process::id()));
    let dir = root.join("fs");
    std::fs::create_dir_all(&dir).unwrap();
    let fs = PathVfs::new(&dir).expect("open the directory");
    let stop = Arc::new(AtomicBool::new(false));
    let helper = {
        let (dir, stop) = (dir.clone(), stop.clone());
        std::thread::spawn(move || {
            let (mut n, mut hit, mut held) = (0u32, 0u32, VecDeque::new());
            let mut check = |f: File| {
                if f.metadata().unwrap().mode() & 0o7777 != 0o755 {
                    hit += 1;
                }
            };
            while !stop.load(Ordering::Relaxed) && n < 200_000 {
                let tmp = dir.join(".decoy");
                std::fs::create_dir(&tmp).unwrap();
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
                held.push_back(File::open(&tmp).unwrap());
                let _ = std::fs::rename(&tmp, dir.join("node"));
                let _ = std::fs::remove_dir(&tmp);
                if held.len() > 128 {
                    check(held.pop_front().unwrap());
                }
                n += 1;
            }
            held.into_iter().for_each(check);
            (n, hit)
        })
    };
    for _ in 0..20_000 {
        let _ = fs.rmdir(ROOT_INO, b"node");
        if let Ok(a) = fs.mkdir(ROOT_INO, b"node", 0o700) {
            fs.forget(a.ino, 1);
        }
    }
    stop.store(true, Ordering::Relaxed);
    let (decoys, hit) = helper.join().expect("helper");
    drop(fs);
    cowfs_vfs_path::force_remove_dir_all(&root);
    assert_eq!(
        hit, 0,
        "{hit} of {decoys} decoys had their mode changed by mkdir"
    );
}
