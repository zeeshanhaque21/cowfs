//! Core behaviour beyond the conformance suite: the mount root, snapshots, persistence,
//! dedup, sparse files, forget accounting, corruption and the Merkle root.

mod common;

use std::collections::HashSet;
use std::process::Command;

use common::*;
use cowfs_core::{validate_snapshot_name, ControlError, Core, Options};
use cowfs_vfs::{Error, FileKind, RenameFlags, SetAttr, Vfs, XattrFlags, ROOT_INO};

#[test]
fn root_lists_snapshots_with_stable_cookies() {
    let f = fixture();
    let c = &f.core;
    assert!(c.readdir(ROOT_INO, 0, 10).unwrap().entries.is_empty());
    let a = c.create_snapshot("a").unwrap();
    let b = c.create_snapshot("b").unwrap();
    let r = c.readdir(ROOT_INO, 0, 10).unwrap();
    assert!(r.eof);
    let names: Vec<_> = r.entries.iter().map(|e| e.name.clone()).collect();
    assert_eq!(names, vec![b"a".to_vec(), b"b".to_vec()]);
    assert_eq!(r.entries[0].ino, a.ino);
    assert_eq!(r.entries[1].ino, b.ino);
    assert!(r.entries.iter().all(|e| e.kind == FileKind::Directory));
    let first = c.readdir(ROOT_INO, 0, 1).unwrap();
    assert!(!first.eof);
    c.remove_snapshot("a").unwrap();
    c.create_snapshot("c").unwrap();
    let rest = c.readdir(ROOT_INO, first.entries[0].cookie, 10).unwrap();
    let names: Vec<_> = rest.entries.iter().map(|e| e.name.clone()).collect();
    assert_eq!(names, vec![b"b".to_vec(), b"c".to_vec()]);
    assert_eq!(c.getattr(ROOT_INO).unwrap().nlink, 4);
    assert_eq!(c.lookup(ROOT_INO, b"a"), Err(Error::NotFound));
    assert_eq!(c.getattr(a.ino), Err(Error::Stale));
    assert_ne!(root_entry(c, "b").ino, root_entry(c, "c").ino);
}

#[test]
fn root_is_read_only_and_snapshots_are_separate_devices() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("a").unwrap();
    c.create_snapshot("b").unwrap();
    let ra = root_entry(c, "a").ino;
    let rb = root_entry(c, "b").ino;
    assert_eq!(c.create(ROOT_INO, b"x", 0o644), Err(Error::ReadOnly));
    assert_eq!(c.mkdir(ROOT_INO, b"x", 0o755), Err(Error::ReadOnly));
    assert_eq!(c.symlink(ROOT_INO, b"x", b"t"), Err(Error::ReadOnly));
    assert_eq!(c.unlink(ROOT_INO, b"a"), Err(Error::ReadOnly));
    assert_eq!(c.rmdir(ROOT_INO, b"a"), Err(Error::ReadOnly));
    assert_eq!(
        c.rename(ROOT_INO, b"a", ROOT_INO, b"z", RenameFlags::default()),
        Err(Error::ReadOnly)
    );
    assert_eq!(truncate(c, ROOT_INO, 0), Err(Error::IsDir));
    assert_eq!(c.write(ROOT_INO, 0, b"x"), Err(Error::IsDir));
    assert_eq!(c.read(ROOT_INO, 0, 1), Err(Error::IsDir));
    let fa = mkfile(c, ra, "f", b"data").ino;
    assert_eq!(c.link(fa, rb, b"l").unwrap_err(), Error::CrossDevice);
    assert_eq!(
        c.rename(ra, b"f", rb, b"f", RenameFlags::default()),
        Err(Error::CrossDevice)
    );
    assert_eq!(
        c.setxattr(ROOT_INO, b"user.a", b"v", XattrFlags::default()),
        Err(Error::ReadOnly)
    );
    let h = c.open(ROOT_INO).unwrap();
    c.release(h).unwrap();
}

