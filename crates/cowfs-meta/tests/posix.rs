use cowfs_meta::{
    BlockId, ChunkRef, Error, FileType, Ino, Marker, Meta, Options, SetAttr, Snapshot, ROOT_INO,
};

fn open() -> (tempfile::TempDir, Meta) {
    let dir = tempfile::tempdir().unwrap();
    let meta = Meta::open(dir.path().join("meta.redb"), Options::default()).unwrap();
    (dir, meta)
}

fn chunk(n: u8, len: u32) -> ChunkRef {
    ChunkRef {
        id: BlockId::of(&[n]),
        len,
    }
}

fn names(s: &Snapshot, dir: Ino) -> Vec<String> {
    let mut out = Vec::new();
    let mut cookie = 0;
    loop {
        let page = s.readdir(dir, cookie, 3).unwrap();
        out.extend(
            page.entries
                .iter()
                .map(|e| String::from_utf8(e.name.clone()).unwrap()),
        );
        cookie = page.next_cookie;
        if page.end {
            return out;
        }
    }
}

#[test]
fn create_lookup_readdir_and_check() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    let dir = s.mkdir(ROOT_INO, b"src", 0o755).unwrap();
    let f = s.create(dir.ino, b"main.rs", 0o644).unwrap();
    assert_eq!(s.lookup(dir.ino, b"main.rs").unwrap().ino, f.ino);
    assert_eq!(s.lookup(dir.ino, b"..").unwrap().ino, ROOT_INO);
    assert_eq!(s.lookup(dir.ino, b".").unwrap().ino, dir.ino);
    assert_eq!(s.getattr(ROOT_INO).unwrap().nlink, 3);
    assert_eq!(s.getattr(f.ino).unwrap().mode, 0o644);
    assert!(matches!(
        s.create(dir.ino, b"main.rs", 0o644),
        Err(Error::Exists)
    ));
    assert!(matches!(s.lookup(dir.ino, b"nope"), Err(Error::NotFound)));
    assert_eq!(names(&s, ROOT_INO), ["src"]);
    m.check().unwrap();
}

#[test]
fn set_content_and_chunks_roundtrip() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
    let cs: Vec<ChunkRef> = (0..300).map(|i| chunk(i as u8, 10)).collect();
    s.set_content(f.ino, &cs, 3000).unwrap();
    assert_eq!(s.chunks(f.ino).unwrap(), cs);
    assert_eq!(s.getattr(f.ino).unwrap().size, 3000);
    s.set_content(f.ino, &cs[..5], 50).unwrap();
    assert_eq!(s.chunks(f.ino).unwrap(), &cs[..5]);
    assert!(s.set_content(f.ino, &cs, 10).is_err());
    let a = s
        .setattr(
            f.ino,
            SetAttr {
                size: Some(30),
                mode: Some(0o600),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!((a.size, a.mode), (30, 0o600));
    assert_eq!(s.chunks(f.ino).unwrap(), &cs[..3]);
    assert!(s
        .setattr(
            f.ino,
            SetAttr {
                size: Some(25),
                ..Default::default()
            }
        )
        .is_err());
    s.setattr(
        f.ino,
        SetAttr {
            size: Some(100),
            ..Default::default()
        },
    )
    .unwrap();
    m.check().unwrap();
}

#[test]
fn hardlinks_share_content_and_survive_unlink() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    let a = s.mkdir(ROOT_INO, b"a", 0o755).unwrap().ino;
    let b = s.mkdir(ROOT_INO, b"b", 0o755).unwrap().ino;
    let f = s.create(a, b"one", 0o644).unwrap();
    let l = s.link(f.ino, b, b"two").unwrap();
    assert_eq!(l.nlink, 2);
    assert_eq!(s.lookup(b, b"two").unwrap().ino, f.ino);
    s.set_content(f.ino, &[chunk(1, 4)], 4).unwrap();
    let via = s.lookup(b, b"two").unwrap();
    assert_eq!(via.size, 4);
    assert_eq!(s.chunks(via.ino).unwrap(), [chunk(1, 4)]);
    let r = s.unlink(a, b"one").unwrap();
    assert!(!r.freed);
    assert_eq!(s.getattr(f.ino).unwrap().nlink, 1);
    assert_eq!(s.chunks(f.ino).unwrap(), [chunk(1, 4)]);
    let r = s.unlink(b, b"two").unwrap();
    assert!(r.freed);
    assert_eq!(r.chunks, [chunk(1, 4)]);
    assert!(matches!(s.getattr(f.ino), Err(Error::NotFound)));
    m.check().unwrap();
}

