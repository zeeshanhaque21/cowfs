//! Every lookup reference the adapter takes must be given back, and every removed inode must be
//! reclaimed. Runs the adapter directly and through the protocol; `tests/mount.rs` repeats it on
//! a real mount.
mod common;

use std::sync::Arc;

use common::counting::CountingVfs;
use common::*;
use cowfs_nfs::{Adapter, AdapterOptions, MountOptions};
use cowfs_vfs::ROOT_INO;
use nfsserve::nfs::sattr3;

fn adapter(hide: bool) -> (Arc<CountingVfs>, Adapter) {
    let v = CountingVfs::new();
    let a = Adapter::new(
        v.clone(),
        AdapterOptions {
            hide_appledouble: hide,
            ..AdapterOptions::default()
        },
    );
    (v, a)
}

fn s() -> sattr3 {
    sattr3::default()
}

/// Root plus what is still linked.
fn assert_drained(v: &CountingVfs, live: usize, what: &str) {
    assert_eq!(
        v.outstanding(),
        0,
        "{what}: lookup references not given back"
    );
    assert_eq!(v.live(), live, "{what}: inodes still held");
}

#[test]
fn rename_over_a_file_with_sidecars_reclaims_both_replaced_inodes() {
    let (v, a) = adapter(true);
    a.create(ROOT_INO, b"a", &s(), true).unwrap();
    a.create(ROOT_INO, b"._a", &s(), true).unwrap();
    a.create(ROOT_INO, b"b", &s(), true).unwrap();
    a.create(ROOT_INO, b"._b", &s(), true).unwrap();
    a.rename(ROOT_INO, b"a", ROOT_INO, b"b").unwrap();
    assert_drained(&v, 2, "after rename");
    a.remove(ROOT_INO, b"b").unwrap();
    assert_drained(&v, 0, "after remove");
}

#[test]
fn the_ten_reference_scenarios_balance() {
    for hide in [true, false] {
        let (v, a) = adapter(hide);
        let (f, _) = a.create(ROOT_INO, b"f", &s(), true).unwrap();
        for _ in 0..3 {
            a.lookup(ROOT_INO, b"f").unwrap();
        }
        a.link(f, ROOT_INO, b"g").unwrap();
        a.lookup(ROOT_INO, b"g").unwrap();
        a.remove(ROOT_INO, b"f").unwrap();
        assert_drained(&v, 1, "one name left");
        a.remove(ROOT_INO, b"g").unwrap();
        assert_drained(&v, 0, "link, remove both");

        a.create(ROOT_INO, b"x", &s(), true).unwrap();
        a.create(ROOT_INO, b"y", &s(), true).unwrap();
        a.rename(ROOT_INO, b"x", ROOT_INO, b"y").unwrap();
        a.remove(ROOT_INO, b"y").unwrap();
        assert_drained(&v, 0, "rename over existing");

        a.mkdir(ROOT_INO, b"d1", &s()).unwrap();
        a.mkdir(ROOT_INO, b"d2", &s()).unwrap();
        a.rename(ROOT_INO, b"d1", ROOT_INO, b"d2").unwrap();
        a.rmdir(ROOT_INO, b"d2").unwrap();
        assert_drained(&v, 0, "rename dir over empty dir");

        a.symlink(ROOT_INO, b"l", b"t").unwrap();
        a.remove(ROOT_INO, b"l").unwrap();
        assert_drained(&v, 0, "symlink");

        a.create(ROOT_INO, b"u", &s(), true).unwrap();
        a.create(ROOT_INO, b"u", &s(), false).unwrap();
        a.create_exclusive(ROOT_INO, b"e", [1; 8]).unwrap();
        a.create_exclusive(ROOT_INO, b"e", [1; 8]).unwrap();
        assert!(a.create_exclusive(ROOT_INO, b"e", [2; 8]).is_err());
        assert!(a.create(ROOT_INO, b"e", &s(), true).is_err());
        a.mkdir(ROOT_INO, b"dd", &s()).unwrap();
        assert!(a.create(ROOT_INO, b"dd", &s(), false).is_err());
        a.remove(ROOT_INO, b"u").unwrap();
        a.remove(ROOT_INO, b"e").unwrap();
        a.rmdir(ROOT_INO, b"dd").unwrap();
        assert_drained(&v, 0, "unchecked recreate, exclusive retry, failed creates");

        a.create(ROOT_INO, b"p", &s(), true).unwrap();
        a.link(a.lookup(ROOT_INO, b"p").unwrap().0, ROOT_INO, b"p2")
            .unwrap();
        a.create(ROOT_INO, b"q", &s(), true).unwrap();
        a.rename(ROOT_INO, b"q", ROOT_INO, b"p").unwrap();
        a.remove(ROOT_INO, b"p2").unwrap();
        a.remove(ROOT_INO, b"p").unwrap();
        assert_drained(&v, 0, "rename over one name of a two-link file");

        let (d, _) = a.mkdir(ROOT_INO, b"d", &s()).unwrap();
        a.create(d, b"._x", &s(), true).unwrap();
        a.create(d, b"m", &s(), true).unwrap();
        a.rename(d, b"m", ROOT_INO, b"moved").unwrap();
        a.remove(ROOT_INO, b"moved").unwrap();
        if !hide {
            assert!(a.rmdir(ROOT_INO, b"d").is_err());
            a.remove(d, b"._x").unwrap();
        }
        a.rmdir(ROOT_INO, b"d").unwrap();
        assert_drained(&v, 0, "sidecar-only dir");
    }
}

#[test]
fn atomic_save_cycles_do_not_accumulate_inodes() {
    let (v, a) = adapter(true);
    a.create(ROOT_INO, b"target", &s(), true).unwrap();
    for i in 0..200 {
        let tmp = format!("tmp{i}");
        let (t, _) = a.create(ROOT_INO, tmp.as_bytes(), &s(), true).unwrap();
        a.write(t, 0, b"data").unwrap();
        a.create(ROOT_INO, format!("._{tmp}").as_bytes(), &s(), true)
            .unwrap();
        a.rename(ROOT_INO, tmp.as_bytes(), ROOT_INO, b"target")
            .unwrap();
    }
    assert_drained(&v, 2, "target and its sidecar");
    a.remove(ROOT_INO, b"target").unwrap();
    assert_drained(&v, 0, "after rm target");
}

#[test]
fn heavy_workload_through_the_protocol_drains() {
    let v = CountingVfs::new();
    let (_s, mut c) = serve(v.clone(), MountOptions::default());
    let root = c.root.clone();
    for i in 0..1000 {
        let n = format!("f{i}");
        let f = c.create_file(&root, &n);
        c.write(&f, 0, b"x", 2);
        assert_eq!(c.remove(&root, &n), OK);
    }
    let (_, d) = c.mkdir(&root, "dir");
    let d = d.unwrap();
    for i in 0..100 {
        let a = format!("a{i}");
        let f = c.create_file(&d, &a);
        c.link(&f, &d, &format!("l{i}"));
        c.rename(&d, &a, &root, &format!("r{i}"));
        c.lookup(&d, &format!("l{i}"));
        c.remove(&d, &format!("l{i}"));
        c.remove(&root, &format!("r{i}"));
    }
    let (_, sub) = c.mkdir(&d, "sub");
    let _ = sub;
    c.rename(&d, "sub", &root, "sub2");
    assert_eq!(c.rmdir(&root, "sub2"), OK);
    assert_eq!(c.rmdir(&root, "dir"), OK);
    assert_drained(&v, 0, "protocol workload");
    assert!(v.lookups.load(std::sync::atomic::Ordering::Relaxed) > 0);
}