#[test]
fn snapshot_name_rules() {
    for ok in [
        "a",
        "main",
        "with space",
        "日本",
        ".hidden",
        "a.b",
        "x".repeat(255).as_str(),
    ] {
        validate_snapshot_name(ok).unwrap();
    }
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "a\0b",
        "._x",
        ".nfs123",
        "x".repeat(256).as_str(),
    ] {
        assert!(
            matches!(
                validate_snapshot_name(bad),
                Err(ControlError::InvalidName(_))
            ),
            "{bad:?}"
        );
    }
    let f = fixture();
    f.core.create_snapshot("s").unwrap();
    assert_eq!(f.core.create_snapshot("s"), Err(ControlError::Exists));
    assert!(matches!(
        f.core.create_snapshot("a/b"),
        Err(ControlError::InvalidName(_))
    ));
    assert_eq!(f.core.remove_snapshot("nope"), Err(ControlError::NotFound));
    assert_eq!(
        f.core.fork_snapshot("nope", "x"),
        Err(ControlError::NotFound)
    );
}

#[test]
fn fork_is_isolated_both_ways_and_inodes_differ() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("a").unwrap();
    let ra = root_entry(c, "a").ino;
    let big = pattern(300_000, 1);
    let x = mkfile(c, ra, "x", &big).ino;
    let d = c.mkdir(ra, b"d", 0o755).unwrap().ino;
    mkfile(c, d, "inner", b"inner");
    c.link(x, d, b"x2").unwrap();
    c.fork_snapshot("a", "b").unwrap();
    let rb = root_entry(c, "b").ino;
    let xb = c.lookup(rb, b"x").unwrap();
    assert_ne!(
        xb.ino, x,
        "the same file in two snapshots must have two numbers"
    );
    assert_eq!(read_all(c, xb.ino), big);
    write_all(c, xb.ino, 10, b"CHANGED-IN-B");
    assert_eq!(
        read_all(c, x),
        big,
        "write to the fork leaked into the source"
    );
    write_all(c, x, 100_000, b"CHANGED-IN-A");
    let mut want_b = big.clone();
    want_b[10..22].copy_from_slice(b"CHANGED-IN-B");
    assert_eq!(
        read_all(c, xb.ino),
        want_b,
        "write to the source leaked into the fork"
    );
    let db = c.lookup(rb, b"d").unwrap().ino;
    let x2b = c.lookup(db, b"x2").unwrap();
    assert_eq!(x2b.ino, xb.ino, "hardlink inside the fork");
    assert_eq!(x2b.nlink, 2);
    c.unlink(db, b"x2").unwrap();
    assert_eq!(
        c.getattr(x).unwrap().nlink,
        2,
        "unlink in the fork changed the source"
    );
    assert_eq!(c.getattr(xb.ino).unwrap().nlink, 1);
    c.unlink(rb, b"x").unwrap();
    assert_eq!(c.lookup(ra, b"x").unwrap().ino, x);
    c.rmdir(rb, b"d").unwrap_err();
    c.check().unwrap();
}

#[test]
fn persists_across_reopen_and_checks_clean() {
    let dir = tempfile::tempdir().unwrap();
    let big = pattern(1_000_000, 2);
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        let r = root_entry(&c, "s").ino;
        let d = c.mkdir(r, b"dir", 0o700).unwrap().ino;
        let f = mkfile(&c, d, "file", &big).ino;
        c.symlink(d, b"link", b"file").unwrap();
        c.link(f, r, b"hard").unwrap();
        c.setxattr(f, b"user.k", b"v", XattrFlags::default())
            .unwrap();
        truncate(&c, f, 900_000).unwrap();
        c.fsync(f, false).unwrap();
    }
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r = root_entry(&c, "s").ino;
    let d = c.lookup(r, b"dir").unwrap();
    assert_eq!(d.mode, 0o700);
    let f = c.lookup(d.ino, b"file").unwrap();
    assert_eq!(f.size, 900_000);
    assert_eq!(f.nlink, 2);
    assert_eq!(read_all(&c, f.ino), &big[..900_000]);
    assert_eq!(c.lookup(r, b"hard").unwrap().ino, f.ino);
    let l = c.lookup(d.ino, b"link").unwrap();
    assert_eq!(c.readlink(l.ino).unwrap(), b"file");
    assert_eq!(c.getxattr(f.ino, b"user.k").unwrap(), b"v");
    c.check().unwrap();
    assert!(c.fsck().unwrap().is_clean());
}

