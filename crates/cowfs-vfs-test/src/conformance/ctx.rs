use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cowfs_vfs::{Attr, DirEntry, Error, Ino, Result, Vfs};

/// Why a check failed. Checks return this instead of panicking so callers can print a table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure(pub String);

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Failure(format!("unexpected error: {e} ({e:?})"))
    }
}

pub type Outcome = std::result::Result<(), Failure>;

/// A fresh empty filesystem plus helpers. Tracks the references handed out by `lookup`,
/// `create`, `mkdir`, `symlink` and `link` so `forget_all` can drop exactly that many.
pub struct Ctx {
    pub fs: Arc<dyn Vfs>,
    /// How the backend wants the xattr name-prefixed cases of `xattr_name_validation` run
    /// (from `Options::xattr_names`).
    pub xattr_names_prefixed: bool,
    /// The backend's declared hardlink limit, when it is small enough to reach cheaply
    /// (from `Options::link_limit`).
    pub link_limit: Option<u32>,
    refs: Mutex<HashMap<Ino, u64>>,
}

impl fmt::Debug for Ctx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ctx")
    }
}

/// Deterministic, hard-to-compress test bytes.
pub fn pattern(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8 | 1
        })
        .collect()
}

const IO_CHUNK: usize = 1 << 20;
const LIST_CAP: usize = 5_000_000;

impl Ctx {
    pub fn new(fs: Arc<dyn Vfs>) -> Self {
        Self {
            fs,
            xattr_names_prefixed: false,
            link_limit: None,
            refs: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_options(fs: Arc<dyn Vfs>, opts: &super::Options) -> Self {
        Self {
            xattr_names_prefixed: opts.xattr_names,
            link_limit: opts.link_limit,
            ..Self::new(fs)
        }
    }

    fn note(&self, r: Result<Attr>) -> Result<Attr> {
        if let Ok(a) = &r {
            let mut m = self.refs.lock().unwrap_or_else(|p| p.into_inner());
            *m.entry(a.ino).or_insert(0) += 1;
        }
        r
    }

    pub fn lookup(&self, parent: Ino, name: &[u8]) -> Result<Attr> {
        self.note(self.fs.lookup(parent, name))
    }

    pub fn create(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.note(self.fs.create(parent, name, mode))
    }

    pub fn mkdir(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.note(self.fs.mkdir(parent, name, mode))
    }

    pub fn mknod(
        &self,
        parent: Ino,
        name: &[u8],
        kind: cowfs_vfs::FileKind,
        mode: u32,
        rdev: u64,
    ) -> Result<Attr> {
        self.note(self.fs.mknod(parent, name, kind, mode, rdev))
    }

    pub fn symlink(&self, parent: Ino, name: &[u8], target: &[u8]) -> Result<Attr> {
        self.note(self.fs.symlink(parent, name, target))
    }

    pub fn link(&self, ino: Ino, parent: Ino, name: &[u8]) -> Result<Attr> {
        self.note(self.fs.link(ino, parent, name))
    }

    /// Creates a regular file with mode 0644 and returns its inode.
    pub fn file(&self, parent: Ino, name: &str) -> Result<Ino> {
        Ok(self.create(parent, name.as_bytes(), 0o644)?.ino)
    }

    /// Creates a directory with mode 0755 and returns its inode.
    pub fn dir(&self, parent: Ino, name: &str) -> Result<Ino> {
        Ok(self.mkdir(parent, name.as_bytes(), 0o755)?.ino)
    }

    /// Drops every reference this context handed out for `ino`.
    pub fn forget_all(&self, ino: Ino) {
        let n = self
            .refs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&ino)
            .unwrap_or(0);
        if n > 0 {
            self.fs.forget(ino, n);
        }
    }

    /// Writes all of `data` at `offset` in 1 MiB calls, requiring full writes.
    pub fn write_all(&self, ino: Ino, offset: u64, data: &[u8]) -> Outcome {
        let mut off = offset;
        for chunk in data.chunks(IO_CHUNK) {
            let n = self.fs.write(ino, off, chunk)?;
            ensure_eq!(n as usize, chunk.len(), "short write at offset {off}");
            off += chunk.len() as u64;
        }
        Ok(())
    }

    /// Reads `len` bytes at `offset` in 1 MiB calls, stopping early only at end of file.
    pub fn read_all(
        &self,
        ino: Ino,
        offset: u64,
        len: usize,
    ) -> std::result::Result<Vec<u8>, Failure> {
        let mut out = Vec::with_capacity(len.min(IO_CHUNK * 8));
        while out.len() < len {
            let want = (len - out.len()).min(IO_CHUNK);
            let got = self.fs.read(ino, offset + out.len() as u64, want as u32)?;
            if got.is_empty() {
                break;
            }
            ensure!(
                got.len() <= want,
                "read returned {} bytes for a request of {want}",
                got.len()
            );
            out.extend_from_slice(&got);
        }
        Ok(out)
    }

    /// Whole file content.
    pub fn content(&self, ino: Ino) -> std::result::Result<Vec<u8>, Failure> {
        let size = self.fs.getattr(ino)?.size;
        self.read_all(ino, 0, size as usize)
    }

    /// Lists a directory with pages of `page` entries, following the cookies.
    pub fn list_paged(&self, dir: Ino, page: usize) -> std::result::Result<Vec<DirEntry>, Failure> {
        let mut out: Vec<DirEntry> = Vec::new();
        let mut cookie = 0;
        let mut resumed_from = std::collections::HashSet::new();
        loop {
            let r = self.fs.readdir(dir, cookie, page)?;
            ensure!(
                r.entries.len() <= page,
                "readdir returned {} entries for max {page}",
                r.entries.len()
            );
            if let Some(last) = r.entries.last() {
                cookie = last.cookie;
                ensure!(
                    resumed_from.insert(cookie),
                    "cookie {cookie} ended two pages: the listing went round in a loop"
                );
            } else {
                ensure!(r.eof, "readdir returned no entries but eof is false");
            }
            out.extend(r.entries);
            ensure!(out.len() <= LIST_CAP, "listing does not terminate");
            if r.eof {
                return Ok(out);
            }
        }
    }

    pub fn list(&self, dir: Ino) -> std::result::Result<Vec<DirEntry>, Failure> {
        self.list_paged(dir, 1000)
    }

    /// Names in listing order.
    pub fn names(&self, dir: Ino) -> std::result::Result<Vec<Vec<u8>>, Failure> {
        Ok(self.list(dir)?.into_iter().map(|e| e.name).collect())
    }

    /// Lets the clock advance so a later timestamp is strictly greater.
    pub fn tick(&self) {
        std::thread::sleep(Duration::from_millis(5));
    }
}
