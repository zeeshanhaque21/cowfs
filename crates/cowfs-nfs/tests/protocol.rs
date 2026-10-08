//! Protocol tests: a raw NFSv3 client against the in-process server over `MemVfs`, no mount.
mod common;

use std::collections::HashSet;

use common::*;
use cowfs_nfs::{AdapterOptions, AppleDoubleMode, CowNfs, MountOptions, HANDLE_LEN};
use nfsserve::nfs::{ftype3, nfs_fh3, nfsstat3};
use nfsserve::vfs::NFSFileSystem;

fn setup() -> (cowfs_nfs::Server, Nfs) {
    serve(memfs(), MountOptions::default())
}

#[test]
fn null_and_unknown_procedures() {
    let (_s, mut c) = setup();
    let (acc, _) = c.raw(100_003, 3, 0, Args::new());
    assert_eq!(acc, 0);
    let (acc, _) = c.raw(100_003, 3, 40, Args::new());
    assert_eq!(acc, 3, "PROC_UNAVAIL");
    let (acc, _) = c.raw(100_999, 3, 0, Args::new());
    assert_eq!(acc, 1, "PROG_UNAVAIL");
    let (st, _) = c.call(11, Args::new());
    assert_eq!(st, NOTSUPP, "MKNOD");
}

#[test]
fn malformed_arguments_get_garbage_args() {
    let (_s, mut c) = setup();
    for proc in [1, 2, 3, 6, 7, 8, 9, 10, 12, 14, 15, 16, 17, 18, 19, 20, 21] {
        let (acc, _) = c.raw(100_003, 3, proc, Args::new().put(&1u32));
        assert_eq!(acc, 4, "proc {proc}");
    }
    let (st, _) = c.getattr(&nfs_fh3 {
        data: vec![1, 2, 3],
    });
    assert_eq!(st, BADHANDLE);
}

#[test]
fn lookup_returns_object_and_directory_attributes() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let (st, fh, obj, dir) = c.lookup(&root, "nope");
    assert_eq!((st, fh.is_none(), obj.is_none()), (NOENT, true, true));
    let dir = dir.expect("directory attributes on failure");
    assert_eq!(dir.ftype, ftype3::NF3DIR);

    let f = c.create_file(&root, "a");
    let (st, fh, obj, dir) = c.lookup(&root, "a");
    assert_eq!(st, OK);
    assert_eq!(fh.unwrap().data, f.data);
    assert_eq!(obj.unwrap().ftype, ftype3::NF3REG);
    assert!(dir.is_some());

    let (st, fh, _, _) = c.lookup(&f, "x");
    assert_eq!(st, NOTDIR);
    assert!(fh.is_none());
    assert_eq!(c.lookup(&root, ".").1.unwrap().data, root.data);
    assert_eq!(c.lookup(&root, "..").1.unwrap().data, root.data);
    assert_eq!(c.lookup(&root, &"x".repeat(256)).0, NAMETOOLONG);
}

#[test]
fn dotdot_walks_up() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let (_, a) = c.mkdir(&root, "a");
    let a = a.unwrap();
    let (_, b) = c.mkdir(&a, "b");
    let b = b.unwrap();
    assert_eq!(c.lookup(&b, "..").1.unwrap().data, a.data);
    assert_eq!(c.lookup(&a, "..").1.unwrap().data, root.data);
    assert_eq!(c.rename(&a, "b", &root, "b2"), OK);
    assert_eq!(
        c.lookup(&b, "..").1.unwrap().data,
        root.data,
        "parent follows a directory rename"
    );
}

