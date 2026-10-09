//! Every durability-relevant call goes through [`Io`], so tests can record the order of writes
//! and fsyncs, and a crash model can rebuild the disk image from them.

#[cfg(feature = "fault-injection")]
use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

/// One recorded durability operation. Names are file names relative to the store directory.
#[derive(Clone, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum Op {
    /// A file or directory was created.
    Create(String),
    /// A file was fsynced.
    Sync(String),
    /// A directory was fsynced.
    DirSync(String),
    /// A file was renamed.
    Rename(String, String),
    /// A file was truncated.
    Truncate(String),
}

/// A data write, a truncation or a whole-file write, for the crash model.
#[derive(Clone, Debug)]
#[doc(hidden)]
#[allow(dead_code)]
pub enum LogOp {
    /// A positional write that is not yet durable.
    Write {
        /// File name.
        file: String,
        /// Offset.
        off: u64,
        /// Bytes.
        data: Vec<u8>,
    },
    /// A file was shortened or extended, not yet durable.
    SetLen {
        /// File name.
        file: String,
        /// New length.
        len: u64,
    },
    /// A whole file was written and fsynced under a temporary name.
    Whole {
        /// Final file name.
        file: String,
        /// Contents.
        data: Vec<u8>,
    },
    /// A file was fsynced.
    Sync {
        /// File name.
        file: String,
    },
    /// A file was created.
    Create {
        /// File name.
        file: String,
    },
    /// A file was unlinked. Durable only once a later [`LogOp::DirSync`] of its directory ran.
    Unlink {
        /// File name.
        file: String,
    },
    /// A directory was fsynced. `dir` is its name, `packs` for the pack directory.
    DirSync {
        /// Directory name.
        dir: String,
    },
    /// A marker the test placed in the log.
    Marker(u64),
}

// The crash-model log only exists with `fault-injection`, so a normal build carries no log, no
// recording entry points and no per-write allocation. Ops are built lazily, only when a log is on.
#[cfg(feature = "fault-injection")]
thread_local! {
    /// Per thread, so tests that run beside each other cannot log into each other's model.
    static LOG: RefCell<Option<Vec<LogOp>>> = const { RefCell::new(None) };
}

/// Start recording data writes and fsyncs into this thread's log. For the crash model.
#[cfg(feature = "fault-injection")]
#[doc(hidden)]
pub fn oplog_start() {
    LOG.with(|l| *l.borrow_mut() = Some(Vec::new()));
}

/// Stop recording and take the log.
#[cfg(feature = "fault-injection")]
#[doc(hidden)]
pub fn oplog_take() -> Vec<LogOp> {
    LOG.with(|l| l.borrow_mut().take().unwrap_or_default())
}

/// Put a marker in the log, so a test can tell where a sync was called.
#[cfg(feature = "fault-injection")]
#[doc(hidden)]
pub fn oplog_marker(v: u64) {
    mark(v);
}

/// The store's own marker call; a no-op without `fault-injection`.
pub(crate) fn mark(v: u64) {
    log_data(|| LogOp::Marker(v));
}

/// Record `op()` if this thread is logging. `op` is not run otherwise, so nothing is allocated.
#[cfg(feature = "fault-injection")]
fn log_data(op: impl FnOnce() -> LogOp) {
    LOG.with(|l| {
        if let Some(v) = &mut *l.borrow_mut() {
            v.push(op());
        }
    });
}

#[cfg(not(feature = "fault-injection"))]
fn log_data(_op: impl FnOnce() -> LogOp) {}

/// Shared log of [`Op`]s, filled while a store built with `Store::open_traced` runs.
#[doc(hidden)]
pub type Trace = Arc<Mutex<Vec<Op>>>;

#[derive(Clone, Debug, Default)]
pub(crate) struct Io {
    trace: Option<Trace>,
    nosync: bool,
}

fn name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}

impl Io {
    pub(crate) fn new(trace: Option<Trace>, nosync: bool) -> Self {
        Self { trace, nosync }
    }

    fn log(&self, op: impl FnOnce() -> Op) {
        if let Some(t) = &self.trace {
            t.lock().unwrap_or_else(PoisonError::into_inner).push(op());
        }
    }

    pub(crate) fn created(&self, path: &Path) {
        self.log(|| Op::Create(name(path)));
        log_data(|| LogOp::Create { file: name(path) });
    }

    /// Write at an offset and log it for the crash model.
    pub(crate) fn write_at(
        &self,
        file: &File,
        path: &Path,
        off: u64,
        buf: &[u8],
    ) -> io::Result<()> {
        log_data(|| LogOp::Write {
            file: name(path),
            off,
            data: buf.to_vec(),
        });
        file.write_all_at(buf, off)?;
        #[cfg(feature = "fault-injection")]
        fault_boundary("write");
        Ok(())
    }

    pub(crate) fn sync_file(&self, file: &File, path: &Path) -> io::Result<()> {
        self.log(|| Op::Sync(name(path)));
        log_data(|| LogOp::Sync { file: name(path) });
        if self.nosync {
            return Ok(());
        }
        file.sync_data()?;
        #[cfg(feature = "fault-injection")]
        {
            fault_boundary("sync");
            if std::env::var("C7D_EXIT_FILE").ok().as_deref() == Some(name(path).as_str())
                && std::env::var("C7D_EXIT_LEN")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    == Some(file.metadata()?.len())
            {
                std::process::exit(77);
            }
        }
        Ok(())
    }

