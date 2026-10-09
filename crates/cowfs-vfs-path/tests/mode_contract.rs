//! One mode contract: `create`, `mkdir` and `mknod` give back exactly the mode asked for, with or
//! without a default ACL (or a setgid bit) on the parent, like the rest of the conformance suite.
//!
//! `cargo test -p cowfs-vfs-path --test mode_contract`

#![cfg(target_os = "linux")]

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::Command;

use cowfs_vfs::{FileKind, Vfs, ROOT_INO};
use cowfs_vfs_path::PathVfs;

fn on_disk(dir: &std::path::Path, name: &str) -> u32 {
    std::fs::symlink_metadata(dir.join(name)).unwrap().mode() & 0o7777
}

/// Every kind, at modes a default ACL or the umask would change; both the returned `Attr` and the
/// backing filesystem must show the requested mode.
fn check(dir: &std::path::Path, label: &str) {
    let fs = PathVfs::new(dir).expect("open the directory");
    for (i, mode) in [0o666, 0o777, 0o600, 0o4755, 0o1777, 0o2755, 0o070]
        .into_iter()
        .enumerate()
    {
        let f = format!("f{i}");
        let d = format!("d{i}");
        let p = format!("p{i}");
        assert_eq!(
            fs.create(ROOT_INO, f.as_bytes(), mode).unwrap().mode,
            mode,
            "{label}: create"
        );
        assert_eq!(
            fs.mkdir(ROOT_INO, d.as_bytes(), mode).unwrap().mode,
            mode,
            "{label}: mkdir"
        );
        let n = fs
            .mknod(ROOT_INO, p.as_bytes(), FileKind::Fifo, mode, 0)
            .unwrap();
        assert_eq!(n.mode, mode, "{label}: mknod");
        for name in [&f, &d, &p] {
            assert_eq!(on_disk(dir, name), mode, "{label}: {name} on disk");
        }
    }
}

fn fresh(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("cowfs-mode-{tag}-{}", std::process::id()));
    cowfs_vfs_path::force_remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn the_mode_is_exact_in_a_plain_directory() {
    let d = fresh("plain");
    check(&d, "plain");
    cowfs_vfs_path::force_remove_dir_all(&d);
}

#[test]
fn the_mode_is_exact_under_a_default_acl() {
    let d = fresh("acl");
    let ok = Command::new("setfacl")
        .args(["-d", "-m", "u::rwx,g::r,o::r"])
        .arg(&d)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("SKIP: no setfacl, or this filesystem takes no default ACL");
        cowfs_vfs_path::force_remove_dir_all(&d);
        return;
    }
    // The ACL really is in force: a native mknod comes out filtered.
    drop(std::fs::File::create(d.join("native")).unwrap());
    assert_ne!(
        on_disk(&d, "native"),
        0o666,
        "the default ACL filters a native create"
    );
    check(&d, "default ACL");
    cowfs_vfs_path::force_remove_dir_all(&d);
}

#[test]
fn the_mode_is_exact_in_a_setgid_directory() {
    let d = fresh("setgid");
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o2755)).unwrap();
    check(&d, "setgid parent");
    cowfs_vfs_path::force_remove_dir_all(&d);
}