#[test]
fn hardlink_across_snapshot_boundary() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    let f = s.create(ROOT_INO, b"one", 0o644).unwrap();
    s.set_content(f.ino, &[chunk(1, 4)], 4).unwrap();
    s.link(f.ino, ROOT_INO, b"two").unwrap();
    let c = s.fork("clone").unwrap();
    assert_eq!(c.getattr(f.ino).unwrap().nlink, 2);
    assert_eq!(c.chunks(f.ino).unwrap(), s.chunks(f.ino).unwrap());
    c.unlink(ROOT_INO, b"one").unwrap();
    assert_eq!(c.getattr(f.ino).unwrap().nlink, 1);
    assert_eq!(s.getattr(f.ino).unwrap().nlink, 2);
    c.set_content(f.ino, &[chunk(2, 8)], 8).unwrap();
    assert_eq!(c.lookup(ROOT_INO, b"two").unwrap().size, 8);
    assert_eq!(s.lookup(ROOT_INO, b"two").unwrap().size, 4);
    assert_eq!(s.chunks(f.ino).unwrap(), [chunk(1, 4)]);
    s.unlink(ROOT_INO, b"one").unwrap();
    s.unlink(ROOT_INO, b"two").unwrap();
    assert_eq!(c.chunks(f.ino).unwrap(), [chunk(2, 8)]);
    m.check().unwrap();
}

#[test]
fn snapshot_isolation_both_directions() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
    s.set_content(f.ino, &[chunk(1, 4)], 4).unwrap();
    let root_before = s.root().unwrap();
    let c = s.fork("clone").unwrap();
    assert_eq!(c.root().unwrap(), root_before);
    c.create(ROOT_INO, b"only-in-clone", 0o644).unwrap();
    c.set_content(f.ino, &[chunk(9, 9)], 9).unwrap();
    assert_eq!(s.root().unwrap(), root_before);
    assert!(s.lookup(ROOT_INO, b"only-in-clone").is_err());
    assert_eq!(s.chunks(f.ino).unwrap(), [chunk(1, 4)]);
    s.mkdir(ROOT_INO, b"only-in-main", 0o755).unwrap();
    s.unlink(ROOT_INO, b"f").unwrap();
    assert!(c.lookup(ROOT_INO, b"only-in-main").is_err());
    assert_eq!(c.chunks(f.ino).unwrap(), [chunk(9, 9)]);
    assert_ne!(s.root().unwrap(), c.root().unwrap());
    let ino_a = s.create(ROOT_INO, b"x", 0o644).unwrap().ino;
    let ino_b = c.create(ROOT_INO, b"y", 0o644).unwrap().ino;
    assert_ne!(ino_a, ino_b);
    m.check().unwrap();
    m.remove_snapshot(c.id()).unwrap();
    assert!(matches!(c.getattr(ROOT_INO), Err(Error::NoSuchSnapshot)));
    assert!(s.lookup(ROOT_INO, b"only-in-main").is_ok());
    m.check().unwrap();
    m.remove_snapshot(s.id()).unwrap();
    m.check().unwrap();
    assert!(m.snapshots().unwrap().is_empty());
}

#[test]
fn snapshot_names_and_listing() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    assert!(matches!(m.new_snapshot("main"), Err(Error::SnapshotExists)));
    let c = s.fork("c").unwrap();
    assert_eq!(c.info().unwrap().parent, Some(s.id()));
    let list = m.snapshots().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(m.snapshot("c").unwrap().id(), c.id());
    assert!(matches!(m.snapshot("zzz"), Err(Error::NoSuchSnapshot)));
}

