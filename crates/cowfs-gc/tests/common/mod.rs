//! Shared fixtures: a store, a metadata database, a collector, and roots that behave like
//! `cowfs-core`'s.

#![allow(dead_code)]

pub mod child;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use cowfs_gc::{Barrier, ExtraRoots, Gc, Options, RootsError};
use cowfs_meta::{ChunkRef, Meta, Snapshot};
use cowfs_store::{BlockId, Options as StoreOptions, Store};

/// A store with a small pack, so a test makes several packs without writing gigabytes.
pub fn small_store_opts(pack: u64) -> StoreOptions {
    StoreOptions {
        max_pack_size: pack,
        ..StoreOptions::default()
    }
}

/// Everything a test needs: a store, a database and a collector over both.
///
/// `keep` holds the `TempDir` so a test that reopens gets the same paths back; it is `None` on a
/// reopened fixture, whose directory is already on disk.
pub struct Fixture {
    pub dir: PathBuf,
    pub store: Arc<Store>,
    pub meta: Arc<Meta>,
    pub gc: Gc,
    /// The `TempDir` that owns `dir`. Shared, so a reopened fixture can hand it on.
    keep: Arc<Mutex<Option<tempfile::TempDir>>>,
    sopts: StoreOptions,
    gopts: Options,
}

/// Options tuned so a small corpus reaches the sweep threshold.
pub fn eager() -> Options {
    Options {
        dead_ratio: 0.0,
        min_dead_bytes: 1,
        io_budget_bytes: 0,
        batch_bytes: 4096,
        ..Options::default()
    }
}

impl Fixture {
    /// Open a fixture. `pack` is the maximum pack size in bytes.
    pub fn with_pack(pack: u64) -> Self {
        Self::new(small_store_opts(pack), Options::default())
    }

