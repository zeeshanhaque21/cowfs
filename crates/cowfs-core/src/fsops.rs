//! Test seams for the durability calls the design argument in `docs/v1-core.md` depends on.
//!
//! Two things, both inert unless a test arms them: a fault rule that makes one `sync` fail, and a
//! trace of the calls in the order they happened.

use std::fs::File;
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum Fault {
    FileSync,
    DirSync,
}

#[derive(Debug)]
struct Rule {
    what: Fault,
    path_has: String,
    times: u32,
}

#[derive(Debug, Default)]
struct State {
    rules: Vec<Rule>,
    trace: Vec<String>,
    armed: bool,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(Mutex::default)
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut s = state().lock().unwrap_or_else(|e| e.into_inner());
    f(&mut s)
}

/// Make the next `times` `sync`s of a file (or directory) whose path contains `path_has` fail.
#[doc(hidden)]
pub fn set_fault(what: Fault, path_has: &str, times: u32) {
    with(|s| {
        s.armed = true;
        s.rules.push(Rule {
            what,
            path_has: path_has.to_string(),
            times,
        });
    });
}

/// Turn the trace on and drop every rule and record.
#[doc(hidden)]
pub fn arm() {
    with(|s| {
        s.armed = true;
        s.rules.clear();
        s.trace.clear();
    });
}

#[doc(hidden)]
pub fn disarm() {
    with(|s| {
        s.armed = false;
        s.rules.clear();
        s.trace.clear();
    });
}

/// The recorded calls, oldest first.
#[doc(hidden)]
pub fn trace_take() -> Vec<String> {
    with(|s| std::mem::take(&mut s.trace))
}

pub(crate) fn note(what: &str) {
    with(|s| {
        if s.armed {
            s.trace.push(what.to_string());
        }
    });
}

fn take_fault(what: Fault, path: &Path) -> io::Result<()> {
    with(|s| {
        if !s.armed {
            return Ok(());
        }
        let name = path.to_string_lossy().into_owned();
        let Some(i) = s
            .rules
            .iter()
            .position(|r| r.what == what && name.contains(&r.path_has))
        else {
            return Ok(());
        };
        if s.rules[i].times == 0 {
            s.rules.remove(i);
            return Ok(());
        }
        s.rules[i].times -= 1;
        Err(io::Error::other(format!("injected sync fault on {name}")))
    })
}

/// Syncs `f` and records `sync_file:<name>`, so a test can assert the ordering around it.
pub(crate) fn sync_file(f: &File, path: &Path) -> io::Result<()> {
    note(&format!("sync_file:{}", file_name(path)));
    take_fault(Fault::FileSync, path)?;
    #[cfg(feature = "fault-injection")]
    rootlog::push(|| rootlog::RootOp::Sync(file_name(path)));
    f.sync_all()
}

/// Syncs the directory `path` and records `sync_dir:<path>`, so a test can name the exact one.
pub(crate) fn sync_dir(path: &Path) -> io::Result<()> {
    note(&format!("sync_dir:{}", path.display()));
    take_fault(Fault::DirSync, path)?;
    #[cfg(feature = "fault-injection")]
    rootlog::push(|| rootlog::RootOp::DirSync);
    File::open(path)?.sync_all()
}

/// Creates `path` and writes `data` to it, not yet durable. The caller syncs it.
pub(crate) fn create_with(path: &Path, data: &[u8]) -> io::Result<File> {
    let mut f = File::create(path)?;
    #[cfg(feature = "fault-injection")]
    {
        rootlog::push(|| rootlog::RootOp::Create(file_name(path)));
        rootlog::push(|| rootlog::RootOp::Write(file_name(path), data.to_vec()));
    }
    f.write_all(data)?;
    Ok(f)
}

/// `std::fs::rename`, in the order the power-loss model records.
pub(crate) fn rename(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(feature = "fault-injection")]
    rootlog::push(|| rootlog::RootOp::Rename(file_name(from), file_name(to)));
    std::fs::rename(from, to)
}

/// `std::fs::remove_file`, in the order the power-loss model records.
pub(crate) fn remove_file(path: &Path) -> io::Result<()> {
    #[cfg(feature = "fault-injection")]
    rootlog::push(|| rootlog::RootOp::Unlink(file_name(path)));
    std::fs::remove_file(path)
}

/// The mount root's durable writes (the swap intent file), recorded for the power-loss model of
/// issue 173. Only with `fault-injection`; nothing is compiled into a normal build.
///
/// Per thread, like the store's op log, and stamped into that log as markers so the two share one
/// order. An op is stamped before it runs, so a cut at its stamp may or may not have applied it.
#[cfg(feature = "fault-injection")]
#[doc(hidden)]
pub mod rootlog {
    use std::cell::RefCell;

    /// Marks a store-log marker as a root op; the op's index is in the low bits.
    pub const ROOT_MARK: u64 = 1 << 41;

    /// One durable-relevant call on a file in the mount root. Names are relative to the root.
    #[derive(Clone, Debug)]
    pub enum RootOp {
        /// A file was created (empty).
        Create(String),
        /// Bytes written at offset 0 of a freshly created file.
        Write(String, Vec<u8>),
        /// A file was fsynced.
        Sync(String),
        /// A file was renamed over `to`.
        Rename(String, String),
        /// A file was unlinked.
        Unlink(String),
        /// The root directory was fsynced.
        DirSync,
    }

    thread_local! {
        static LOG: RefCell<Option<Vec<RootOp>>> = const { RefCell::new(None) };
    }

    /// Start recording this thread's root ops.
    pub fn start() {
        LOG.with(|l| *l.borrow_mut() = Some(Vec::new()));
    }

    /// Stop recording and take the log.
    pub fn take() -> Vec<RootOp> {
        LOG.with(|l| l.borrow_mut().take().unwrap_or_default())
    }

    pub(crate) fn push(op: impl FnOnce() -> RootOp) {
        let idx = LOG.with(|l| {
            l.borrow_mut().as_mut().map(|v| {
                v.push(op());
                v.len() as u64 - 1
            })
        });
        if let Some(i) = idx {
            cowfs_store::oplog_marker(ROOT_MARK | i);
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}