#[test]
fn rename_cases() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    let a = s.mkdir(ROOT_INO, b"a", 0o755).unwrap().ino;
    let b = s.mkdir(a, b"b", 0o755).unwrap().ino;
    let f1 = s.create(ROOT_INO, b"f1", 0o644).unwrap();
    let f2 = s.create(ROOT_INO, b"f2", 0o644).unwrap();
    s.set_content(f1.ino, &[chunk(1, 1)], 1).unwrap();
    s.set_content(f2.ino, &[chunk(2, 2)], 2).unwrap();

    let r = s.rename(ROOT_INO, b"f1", ROOT_INO, b"f2").unwrap().unwrap();
    assert!(r.freed);
    assert_eq!(s.lookup(ROOT_INO, b"f2").unwrap().ino, f1.ino);
    assert!(s.lookup(ROOT_INO, b"f1").is_err());
    assert!(matches!(s.getattr(f2.ino), Err(Error::NotFound)));

    assert!(matches!(
        s.rename(ROOT_INO, b"a", b, b"a"),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        s.rename(ROOT_INO, b"a", a, b"a2"),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        s.rename(ROOT_INO, b"f2", ROOT_INO, b"a"),
        Err(Error::IsDir)
    ));
    assert!(matches!(
        s.rename(a, b"b", ROOT_INO, b"f2"),
        Err(Error::NotDir)
    ));

    let empty = s.mkdir(ROOT_INO, b"empty", 0o755).unwrap().ino;
    let full = s.mkdir(ROOT_INO, b"full", 0o755).unwrap().ino;
    s.create(full, b"x", 0o644).unwrap();
    assert!(matches!(
        s.rename(a, b"b", ROOT_INO, b"full"),
        Err(Error::NotEmpty)
    ));
    s.rename(a, b"b", ROOT_INO, b"empty").unwrap();
    assert_eq!(s.lookup(ROOT_INO, b"empty").unwrap().ino, b);
    assert!(matches!(s.getattr(empty), Err(Error::NotFound)));
    assert_eq!(s.lookup(b, b"..").unwrap().ino, ROOT_INO);
    assert_eq!(s.getattr(a).unwrap().nlink, 2);
    assert_eq!(s.getattr(ROOT_INO).unwrap().nlink, 5);

    let l = s.link(f1.ino, ROOT_INO, b"f3").unwrap();
    assert_eq!(l.nlink, 2);
    assert!(s
        .rename(ROOT_INO, b"f2", ROOT_INO, b"f3")
        .unwrap()
        .is_none());
    assert!(s
        .rename(ROOT_INO, b"f2", ROOT_INO, b"f2")
        .unwrap()
        .is_none());
    assert!(s.lookup(ROOT_INO, b"f2").is_ok());
    m.check().unwrap();
}

#[test]
fn symlinks_and_xattrs() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    let l = s.symlink(ROOT_INO, b"l", b"../target").unwrap();
    assert_eq!(l.kind, FileType::Symlink);
    assert_eq!(l.size, 9);
    assert_eq!(s.readlink(l.ino).unwrap(), b"../target");
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
    assert!(s.readlink(f.ino).is_err());
    s.setxattr(f.ino, b"user.a", b"1").unwrap();
    s.setxattr(f.ino, b"user.b", b"22").unwrap();
    s.setxattr(f.ino, b"user.a", b"333").unwrap();
    assert_eq!(s.getxattr(f.ino, b"user.a").unwrap(), b"333");
    assert_eq!(
        s.listxattr(f.ino).unwrap(),
        [b"user.a".to_vec(), b"user.b".to_vec()]
    );
    s.removexattr(f.ino, b"user.a").unwrap();
    assert!(matches!(s.getxattr(f.ino, b"user.a"), Err(Error::NoAttr)));
    assert!(matches!(
        s.removexattr(f.ino, b"user.a"),
        Err(Error::NoAttr)
    ));
    s.unlink(ROOT_INO, b"l").unwrap();
    s.unlink(ROOT_INO, b"f").unwrap();
    m.check().unwrap();
}

#[test]
fn batch_is_atomic() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    let before = s.root().unwrap();
    let r: Result<(), Error> = s.batch(|tx| {
        tx.create(ROOT_INO, b"a", 0o644)?;
        tx.create(ROOT_INO, b"b", 0o644)?;
        tx.create(ROOT_INO, b"a", 0o644)?;
        Ok(())
    });
    assert!(matches!(r, Err(Error::Exists)));
    assert_eq!(s.root().unwrap(), before);
    assert!(s.lookup(ROOT_INO, b"a").is_err());
    s.batch(|tx| {
        let d = tx.mkdir(ROOT_INO, b"d", 0o755)?;
        tx.create(d.ino, b"x", 0o644)?;
        assert!(tx.lookup(d.ino, b"x").is_ok());
        Ok(())
    })
    .unwrap();
    assert!(s.lookup(ROOT_INO, b"d").is_ok());
    m.check().unwrap();
}

#[test]
fn bad_names_rejected() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    for n in [&b""[..], b".", b"..", b"a/b", b"a\0b"] {
        assert!(matches!(
            s.create(ROOT_INO, n, 0o644),
            Err(Error::Invalid(_))
        ));
    }
    assert!(matches!(
        s.create(ROOT_INO, &[b'x'; 256], 0o644),
        Err(Error::NameTooLong)
    ));
    s.create(ROOT_INO, &[b'x'; 255], 0o644).unwrap();
    assert!(matches!(
        s.link(ROOT_INO, ROOT_INO, b"d"),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn readdir_is_stable_and_resumable_under_removal() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    let f = s.create(ROOT_INO, b"file", 0o644).unwrap();
    let mut all = vec!["file".to_string()];
    for i in 0..40 {
        let n = format!("link{i:02}");
        s.link(f.ino, ROOT_INO, n.as_bytes()).unwrap();
        all.push(n);
    }
    let mut seen = Vec::new();
    let mut cookie = 0;
    let mut removed = Vec::new();
    loop {
        let page = s.readdir(ROOT_INO, cookie, 5).unwrap();
        for e in &page.entries {
            seen.push(String::from_utf8(e.name.clone()).unwrap());
        }
        cookie = page.next_cookie;
        if page.end {
            break;
        }
        let victim = format!("link{:02}", seen.len() % 40);
        if s.unlink(ROOT_INO, victim.as_bytes()).is_ok() {
            removed.push(victim);
        }
    }
    let mut uniq = seen.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), seen.len(), "duplicate entries");
    for n in &all {
        if !removed.contains(n) {
            assert!(seen.contains(n), "dropped surviving entry {n}");
        }
    }
    m.check().unwrap();
}