#[test]
fn drop_makes_everything_durable() {
    let dir = tempfile::tempdir().unwrap();
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        let r = root_entry(&c, "s").ino;
        for i in 0..200 {
            mkfile(&c, r, &format!("f{i}"), &pattern(5000 + i, i as u64));
        }
    }
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r = root_entry(&c, "s").ino;
    for i in 0..200 {
        let a = c.lookup(r, format!("f{i}").as_bytes()).unwrap();
        assert_eq!(read_all(&c, a.ino), pattern(5000 + i, i as u64));
    }
    c.check().unwrap();
}

#[test]
fn identical_files_store_once_and_forks_store_only_the_delta() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("a").unwrap();
    let r = root_entry(c, "a").ino;
    let data = pattern(10 << 20, 3);
    mkfile(c, r, "one", &data);
    c.flush().unwrap();
    let s1 = c.store().stats();
    mkfile(c, r, "two", &data);
    c.flush().unwrap();
    let s2 = c.store().stats();
    assert_eq!(
        s2.blocks, s1.blocks,
        "a second identical 10 MiB file stored new blocks"
    );
    assert_eq!(s2.stored_bytes, s1.stored_bytes);
    assert!(
        s2.dedup_bytes >= s1.dedup_bytes + (10 << 20) - (1 << 20),
        "{s2:?} vs {s1:?}"
    );
    c.fork_snapshot("a", "b").unwrap();
    let rb = root_entry(c, "b").ino;
    let fb = c.lookup(rb, b"one").unwrap().ino;
    write_all(c, fb, 5 << 20, b"one small edit in the middle");
    c.flush().unwrap();
    let s3 = c.store().stats();
    let new_blocks = s3.blocks - s2.blocks;
    assert!(
        (1..=3).contains(&new_blocks),
        "an edit stored {new_blocks} new blocks"
    );
    assert!(s3.stored_bytes - s2.stored_bytes <= 3 * (256 << 10));
    let mut want = data.clone();
    want[5 << 20..(5 << 20) + 28].copy_from_slice(b"one small edit in the middle");
    assert_eq!(read_all(c, fb), want);
    assert_eq!(read_all(c, c.lookup(r, b"one").unwrap().ino), data);
}

fn rss_kib() -> u64 {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

#[test]
fn a_terabyte_sparse_file_costs_no_memory() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    let ino = c.create(r, b"sparse", 0o644).unwrap().ino;
    let before = rss_kib();
    let tb = 1u64 << 40;
    truncate(c, ino, tb).unwrap();
    assert_eq!(c.getattr(ino).unwrap().size, tb);
    assert_eq!(c.getattr(ino).unwrap().blocks, 0);
    write_all(c, ino, tb - 4096, &pattern(4096, 9));
    assert_eq!(c.getattr(ino).unwrap().size, tb);
    c.fsync(ino, false).unwrap();
    assert_eq!(c.read(ino, tb - 4096, 4096).unwrap(), pattern(4096, 9));
    assert_eq!(c.read(ino, tb / 2, 100_000).unwrap(), vec![0u8; 100_000]);
    assert_eq!(c.read(ino, 0, 10).unwrap(), vec![0u8; 10]);
    assert_eq!(c.read(ino, tb - 4100, 10).unwrap(), {
        let mut v = vec![0u8; 4];
        v.extend_from_slice(&pattern(4096, 9)[..6]);
        v
    });
    let after = rss_kib();
    assert!(
        after < before + 256 * 1024,
        "rss grew {} KiB",
        after - before
    );
    assert!(c.getattr(ino).unwrap().blocks * 512 <= 1 << 20);
    truncate(c, ino, 0).unwrap();
    assert_eq!(c.getattr(ino).unwrap().blocks, 0);
    c.check().unwrap();
}