#[test]
fn create_modes() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    c.write(&f, 0, b"hello", 2);

    let (st, _, _) = c.create(&root, "f", 1, sattr_mode(0o644), [0; 8]);
    assert_eq!(st, EXIST, "guarded on an existing name");

    let (st, fh, a) = c.create(&root, "f", 0, sattr3_default(), [0; 8]);
    assert_eq!(
        (st, fh.unwrap().data),
        (OK, f.data.clone()),
        "unchecked reuses"
    );
    assert_eq!(a.unwrap().size, 5, "and keeps the content");

    let (st, _, a) = c.create(&root, "f", 0, sattr_size(0), [0; 8]);
    assert_eq!(
        (st, a.unwrap().size),
        (OK, 0),
        "unchecked with size 0 truncates"
    );

    let verf = [1, 2, 3, 4, 5, 6, 7, 8];
    let (st, fh1, _) = c.create(&root, "x", 2, sattr3_default(), verf);
    assert_eq!(st, OK);
    let (st, fh2, _) = c.create(&root, "x", 2, sattr3_default(), verf);
    assert_eq!(st, OK, "same verifier is a retry");
    assert_eq!(fh1.unwrap().data, fh2.unwrap().data);
    let (st, _, _) = c.create(&root, "x", 2, sattr3_default(), [9; 8]);
    assert_eq!(st, EXIST, "another verifier is a conflict");

    let (st, _) = c.mkdir(&root, "d");
    assert_eq!(st, OK);
    let (st, _, _) = c.create(&root, "d", 0, sattr3_default(), [0; 8]);
    assert_eq!(st, ISDIR);
    let (st, _, _) = c.create(&root, "..", 0, sattr3_default(), [0; 8]);
    assert_eq!(st, EXIST);
    let (st, _, _) = c.create(&root, "a/b", 1, sattr3_default(), [0; 8]);
    assert_eq!(st, INVAL);
}

fn sattr3_default() -> nfsserve::nfs::sattr3 {
    nfsserve::nfs::sattr3::default()
}

#[test]
fn read_write_commit_and_eof() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    assert_eq!(
        c.write(&f, 0, b"0123456789", 0),
        (OK, 10, 0),
        "unstable stays unstable"
    );
    assert_eq!(c.write(&f, 10, b"abc", 2), (OK, 3, 2));
    assert_eq!(c.commit(&f), OK);

    assert_eq!(c.read(&f, 0, 4), (OK, b"0123".to_vec(), false));
    assert_eq!(c.read(&f, 8, 100), (OK, b"89abc".to_vec(), true));
    assert_eq!(c.read(&f, 13, 10), (OK, vec![], true));
    assert_eq!(c.read(&f, 9_999, 10), (OK, vec![], true));
    assert_eq!(
        c.read(&f, 3, 10),
        (OK, b"3456789abc".to_vec(), true),
        "exact end is eof"
    );

    c.write(&f, 20, b"z", 2);
    let (_, hole, _) = c.read(&f, 13, 7);
    assert_eq!(hole, vec![0; 7], "holes read as zeros");
    assert_eq!(c.attrs(&f).size, 21);

    let (st, _, _) = c.read(&root, 0, 10);
    assert_eq!(st, ISDIR);
}

#[test]
fn write_and_read_work_for_files_created_read_only() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let (st, f, a) = c.create(&root, "ro", 1, sattr_mode(0o444), [0; 8]);
    assert_eq!(st, OK);
    assert_eq!(a.unwrap().mode, 0o444);
    let f = f.unwrap();
    assert_eq!(c.write(&f, 0, b"data", 2).0, OK);
    assert_eq!(c.read(&f, 0, 10), (OK, b"data".to_vec(), true));
    assert_eq!(c.setattr(&f, sattr_size(2)).0, OK);
    assert_eq!(c.setattr(&f, sattr_mode(0o400)).1.unwrap().mode, 0o400);
    assert_eq!(c.write(&f, 2, b"!!", 2).0, OK);
}