#[test]
fn readdir_cookie_survives_removing_the_cookie_entry() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    for i in 0..10 {
        s.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
            .unwrap();
    }
    let page = s.readdir(ROOT_INO, 0, 4).unwrap();
    assert_eq!(page.entries.len(), 4);
    assert!(!page.end);
    let last = page.entries.last().unwrap().name.clone();
    s.unlink(ROOT_INO, &last).unwrap();
    let rest = s.readdir(ROOT_INO, page.next_cookie, 100).unwrap();
    assert_eq!(rest.entries.len(), 6);
    assert!(rest.end);
    assert_eq!(rest.entries[0].name, b"f4");
}

#[test]
fn live_blocks_skips_shared_subtrees() {
    let (_d, m) = open();
    let s = m.new_snapshot("main").unwrap();
    let mut expect = Vec::new();
    for i in 0..300u32 {
        let f = s
            .create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
            .unwrap();
        let c = ChunkRef {
            id: BlockId::of(&i.to_le_bytes()),
            len: 1,
        };
        s.set_content(f.ino, &[c], 1).unwrap();
        expect.push(c.id);
    }
    let mut marker = Marker::new();
    let mut got: Vec<BlockId> = s
        .live_blocks(&mut marker)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    got.sort();
    got.dedup();
    expect.sort();
    assert_eq!(got, expect);
    let walked = marker.len();
    assert!(walked > 1);

    let c = s.fork("c").unwrap();
    let f = c.create(ROOT_INO, b"new", 0o644).unwrap();
    let extra = ChunkRef {
        id: BlockId::of(b"extra"),
        len: 1,
    };
    c.set_content(f.ino, &[extra], 1).unwrap();
    let again: Vec<BlockId> = c
        .live_blocks(&mut marker)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(again.contains(&extra.id));
    assert!(again.len() < 300, "shared subtrees were not skipped");
    let mut fresh = Marker::new();
    let full: Vec<BlockId> = c
        .live_blocks(&mut fresh)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(full.len(), 301);
}

#[test]
fn reopen_preserves_everything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let (root, ino);
    {
        let m = Meta::open(&path, Options::default()).unwrap();
        let s = m.new_snapshot("main").unwrap();
        ino = s.create(ROOT_INO, b"f", 0o644).unwrap().ino;
        s.set_content(ino, &[chunk(1, 3)], 3).unwrap();
        root = s.root().unwrap();
    }
    let m = Meta::open(&path, Options::default()).unwrap();
    let s = m.snapshot("main").unwrap();
    assert_eq!(s.root().unwrap(), root);
    assert_eq!(s.chunks(ino).unwrap(), [chunk(1, 3)]);
    let n = s.create(ROOT_INO, b"g", 0o644).unwrap().ino;
    assert!(n.0 > ino.0);
    m.check().unwrap();
}

#[test]
fn many_files_split_the_tree_and_check_passes() {
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        node_size: 512,
        ..Options::default()
    };
    let m = Meta::open(dir.path().join("m.redb"), opts).unwrap();
    let s = m.new_snapshot("main").unwrap();
    s.batch(|tx| {
        for i in 0..2000 {
            let f = tx.create(ROOT_INO, format!("file-{i:05}").as_bytes(), 0o644)?;
            tx.set_content(f.ino, &[chunk((i % 200) as u8, 5)], 5)?;
        }
        Ok(())
    })
    .unwrap();
    m.check().unwrap();
    assert_eq!(s.readdir(ROOT_INO, 0, 5000).unwrap().entries.len(), 2000);
    s.batch(|tx| {
        for i in (0..2000).step_by(2) {
            tx.unlink(ROOT_INO, format!("file-{i:05}").as_bytes())?;
        }
        Ok(())
    })
    .unwrap();
    m.check().unwrap();
    assert_eq!(s.readdir(ROOT_INO, 0, 5000).unwrap().entries.len(), 1000);
}
