//! Every durability-relevant call goes through [`Io`], so tests can record the order of writes
//! and fsyncs, and a crash model can rebuild the disk image from them.

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
    /// A directory was fsynced.
    DirSync,
    /// A marker the test placed in the log.
    Marker(u64),
}

thread_local! {
    /// Per thread, so tests that run beside each other cannot log into each other's model.
    static LOG: RefCell<Option<Vec<LogOp>>> = const { RefCell::new(None) };
}

/// Start recording data writes and fsyncs into this thread's log. For the crash model.
#[doc(hidden)]
pub fn oplog_start() {
    LOG.with(|l| *l.borrow_mut() = Some(Vec::new()));
}

/// Stop recording and take the log.
#[doc(hidden)]
pub fn oplog_take() -> Vec<LogOp> {
    LOG.with(|l| l.borrow_mut().take().unwrap_or_default())
}

/// Put a marker in the log, so a test can tell where a sync was called.
#[doc(hidden)]
pub fn oplog_marker(v: u64) {
    log_data(LogOp::Marker(v));
}

fn log_data(op: LogOp) {
    LOG.with(|l| {
        if let Some(v) = &mut *l.borrow_mut() {
            v.push(op);
        }
    });
}

// The log itself lives in the thread local above.

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
        log_data(LogOp::Create { file: name(path) });
    }

    /// Write at an offset and log it for the crash model.
    pub(crate) fn write_at(
        &self,
        file: &File,
        path: &Path,
        off: u64,
        buf: &[u8],
    ) -> io::Result<()> {
        log_data(LogOp::Write {
            file: name(path),
            off,
            data: buf.to_vec(),
        });
        file.write_all_at(buf, off)
    }

    pub(crate) fn sync_file(&self, file: &File, path: &Path) -> io::Result<()> {
        self.log(|| Op::Sync(name(path)));
        log_data(LogOp::Sync { file: name(path) });
        if self.nosync {
            return Ok(());
        }
        file.sync_data()
    }

    pub(crate) fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.log(|| Op::DirSync(name(dir)));
        log_data(LogOp::DirSync);
        let d = File::open(dir)?;
        if self.nosync {
            return Ok(());
        }
        d.sync_all()
    }

    pub(crate) fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.log(|| Op::Rename(name(from), name(to)));
        std::fs::rename(from, to)
    }

    pub(crate) fn truncate(&self, file: &File, path: &Path, len: u64) -> io::Result<()> {
        self.log(|| Op::Truncate(name(path)));
        log_data(LogOp::SetLen {
            file: name(path),
            len,
        });
        file.set_len(len)
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
        self.sync_file(&f, &tmp)?;
        self.rename(&tmp, &dst)?;
        self.sync_dir(dir)
    }

    /// Log a whole-file write for the crash model, after the real one.
    pub(crate) fn log_whole(&self, dir: &Path, file_name: &str, data: &[u8]) {
        let _ = dir;
        log_data(LogOp::Whole {
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