    pub fn new(sopts: StoreOptions, gopts: Options) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(Store::open(dir.path().join("store"), sopts).expect("store"));
        let meta = Arc::new(
            Meta::open(
                dir.path().join("meta"),
                cowfs_meta::Options {
                    background: false,
                    ..cowfs_meta::Options::default()
                },
            )
            .expect("meta"),
        );
        let gc = Gc::open(
            dir.path().join("gcstate"),
            Arc::clone(&store),
            Arc::clone(&meta),
            gopts,
        )
        .expect("gc");
        Self {
            dir: dir.path().to_path_buf(),
            keep: Arc::new(Mutex::new(Some(dir))),
            store,
            meta,
            gc,
            sopts,
            gopts,
        }
    }

    /// A fixture whose collector frees eagerly, so a test with a small corpus sweeps.
    pub fn eager(pack: u64) -> Self {
        Self::new(small_store_opts(pack), eager())
    }

    /// A fixture whose metadata database runs `hook` before every durable commit.
    ///
    /// The hook can be armed from a test to make the next `sync()` fail with a non-`NoSuchSnapshot`
    /// error, which is how a test proves the collector propagates such an error instead of skipping
    /// the snapshot it hit it on.
    pub fn with_hook(pack: u64, hook: cowfs_meta::SyncHook) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let store =
            Arc::new(Store::open(dir.path().join("store"), small_store_opts(pack)).expect("store"));
        let meta = Arc::new(
            Meta::open(
                dir.path().join("meta"),
                cowfs_meta::Options {
                    background: false,
                    before_sync: Some(hook),
                    ..cowfs_meta::Options::default()
                },
            )
            .expect("meta"),
        );
        let gc = Gc::open(
            dir.path().join("gcstate"),
            Arc::clone(&store),
            Arc::clone(&meta),
            eager(),
        )
        .expect("gc");
        Self {
            dir: dir.path().to_path_buf(),
            keep: Arc::new(Mutex::new(Some(dir))),
            store,
            meta,
            gc,
            sopts: small_store_opts(pack),
            gopts: eager(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Drop the store and the database and open both again on the same directories, which is what
    /// a clean restart does. Takes `self`, so the old handles are gone before the reopen.
    pub fn reopen(self) -> Fixture {
        let Self {
            dir,
            store,
            meta,
            gc,
            keep,
            sopts,
            gopts,
        } = self;
        let store_dir = dir.join("store");
        let meta_path = dir.join("meta");
        let gc_dir = dir.join("gcstate");
        drop(gc);
        drop(store);
        drop(meta);
        let store = Arc::new(Store::open(&store_dir, sopts).expect("reopen store"));
        let meta = Arc::new(
            Meta::open(
                &meta_path,
                cowfs_meta::Options {
                    background: false,
                    ..cowfs_meta::Options::default()
                },
            )
            .expect("reopen meta"),
        );
        let gc =
            Gc::open(&gc_dir, Arc::clone(&store), Arc::clone(&meta), gopts).expect("reopen gc");
        Fixture {
            dir,
            store,
            meta,
            gc,
            keep,
            sopts,
            gopts,
        }
    }

    /// Delete the store directory, which is what a crash that lost an unfsynced create leaves for
    /// a test that wants to build the next image from scratch.
    pub fn wipe_store(&self) {
        let _ = std::fs::remove_dir_all(self.store_dir());
    }

    /// The handles a concurrent test needs, which are `Send + Sync`.
    ///
    /// The fixture itself is not, because it owns a `TempDir` and a `Mutex`, so a threaded test
    /// takes these instead and leaves the fixture to the main thread.
    /// Take over a directory a caller owns, with handles the caller already opened.
    pub fn adopt(
        dir: PathBuf,
        store: Arc<Store>,
        meta: Arc<Meta>,
        gc: Gc,
        sopts: StoreOptions,
        gopts: Options,
    ) -> Fixture {
        Fixture {
            dir,
            store,
            meta,
            gc,
            keep: Arc::new(Mutex::new(None)),
            sopts,
            gopts,
        }
    }

    pub fn parts(&self) -> Parts<'_> {
        Parts {
            store: Arc::clone(&self.store),
            meta: Arc::clone(&self.meta),
            gc: &self.gc,
        }
    }

    /// Close the store and the database and keep the directory, so a test can damage the files and
    /// reopen them. The caller owns the returned path and deletes it.
    pub fn persist(self) -> PathBuf {
        let path = self.path().to_path_buf();
        let keep = self.keep.lock().unwrap().take();
        let Self {
            dir,
            store,
            meta,
            gc,
            keep: _,
            sopts: _,
            gopts: _,
        } = self;
        drop(gc);
        drop(store);
        drop(meta);
        let _ = dir;
        // The `TempDir` would delete the directory on drop, so it is leaked on purpose. A test that
        // damages files owns the cleanup and does it with `fs::remove_dir_all`.
        std::mem::forget(keep);
        path
    }

    /// Store a file's content and set it on a snapshot, the way a mount would.
    ///
    /// `name` is created under the root, or reused when it already exists, so a rewrite of the
    /// same file deduplicates exactly as a real write does.
    pub fn write(&self, snap: &Snapshot, name: &[u8], data: &[u8]) -> Vec<ChunkRef> {
        let chunks = self.store.ingest_bytes(data).expect("ingest");
        let ino = match snap.lookup(cowfs_meta::ROOT_INO, name) {
            Ok(a) => a.ino,
            Err(_) => {
                snap.batch(|tx| tx.create(cowfs_meta::ROOT_INO, name, 0o644))
                    .expect("create")
                    .ino
            }
        };
        snap.batch(|tx| tx.set_content(ino, &chunks, data.len() as u64))
            .expect("set content");
        chunks
    }

    /// Every block any live snapshot references right now.
    ///
    /// A hole is not a block, so it is dropped: the raw walk yields it, and a caller that wants to
    /// know what the store must hold does not want it.
    pub fn live_blocks(&self) -> std::collections::HashSet<BlockId> {
        let mut marker = cowfs_meta::Marker::new();
        let mut out = std::collections::HashSet::new();
        for info in self.meta.durable_snapshots().expect("snaps") {
            let snap = self.meta.snapshot_by_id(info.id).expect("snap");
            for b in snap.live_blocks(&mut marker).expect("walk") {
                let b = b.expect("block");
                if b != cowfs_gc::HOLE {
                    out.insert(b);
                }
            }
        }
        out
    }

    /// A copy of every file in the store directory, name and contents.
    pub fn store_files(&self) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![self.store_dir()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(b) = std::fs::read(&p) {
                    out.push((
                        p.strip_prefix(self.store_dir())
                            .unwrap_or(&p)
                            .to_string_lossy()
                            .into_owned(),
                        b,
                    ));
                }
            }
        }
        out.sort();
        out
    }

    pub fn store_dir(&self) -> PathBuf {
        self.path().join("store")
    }

    /// Store a file's content under `dir`, creating the name if it is not there.
    pub fn write_at(&self, snap: &Snapshot, dir: cowfs_meta::Ino, name: &[u8], data: &[u8]) {
        let chunks = self.store.ingest_bytes(data).expect("ingest");
        let ino = match snap.lookup(dir, name) {
            Ok(a) => a.ino,
            Err(_) => {
                snap.batch(|tx| tx.create(dir, name, 0o644))
                    .expect("create")
                    .ino
            }
        };
        snap.batch(|tx| tx.set_content(ino, &chunks, data.len() as u64))
            .expect("set content");
    }

    /// The root of a snapshot, so a test can prove a walk did not change it.
    pub fn fork_is_isolated(&self, snap: &Snapshot) -> bool {
        let before = snap.root().expect("root");
        let _ = self.gc.collect(None);
        snap.root().expect("root") == before
    }

    pub fn gc_dir(&self) -> PathBuf {
        self.path().join("gcstate")
    }
}