#[test]
fn setattr_never_follows_symlinks() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let target = c.create_file(&root, "target");
    c.setattr(&target, sattr_mtime(1000, 5));
    let (st, link) = c.symlink(&root, "link", "target");
    assert_eq!(st, OK);
    let link = link.unwrap();
    let (st, dangling) = c.symlink(&root, "dangling", "missing");
    assert_eq!(st, OK);
    let dangling = dangling.unwrap();

    let (st, after) = c.setattr(&link, sattr_mtime(2000, 7));
    assert_eq!(st, OK);
    let after = after.unwrap();
    assert_eq!(after.ftype, ftype3::NF3LNK);
    assert_eq!((after.mtime.seconds, after.mtime.nseconds), (2000, 7));
    let t = c.attrs(&target);
    assert_eq!(
        (t.mtime.seconds, t.mtime.nseconds),
        (1000, 5),
        "the target is untouched"
    );

    let (st, a) = c.setattr(&dangling, sattr_mtime(3000, 0));
    assert_eq!(st, OK, "a dangling link takes its own times");
    assert_eq!(a.unwrap().mtime.seconds, 3000);
}

#[test]
fn setattr_variants() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    c.write(&f, 0, b"hello world", 2);
    let (_, a) = c.setattr(&f, sattr_size(5));
    assert_eq!(a.unwrap().size, 5);
    assert_eq!(c.read(&f, 0, 100).1, b"hello".to_vec());
    let (_, a) = c.setattr(&f, sattr_size(8));
    assert_eq!(a.unwrap().size, 8);
    assert_eq!(c.read(&f, 0, 100).1, b"hello\0\0\0".to_vec());
    let (st, a) = c.setattr(&f, sattr_mode(0o170_600));
    assert_eq!(
        (st, a.unwrap().mode),
        (OK, 0o600),
        "type bits are masked off"
    );
    assert_eq!(c.setattr(&root, sattr_size(0)).0, ISDIR);
    let (st, _) = c.setattr(&f, sattr3_default());
    assert_eq!(st, OK, "empty setattr succeeds");
}

#[test]
fn chown_to_another_uid_is_refused_and_changes_nothing() {
    use nfsserve::nfs::{set_gid3, set_uid3};
    const PERM: u32 = nfsstat3::NFS3ERR_PERM as u32;
    let own = |u, g| nfsserve::nfs::sattr3 {
        uid: set_uid3::uid(u),
        gid: set_gid3::gid(g),
        ..sattr3_default()
    };
    let opts = MountOptions {
        appledouble: AppleDoubleMode::Hide,
        ..MountOptions::default()
    };
    let (_s, mut c) = serve(memfs(), opts);
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    let side = c.create_file(&root, "._f");
    let a = c.attrs(&f);
    let (uid, gid) = (a.uid, a.gid);
    // Everything is owned by the mounter, so another uid is a change the filesystem will not make.
    assert_eq!(c.setattr(&f, own(uid + 1, gid)).0, PERM, "other uid");
    assert_eq!(
        c.setattr(&side, own(uid + 1, gid)).0,
        PERM,
        "other uid, sidecar"
    );
    // A refused chown refuses the whole request, as native does: the mode in it is not applied.
    let mixed = nfsserve::nfs::sattr3 {
        uid: set_uid3::uid(uid + 1),
        ..sattr_mode(0o600)
    };
    assert_eq!(c.setattr(&f, mixed).0, PERM);
    assert_eq!(
        c.attrs(&f).mode,
        a.mode,
        "mode untouched by a refused chown"
    );
    // The current uid changes nothing, and a gid is accepted and ignored.
    assert_eq!(c.setattr(&f, own(uid, gid)).0, OK, "same owner");
    assert_eq!(c.setattr(&f, own(uid, gid + 1)).0, OK, "any gid");
    assert_eq!(c.attrs(&f).gid, gid, "gid unchanged");
}

