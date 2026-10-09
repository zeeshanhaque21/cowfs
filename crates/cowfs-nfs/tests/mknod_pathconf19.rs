//! MKNOD refusals and PATHCONF values over raw NFSv3, against the in-process server over `MemVfs`.
mod common;

use common::*;
use cowfs_nfs::MountOptions;
use nfsserve::nfs::{ftype3, nfs_fh3, post_op_attr, specdata3};

fn mknod(c: &mut Nfs, dir: &nfs_fh3, n: &str, kind: ftype3) -> u32 {
    let mut a = Args::new()
        .put(&dirop(dir, n))
        .put(&(kind as u32))
        .put(&sattr_mode(0o600));
    if matches!(kind, ftype3::NF3CHR | ftype3::NF3BLK) {
        a = a.put(&specdata3 {
            specdata1: 1,
            specdata2: 3,
        });
    }
    c.call(11, a).0
}

#[test]
fn mknod_refuses_fifo_socket_and_devices_without_creating_anything() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let before = c.names(&root);
    for (kind, n) in [
        (ftype3::NF3FIFO, "fifo"),
        (ftype3::NF3SOCK, "sock"),
        (ftype3::NF3CHR, "chr"),
        (ftype3::NF3BLK, "blk"),
    ] {
        assert_eq!(mknod(&mut c, &root, n, kind), NOTSUPP, "MKNOD {n}");
        assert_eq!(c.lookup(&root, n).0, NOENT, "{n} must not exist");
    }
    assert_eq!(c.names(&root), before, "readdir shows no new entry");
}

#[test]
fn pathconf_reports_the_advertised_values() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let (st, mut r) = c.call(20, Args::new().put(&root));
    assert_eq!(st, OK);
    let _a: post_op_attr = dec(&mut r);
    let linkmax: u32 = dec(&mut r);
    let name_max: u32 = dec(&mut r);
    let no_trunc: bool = dec(&mut r);
    let chown_restricted: bool = dec(&mut r);
    let case_insensitive: bool = dec(&mut r);
    let case_preserving: bool = dec(&mut r);
    assert_eq!(name_max, 255);
    assert!(no_trunc);
    assert!(chown_restricted);
    assert!(!case_insensitive);
    assert!(case_preserving);
    // Advertised nfsserve default, not enforced by cowfs; known limitation tracked in #19.
    assert_eq!(linkmax, 65000);
}
