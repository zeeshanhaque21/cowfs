//! The ingest walk: what it copies, what it refuses, what it leaves behind, and what a repeat of
//! the same content costs.

mod common;

use std::fs;
use std::os::unix::fs::{symlink, MetadataExt};

use cowfs_core::{ingest, Core, Hooks, ImportError};
use cowfs_vfs::{FileKind, Vfs, ROOT_INO};

fn run(
    core: &Core,
    dir: &std::path::Path,
    name: &str,
) -> Result<cowfs_core::Ingested, ImportError> {
    let mut hooks = Hooks {
        progress: &mut |_, _| true,
    };
    ingest(core, dir, name, &mut hooks)
}

fn stop_after(
    core: &Core,
    dir: &std::path::Path,
    name: &str,
    limit: u64,
) -> Result<(), ImportError> {
    let mut hooks = Hooks {
        progress: &mut |done, _| done < limit,
    };
    ingest(core, dir, name, &mut hooks).map(|_| ())
}

#[test]
fn ingests_a_tree_of_text_binary_empty_and_utf8_names_at_depth() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(src.join("a/b/c")).unwrap();
    fs::write(src.join("a/b/c/deep.txt"), b"deep").unwrap();
    fs::write(src.join("a/binary"), common::pattern(300_000, 7)).unwrap();
    fs::write(src.join("a/empty"), b"").unwrap();
    fs::write(src.join("caf\u{e9} \u{1f600}.txt"), "utf-8 name".as_bytes()).unwrap();

    let got = run(&f.core, &src, "imported").unwrap();
    assert_eq!((got.files, got.bytes), (4, 300_014), "{got:?}");

    let view = f.core.snapshot_view("imported").unwrap();
    let deep = view
        .lookup(view.lookup(ROOT_INO, b"a").unwrap().ino, b"b")
        .unwrap();
    let c = view.lookup(deep.ino, b"c").unwrap();
    let file = view.lookup(c.ino, b"deep.txt").unwrap();
    assert_eq!(common::read_all(&view, file.ino), b"deep");
    let binary = view
        .lookup(view.lookup(ROOT_INO, b"a").unwrap().ino, b"binary")
        .unwrap();
    assert_eq!(
        common::read_all(&view, binary.ino),
        common::pattern(300_000, 7)
    );
    let empty = view
        .lookup(view.lookup(ROOT_INO, b"a").unwrap().ino, b"empty")
        .unwrap();
    assert_eq!(view.getattr(empty.ino).unwrap().size, 0);
    let utf8 = view
        .lookup(ROOT_INO, "caf\u{e9} \u{1f600}.txt".as_bytes())
        .unwrap();
    assert_eq!(common::read_all(&view, utf8.ino), b"utf-8 name");

    // The mode of every entry is the source's, not the default the core would pick.
    let src_mode = fs::symlink_metadata(src.join("a/binary")).unwrap().mode() & 0o7777;
    assert_eq!(view.getattr(binary.ino).unwrap().mode, src_mode);
}

#[test]
fn keeps_symlinks_as_symlinks_and_never_follows_them() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("real"), b"content").unwrap();
    symlink("real", src.join("link")).unwrap();
    symlink("/nowhere/at/all", src.join("dangling")).unwrap();
    symlink("..", src.join("up")).unwrap();

    run(&f.core, &src, "links").unwrap();
    let view = f.core.snapshot_view("links").unwrap();
    for (name, want) in [
        ("link", "real"),
        ("dangling", "/nowhere/at/all"),
        ("up", ".."),
    ] {
        let a = view.lookup(ROOT_INO, name.as_bytes()).unwrap();
        assert_eq!(a.kind, FileKind::Symlink, "{name}");
        assert_eq!(view.readlink(a.ino).unwrap(), want.as_bytes(), "{name}");
    }
}

#[test]
fn nothing_is_special_cased_so_git_comes_across_like_any_other_directory() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(src.join(".git/refs")).unwrap();
    fs::write(src.join(".git/HEAD"), b"ref: refs/heads/main\n").unwrap();
    fs::write(src.join("README"), b"hi").unwrap();

    run(&f.core, &src, "repo").unwrap();
    let view = f.core.snapshot_view("repo").unwrap();
    let git = view.lookup(ROOT_INO, b".git").unwrap();
    assert_eq!(git.kind, FileKind::Directory);
    assert!(
        view.lookup(git.ino, b"refs").is_ok(),
        "the subdirectory is there too"
    );
    let head = view.lookup(git.ino, b"HEAD").unwrap();
    assert_eq!(common::read_all(&view, head.ino), b"ref: refs/heads/main\n");
    assert!(view.lookup(ROOT_INO, b"README").is_ok());
}

