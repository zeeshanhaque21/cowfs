//! The runner itself: levels, skips, timeouts, panic and hang isolation.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cowfs_vfs::{
    Attr, FileHandle, Ino, ReadDir, RenameFlags, Result, SetAttr, StatFs, Vfs, XattrFlags,
};
use cowfs_vfs_test::conformance::{all_checks, run_all, Level, Options};
use cowfs_vfs_test::{Fault, MemVfs};

fn mem() -> Arc<dyn Vfs> {
    Arc::new(MemVfs::new())
}

#[derive(Clone, Copy)]
enum Misbehave {
    Hang,
    Panic,
}

/// Delegates to `MemVfs` but `getattr` hangs or panics.
struct Bad(MemVfs, Misbehave);

impl Vfs for Bad {
    fn lookup(&self, p: Ino, n: &[u8]) -> Result<Attr> {
        self.0.lookup(p, n)
    }
    fn forget(&self, i: Ino, c: u64) {
        self.0.forget(i, c)
    }
    fn getattr(&self, i: Ino) -> Result<Attr> {
        match self.1 {
            Misbehave::Hang => std::thread::sleep(Duration::from_secs(30)),
            Misbehave::Panic => panic!("backend bug"),
        }
        self.0.getattr(i)
    }
    fn setattr(&self, i: Ino, c: SetAttr) -> Result<Attr> {
        self.0.setattr(i, c)
    }
    fn readlink(&self, i: Ino) -> Result<Vec<u8>> {
        self.0.readlink(i)
    }
    fn create(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
        self.0.create(p, n, m)
    }
    fn mkdir(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
        self.0.mkdir(p, n, m)
    }
    fn symlink(&self, p: Ino, n: &[u8], t: &[u8]) -> Result<Attr> {
        self.0.symlink(p, n, t)
    }
    fn link(&self, i: Ino, p: Ino, n: &[u8]) -> Result<Attr> {
        self.0.link(i, p, n)
    }
    fn unlink(&self, p: Ino, n: &[u8]) -> Result<()> {
        self.0.unlink(p, n)
    }
    fn rmdir(&self, p: Ino, n: &[u8]) -> Result<()> {
        self.0.rmdir(p, n)
    }
    fn rename(&self, p: Ino, n: &[u8], q: Ino, m: &[u8], f: RenameFlags) -> Result<()> {
        self.0.rename(p, n, q, m, f)
    }
    fn open(&self, i: Ino) -> Result<FileHandle> {
        self.0.open(i)
    }
    fn release(&self, h: FileHandle) -> Result<()> {
        self.0.release(h)
    }
    fn read(&self, i: Ino, o: u64, s: u32) -> Result<Vec<u8>> {
        self.0.read(i, o, s)
    }
    fn write(&self, i: Ino, o: u64, d: &[u8]) -> Result<u32> {
        self.0.write(i, o, d)
    }
    fn flush(&self, i: Ino) -> Result<()> {
        self.0.flush(i)
    }
    fn fsync(&self, i: Ino, d: bool) -> Result<()> {
        self.0.fsync(i, d)
    }
    fn readdir(&self, d: Ino, c: u64, m: usize) -> Result<ReadDir> {
        self.0.readdir(d, c, m)
    }
    fn statfs(&self) -> Result<StatFs> {
        self.0.statfs()
    }
    fn getxattr(&self, i: Ino, n: &[u8]) -> Result<Vec<u8>> {
        self.0.getxattr(i, n)
    }
    fn setxattr(&self, i: Ino, n: &[u8], v: &[u8], f: XattrFlags) -> Result<()> {
        self.0.setxattr(i, n, v, f)
    }
    fn listxattr(&self, i: Ino) -> Result<Vec<Vec<u8>>> {
        self.0.listxattr(i)
    }
    fn removexattr(&self, i: Ino, n: &[u8]) -> Result<()> {
        self.0.removexattr(i, n)
    }
}

fn opts(filter: &str) -> Options {
    Options {
        filter: Some(filter.into()),
        ..Options::default()
    }
}

#[test]
fn check_names_are_unique_and_levels_all_used() {
    let checks = all_checks();
    let mut names: Vec<_> = checks.iter().map(|c| c.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), checks.len());
    for l in [Level::Posix, Level::Portable, Level::Cowfs] {
        assert!(checks.iter().any(|c| c.level == l), "no {l} check");
    }
    assert!(Level::Posix < Level::Portable && Level::Portable < Level::Cowfs);
}

#[test]
fn level_selects_cumulatively_and_table_prints_it() {
    let r = run_all(
        &mem,
        &Options {
            level: Some(Level::Posix),
            ..opts("basic::")
        },
    );
    assert!(r.passed(), "{}", r.table());
    assert!(r.results.iter().all(|x| x.check.level == Level::Posix));
    assert!(r.above_level > 0);
    assert!(r.table().contains("posix"));
    let all = run_all(&mem, &opts("basic::"));
    assert!(all.results.len() > r.results.len());
    assert_eq!(all.above_level, 0);
    let portable = run_all(
        &mem,
        &Options {
            level: Some(Level::Portable),
            ..opts("basic::")
        },
    );
    assert!(portable
        .results
        .iter()
        .all(|x| x.check.level <= Level::Portable));
    assert!(portable.results.len() > r.results.len() && portable.results.len() < all.results.len());
}

