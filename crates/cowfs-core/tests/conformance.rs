//! The conformance suite from `cowfs-vfs-test`, run through a `Core` snapshot view.

use std::sync::Arc;

use cowfs_core::{Core, Options};
use cowfs_vfs::Vfs;
use cowfs_vfs_test::conformance::{run_all, Options as SuiteOptions};
use cowfs_vfs_test::conformance_tests;

fn opts() -> Options {
    Options::default()
}

fn factory() -> Arc<dyn Vfs> {
    // statfs_free_after_unlink needs unflushed data: freed space of stored blocks waits for GC (#10)
    let opts = Options {
        background: false,
        ..opts()
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let core = Core::open(dir.path(), opts).expect("open");
    core.create_snapshot("main").expect("snapshot");
    let view = core.snapshot_view("main").expect("view");
    Arc::new(Keep {
        view,
        _dir: dir,
        _core: core,
    })
}

/// Keeps the temporary directory alive for as long as the filesystem is used.
#[derive(Debug)]
struct Keep {
    view: cowfs_core::SnapshotView,
    _dir: tempfile::TempDir,
    _core: Core,
}

macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        impl Vfs for Keep {$(
            fn $name(&self, $($arg: $ty),*) -> $ret { self.view.$name($($arg),*) }
        )*}
    };
}

use cowfs_vfs::FallocMode;
use cowfs_vfs::{Attr, FileHandle, Ino, ReadDir, RenameFlags, Result, SetAttr, StatFs, XattrFlags};

forward! {
    lookup(parent: Ino, name: &[u8]) -> Result<Attr>;
    forget(ino: Ino, count: u64) -> ();
    getattr(ino: Ino) -> Result<Attr>;
    setattr(ino: Ino, changes: SetAttr) -> Result<Attr>;
    readlink(ino: Ino) -> Result<Vec<u8>>;
    create(parent: Ino, name: &[u8], mode: u32) -> Result<Attr>;
    mkdir(parent: Ino, name: &[u8], mode: u32) -> Result<Attr>;
    symlink(parent: Ino, name: &[u8], target: &[u8]) -> Result<Attr>;
    link(ino: Ino, new_parent: Ino, new_name: &[u8]) -> Result<Attr>;
    unlink(parent: Ino, name: &[u8]) -> Result<()>;
    rmdir(parent: Ino, name: &[u8]) -> Result<()>;
    rename(parent: Ino, name: &[u8], new_parent: Ino, new_name: &[u8], flags: RenameFlags) -> Result<()>;
    open(ino: Ino) -> Result<FileHandle>;
    release(handle: FileHandle) -> Result<()>;
    read(ino: Ino, offset: u64, size: u32) -> Result<Vec<u8>>;
    write(ino: Ino, offset: u64, data: &[u8]) -> Result<u32>;
    flush(ino: Ino) -> Result<()>;
    fsync(ino: Ino, data_only: bool) -> Result<()>;
    readdir(dir: Ino, cookie: u64, max: usize) -> Result<ReadDir>;
    statfs() -> Result<StatFs>;
    getxattr(ino: Ino, name: &[u8]) -> Result<Vec<u8>>;
    setxattr(ino: Ino, name: &[u8], value: &[u8], flags: XattrFlags) -> Result<()>;
    listxattr(ino: Ino) -> Result<Vec<Vec<u8>>>;
    removexattr(ino: Ino, name: &[u8]) -> Result<()>;
    fallocate(ino: Ino, mode: FallocMode, offset: u64, len: u64) -> Result<Attr>;
}

conformance_tests!(|| -> Arc<dyn Vfs> { factory() });

#[test]
fn run_all_prints_a_table() {
    let f = || factory();
    let report = run_all(&f, &SuiteOptions::default());
    println!("{}", report.table());
    assert!(report.passed(), "{}", report.table());
    // Core implements fallocate: the six checks must run and pass, never be skipped or missing.
    assert!(
        report
            .skipped
            .iter()
            .all(|k| !k.check.name.starts_with("fallocate")),
        "{}",
        report.table()
    );
    let ran = |n: &str| {
        report
            .results
            .iter()
            .any(|r| r.check.name == n && r.outcome.is_ok())
    };
    for n in [
        "fallocate_punch_reads_zeros_keeps_size",
        "fallocate_zero_range_modes",
        "fallocate_allocate_and_keep_size",
        "fallocate_errors",
        "fallocate_content_change_bumps_mtime_and_ctime",
        "fallocate_random_sequence_matches_model",
    ] {
        assert!(
            ran(n),
            "{n} did not run and pass on Core\n{}",
            report.table()
        );
    }
}