#[test]
fn forget_accounting_drains_every_map() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    let d = c.mkdir(r, b"d", 0o755).unwrap().ino;
    for i in 0..100_000u32 {
        let name = format!("f{}", i % 50);
        let a = c.create(d, name.as_bytes(), 0o644).unwrap();
        c.write(a.ino, 0, b"x").unwrap();
        if i % 3 == 0 {
            let l = c.lookup(d, name.as_bytes()).unwrap();
            assert_eq!(l.ino, a.ino);
            c.forget(a.ino, 1);
        }
        c.unlink(d, name.as_bytes()).unwrap();
        c.forget(a.ino, 1);
        if i % 10_000 == 9_999 {
            c.flush().unwrap();
        }
    }
    c.flush().unwrap();
    let s = c.stats();
    assert_eq!(s.forget_underflows, 0);
    assert_eq!(s.aliases, 1, "{s:?}");
    assert_eq!(s.dirty_bytes, 0);
    assert_eq!(s.pending_ops, 0);
    assert!(s.nodes <= 3, "{s:?}");
    assert!(c.readdir(d, 0, 10).unwrap().entries.is_empty());
    c.forget(d, 1);
    c.check().unwrap();
}

#[test]
fn creates_are_batched() {
    let f = fixture_with(Options {
        max_pending_ops: 1000,
        ..test_opts()
    });
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    let before = c.stats().batches;
    let t = std::time::Instant::now();
    for i in 0..5000 {
        let a = c.create(r, format!("f{i}").as_bytes(), 0o644).unwrap();
        c.write(a.ino, 0, &pattern(2000, i)).unwrap();
        c.forget(a.ino, 1);
    }
    c.flush().unwrap();
    let s = c.stats();
    println!(
        "5000 creates in {:?}, {} batches",
        t.elapsed(),
        s.batches - before
    );
    assert_eq!(s.ops_committed, 10_000, "{s:?}");
    assert!(s.batches - before <= 12, "{} batches", s.batches - before);
    assert_eq!(c.readdir(r, 0, 10_000).unwrap().entries.len(), 5000);
}

fn one_pack(dir: &std::path::Path) -> std::path::PathBuf {
    std::fs::read_dir(dir.join("store/packs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
}

#[test]
fn corrupt_block_under_an_open_core_is_eio_never_wrong_data() {
    let dir = tempfile::tempdir().unwrap();
    let data = pattern(600_000, 7);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let a = mkfile(&c, r, "f", &data);
    c.sync().unwrap();
    c.drop_caches();
    let pack = one_pack(dir.path());
    let mut bytes = std::fs::read(&pack).unwrap();
    bytes[16 + 52 + 70_000] ^= 0x55;
    std::fs::write(&pack, &bytes).unwrap();
    let mut saw_error = false;
    let mut off = 0u64;
    while off < 600_000 {
        match c.read(a.ino, off, 65536) {
            Ok(got) => assert_eq!(
                got,
                data[off as usize..off as usize + got.len()],
                "wrong data at {off}"
            ),
            Err(e) => {
                assert_eq!(e.errno(), libc_eio());
                saw_error = true;
            }
        }
        off += 65536;
    }
    assert!(saw_error, "the damaged chunk was read without an error");
    assert!(!c.fsck().unwrap().is_clean());
}

#[test]
fn a_store_that_lost_durable_data_is_not_opened() {
    let dir = tempfile::tempdir().unwrap();
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        let r = root_entry(&c, "s").ino;
        mkfile(&c, r, "f", &pattern(600_000, 7));
        c.sync().unwrap();
    }
    let pack = one_pack(dir.path());
    let mut bytes = std::fs::read(&pack).unwrap();
    bytes[16 + 52 + 70_000] ^= 0x55;
    std::fs::write(&pack, &bytes).unwrap();
    let _ = std::fs::remove_file(dir.path().join("store/index.cix"));
    match Core::open(dir.path(), test_opts()) {
        Err(Error::Corrupt(m)) => assert!(m.contains("corruption"), "{m}"),
        Err(e) => panic!("wrong error {e:?}"),
        Ok(_) => panic!("a store with damaged durable data was opened"),
    }
}

fn libc_eio() -> i32 {
    Error::Io(String::new()).errno()
}

#[test]
fn merkle_root_changes_iff_content_changes() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("a").unwrap();
    let r = root_entry(c, "a").ino;
    let x = mkfile(c, r, "x", b"hello").ino;
    let root0 = c.merkle_root("a").unwrap();
    assert_eq!(c.merkle_root("a").unwrap(), root0);
    c.lookup(r, b"x").unwrap();
    c.getattr(x).unwrap();
    c.read(x, 0, 10).unwrap();
    c.readdir(r, 0, 10).unwrap();
    c.fsync(x, false).unwrap();
    assert_eq!(c.merkle_root("a").unwrap(), root0, "reads changed the root");
    c.fork_snapshot("a", "b").unwrap();
    assert_eq!(
        c.merkle_root("b").unwrap(),
        root0,
        "a fork must have the source's root"
    );
    write_all(c, x, 0, b"HELLO");
    let root1 = c.merkle_root("a").unwrap();
    assert_ne!(root1, root0, "a write did not change the root");
    assert_eq!(c.merkle_root("b").unwrap(), root0, "the fork's root moved");
    c.mkdir(r, b"d", 0o755).unwrap();
    let root2 = c.merkle_root("a").unwrap();
    assert_ne!(root2, root1);
    c.rmdir(r, b"d").unwrap();
    c.setattr(
        x,
        SetAttr {
            mode: Some(0o600),
            ..SetAttr::default()
        },
    )
    .unwrap();
    assert_ne!(c.merkle_root("a").unwrap(), root2);
}

