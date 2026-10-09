//! Shared fixtures for the mount tests: a `MemVfs` wrapper with fault injection and counters,
//! and a mounted fixture with a watchdog. Uses only API that every version of the crate has.
#![allow(dead_code)]

use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use cowfs_fuse::{Mount, MountOptions};
use cowfs_vfs::Error;
use cowfs_vfs::*;
use cowfs_vfs_test::MemVfs;
use tempfile::TempDir;

/// `MemVfs` plus knobs (delays, panics, bad replies, inode reuse) and call counters.
#[derive(Default)]
pub struct Probe {
    pub inner: MemVfs,
    pub read_delay_ms: AtomicU64,
    pub panic_read: AtomicBool,
    pub bad_attrs: AtomicBool,
    pub panics: AtomicU64,
    pub reuse_ino_for_create: Mutex<Option<Ino>>,
    /// `open` fails, to prove a failed create leaves no file behind.
    pub open_fails: AtomicBool,
    /// While greater than zero, every `read` blocks for this long before giving up, so a lane
    /// stays wedged until the test sets it back to zero.
    pub wedge_read_ms: AtomicU64,
    /// Return a short page, then an empty one, without ever setting eof.
    pub empty_no_eof: AtomicBool,
    /// Number cookies from 0, which the trait reserves for "from the start".
    pub zero_cookies: AtomicBool,
    /// `rename` sleeps this long, so overlapping calls are visible.
    pub rename_delay_ms: AtomicU64,
    /// Renames in flight in the Vfs right now.
    pub renames_now: AtomicI64,
    /// Highest number of renames seen in flight at once.
    pub renames_max: AtomicI64,
    /// Every `rename` the Vfs was asked to do.
    pub renames: AtomicU64,
    /// Parent pairs of the renames in flight, so overlap on a shared directory is visible.
    pub renaming: Mutex<Vec<(Ino, Ino)>>,
    /// Renames in flight that share a directory with another in-flight rename.
    pub renames_shared: AtomicI64,
    pub renames_shared_max: AtomicI64,
    pub refs: AtomicI64,
    pub forgotten: AtomicU64,
    pub opens: AtomicU64,
    pub releases: AtomicU64,
    pub flushes: AtomicU64,
    pub fsyncs: AtomicU64,
    pub fsyncs_data_only: AtomicU64,
}

impl Probe {
    fn hand(&self, r: Result<Attr>) -> Result<Attr> {
        r.map(|mut a| {
            self.refs.fetch_add(1, SeqCst);
            self.tweak(&mut a);
            a
        })
    }

    fn tweak(&self, a: &mut Attr) {
        if self.bad_attrs.load(SeqCst) {
            a.size = u64::MAX;
            a.blocks = u64::MAX;
            a.mode = 0xFFFF_FFFF;
            a.nlink = if a.kind == FileKind::Directory {
                0
            } else {
                u32::MAX
            };
        }
    }
}