#[test]
fn refuses_a_source_that_is_not_a_directory_or_is_missing() {
    let f = common::fixture();
    let file = f.dir.path().join("plain");
    fs::write(&file, b"x").unwrap();
    assert!(matches!(
        run(&f.core, &file, "s"),
        Err(ImportError::Invalid(_))
    ));
    assert!(run(&f.core, &f.dir.path().join("gone"), "s").is_err());
    assert!(f.core.list_snapshots().unwrap().is_empty());
}

/// A source with a fifo, a unix socket and (as root) a device node is imported as the same kinds of
/// node, with their modes and device number, and the read-back verification accepts them.
#[test]
fn imports_fifo_socket_and_device_nodes_as_the_same_kind() {
    use std::os::unix::net::UnixListener;
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("ok"), b"x").unwrap();
    let tool = |args: &[&str]| {
        std::process::Command::new(args[0])
            .args(&args[1..])
            .status()
            .expect("tool")
            .success()
    };
    let path = |n: &str| src.join(n).to_str().unwrap().to_string();
    assert!(
        tool(&["mkfifo", "-m", "640", &path("pipe")]),
        "the test needs a fifo"
    );
    let _sock = UnixListener::bind(src.join("sock")).unwrap();
    let dev = tool(&["mknod", &path("chr"), "c", "1", "3"]);
    let ingested = run(&f.core, &src, "withnodes").unwrap();
    assert_eq!(ingested.files, 1, "only the regular file counts as a file");

    let view = f.core.snapshot_view("withnodes").unwrap();
    let pipe = view.lookup(ROOT_INO, b"pipe").unwrap();
    assert_eq!(
        (pipe.kind, pipe.mode, pipe.nlink, pipe.size),
        (FileKind::Fifo, 0o640, 1, 0)
    );
    assert_eq!(
        view.lookup(ROOT_INO, b"sock").unwrap().kind,
        FileKind::Socket
    );
    if dev {
        let chr = view.lookup(ROOT_INO, b"chr").unwrap();
        assert_eq!(chr.kind, FileKind::CharDevice);
        assert_eq!(chr.rdev, cowfs_vfs::makedev(1, 3));
    } else {
        eprintln!("not root: the device node case did not run");
        assert!(view.lookup(ROOT_INO, b"chr").is_err());
    }
    f.core.check().unwrap();
}

#[test]
fn a_second_import_of_the_same_content_costs_no_new_bytes() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("sub/a"), common::pattern(200_000, 3)).unwrap();
    fs::write(src.join("b"), common::pattern(200_000, 3)).unwrap();

    let first = run(&f.core, &src, "one").unwrap();
    assert!(first.blocks > 0 && first.stored_bytes > 0, "{first:?}");
    let again = run(&f.core, &src, "two").unwrap();
    assert_eq!(again.blocks, 0, "every block was already stored: {again:?}");
    assert_eq!(again.stored_bytes, 0, "{again:?}");
    assert_eq!((again.files, again.bytes), (first.files, first.bytes));
    let names: Vec<String> = f
        .core
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["one", "two"]);
}

#[test]
fn a_name_that_exists_fails_rather_than_writing_a_second_copy() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a"), b"hello").unwrap();
    run(&f.core, &src, "taken").unwrap();
    let before = f.core.store().stats();
    let e = run(&f.core, &src, "taken").unwrap_err();
    assert!(
        matches!(e, ImportError::Core(cowfs_core::ControlError::Exists)),
        "{e:?}"
    );
    let after = f.core.store().stats();
    assert_eq!(before.blocks, after.blocks, "nothing was written");
    assert_eq!(f.core.list_snapshots().unwrap().len(), 1);
}

