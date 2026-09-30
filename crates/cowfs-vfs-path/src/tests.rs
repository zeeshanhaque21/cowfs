use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "cowfs-pathvfs-unit-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).expect("create scratch dir");
        Self(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        force_remove_dir_all(&self.0);
    }
}

fn fs() -> (Scratch, PathVfs) {
    let s = Scratch::new();
    let v = PathVfs::new(&s.0).expect("open scratch dir");
    (s, v)
}

#[test]
fn hardlinked_names_share_an_ino_and_nlink_comes_from_the_backing_fs() {
    let (_s, v) = fs();
    let a = v.create(ROOT_INO, b"a", 0o644).expect("create");
    let b = v.link(a.ino, ROOT_INO, b"b").expect("link");
    assert_eq!(a.ino, b.ino);
    assert_eq!(b.nlink, 2);
    assert_eq!(v.lookup(ROOT_INO, b"b").expect("lookup").ino, a.ino);
    v.unlink(ROOT_INO, b"a").expect("unlink");
    assert_eq!(v.getattr(a.ino).expect("getattr").nlink, 1);
}

#[test]
fn unlinked_but_referenced_file_stays_usable_until_forgotten() {
    let (_s, v) = fs();
    let a = v.create(ROOT_INO, b"a", 0o644).expect("create");
    v.write(a.ino, 0, b"hello").expect("write");
    v.unlink(ROOT_INO, b"a").expect("unlink");
    assert_eq!(v.getattr(a.ino).expect("getattr").nlink, 0);
    assert_eq!(v.read(a.ino, 0, 10).expect("read"), b"hello");
    v.forget(a.ino, 1);
    assert_eq!(v.getattr(a.ino), Err(Error::Stale));
}

#[test]
fn handle_pins_an_unreferenced_unlinked_file() {
    let (_s, v) = fs();
    let a = v.create(ROOT_INO, b"a", 0o644).expect("create");
    let h = v.open(a.ino).expect("open");
    v.unlink(ROOT_INO, b"a").expect("unlink");
    v.forget(a.ino, 1);
    v.write(a.ino, 0, b"x").expect("write while pinned");
    v.release(h).expect("release");
    assert_eq!(v.getattr(a.ino), Err(Error::Stale));
    assert_eq!(v.release(h), Err(Error::InvalidArgument));
}

#[test]
fn create_sets_the_exact_mode_despite_the_umask() {
    let (_s, v) = fs();
    assert_eq!(v.create(ROOT_INO, b"f", 0o666).expect("create").mode, 0o666);
    assert_eq!(v.mkdir(ROOT_INO, b"d", 0o777).expect("mkdir").mode, 0o777);
}

#[test]
fn readonly_file_stays_writable_through_its_descriptor() {
    let (_s, v) = fs();
    let a = v.create(ROOT_INO, b"ro", 0o444).expect("create");
    assert_eq!(v.write(a.ino, 0, b"data").expect("write"), 4);
    assert_eq!(v.getattr(a.ino).expect("getattr").mode, 0o444);
}

#[test]
fn readdir_cookies_survive_removal_of_the_entry() {
    let (_s, v) = fs();
    for n in ["a", "b", "c", "d"] {
        v.create(ROOT_INO, n.as_bytes(), 0o644).expect("create");
    }
    let first = v.readdir(ROOT_INO, 0, 2).expect("readdir");
    assert!(!first.eof);
    let last = first.entries.last().expect("entry");
    v.unlink(ROOT_INO, &last.name).expect("unlink");
    let rest = v.readdir(ROOT_INO, last.cookie, 10).expect("resume");
    let names: Vec<&[u8]> = rest.entries.iter().map(|e| e.name.as_slice()).collect();
    assert_eq!(names, [b"c".as_slice(), b"d"]);
    assert!(rest.eof);
}

#[test]
fn symlink_is_never_followed() {
    let (_s, v) = fs();
    let t = v.create(ROOT_INO, b"t", 0o644).expect("create");
    let l = v.symlink(ROOT_INO, b"l", b"t").expect("symlink");
    assert_eq!(l.kind, FileKind::Symlink);
    assert_eq!(v.readlink(l.ino).expect("readlink"), b"t");
    assert_ne!(l.ino, t.ino);
    assert_eq!(v.write(l.ino, 0, b"x"), Err(Error::InvalidArgument));
}

#[test]
fn rename_keeps_the_ino_and_replaced_file_loses_a_link() {
    let (_s, v) = fs();
    let a = v.create(ROOT_INO, b"a", 0o644).expect("create a");
    let b = v.create(ROOT_INO, b"b", 0o644).expect("create b");
    v.rename(ROOT_INO, b"a", ROOT_INO, b"b", RenameFlags::default())
        .expect("rename");
    assert_eq!(v.lookup(ROOT_INO, b"b").expect("lookup").ino, a.ino);
    assert_eq!(v.getattr(b.ino).expect("getattr").nlink, 0);
    assert_eq!(
        v.rename(
            ROOT_INO,
            b"b",
            ROOT_INO,
            b"c",
            RenameFlags { no_replace: true }
        ),
        Ok(())
    );
    v.create(ROOT_INO, b"d", 0o644).expect("create d");
    assert_eq!(
        v.rename(
            ROOT_INO,
            b"c",
            ROOT_INO,
            b"d",
            RenameFlags { no_replace: true }
        ),
        Err(Error::Exists)
    );
}

#[test]
fn many_referenced_files_do_not_exhaust_descriptors() {
    let (_s, v) = fs();
    let dir = v.mkdir(ROOT_INO, b"d", 0o755).expect("mkdir").ino;
    for i in 0..2000 {
        let n = format!("f{i}");
        v.create(dir, n.as_bytes(), 0o644).expect("create");
    }
    let a = v.lookup(dir, b"f0").expect("lookup");
    v.write(a.ino, 0, b"still works")
        .expect("write after eviction");
    assert_eq!(v.read(a.ino, 0, 20).expect("read"), b"still works");
}

#[test]
fn errno_mapping() {
    let e = |n| io_err(io::Error::from_raw_os_error(n));
    assert_eq!(e(libc::ENOENT), Error::NotFound);
    assert_eq!(e(libc::ENOTEMPTY), Error::NotEmpty);
    assert_eq!(e(Error::NoAttr.errno()), Error::NoAttr);
    assert_eq!(e(libc::EPERM), Error::PermissionDenied);
    assert!(matches!(e(libc::EBADF), Error::Io(_)));
}

#[test]
fn read_at_a_huge_offset_is_empty_not_an_error() {
    let (_s, v) = fs();
    let a = v.create(ROOT_INO, b"a", 0o644).expect("create");
    v.write(a.ino, 0, b"x").expect("write");
    assert!(v.read(a.ino, u64::MAX >> 1, 5).expect("read").is_empty());
    assert!(v.read(a.ino, u64::MAX, 5).expect("read").is_empty());
}