#[test]
fn namespace_operations() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let (_, d) = c.mkdir(&root, "d");
    let d = d.unwrap();
    assert_eq!(c.mkdir(&root, "d").0, EXIST);
    let f = c.create_file(&d, "f");
    assert_eq!(c.rmdir(&root, "d"), NOTEMPTY);
    assert_eq!(c.remove(&root, "d"), ISDIR);
    assert_eq!(c.rmdir(&d, "f"), NOTDIR);

    let (st, l) = c.symlink(&d, "l", "../f target");
    assert_eq!(st, OK);
    let l = l.unwrap();
    assert_eq!(c.readlink(&l), (OK, b"../f target".to_vec()));
    assert_eq!(c.readlink(&f).0, INVAL);
    assert_eq!(c.symlink(&d, "l", "x").0, EXIST);
    assert_eq!(c.symlink(&d, "e", "").0, INVAL);
    assert_eq!(c.attrs(&l).size, "../f target".len() as u64);

    let (st, a) = c.link(&f, &root, "hard");
    assert_eq!(st, OK);
    assert_eq!(a.unwrap().nlink, 2);
    assert_eq!(
        c.must_lookup(&root, "hard").data,
        f.data,
        "one inode, one handle"
    );
    c.write(&f, 0, b"shared", 2);
    let h = c.must_lookup(&root, "hard");
    assert_eq!(c.read(&h, 0, 10).1, b"shared".to_vec());
    assert_eq!(
        c.link(&d, &root, "dirlink").0,
        nfsstat3::NFS3ERR_ACCES as u32
    );
    assert_eq!(c.link(&f, &root, "hard").0, EXIST);
    assert_eq!(c.remove(&d, "f"), OK);
    assert_eq!(c.attrs(&h).nlink, 1);
    assert_eq!(
        c.read(&h, 0, 10).1,
        b"shared".to_vec(),
        "data outlives the first name"
    );

    assert_eq!(c.rename(&root, "hard", &d, "moved"), OK);
    assert_eq!(c.lookup(&root, "hard").0, NOENT);
    assert_eq!(c.must_lookup(&d, "moved").data, f.data);
    assert_eq!(c.rename(&root, "missing", &d, "x"), NOENT);
    let g = c.create_file(&root, "g");
    assert_eq!(c.rename(&root, "g", &d, "moved"), OK, "replaces a file");
    assert_eq!(c.must_lookup(&d, "moved").data, g.data);
    assert_eq!(
        c.rename(&root, "d", &d, "inside"),
        INVAL,
        "into its own subtree"
    );
    assert_eq!(c.remove(&d, "moved"), OK);
    assert_eq!(c.remove(&d, "l"), OK);
    assert_eq!(c.rmdir(&root, "d"), OK);
    assert_eq!(
        c.getattr(&d).0,
        STALE,
        "a removed directory's handle is stale"
    );
}

#[test]
fn readdir_pages_are_complete_and_unique() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let (_, d) = c.mkdir(&root, "d");
    let d = d.unwrap();
    for i in 0..300 {
        c.create_file(&d, &format!("file-{i:04}"));
    }
    for plus in [false, true] {
        let all = c.list(&d, plus, 600);
        let names: HashSet<_> = all.iter().map(|e| e.name.clone()).collect();
        assert_eq!(all.len(), 300, "plus={plus}");
        assert_eq!(names.len(), 300);
        assert!(all.iter().all(|e| e.cookie != 0));
        assert!(!names.contains(".") && !names.contains(".."));
        if plus {
            assert!(all.iter().all(|e| e.attr.is_some() && e.fh.is_some()));
            let e = &all[7];
            assert_eq!(e.attr.unwrap().fileid, e.fileid);
            assert_eq!(e.fh.as_ref().unwrap().data, c.must_lookup(&d, &e.name).data);
        } else {
            assert!(all.iter().all(|e| e.attr.is_none()));
        }
    }
    let (st, page, eof) = c.readdir_page(&d, 0, true, 100_000);
    assert_eq!((st, page.len(), eof), (OK, 300, true));
    let (st, page, eof) = c.readdir_page(&d, page[299].cookie, true, 100_000);
    assert_eq!((st, page.len(), eof), (OK, 0, true));
    let (st, ..) = c.readdir_page(
        &nfs_fh3 {
            data: vec![0; HANDLE_LEN],
        },
        0,
        false,
        4096,
    );
    assert_eq!(st, STALE, "generation 0 predates this server");
}