#[test]
fn cancelling_stops_the_ingest_and_leaves_no_snapshot() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    for n in 0..8 {
        fs::write(src.join(format!("f{n}")), common::pattern(400_000, n)).unwrap();
    }
    let e = stop_after(&f.core, &src, "cancelled", 100_000).unwrap_err();
    assert!(matches!(e, ImportError::Cancelled), "{e:?}");
    assert!(
        f.core.list_snapshots().unwrap().is_empty(),
        "the staging snapshot is not a snapshot a caller can see"
    );
    assert!(f.core.snapshot_view("cancelled").is_err());
    // The store is still usable and a fresh import of the same source works.
    let got = run(&f.core, &src, "after").unwrap();
    assert_eq!(got.files, 8);
}

#[test]
fn a_source_that_changes_under_the_import_is_a_mismatch_not_a_success() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a"), b"hello").unwrap();
    fs::write(src.join("b"), b"world").unwrap();
    // Change `a` after both files have been read, so the source is a different tree by the time the
    // imported snapshot is read back and compared with it.
    // The count of the source reports a total of 0 and is before any read, so it is skipped.
    let changed = std::cell::Cell::new(false);
    let mut hooks = Hooks {
        progress: &mut |done, total| {
            if total > 0 && done == total && !changed.replace(true) {
                fs::write(src.join("a"), b"hello, world").unwrap();
            }
            true
        },
    };
    let e = ingest(&f.core, &src, "raced", &mut hooks);
    assert!(e.is_err(), "the import must not report success: {e:?}");
    let ImportError::Mismatch { path, .. } = e.unwrap_err() else {
        panic!("expected a mismatch");
    };
    assert!(path.ends_with("a"), "{path}");
    assert!(f.core.list_snapshots().unwrap().is_empty());
}

#[test]
fn only_the_changed_file_costs_new_bytes_on_a_later_import() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    let shared = common::pattern(400_000, 11);
    fs::write(src.join("a"), &shared).unwrap();
    fs::write(src.join("b"), common::pattern(400_000, 12)).unwrap();

    let first = run(&f.core, &src, "one").unwrap();
    fs::write(src.join("b"), common::pattern(400_000, 13)).unwrap();
    let second = run(&f.core, &src, "two").unwrap();
    assert!(
        second.blocks < first.blocks / 2,
        "half the content was unchanged, so at most half the blocks are new: {first:?} {second:?}"
    );
    assert_eq!(second.files, first.files);
}

#[test]
fn the_source_directory_is_never_written_to() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(src.join("d")).unwrap();
    fs::write(src.join("d/a"), b"content").unwrap();
    fs::write(src.join("top"), b"more").unwrap();
    let before = list(&src);

    run(&f.core, &src, "readonly").unwrap();
    assert_eq!(list(&src), before);
}

fn list(dir: &std::path::Path) -> Vec<(String, u64, u32)> {
    let mut out: Vec<(String, u64, u32)> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            let md = fs::symlink_metadata(e.path()).unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                md.len(),
                md.mode(),
            )
        })
        .collect();
    out.sort();
    out
}

fn set_mtime(path: &std::path::Path, secs: u64, nanos: u32) {
    let t = std::time::UNIX_EPOCH + std::time::Duration::new(secs, nanos);
    fs::File::open(path).unwrap().set_modified(t).unwrap();
}

/// Issue 290: a build tool decides what is fresh from mtimes, so a copy that resets them looks
/// changed and is rebuilt.
#[test]
fn keeps_the_modification_time_of_every_node_to_the_nanosecond() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(src.join("d")).unwrap();
    fs::write(src.join("d/file"), b"content").unwrap();
    fs::write(src.join("empty"), b"").unwrap();
    symlink("d/file", src.join("link")).unwrap();
    // Set last: writing a child moves its parent's time.
    set_mtime(&src.join("d/file"), 1_600_000_000, 123_456_789);
    set_mtime(&src.join("empty"), 1_500_000_000, 1);
    set_mtime(&src.join("d"), 1_400_000_000, 999_999_999);
    set_mtime(&src, 1_300_000_000, 5);

    run(&f.core, &src, "times").unwrap();
    let view = f.core.snapshot_view("times").unwrap();
    let mtime = |ino| {
        let m = view.getattr(ino).unwrap().mtime;
        (m.secs, m.nanos)
    };
    let d = view.lookup(ROOT_INO, b"d").unwrap();
    let file = view.lookup(d.ino, b"file").unwrap();
    let empty = view.lookup(ROOT_INO, b"empty").unwrap();
    assert_eq!(mtime(file.ino), (1_600_000_000, 123_456_789));
    assert_eq!(mtime(empty.ino), (1_500_000_000, 1));
    assert_eq!(mtime(d.ino), (1_400_000_000, 999_999_999));
    assert_eq!(mtime(ROOT_INO), (1_300_000_000, 5));
    let link = view.lookup(ROOT_INO, b"link").unwrap();
    let want = fs::symlink_metadata(src.join("link")).unwrap();
    assert_eq!(mtime(link.ino), (want.mtime(), want.mtime_nsec() as u32));
}

