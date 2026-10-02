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
fn refuses_a_source_that_is_not_a_directory_and_an_entry_the_core_cannot_hold() {
    let f = common::fixture();
    let file = f.dir.path().join("plain");
    fs::write(&file, b"x").unwrap();
    assert!(matches!(
        run(&f.core, &file, "s"),
        Err(ImportError::Invalid(_))
    ));
    assert!(run(&f.core, &f.dir.path().join("gone"), "s").is_err());
    assert!(f.core.list_snapshots().unwrap().is_empty());

    // A fifo: nothing in a cowfs snapshot can be one, so the import refuses rather than skip it.
    let src = f.dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("ok"), b"x").unwrap();
    let fifo = src.join("pipe");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo")
            .success(),
        "the test needs a fifo"
    );
    let e = run(&f.core, &src, "withfifo").unwrap_err();
    let ImportError::Invalid(msg) = e else {
        panic!("{e:?}");
    };
    assert!(msg.contains("pipe"), "{msg}");
    assert!(
        f.core.list_snapshots().unwrap().is_empty(),
        "a refused import leaves no snapshot"
    );
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
    let seen = std::cell::Cell::new(0u32);
    let mut hooks = Hooks {
        progress: &mut |_, _| {
            if seen.get() == 1 {
                fs::write(src.join("a"), b"hello, world").unwrap();
            }
            seen.set(seen.get() + 1);
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