#[test]
fn hardlinks_in_one_directory_list_once_each() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let (_, d) = c.mkdir(&root, "d");
    let d = d.unwrap();
    for i in 0..200 {
        let f = c.create_file(&d, &format!("o{i:03}"));
        assert_eq!(c.link(&f, &d, &format!("o{i:03}.link")).0, OK);
    }
    for dircount in [300, 700, 4096] {
        let all = c.list(&d, true, dircount);
        let names: HashSet<_> = all.iter().map(|e| e.name.clone()).collect();
        assert_eq!(all.len(), 400, "dircount {dircount}");
        assert_eq!(names.len(), 400);
        let ids: HashSet<_> = all.iter().map(|e| e.fileid).collect();
        assert_eq!(ids.len(), 200, "pairs share a file id");
        let attrs = all.iter().find(|e| e.name == "o007").unwrap().attr.unwrap();
        assert_eq!(attrs.nlink, 2);
    }
}

#[test]
fn deleting_while_listing_neither_repeats_nor_drops() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let (_, d) = c.mkdir(&root, "d");
    let d = d.unwrap();
    for i in 0..300 {
        c.create_file(&d, &format!("f{i:04}"));
    }
    let mut removed = HashSet::new();
    let dd = d.clone();
    let all = c.list_with(&d, true, 500, |c, page| {
        for e in page {
            assert_eq!(c.remove(&dd, &e.name), OK);
            removed.insert(e.name.clone());
        }
    });
    let seen: Vec<_> = all.iter().map(|e| e.name.clone()).collect();
    let uniq: HashSet<_> = seen.iter().cloned().collect();
    assert_eq!(seen.len(), uniq.len(), "no entry twice");
    assert_eq!(
        uniq.len(),
        300,
        "every entry that lived through the whole listing is seen"
    );
    assert_eq!(removed.len(), 300);
    assert!(c.names(&d).is_empty());
}

#[test]
fn appledouble_is_hidden_but_present() {
    let opts = MountOptions {
        appledouble: AppleDoubleMode::Hide,
        ..MountOptions::default()
    };
    let (_s, mut c) = serve(memfs(), opts);
    let root = c.root.clone();
    c.create_file(&root, "doc");
    let side = c.create_file(&root, "._doc");
    c.write(&side, 0, b"xattrs", 2);
    let _ = c.create_file(&root, "._orphan");
    assert_eq!(c.names(&root), vec!["doc"], "hidden from readdirplus");
    let plain: Vec<_> = c
        .list(&root, false, 4096)
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(plain, vec!["doc"], "and from readdir");
    assert_eq!(
        c.must_lookup(&root, "._doc").data,
        side.data,
        "still resolvable"
    );

    assert_eq!(c.rename(&root, "doc", &root, "doc2"), OK);
    assert_eq!(c.lookup(&root, "._doc").0, NOENT);
    let moved = c.must_lookup(&root, "._doc2");
    assert_eq!(
        c.read(&moved, 0, 10).1,
        b"xattrs".to_vec(),
        "sidecar moves with the file"
    );
    assert_eq!(c.remove(&root, "doc2"), OK);
    assert_eq!(
        c.lookup(&root, "._doc2").0,
        NOENT,
        "sidecar removed with the file"
    );

    let (_, d) = c.mkdir(&root, "dir");
    let d = d.unwrap();
    c.create_file(&d, "._x");
    assert_eq!(
        c.rmdir(&root, "dir"),
        OK,
        "a directory holding only sidecars is empty"
    );
    let (_, d) = c.mkdir(&root, "dir2");
    let d = d.unwrap();
    c.create_file(&d, "._x");
    c.create_file(&d, "real");
    assert_eq!(c.rmdir(&root, "dir2"), NOTEMPTY);
    assert!(
        c.lookup(&d, "._x").1.is_some(),
        "a refused rmdir removes nothing"
    );
}

#[test]
fn appledouble_shows_when_not_hidden() {
    let opts = MountOptions {
        appledouble: AppleDoubleMode::Store,
        ..MountOptions::default()
    };
    let (_s, mut c) = serve(memfs(), opts);
    let root = c.root.clone();
    c.create_file(&root, "doc");
    c.create_file(&root, "._doc");
    assert_eq!(c.names(&root), vec!["._doc", "doc"]);
    assert_eq!(c.remove(&root, "doc"), OK);
    assert_eq!(
        c.names(&root),
        vec!["._doc"],
        "no special handling when shown"
    );
}