impl Vfs for Probe {
    fn lookup(&self, p: Ino, n: &[u8]) -> Result<Attr> {
        self.hand(self.inner.lookup(p, n))
    }
    fn forget(&self, i: Ino, c: u64) {
        self.refs.fetch_sub(c as i64, SeqCst);
        self.forgotten.fetch_add(c, SeqCst);
        self.inner.forget(i, c)
    }
    fn getattr(&self, i: Ino) -> Result<Attr> {
        self.inner.getattr(i).map(|mut a| {
            self.tweak(&mut a);
            a
        })
    }
    fn setattr(&self, i: Ino, c: SetAttr) -> Result<Attr> {
        self.inner.setattr(i, c)
    }
    fn readlink(&self, i: Ino) -> Result<Vec<u8>> {
        self.inner.readlink(i)
    }
    fn create(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
        let mut r = self.inner.create(p, n, m);
        if let (Ok(a), Some(ino)) = (&mut r, *self.reuse_ino_for_create.lock().unwrap()) {
            a.ino = ino;
        }
        self.hand(r)
    }
    fn mkdir(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
        self.hand(self.inner.mkdir(p, n, m))
    }
    fn mknod(&self, p: Ino, n: &[u8], k: cowfs_vfs::FileKind, m: u32, r: u64) -> Result<Attr> {
        self.hand(self.inner.mknod(p, n, k, m, r))
    }
    fn symlink(&self, p: Ino, n: &[u8], t: &[u8]) -> Result<Attr> {
        self.hand(self.inner.symlink(p, n, t))
    }
    fn link(&self, i: Ino, p: Ino, n: &[u8]) -> Result<Attr> {
        self.hand(self.inner.link(i, p, n))
    }
    fn unlink(&self, p: Ino, n: &[u8]) -> Result<()> {
        self.inner.unlink(p, n)
    }
    fn rmdir(&self, p: Ino, n: &[u8]) -> Result<()> {
        self.inner.rmdir(p, n)
    }
    fn rename(&self, p: Ino, n: &[u8], p2: Ino, n2: &[u8], f: RenameFlags) -> Result<()> {
        self.renames.fetch_add(1, SeqCst);
        // Renames that touch a common directory must not overlap; disjoint ones may.
        let shared = {
            let mut busy = self.renaming.lock().unwrap_or_else(|e| e.into_inner());
            let shares = busy
                .iter()
                .any(|(a, b)| *a == p || *a == p2 || *b == p || *b == p2);
            if shares {
                self.renames_shared.fetch_add(1, SeqCst);
                self.renames_shared_max
                    .fetch_max(self.renames_shared.load(SeqCst), SeqCst);
            }
            busy.push((p, p2));
            shares
        };
        let _ = shared;
        self.renames_now.fetch_add(1, SeqCst);
        self.renames_max
            .fetch_max(self.renames_now.load(SeqCst), SeqCst);
        let d = self.rename_delay_ms.load(SeqCst);
        if d > 0 {
            std::thread::sleep(Duration::from_millis(d));
        }
        let r = self.inner.rename(p, n, p2, n2, f);
        self.renames_now.fetch_sub(1, SeqCst);
        self.renames_shared.fetch_sub(1, SeqCst);
        self.renaming
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(a, b)| !(*a == p && *b == p2));
        r
    }
    fn open(&self, i: Ino) -> Result<FileHandle> {
        if self.open_fails.load(SeqCst) {
            return Err(Error::Io("probe: open refused".into()));
        }
        let r = self.inner.open(i);
        if r.is_ok() {
            self.opens.fetch_add(1, SeqCst);
        }
        r
    }
    fn release(&self, h: FileHandle) -> Result<()> {
        let r = self.inner.release(h);
        if r.is_ok() {
            self.releases.fetch_add(1, SeqCst);
        }
        r
    }
    fn read(&self, i: Ino, o: u64, s: u32) -> Result<Vec<u8>> {
        let d = self.read_delay_ms.load(SeqCst);
        let wedge = self.wedge_read_ms.load(SeqCst);
        if wedge > 0 {
            std::thread::sleep(Duration::from_millis(wedge));
        }
        if d > 0 {
            std::thread::sleep(Duration::from_millis(d));
        }
        if self.panic_read.load(SeqCst) {
            self.panics.fetch_add(1, SeqCst);
            panic!("injected Vfs panic");
        }
        self.inner.read(i, o, s)
    }
    fn write(&self, i: Ino, o: u64, d: &[u8]) -> Result<u32> {
        self.inner.write(i, o, d)
    }
    fn flush(&self, i: Ino) -> Result<()> {
        self.flushes.fetch_add(1, SeqCst);
        self.inner.flush(i)
    }
    fn fsync(&self, i: Ino, data_only: bool) -> Result<()> {
        self.fsyncs.fetch_add(1, SeqCst);
        if data_only {
            self.fsyncs_data_only.fetch_add(1, SeqCst);
        }
        self.inner.fsync(i, data_only)
    }
    fn readdir(&self, d: Ino, c: u64, m: usize) -> Result<ReadDir> {
        let mut r = if self.zero_cookies.load(SeqCst) {
            // A conforming 0-based Vfs: "after cookie c" excludes c.
            self.inner
                .readdir(d, if c == 0 { 0 } else { c.saturating_add(1) }, m)
                .map(|mut l| {
                    for e in l.entries.iter_mut() {
                        e.cookie = e.cookie.saturating_sub(1);
                    }
                    l
                })
        } else {
            self.inner.readdir(d, c, m)
        };
        if false {
            r = r.map(|mut l| {
                for e in l.entries.iter_mut() {
                    e.cookie = e.cookie.saturating_sub(1);
                }
                l
            });
        }
        if self.empty_no_eof.load(SeqCst) {
            r = r.map(|mut l| {
                l.eof = false;
                if l.entries.len() > 1 {
                    l.entries.truncate(1);
                }
                l
            });
            if r.as_ref().is_ok_and(|l| l.entries.is_empty()) {
                r = Err(Error::Io("probe: empty page, never eof".into()));
            }
        }
        r
    }
    fn statfs(&self) -> Result<StatFs> {
        self.inner.statfs()
    }
    fn getxattr(&self, i: Ino, n: &[u8]) -> Result<Vec<u8>> {
        self.inner.getxattr(i, n)
    }
    fn setxattr(&self, i: Ino, n: &[u8], v: &[u8], f: XattrFlags) -> Result<()> {
        self.inner.setxattr(i, n, v, f)
    }
    fn listxattr(&self, i: Ino) -> Result<Vec<Vec<u8>>> {
        self.inner.listxattr(i)
    }
    fn removexattr(&self, i: Ino, n: &[u8]) -> Result<()> {
        self.inner.removexattr(i, n)
    }
    fn fallocate(&self, i: Ino, m: FallocMode, o: u64, l: u64) -> Result<Attr> {
        self.inner.fallocate(i, m, o, l)
    }
}

