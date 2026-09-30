//! Every durability-relevant call goes through [`Io`] so tests can record and check their order.

use std::fs::File;
use std::io;
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
    }

    pub(crate) fn sync_file(&self, file: &File, path: &Path) -> io::Result<()> {
        self.log(|| Op::Sync(name(path)));
        if self.nosync {
            return Ok(());
        }
        file.sync_data()
    }

    pub(crate) fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.log(|| Op::DirSync(name(dir)));
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
        file.set_len(len)
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