/// The shareable handles of a fixture, for a test that runs threads.
pub struct Parts<'a> {
    pub store: Arc<Store>,
    pub meta: Arc<Meta>,
    pub gc: &'a Gc,
}

impl Parts<'_> {
    /// Store content and write it into a file, the way a mount would. `Send + Sync`, so a thread
    /// can hold a reference to it.
    pub fn write(&self, snap: &Snapshot, name: &[u8], data: &[u8]) {
        let chunks = self.store.ingest_bytes(data).expect("ingest");
        let ino = match snap.lookup(cowfs_meta::ROOT_INO, name) {
            Ok(a) => a.ino,
            Err(_) => {
                snap.batch(|tx| tx.create(cowfs_meta::ROOT_INO, name, 0o644))
                    .expect("create")
                    .ino
            }
        };
        snap.batch(|tx| tx.set_content(ino, &chunks, data.len() as u64))
            .expect("set content");
    }

    /// Every block any live snapshot references, holes dropped.
    ///
    /// A snapshot removed between the listing and the lookup, or between the lookup and the walk,
    /// is skipped: it is gone, so its blocks are not live, and a test that removes snapshots under a
    /// collector must not fail here.
    pub fn live(&self) -> std::collections::HashSet<BlockId> {
        self.live_with(|_| {})
    }

    /// [`Self::live`], calling `after_lookup` with each snapshot id once it is looked up and
    /// before its walk starts. A test uses it to land a removal in that window deterministically.
    pub fn live_with(
        &self,
        mut after_lookup: impl FnMut(cowfs_meta::SnapshotId),
    ) -> std::collections::HashSet<BlockId> {
        let mut marker = cowfs_meta::Marker::new();
        let mut out = std::collections::HashSet::new();
        for info in self.meta.durable_snapshots().expect("snaps") {
            let Ok(snap) = self.meta.snapshot_by_id(info.id) else {
                continue;
            };
            after_lookup(info.id);
            let walk = match snap.live_blocks(&mut marker) {
                Ok(w) => w,
                // Removed in the window: gone, so not live. Any other error is a real failure.
                Err(cowfs_meta::Error::NoSuchSnapshot) => continue,
                Err(e) => panic!("walk: {e:?}"),
            };
            for b in walk {
                let b = b.expect("block");
                if b != cowfs_gc::HOLE {
                    out.insert(b);
                }
            }
        }
        out
    }
}

/// Roots that behave like `cowfs-core`'s: a pinned set, and a barrier that holds a writer lock
/// for exactly as long as it is alive.
///
/// A writer calls [`Roots::write`] to make a commit visible, which takes the same lock. So while a
/// barrier is alive no commit can start, and one that started before it finishes first. That is
/// the ordering `cowfs-core` would give with its flusher lock.
pub struct Roots {
    inner: Arc<Inner>,
    pinned: Mutex<Vec<BlockId>>,
    offers: bool,
}