#[test]
fn control_plane_operations() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("base").unwrap();
    let rb = root_entry(c, "base").ino;
    mkfile(c, rb, "f", b"v1");
    c.fork_snapshot("base", "work").unwrap();
    let rw = root_entry(c, "work").ino;
    let w = c.lookup(rw, b"f").unwrap().ino;
    write_all(c, w, 0, b"v2");
    let h = c.open(w).unwrap();
    assert_eq!(c.remove_snapshot("work"), Err(ControlError::Busy));
    c.release(h).unwrap();
    let e = c.promote_base("work", "base").unwrap();
    assert_eq!(e.name, "base");
    let rb2 = root_entry(c, "base").ino;
    assert_ne!(rb2, rb);
    assert_eq!(read_all(c, c.lookup(rb2, b"f").unwrap().ino), b"v2");
    assert_eq!(
        c.getattr(rb),
        Err(Error::Stale),
        "the replaced base is gone"
    );
    let renamed = c.rename_snapshot("work", "renamed").unwrap();
    assert_eq!(renamed.name, "renamed");
    assert_eq!(c.lookup(ROOT_INO, b"work"), Err(Error::NotFound));
    let list = c.list_snapshots().unwrap();
    let names: HashSet<_> = list.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["base", "renamed"].into_iter().collect());
    assert!(list.iter().any(|e| e.parent.is_some()));
    assert_eq!(
        c.promote_base("base", "base"),
        Err(ControlError::InvalidName(
            "source and target are the same snapshot"
        ))
    );
    c.check().unwrap();
}

#[test]
fn open_unlinked_data_is_pinned_until_released() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    let a = c.create(r, b"tmp", 0o600).unwrap();
    let h = c.open(a.ino).unwrap();
    let data = pattern(3 << 20, 4);
    write_all(c, a.ino, 0, &data);
    c.flush().unwrap();
    c.unlink(r, b"tmp").unwrap();
    c.flush().unwrap();
    assert_eq!(c.getattr(a.ino).unwrap().nlink, 0);
    assert_eq!(read_all(c, a.ino), data);
    let pinned = c.pinned_blocks();
    assert!(!pinned.is_empty());
    assert!(pinned.iter().all(|b| c.store().contains(*b)));
    c.release(h).unwrap();
    c.forget(a.ino, 1);
    assert_eq!(c.getattr(a.ino), Err(Error::Stale));
    assert!(c.pinned_blocks().is_empty());
    c.check().unwrap();
}