#[test]
fn keeps_hard_links_as_one_file_counting_only_the_names_inside_the_tree() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(src.join("deps")).unwrap();
    fs::write(src.join("a"), b"shared bytes").unwrap();
    fs::hard_link(src.join("a"), src.join("deps/b")).unwrap();
    fs::hard_link(src.join("a"), src.join("deps/c")).unwrap();
    fs::write(src.join("other"), b"shared bytes").unwrap();
    // A second name outside the imported tree is not a name inside it.
    fs::write(src.join("kept"), b"kept").unwrap();
    fs::hard_link(src.join("kept"), f.dir.path().join("outside")).unwrap();

    let got = run(&f.core, &src, "links").unwrap();
    assert_eq!((got.files, got.bytes), (5, 12 * 4 + 4), "{got:?}");

    let view = f.core.snapshot_view("links").unwrap();
    let deps = view.lookup(ROOT_INO, b"deps").unwrap();
    let a = view.lookup(ROOT_INO, b"a").unwrap();
    let b = view.lookup(deps.ino, b"b").unwrap();
    let c = view.lookup(deps.ino, b"c").unwrap();
    assert_eq!((a.ino, a.nlink), (b.ino, 3), "{a:?} {b:?}");
    assert_eq!(c.ino, a.ino);
    assert_eq!(common::read_all(&view, c.ino), b"shared bytes");
    let other = view.lookup(ROOT_INO, b"other").unwrap();
    assert_ne!(other.ino, a.ino, "equal content is not the same file");
    assert_eq!(other.nlink, 1);
    let kept = view.lookup(ROOT_INO, b"kept").unwrap();
    assert_eq!(kept.nlink, 1, "{kept:?}");
}

/// A caller that gives up on silence needs the read-back and the count of the source to report
/// too, not only the write (issue 290).
#[test]
fn reports_progress_while_it_counts_the_source_and_while_it_reads_back() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..10 {
        fs::write(src.join(format!("f{i}")), b"").unwrap();
    }
    let mut calls = 0u32;
    let mut hooks = Hooks {
        progress: &mut |_, _| {
            calls += 1;
            true
        },
    };
    ingest(&f.core, &src, "quiet", &mut hooks).unwrap();
    // Empty files have no bytes to report, so each phase reports once per entry: count, write,
    // read back.
    assert!(calls >= 30, "{calls} calls");
}

/// A symlink and a fifo can have two names too. The core accepts `link` on both, so the import
/// keeps each as one inode and does not fail.
#[test]
fn a_hard_linked_symlink_and_fifo_import_instead_of_failing() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("target"), b"t").unwrap();
    symlink("target", src.join("s1")).unwrap();
    let fifo = src.join("p1");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    // `-P` links the symlink itself on both Linux and macOS (macOS follows it by default).
    let linked = |from: &std::path::Path, to: &std::path::Path| {
        std::process::Command::new("ln")
            .arg("-P")
            .arg(from)
            .arg(to)
            .status()
            .unwrap()
            .success()
    };
    assert!(
        linked(&src.join("s1"), &src.join("s2")),
        "the test needs a symlink link"
    );
    assert!(linked(&fifo, &src.join("p2")), "the test needs a fifo link");

    run(&f.core, &src, "links").unwrap();
    let view = f.core.snapshot_view("links").unwrap();
    let (p1, p2) = (
        view.lookup(ROOT_INO, b"p1").unwrap(),
        view.lookup(ROOT_INO, b"p2").unwrap(),
    );
    assert_eq!((p1.kind, p2.kind), (FileKind::Fifo, FileKind::Fifo));
    assert_eq!((p1.ino, p1.nlink), (p2.ino, 2), "the core links a fifo");
    let s1 = view.lookup(ROOT_INO, b"s1").unwrap();
    let s2 = view.lookup(ROOT_INO, b"s2").unwrap();
    assert_eq!((s1.kind, s2.kind), (FileKind::Symlink, FileKind::Symlink));
    assert_eq!((s1.ino, s1.nlink), (s2.ino, 2), "the core links a symlink");
    assert_eq!(view.readlink(s1.ino).unwrap(), b"target");
    assert_eq!(view.readlink(s2.ino).unwrap(), b"target");
    f.core.check().unwrap();
}