pub fn fuse_usable() -> bool {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .is_ok()
        && ["fusermount3", "fusermount"]
            .iter()
            .any(|b| Command::new(b).arg("-V").output().is_ok())
}

pub fn is_mounted(dir: &std::path::Path) -> bool {
    let want = dir.to_string_lossy().into_owned();
    fs::read_to_string("/proc/mounts")
        .unwrap()
        .lines()
        .any(|l| l.split(' ').nth(1) == Some(want.as_str()))
}

pub fn errno(r: std::io::Result<impl Sized>) -> i32 {
    r.err().and_then(|e| e.raw_os_error()).unwrap_or(0)
}

/// Aborts the FUSE connection of the mount at `dir` through fusectl, which fails every waiting
/// and future request, including those of processes that hold files open on a lazily unmounted
/// mount. Silent when fusectl is not usable.
pub fn abort_connection(dir: &std::path::Path) {
    let want = dir.to_string_lossy().into_owned();
    let Ok(info) = fs::read_to_string("/proc/self/mountinfo") else {
        return;
    };
    let dev = info.lines().find_map(|l| {
        let f: Vec<_> = l.split(' ').collect();
        (f.get(4) == Some(&want.as_str())).then(|| f[2].split(':').nth(1).map(str::to_owned))?
    });
    if let Some(minor) = dev {
        let _ = fs::write(format!("/sys/fs/fuse/connections/{minor}/abort"), "1");
    }
}

/// A mounted `Probe`. A watchdog lazily unmounts after two minutes, so a hung test fails
/// with `ENOTCONN` instead of hanging the run.
pub struct Fixture {
    pub mount: Option<Mount>,
    pub vfs: Arc<Probe>,
    pub dir: PathBuf,
    pub tmp: TempDir,
    _watchdog: mpsc::Sender<()>,
}

impl Fixture {
    pub fn new(opts: &str) -> Option<Self> {
        Self::with(opts, |_| {})
    }

    /// `prepare` runs on the bare `MemVfs` before the mount exists.
    pub fn with(opts: &str, prepare: impl FnOnce(&MemVfs)) -> Option<Self> {
        if !fuse_usable() {
            eprintln!("SKIP: /dev/fuse or fusermount3 not usable");
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mnt");
        fs::create_dir(&dir).unwrap();
        let vfs = Arc::new(Probe::default());
        prepare(&vfs.inner);
        let opts: MountOptions = opts.parse().unwrap();
        let mount = Mount::new(vfs.clone(), &dir, opts).unwrap();
        let (tx, rx) = mpsc::channel::<()>();
        let d = dir.clone();
        std::thread::spawn(move || {
            if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_secs(120))
            {
                eprintln!("WATCHDOG: test hung, unmounting {}", d.display());
                let _ = Command::new("fusermount3")
                    .arg("-u")
                    .arg("-z")
                    .arg(&d)
                    .status();
            }
        });
        Some(Self {
            mount: Some(mount),
            vfs,
            dir,
            tmp,
            _watchdog: tx,
        })
    }

    pub fn p(&self, n: &str) -> PathBuf {
        self.dir.join(n)
    }

    /// The `MemVfs` under the wrapper: changes made through it are "behind the mount" and are
    /// not counted.
    pub fn raw(&self) -> &MemVfs {
        &self.vfs.inner
    }

    pub fn mount(&self) -> &Mount {
        self.mount.as_ref().unwrap()
    }
}

/// Polls `f` every 20 ms for up to `secs` seconds.
pub fn eventually(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    f()
}