#[test]
fn filesystem_information() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let (st, mut r) = c.call(19, Args::new().put(&root));
    assert_eq!(st, OK);
    let _a: nfsserve::nfs::post_op_attr = dec(&mut r);
    let rtmax: u32 = dec(&mut r);
    assert_eq!(rtmax, 1 << 20);
    let (st, mut r) = c.call(18, Args::new().put(&root));
    assert_eq!(st, OK);
    let _a: nfsserve::nfs::post_op_attr = dec(&mut r);
    let tbytes: u64 = dec(&mut r);
    assert!(tbytes > 0);
    let (st, mut r) = c.call(20, Args::new().put(&root));
    assert_eq!(st, OK);
    let _a: nfsserve::nfs::post_op_attr = dec(&mut r);
    let (linkmax, name_max): (u32, u32) = (dec(&mut r), dec(&mut r));
    assert_eq!((linkmax > 0, name_max), (true, 255));
}

#[test]
fn access_reports_owner_bits() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    let all = 0x3f;
    let (st, got) = c.access(&f, all);
    assert_eq!(st, OK);
    assert_eq!(got & 0x01, 0x01, "read");
    assert_eq!(got & 0x0c, 0x0c, "modify and extend");
    assert_eq!(got & 0x20, 0, "not executable");
    c.setattr(&f, sattr_mode(0o755));
    assert_eq!(c.access(&f, all).1 & 0x20, 0x20);
    c.setattr(&f, sattr_mode(0o444));
    assert_eq!(
        c.access(&f, all).1 & 0x0c,
        0,
        "read-only mode refuses modify"
    );
    let (_, got) = c.access(&root, all);
    assert_eq!(got & 0x12, 0x12, "lookup and delete in a directory");
}

#[test]
fn handles_encode_decode_and_expire() {
    let fs = CowNfs::new(memfs(), AdapterOptions::default()).unwrap();
    let fh = fs.id_to_fh(77);
    assert_eq!(fh.data.len(), cowfs_nfs::HANDLE_LEN);
    assert_eq!(fs.fh_to_id(&fh), Ok(77));
    assert_eq!(
        fs.id_to_fh(77).data,
        fh.data,
        "stable for the server's lifetime"
    );
    assert_eq!(
        fs.fh_to_id(&nfs_fh3 { data: vec![] }),
        Err(nfsstat3::NFS3ERR_BADHANDLE)
    );
    assert_eq!(
        fs.fh_to_id(&nfs_fh3 { data: vec![0; 17] }),
        Err(nfsstat3::NFS3ERR_BADHANDLE)
    );

    let mut old = fh.data.clone();
    old[..8].copy_from_slice(&(fs.generation() - 1).to_le_bytes());
    assert_eq!(
        fs.fh_to_id(&nfs_fh3 { data: old }),
        Err(nfsstat3::NFS3ERR_STALE),
        "a restarted server"
    );
    let mut new = fh.data;
    new[..8].copy_from_slice(&(fs.generation() + 1).to_le_bytes());
    assert_eq!(
        fs.fh_to_id(&nfs_fh3 { data: new }),
        Err(nfsstat3::NFS3ERR_BADHANDLE)
    );
}

#[test]
fn two_generations_reject_each_others_handles() {
    let (_s1, c1) = setup();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let (_s2, mut c2) = setup();
    let (st, _) = c2.getattr(&c1.root);
    assert_eq!(st, STALE);
}