#[derive(Debug, Default)]
struct Inner {
    gate: Mutex<GateState>,
    cv: Condvar,
    taken: AtomicUsize,
    held_us: AtomicU64,
    /// How long writers waited for a barrier, summed. A test can show the stall is bounded.
    waited_us: AtomicU64,
}

#[derive(Debug, Default)]
struct GateState {
    /// True while a barrier is alive, so no new commit may start.
    held: bool,
    /// Commits inside their critical section.
    writers: usize,
}

impl Roots {
    pub fn new() -> Arc<Self> {
        Self::build(true)
    }

    /// A `Roots` that offers no barrier, so a cycle must report and not free.
    pub fn no_barrier() -> Arc<Self> {
        Self::build(false)
    }

    fn build(offers: bool) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(Inner::default()),
            pinned: Mutex::new(Vec::new()),
            offers,
        })
    }

    pub fn pin(&self, b: BlockId) {
        self.pinned.lock().unwrap().push(b);
    }

    pub fn unpin_all(&self) {
        self.pinned.lock().unwrap().clear();
    }

    /// Run a commit, ordered against any barrier.
    ///
    /// Waits while a barrier is held, then holds the gate for the commit itself, so a barrier that
    /// arrives during it waits for the commit rather than cutting it in half.
    pub fn write<T>(&self, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let mut g = self.inner.gate.lock().unwrap();
        while g.held {
            g = self.inner.cv.wait(g).unwrap();
        }
        g.writers += 1;
        drop(g);
        let out = f();
        let mut g = self.inner.gate.lock().unwrap();
        g.writers -= 1;
        self.inner.cv.notify_all();
        self.inner
            .waited_us
            .fetch_add(start.elapsed().as_micros() as u64, Relaxed);
        out
    }

    /// Microseconds a barrier was held, summed over every cycle.
    pub fn held_us(&self) -> u64 {
        self.inner.held_us.load(Relaxed)
    }

    /// Microseconds writers spent waiting for a barrier, summed over every write.
    pub fn waited_us(&self) -> u64 {
        self.inner.waited_us.load(Relaxed)
    }

    /// True while a barrier is held. Lets a test ask when the writer gate is closed.
    pub fn barrier_live(&self) -> bool {
        self.inner.gate.lock().unwrap().held
    }

    /// How many barriers were taken.
    pub fn barrier_taken(&self) -> usize {
        self.inner.taken.load(Relaxed)
    }
}

impl ExtraRoots for Roots {
    fn pinned_blocks(&self) -> std::result::Result<Vec<BlockId>, RootsError> {
        Ok(self.pinned.lock().unwrap().clone())
    }

    fn reference_barrier(&self) -> std::result::Result<Option<Box<dyn Barrier>>, RootsError> {
        if !self.offers {
            return Ok(None);
        }
        // Constructing the value stalls nobody. The gate is taken in `take`, so the writers this
        // test measures are only blocked for the window the collector actually needs.
        Ok(Some(Box::new(Gate {
            inner: Arc::clone(&self.inner),
        }) as Box<dyn Barrier>))
    }
}

/// Acquires the writer gate on `take`, so a collect only stalls writers for its unlink window.
struct Gate {
    inner: Arc<Inner>,
}

impl Barrier for Gate {
    fn take(&mut self) -> Option<Box<dyn cowfs_gc::Held>> {
        let start = Instant::now();
        let mut g = self.inner.gate.lock().unwrap();
        g.held = true;
        while g.writers > 0 {
            g = self.inner.cv.wait(g).unwrap();
        }
        drop(g);
        self.inner.taken.fetch_add(1, Relaxed);
        Some(Box::new(Guard {
            inner: Arc::clone(&self.inner),
            start,
        }))
    }
}

/// Holds the gate, and releases it on drop.
struct Guard {
    inner: Arc<Inner>,
    start: Instant,
}

impl cowfs_gc::Held for Guard {}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut g = self.inner.gate.lock().unwrap();
        g.held = false;
        self.inner.cv.notify_all();
        self.inner
            .held_us
            .fetch_add(self.start.elapsed().as_micros() as u64, Relaxed);
    }
}