#[test]
fn skips_are_reported_with_reason_and_counted() {
    let o = Options {
        skip: vec![("root_is_directory".into(), "because reasons".into())],
        ..opts("basic::")
    };
    let r = run_all(&mem, &o);
    assert!(r.passed());
    assert_eq!(r.skipped_count(), 1);
    assert!(r
        .results
        .iter()
        .all(|x| x.check.name != "root_is_directory"));
    let t = r.table();
    assert!(
        t.contains("SKIP") && t.contains("because reasons") && t.contains("1 skipped"),
        "{t}"
    );
}

#[test]
fn a_skip_of_a_failing_check_hides_only_that_check() {
    let broken = || -> Arc<dyn Vfs> { Arc::new(MemVfs::with_fault(Fault::ModeNotMasked)) };
    let bad = run_all(&broken, &opts("mask"));
    assert!(!bad.passed());
    let o = Options {
        skip: bad
            .failures()
            .iter()
            .map(|f| (f.check.name.to_string(), "known".to_string()))
            .collect(),
        ..opts("mask")
    };
    let r = run_all(&broken, &o);
    assert!(r.passed());
    assert_eq!(r.skipped_count(), bad.failures().len());
}

#[test]
fn a_skip_naming_no_check_is_an_error() {
    let o = Options {
        skip: vec![("no_such_check".into(), "typo".into())],
        ..opts("basic::root")
    };
    let r = run_all(&mem, &o);
    assert!(!r.passed());
    assert!(r.table().contains("no_such_check"));
}

#[test]
fn assert_ok_panics_on_failure_and_passes_with_skips() {
    let broken = || -> Arc<dyn Vfs> { Arc::new(MemVfs::with_fault(Fault::ModeNotMasked)) };
    let r = run_all(&broken, &opts("mask"));
    assert!(catch_unwind(AssertUnwindSafe(|| r.assert_ok())).is_err());
    let o = Options {
        skip: vec![("root_is_directory".into(), "why".into())],
        ..opts("basic::root")
    };
    run_all(&mem, &o).assert_ok();
}

#[test]
fn a_hang_is_reported_leaked_and_does_not_affect_later_checks() {
    let calls = AtomicUsize::new(0);
    let factory = || -> Arc<dyn Vfs> {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Arc::new(Bad(MemVfs::new(), Misbehave::Hang))
        } else {
            mem()
        }
    };
    let o = Options {
        timeout: Some(Duration::from_millis(500)),
        ..opts("basic::")
    };
    let r = run_all(&factory, &o);
    assert_eq!(r.leaked_count(), 1, "{}", r.table());
    assert_eq!(r.failures().len(), 1, "{}", r.table());
    assert!(r.table().contains("LEAKED THREAD"));
}

#[test]
fn a_panic_is_a_failure_and_does_not_affect_later_checks() {
    let calls = AtomicUsize::new(0);
    let factory = || -> Arc<dyn Vfs> {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Arc::new(Bad(MemVfs::new(), Misbehave::Panic))
        } else {
            mem()
        }
    };
    let r = run_all(&factory, &opts("basic::"));
    assert_eq!(r.failures().len(), 1, "{}", r.table());
    assert_eq!(r.leaked_count(), 0);
    assert!(r.failures()[0]
        .outcome
        .as_ref()
        .is_err_and(|f| f.0.contains("backend bug")));
}

#[test]
fn the_link_limit_check_is_skipped_without_a_declared_limit() {
    let r = run_all(&mem, &opts("hardlink_limit"));
    assert!(r.passed());
    assert!(
        r.results.is_empty(),
        "the check ran without a declared limit"
    );
    assert_eq!(r.skipped_count(), 1);
    assert!(r.table().contains("Options::link_limit"));

    let small = || -> Arc<dyn Vfs> { Arc::new(MemVfs::with_link_max(50)) };
    let o = Options {
        link_limit: Some(50),
        ..opts("hardlink_limit")
    };
    let r = run_all(&small, &o);
    assert!(r.passed(), "{}", r.table());
    assert_eq!(r.results.len(), 1);
    assert_eq!(r.skipped_count(), 0);
    // The default limit is far higher, so it still runs (slow), but the declared 50 is enforced.
    let r = run_all(&mem, &o);
    assert!(
        !r.passed(),
        "the default limit accepted more names than declared"
    );
}

#[test]
fn xattr_names_prefixed_is_opt_in() {
    let r = run_all(&mem, &opts("xattr_name_validation"));
    assert!(r.passed(), "{}", r.table());
    let o = Options {
        xattr_names: true,
        ..opts("xattr_name_validation")
    };
    assert!(
        run_all(&mem, &o).passed(),
        "prefixed name rejected by MemVfs"
    );
    // A prefixed name is not what a bare-name backend would use, so both settings must pass.
    assert!(run_all(
        &mem,
        &Options {
            xattr_names: false,
            ..o.clone()
        }
    )
    .passed());
}