#[test]
fn many_connections_share_one_server() {
    let (s, mut c) = setup();
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    c.write(&f, 0, b"x", 2);
    let threads: Vec<_> = (0..4)
        .map(|i| {
            let port = s.port();
            let root = root.clone();
            std::thread::spawn(move || {
                let mut c = Nfs::attach(port, root.clone());
                for j in 0..50 {
                    let n = format!("t{i}-{j}");
                    let f = c.create_file(&root, &n);
                    assert_eq!(c.write(&f, 0, n.as_bytes(), 2).0, OK);
                    assert_eq!(c.read(&f, 0, 64).1, n.as_bytes());
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(c.names(&root).len(), 201);
}

#[test]
fn oversized_and_bad_frames_do_not_kill_the_server() {
    let (s, mut c) = setup();
    let root = c.root.clone();
    {
        use std::io::Write;
        let mut bad = std::net::TcpStream::connect(("127.0.0.1", s.port())).unwrap();
        bad.write_all(&(u32::MAX).to_be_bytes()).unwrap();
        bad.write_all(&[0; 64]).unwrap();
    }
    let mut c2 = Nfs::attach(s.port(), root.clone());
    assert_eq!(c2.getattr(&root).0, OK);
    assert_eq!(c.getattr(&root).0, OK);
    let (acc, _) = c.raw(
        100_003,
        3,
        6,
        Args::new().put(&root).put(&0u64).put(&u32::MAX),
    );
    assert_eq!(acc, 0);
}

#[test]
fn owner_override_applies_to_every_reply() {
    let server = cowfs_nfs::Server::start(
        memfs(),
        &MountOptions {
            check_peer_uid: false,
            ..MountOptions::default()
        },
        Some((501, 20)),
    )
    .unwrap();
    let mut c = Nfs::connect(server.port(), server.export_name());
    let root = c.root.clone();
    assert_eq!((c.attrs(&root).uid, c.attrs(&root).gid), (501, 20));
    let f = c.create_file(&root, "f");
    assert_eq!(c.attrs(&f).uid, 501);
    let listed = c.list(&root, true, 4096);
    assert_eq!(listed[0].attr.unwrap().uid, 501);
    let (_, _, obj, dir) = c.lookup(&root, "f");
    assert_eq!((obj.unwrap().gid, dir.unwrap().uid), (20, 501));
}

#[test]
fn failures_still_carry_attributes() {
    let (_s, mut c) = setup();
    let root = c.root.clone();
    let f = c.create_file(&root, "f");

    let (st, mut r) = c.call(6, Args::new().put(&root).put(&0u64).put(&10u32));
    assert_eq!(st, ISDIR);
    assert!(attr(dec(&mut r)).is_some(), "READ failure");
    let (st, mut r) = c.call(5, Args::new().put(&f));
    assert_eq!(st, INVAL);
    assert!(attr(dec(&mut r)).is_some(), "READLINK failure");
    let (st, mut r) = c.call(
        16,
        Args::new().put(&f).put(&0u64).put(&[0u8; 8]).put(&4096u32),
    );
    assert_eq!(st, NOTDIR);
    assert!(attr(dec(&mut r)).is_some(), "READDIR failure");

    let wcc_after = |mut r: Rd| -> bool {
        let w: nfsserve::nfs::wcc_data = dec(&mut r);
        attr(w.after).is_some() && matches!(w.before, nfsserve::nfs::pre_op_attr::attributes(_))
    };
    let (st, r) = c.call(
        9,
        Args::new().put(&dirop(&root, "f")).put(&sattr_mode(0o755)),
    );
    assert_eq!(st, EXIST);
    assert!(wcc_after(r), "MKDIR failure");
    let (st, r) = c.call(12, Args::new().put(&dirop(&root, "missing")));
    assert_eq!(st, NOENT);
    assert!(wcc_after(r), "REMOVE failure");
    let (st, r) = c.call(13, Args::new().put(&dirop(&root, "f")));
    assert_eq!(st, NOTDIR);
    assert!(wcc_after(r), "RMDIR failure");
    let a = Args::new()
        .put(&root)
        .put(&0u64)
        .put(&1u32)
        .put(&2u32)
        .put(&b"x".to_vec());
    let (st, r) = c.call(7, a);
    assert_eq!(st, ISDIR);
    assert!(wcc_after(r), "WRITE failure");
}