    pub(crate) fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.log(|| Op::DirSync(name(dir)));
        log_data(|| LogOp::DirSync { dir: name(dir) });
        let d = File::open(dir)?;
        if self.nosync {
            return Ok(());
        }
        d.sync_all()?;
        #[cfg(feature = "fault-injection")]
        fault_boundary("sync");
        Ok(())
    }

    /// Unlink a file, logged for the crash model. A `NotFound` is returned without a log entry.
    pub(crate) fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)?;
        log_data(|| LogOp::Unlink { file: name(path) });
        #[cfg(feature = "fault-injection")]
        fault_boundary("unlink");
        Ok(())
    }

    pub(crate) fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.log(|| Op::Rename(name(from), name(to)));
        std::fs::rename(from, to)?;
        #[cfg(feature = "fault-injection")]
        fault_boundary("rename");
        Ok(())
    }

    pub(crate) fn truncate(&self, file: &File, path: &Path, len: u64) -> io::Result<()> {
        self.log(|| Op::Truncate(name(path)));
        log_data(|| LogOp::SetLen {
            file: name(path),
            len,
        });
        file.set_len(len)?;
        #[cfg(feature = "fault-injection")]
        fault_boundary("truncate");
        Ok(())
    }

    /// Write a whole file, fsync it, rename it into place and fsync the directory.
    pub(crate) fn write_whole(&self, dir: &Path, file_name: &str, data: &[u8]) -> io::Result<()> {
        let tmp = dir.join(format!("{file_name}.tmp"));
        let dst = dir.join(file_name);
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        self.created(&tmp);
        f.write_all(data)?;
        #[cfg(feature = "fault-injection")]
        fault_boundary("write");
        self.sync_file(&f, &tmp)?;
        self.rename(&tmp, &dst)?;
        self.sync_dir(dir)
    }

    /// Log a whole-file write for the crash model, after the real one.
    pub(crate) fn log_whole(&self, dir: &Path, file_name: &str, data: &[u8]) {
        let _ = dir;
        log_data(|| LogOp::Whole {
            file: file_name.to_owned(),
            data: data.to_vec(),
        });
    }

    /// Create `dir` and any missing parents, fsyncing each new directory and its parent.
    pub(crate) fn create_dir_durable(&self, dir: &Path) -> io::Result<()> {
        if dir.is_dir() {
            return Ok(());
        }
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.create_dir_durable(parent)?;
        }
        match std::fs::create_dir(dir) {
            Ok(()) => self.created(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(e) => return Err(e),
        }
        self.sync_dir(dir)?;
        let parent = dir.parent().filter(|p| !p.as_os_str().is_empty());
        self.sync_dir(parent.unwrap_or_else(|| Path::new(".")))
    }
}

#[cfg(feature = "fault-injection")]
fn fault_boundary(kind: &str) {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static ALL: AtomicU64 = AtomicU64::new(0);
    static SYNCS: AtomicU64 = AtomicU64::new(0);
    let n = ALL.fetch_add(1, Relaxed) + 1;
    let target = |key| std::env::var(key).ok().and_then(|v| v.parse::<u64>().ok());
    let sync = kind == "sync" && target("C7D_EXIT_SYNC_N") == Some(SYNCS.fetch_add(1, Relaxed) + 1);
    if target("C7D_EXIT_BOUNDARY_N") == Some(n) || sync {
        std::process::exit(77);
    }
}

#[cfg(test)]
// A counting allocator cannot be written without `unsafe`; this test module is the one place.
#[allow(unsafe_code)]
mod alloc_tests {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    struct Counting;
    thread_local! {
        static BYTES: Cell<usize> = const { Cell::new(0) };
        static ALLOCS: Cell<usize> = const { Cell::new(0) };
    }
    // SAFETY: forwards to `System`; the counters are const-initialised thread locals, so counting
    // never allocates.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let _ = BYTES.try_with(|b| b.set(b.get() + l.size()));
            let _ = ALLOCS.try_with(|a| a.set(a.get() + 1));
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
    }
    #[global_allocator]
    static A: Counting = Counting;

    /// Issue 247: with no crash-model log active, a write must not copy its buffer or build a name.
    #[test]
    fn write_at_allocates_nothing_without_a_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pack-00000001.cpk");
        let file = File::create(&path).unwrap();
        let io = Io::new(None, true);
        let buf = vec![7u8; 1 << 20];
        io.write_at(&file, &path, 0, &buf).unwrap(); // warm up
        let (b0, a0) = (BYTES.get(), ALLOCS.get());
        for i in 0..64 {
            io.write_at(&file, &path, i * buf.len() as u64, &buf)
                .unwrap();
        }
        let (bytes, allocs) = (BYTES.get() - b0, ALLOCS.get() - a0);
        assert_eq!(
            (bytes, allocs),
            (0, 0),
            "write_at allocated on the no-log path"
        );
    }
}