#[test]
fn unlink_of_a_not_yet_committed_file_never_reaches_meta() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    c.flush().unwrap();
    let before = c.stats();
    for i in 0..500 {
        let a = c.create(r, format!("t{i}").as_bytes(), 0o644).unwrap();
        c.write(a.ino, 0, b"scratch").unwrap();
        c.unlink(r, format!("t{i}").as_bytes()).unwrap();
        c.forget(a.ino, 1);
    }
    c.flush().unwrap();
    let after = c.stats();
    assert_eq!(after.elided - before.elided, 500);
    assert_eq!(
        after.ops_committed, before.ops_committed,
        "elided creates were committed"
    );
    assert!(c.readdir(r, 0, 10).unwrap().entries.is_empty());
}

#[test]
fn background_flusher_commits_without_being_asked() {
    let f = fixture_with(Options {
        background: true,
        flush_interval: std::time::Duration::from_millis(40),
        sync_interval: std::time::Duration::from_millis(80),
        ..Options::default()
    });
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    mkfile(c, r, "f", &pattern(100_000, 5));
    let t = std::time::Instant::now();
    while c.stats().batches == 0 || c.stats().pending_ops > 0 || c.stats().dirty_bytes > 0 {
        assert!(
            t.elapsed().as_secs() < 10,
            "background flusher did not run: {:?}",
            c.stats()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(c.stats().batches >= 1);
    c.check().unwrap();
}

#[test]
fn an_inode_number_is_never_reused_for_another_file() {
    use std::collections::HashMap;
    let dir = tempfile::tempdir().unwrap();
    let mut seen: HashMap<u64, String> = HashMap::new();
    let mut note = |ino: u64, what: String| {
        if let Some(prev) = seen.insert(ino, what.clone()) {
            assert_eq!(prev, what, "inode {ino} named two different files");
        }
    };
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    for round in 0..300u32 {
        let name = format!("f{}", round % 5);
        let a = c.create(r, name.as_bytes(), 0o644).unwrap();
        note(a.ino, format!("session1 file {round}"));
        if round % 7 == 0 {
            c.flush().unwrap();
        }
        c.unlink(r, name.as_bytes()).unwrap();
        c.forget(a.ino, 1);
        if round % 50 == 49 {
            c.flush().unwrap();
        }
    }
    let old_root = r;
    c.remove_snapshot("s").unwrap();
    c.create_snapshot("s").unwrap();
    let r2 = root_entry(&c, "s").ino;
    assert_ne!(old_root, r2, "a removed snapshot's root number was reused");
    for i in 0..50 {
        let a = c.create(r2, format!("g{i}").as_bytes(), 0o644).unwrap();
        note(a.ino, format!("after snapshot recreate {i}"));
    }
    let g0 = c.lookup(r2, b"g0").unwrap().ino;
    let kept: Vec<u64> = (0..50)
        .map(|i| c.lookup(r2, format!("g{i}").as_bytes()).unwrap().ino)
        .collect();
    assert_eq!(kept[0], g0);
    for i in 0..25 {
        c.unlink(r2, format!("g{i}").as_bytes()).unwrap();
    }
    c.sync().unwrap();
    drop(c);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r3 = root_entry(&c, "s").ino;
    let mut after_restart: HashMap<u64, String> = HashMap::new();
    for i in 25..50 {
        let a = c.lookup(r3, format!("g{i}").as_bytes()).unwrap();
        after_restart.insert(a.ino, format!("g{i}"));
    }
    for i in 0..40 {
        let a = c.create(r3, format!("h{i}").as_bytes(), 0o644).unwrap();
        assert!(
            !after_restart.contains_key(&a.ino),
            "a file created after a restart got the number of a live file"
        );
    }
    c.sync().unwrap();
    drop(c);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r4 = root_entry(&c, "s").ino;
    let mut all: HashSet<u64> = HashSet::new();
    for i in 25..50 {
        assert!(all.insert(c.lookup(r4, format!("g{i}").as_bytes()).unwrap().ino));
    }
    for i in 0..40 {
        assert!(
            all.insert(c.lookup(r4, format!("h{i}").as_bytes()).unwrap().ino),
            "h{i} shares its number with another file after restart"
        );
    }
    c.check().unwrap();
}