/// A source file that gains a name while it is being imported cannot be told from a lost link, so
/// the read-back must say so (issue 290). The new name is made after the write and before the
/// read-back.
#[test]
fn a_hard_link_made_during_the_import_is_a_mismatch() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a"), b"hello").unwrap();
    fs::write(src.join("b"), b"world").unwrap();
    let done = std::cell::Cell::new(false);
    let mut hooks = Hooks {
        progress: &mut |d, total| {
            if total > 0 && d == total && !done.replace(true) {
                fs::hard_link(src.join("a"), src.join("a2")).unwrap();
            }
            true
        },
    };
    let e = ingest(&f.core, &src, "raced", &mut hooks).unwrap_err();
    assert!(matches!(e, ImportError::Mismatch { .. }), "{e:?}");
    assert!(f.core.list_snapshots().unwrap().is_empty());
}

/// Two names of one source file must be one imported file, and a name of a link count that does not
/// match must be reported. Here a file that was one of two names loses the other one mid-import.
#[test]
fn a_link_count_that_changes_during_the_import_is_a_mismatch() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a"), b"hello").unwrap();
    fs::hard_link(src.join("a"), src.join("b")).unwrap();
    let done = std::cell::Cell::new(false);
    let mut hooks = Hooks {
        progress: &mut |d, total| {
            if total > 0 && d == total && !done.replace(true) {
                fs::remove_file(src.join("b")).unwrap();
            }
            true
        },
    };
    let e = ingest(&f.core, &src, "raced", &mut hooks).unwrap_err();
    assert!(matches!(e, ImportError::Mismatch { .. }), "{e:?}");
    assert!(f.core.list_snapshots().unwrap().is_empty());
}

/// Replacing a file with another file of the same name and size but a different inode, after the
/// write, is a different source file in the same place: the identity check sees two source files
/// where the import wrote one.
#[test]
fn two_source_files_never_share_one_imported_inode() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a"), b"hello").unwrap();
    fs::hard_link(src.join("a"), src.join("b")).unwrap();
    let done = std::cell::Cell::new(false);
    let mut hooks = Hooks {
        progress: &mut |d, total| {
            if total > 0 && d == total && !done.replace(true) {
                // `b` becomes a separate file with the same bytes and mtime.
                let m = fs::metadata(src.join("b")).unwrap();
                fs::remove_file(src.join("b")).unwrap();
                fs::write(src.join("b"), b"hello").unwrap();
                fs::File::open(src.join("b"))
                    .unwrap()
                    .set_modified(m.modified().unwrap())
                    .unwrap();
            }
            true
        },
    };
    let e = ingest(&f.core, &src, "raced", &mut hooks).unwrap_err();
    assert!(matches!(e, ImportError::Mismatch { .. }), "{e:?}");
    assert!(f.core.list_snapshots().unwrap().is_empty());
}

/// Touching a source file during the import without changing its bytes now fails the read-back,
/// because the imported mtime is no longer the source's.
#[test]
fn a_source_touched_during_the_import_is_a_mismatch() {
    let f = common::fixture();
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("a"), b"hello").unwrap();
    set_mtime(&src.join("a"), 1_600_000_000, 1);
    let done = std::cell::Cell::new(false);
    let mut hooks = Hooks {
        progress: &mut |d, total| {
            if total > 0 && d == total && !done.replace(true) {
                set_mtime(&src.join("a"), 1_700_000_000, 2);
            }
            true
        },
    };
    let e = ingest(&f.core, &src, "raced", &mut hooks).unwrap_err();
    let ImportError::Mismatch { reason, .. } = e else {
        panic!("expected a mismatch")
    };
    assert!(reason.contains("mtime"), "{reason}");
}
