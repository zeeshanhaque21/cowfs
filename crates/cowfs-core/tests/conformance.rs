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

use cowfs_vfs::{
    Attr, FileHandle, FileKind, Ino, ReadDir, RenameFlags, Result, SetAttr, StatFs, XattrFlags,
};

forward! {
    lookup(parent: Ino, name: &[u8]) -> Result<Attr>;
    forget(ino: Ino, count: u64) -> ();
    getattr(ino: Ino) -> Result<Attr>;
    setattr(ino: Ino, changes: SetAttr) -> Result<Attr>;
    readlink(ino: Ino) -> Result<Vec<u8>>;
    create(parent: Ino, name: &[u8], mode: u32) -> Result<Attr>;
    mkdir(parent: Ino, name: &[u8], mode: u32) -> Result<Attr>;
    mknod(parent: Ino, name: &[u8], kind: FileKind, mode: u32, rdev: u64) -> Result<Attr>;
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
}

/// Checks Core cannot pass until #103 slice B; each is an ignored test with this reason.
const NO_FALLOCATE: &[&str] = &[
    "fallocate_punch_reads_zeros_keeps_size",
    "fallocate_zero_range_modes",
    "fallocate_allocate_and_keep_size",
    "fallocate_errors",
    "fallocate_content_change_bumps_mtime_and_ctime",
    "fallocate_random_sequence_matches_model",
];

conformance_tests!(
    || -> Arc<dyn Vfs> { factory() },
    skip = {
        fallocate_punch_reads_zeros_keeps_size => "fallocate not implemented in Core yet (#103 slice B)",
        fallocate_zero_range_modes => "fallocate not implemented in Core yet (#103 slice B)",
        fallocate_allocate_and_keep_size => "fallocate not implemented in Core yet (#103 slice B)",
        fallocate_errors => "fallocate not implemented in Core yet (#103 slice B)",
        fallocate_content_change_bumps_mtime_and_ctime => "fallocate not implemented in Core yet (#103 slice B)",
        fallocate_random_sequence_matches_model => "fallocate not implemented in Core yet (#103 slice B)"
    }
);

#[test]
fn run_all_prints_a_table() {
    let f = || factory();
    let opts = SuiteOptions {
        skip: NO_FALLOCATE
            .iter()
            .map(|n| (n.to_string(), "#103 slice B".to_string()))
            .collect(),
        ..SuiteOptions::default()
    };
    let report = run_all(&f, &opts);
    println!("{}", report.table());
    assert!(report.passed(), "{}", report.table());
}
