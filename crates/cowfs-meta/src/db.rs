//! Database handle, snapshots, and the transaction plumbing.

use crate::error::guard;
use crate::node::NodeId;
use crate::ptree::{MemTree, NodeWriter};
use crate::read::{self, RoView};
use crate::tx::Tx;
use crate::types::*;
use crate::walk::{LiveBlocks, Marker};
use crate::{Error, Result};
use cowfs_store::ChunkRef;
use redb::{
    Builder, Database, ReadableDatabase, ReadableTable, StorageBackend, TableDefinition,
    WriteTransaction,
};
use std::io;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub(crate) const NODES: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("nodes");
pub(crate) const REFS: TableDefinition<[u8; 32], u64> = TableDefinition::new("refs");
pub(crate) const SNAPSHOTS: TableDefinition<u64, &[u8]> = TableDefinition::new("snapshots");
pub(crate) const SNAP_NAMES: TableDefinition<&str, u64> = TableDefinition::new("snap_names");
pub(crate) const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

pub(crate) const FORMAT_VERSION: u64 = 1;

/// Hook run before every durable commit; the mount layer sets it to the block store's `sync`.
pub type SyncHook = Arc<dyn Fn() -> io::Result<()> + Send + Sync>;

/// Tuning knobs for [`Meta::open`].
#[derive(Clone)]
pub struct Options {
    /// Target encoded size of a tree node in bytes. Fixed when the database is created.
    pub node_size: usize,
    /// A durable commit happens at the latest after this many mutating transactions.
    pub sync_every_ops: u32,
    /// A durable commit happens at the latest when a mutation arrives this long after the last one.
    pub sync_interval: Duration,
    /// Called before every durable commit. If it fails the commit does not happen.
    pub before_sync: Option<SyncHook>,
    /// redb page cache size in bytes.
    pub cache_size: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            node_size: 4096,
            sync_every_ops: 256,
            sync_interval: Duration::from_secs(1),
            before_sync: None,
            cache_size: 64 << 20,
        }
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("node_size", &self.node_size)
            .field("sync_every_ops", &self.sync_every_ops)
            .field("sync_interval", &self.sync_interval)
            .field("before_sync", &self.before_sync.is_some())
            .field("cache_size", &self.cache_size)
            .finish()
    }
}

#[derive(Debug)]
struct SyncState {
    pending: u32,
    last_sync: Instant,
}

/// Owns the redb handle so a panic in redb's close-time commit on a damaged file cannot escape `drop`.
pub(crate) struct Db(Option<Database>);

impl std::ops::Deref for Db {
    type Target = Database;

    fn deref(&self) -> &Database {
        match &self.0 {
            Some(db) => db,
            None => unreachable!("database used after drop"),
        }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if let Some(db) = self.0.take() {
            let _ = catch_unwind(AssertUnwindSafe(move || drop(db)));
        }
    }
}

pub(crate) struct Inner {
    pub(crate) db: Db,
    opts: Options,
    node_max: usize,
    state: Mutex<SyncState>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner").finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Inner {
    fn run_hook(&self) -> Result<()> {
        match &self.opts.before_sync {
            Some(h) => h().map_err(Error::Hook),
            None => Ok(()),
        }
    }

    fn begin(&self, durable: bool) -> Result<WriteTransaction> {
        let mut wtx = self.db.begin_write()?;
        if durable {
            wtx.set_two_phase_commit(true);
        } else {
            wtx.set_durability(redb::Durability::None)?;
        }
        Ok(wtx)
    }

    /// Runs one mutating transaction. `f` returns whether anything changed; if not, nothing commits.
    pub(crate) fn write<T>(
        &self,
        force_durable: bool,
        f: impl FnOnce(&WriteTransaction) -> Result<(T, bool)>,
    ) -> Result<T> {
        let mut st = lock(&self.state);
        let durable = force_durable
            || st.pending + 1 >= self.opts.sync_every_ops
            || st.last_sync.elapsed() >= self.opts.sync_interval;
        if durable {
            self.run_hook()?;
        }
        guard(|| {
            let wtx = self.begin(durable)?;
            let (out, changed) = f(&wtx)?;
            if !changed {
                return Ok(out);
            }
            wtx.commit()?;
            if durable {
                st.pending = 0;
                st.last_sync = Instant::now();
            } else {
                st.pending += 1;
            }
            Ok(out)
        })
    }

    pub(crate) fn sync(&self) -> Result<()> {
        let mut st = lock(&self.state);
        if st.pending == 0 {
            return Ok(());
        }
        self.run_hook()?;
        guard(|| {
            self.begin(true)?.commit()?;
            st.pending = 0;
            st.last_sync = Instant::now();
            Ok(())
        })
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.sync();
    }
}

fn corrupt(what: &str) -> Error {
    Error::Corrupt(what.to_string())
}

pub(crate) fn encode_snap(s: &SnapshotInfo) -> Vec<u8> {
    let mut b = Vec::with_capacity(2 + s.name.len() + 32 + 12 + 8);
    b.extend((s.name.len() as u16).to_le_bytes());
    b.extend_from_slice(s.name.as_bytes());
    b.extend_from_slice(s.root.as_bytes());
    b.extend(s.created.secs.to_le_bytes());
    b.extend(s.created.nanos.to_le_bytes());
    b.extend(s.parent.map_or(0, |p| p.0).to_le_bytes());
    b
}

pub(crate) fn decode_snap(id: u64, b: &[u8]) -> Result<SnapshotInfo> {
    let bad = || corrupt("bad snapshot row");
    let nl = usize::from(u16::from_le_bytes(
        b.get(..2).and_then(|s| s.try_into().ok()).ok_or_else(bad)?,
    ));
    let rest = b.get(2 + nl..).ok_or_else(bad)?;
    if rest.len() != 32 + 12 + 8 {
        return Err(bad());
    }
    let name = std::str::from_utf8(&b[2..2 + nl])
        .map_err(|_| bad())?
        .to_string();
    let arr = |r: std::ops::Range<usize>| rest.get(r).ok_or_else(bad);
    let root = NodeId::from_bytes(arr(0..32)?.try_into().map_err(|_| bad())?);
    let secs = i64::from_le_bytes(arr(32..40)?.try_into().map_err(|_| bad())?);
    let nanos = u32::from_le_bytes(arr(40..44)?.try_into().map_err(|_| bad())?);
    let parent = u64::from_le_bytes(arr(44..52)?.try_into().map_err(|_| bad())?);
    Ok(SnapshotInfo {
        id: SnapshotId(id),
        name,
        root,
        created: Timestamp { secs, nanos },
        parent: (parent != 0).then_some(SnapshotId(parent)),
    })
}

pub(crate) fn read_snap<T: ReadableTable<u64, &'static [u8]>>(
    t: &T,
    id: SnapshotId,
) -> Result<SnapshotInfo> {
    let g = t.get(id.0)?.ok_or(Error::NoSuchSnapshot)?;
    decode_snap(id.0, g.value())
}

pub(crate) fn meta_get<T: ReadableTable<&'static str, u64>>(t: &T, k: &str) -> Result<u64> {
    t.get(k)?
        .map(|g| g.value())
        .ok_or_else(|| Error::Corrupt(format!("missing meta key {k}")))
}

/// The metadata store: one redb file holding every snapshot's tree.
///
/// Cloning is cheap and shares the database. All methods take `&self`; redb allows one writer at a
/// time and any number of readers, so writers are serialized and readers never block.
#[derive(Clone, Debug)]
pub struct Meta {
    pub(crate) inner: Arc<Inner>,
}

impl Meta {
    /// Opens the database at `path`, creating it if the file is missing or empty.
    pub fn open(path: impl AsRef<Path>, opts: Options) -> Result<Meta> {
        guard(|| Self::init(builder(&opts).create(path)?, opts.clone()))
    }

    /// Like [`Meta::open`] on a caller-supplied redb backend (used by crash-injection tests).
    pub fn open_with_backend(backend: impl StorageBackend, opts: Options) -> Result<Meta> {
        guard(|| Self::init(builder(&opts).create_with_backend(backend)?, opts.clone()))
    }

    fn init(db: Database, opts: Options) -> Result<Meta> {
        if opts.node_size < 256 {
            return Err(Error::Invalid("node_size below 256"));
        }
        let existing = {
            let rtx = db.begin_read()?;
            match rtx.open_table(META) {
                Ok(t) => Some((meta_get(&t, "version")?, meta_get(&t, "node_size")?)),
                Err(redb::TableError::TableDoesNotExist(_)) => None,
                Err(e) => return Err(e.into()),
            }
        };
        let node_max = match existing {
            Some((v, n)) => {
                if v != FORMAT_VERSION {
                    return Err(Error::Corrupt(format!("unsupported format version {v}")));
                }
                usize::try_from(n)
                    .ok()
                    .filter(|&n| n >= 256)
                    .ok_or_else(|| corrupt("bad node size"))?
            }
            None => {
                let mut wtx = db.begin_write()?;
                wtx.set_two_phase_commit(true);
                {
                    wtx.open_table(NODES)?;
                    wtx.open_table(REFS)?;
                    wtx.open_table(SNAPSHOTS)?;
                    wtx.open_table(SNAP_NAMES)?;
                    let mut m = wtx.open_table(META)?;
                    m.insert("version", FORMAT_VERSION)?;
                    m.insert("node_size", opts.node_size as u64)?;
                    m.insert("next_ino", 2)?;
                    m.insert("next_snapshot", 1)?;
                }
                wtx.commit()?;
                opts.node_size
            }
        };
        Ok(Meta {
            inner: Arc::new(Inner {
                db: Db(Some(db)),
                node_max,
                state: Mutex::new(SyncState {
                    pending: 0,
                    last_sync: Instant::now(),
                }),
                opts,
            }),
        })
    }

    /// Creates a snapshot holding an empty tree (just the root directory).
    pub fn new_snapshot(&self, name: &str) -> Result<Snapshot> {
        self.add_snapshot(name, None)
    }

    fn add_snapshot(&self, name: &str, from: Option<SnapshotId>) -> Result<Snapshot> {
        if name.is_empty() || name.len() > usize::from(u16::MAX) {
            return Err(Error::Invalid("bad snapshot name"));
        }
        let node_max = self.inner.node_max;
        let id = self.inner.write(true, |wtx| {
            let mut names = wtx.open_table(SNAP_NAMES)?;
            let mut snaps = wtx.open_table(SNAPSHOTS)?;
            let mut meta = wtx.open_table(META)?;
            let mut w = NodeWriter::new(wtx.open_table(NODES)?, wtx.open_table(REFS)?);
            if names.get(name)?.is_some() {
                return Err(Error::SnapshotExists);
            }
            let root = match from {
                Some(src) => read_snap(&snaps, src)?.root,
                None => {
                    let mut tree = MemTree::empty(node_max);
                    let now = Timestamp::now();
                    let rec = InodeRec {
                        kind: FileType::Dir,
                        mode: 0o755,
                        nlink: 2,
                        size: 0,
                        atime: now,
                        mtime: now,
                        ctime: now,
                        parent: ROOT_INO.0,
                        next_cookie: 1,
                    };
                    tree.insert(&w.nodes, &key(ROOT_INO, K_INODE, &[]), rec.encode())?;
                    tree.flush(&mut w)?
                }
            };
            w.add_ref(root);
            w.settle()?;
            let id = meta_get(&meta, "next_snapshot")?;
            meta.insert("next_snapshot", id + 1)?;
            let info = SnapshotInfo {
                id: SnapshotId(id),
                name: name.to_string(),
                root,
                created: Timestamp::now(),
                parent: from,
            };
            snaps.insert(id, encode_snap(&info).as_slice())?;
            names.insert(name, id)?;
            Ok((SnapshotId(id), true))
        })?;
        Ok(Snapshot {
            inner: self.inner.clone(),
            id,
        })
    }

    /// Opens a snapshot by name.
    pub fn snapshot(&self, name: &str) -> Result<Snapshot> {
        guard(|| {
            let rtx = self.inner.db.begin_read()?;
            let id = rtx
                .open_table(SNAP_NAMES)?
                .get(name)?
                .map(|g| g.value())
                .ok_or(Error::NoSuchSnapshot)?;
            Ok(Snapshot {
                inner: self.inner.clone(),
                id: SnapshotId(id),
            })
        })
    }

    /// Opens a snapshot by id.
    pub fn snapshot_by_id(&self, id: SnapshotId) -> Result<Snapshot> {
        guard(|| {
            let rtx = self.inner.db.begin_read()?;
            read_snap(&rtx.open_table(SNAPSHOTS)?, id)?;
            Ok(Snapshot {
                inner: self.inner.clone(),
                id,
            })
        })
    }

    /// Lists all snapshots in id order.
    pub fn snapshots(&self) -> Result<Vec<SnapshotInfo>> {
        guard(|| {
            let rtx = self.inner.db.begin_read()?;
            rtx.open_table(SNAPSHOTS)?
                .iter()?
                .map(|r| {
                    let (k, v) = r?;
                    decode_snap(k.value(), v.value())
                })
                .collect()
        })
    }

    /// Removes a snapshot and frees every tree node no other snapshot shares.
    pub fn remove_snapshot(&self, id: SnapshotId) -> Result<()> {
        self.inner.write(true, |wtx| {
            let mut names = wtx.open_table(SNAP_NAMES)?;
            let mut snaps = wtx.open_table(SNAPSHOTS)?;
            let mut w = NodeWriter::new(wtx.open_table(NODES)?, wtx.open_table(REFS)?);
            let info = read_snap(&snaps, id)?;
            w.drop_ref(info.root);
            w.settle()?;
            snaps.remove(id.0)?;
            names.remove(info.name.as_str())?;
            Ok(((), true))
        })
    }

    /// Makes every earlier mutation durable (runs `before_sync` first).
    pub fn sync(&self) -> Result<()> {
        self.inner.sync()
    }

    /// Verifies every structural and semantic invariant. See `docs/v1-meta.md`.
    pub fn check(&self) -> Result<()> {
        guard(|| crate::check::check(&self.inner))
    }
}

fn builder(opts: &Options) -> Builder {
    let mut b = Builder::new();
    b.set_cache_size(opts.cache_size);
    b
}

/// A cheap `Send + Sync` handle to one snapshot. All methods take `&self`.
///
/// A snapshot is a full writable tree. Changes made through one handle never appear in another
/// snapshot, including the one it was forked from.
#[derive(Clone, Debug)]
pub struct Snapshot {
    inner: Arc<Inner>,
    id: SnapshotId,
}

macro_rules! forward_writes {
    ($($(#[$doc:meta])* $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        impl Snapshot {$(
            $(#[$doc])*
            pub fn $name(&self, $($arg: $ty),*) -> Result<$ret> {
                self.batch(|tx| tx.$name($($arg),*))
            }
        )*}
    };
}

forward_writes! {
    /// Creates an empty regular file. See [`Tx::create`].
    create(dir: Ino, name: &[u8], mode: u32) -> Attr;
    /// Creates an empty directory. See [`Tx::mkdir`].
    mkdir(dir: Ino, name: &[u8], mode: u32) -> Attr;
    /// Creates a symbolic link. See [`Tx::symlink`].
    symlink(dir: Ino, name: &[u8], target: &[u8]) -> Attr;
    /// Adds a hardlink. See [`Tx::link`].
    link(ino: Ino, dir: Ino, name: &[u8]) -> Attr;
    /// Removes a name of a file or symlink. See [`Tx::unlink`].
    unlink(dir: Ino, name: &[u8]) -> Removed;
    /// Removes an empty directory. See [`Tx::rmdir`].
    rmdir(dir: Ino, name: &[u8]) -> Removed;
    /// Renames atomically. See [`Tx::rename`].
    rename(from_dir: Ino, from_name: &[u8], to_dir: Ino, to_name: &[u8]) -> Option<Removed>;
    /// Changes mode, times or size. See [`Tx::setattr`].
    setattr(ino: Ino, set: SetAttr) -> Attr;
    /// Replaces a file's chunk list and size. See [`Tx::set_content`].
    set_content(ino: Ino, chunks: &[ChunkRef], size: u64) -> Attr;
    /// Sets an extended attribute. See [`Tx::setxattr`].
    setxattr(ino: Ino, name: &[u8], value: &[u8]) -> ();
    /// Removes an extended attribute. See [`Tx::removexattr`].
    removexattr(ino: Ino, name: &[u8]) -> ();
}

impl Snapshot {
    /// The snapshot id.
    pub fn id(&self) -> SnapshotId {
        self.id
    }

    /// Name, root, creation time and parent of this snapshot.
    pub fn info(&self) -> Result<SnapshotInfo> {
        guard(|| {
            let rtx = self.inner.db.begin_read()?;
            read_snap(&rtx.open_table(SNAPSHOTS)?, self.id)
        })
    }

    /// The Merkle root of this snapshot's tree.
    pub fn root(&self) -> Result<NodeId> {
        Ok(self.info()?.root)
    }

    /// Creates a writable clone named `name`. Costs one row and one counter, independent of tree size.
    pub fn fork(&self, name: &str) -> Result<Snapshot> {
        Meta {
            inner: self.inner.clone(),
        }
        .add_snapshot(name, Some(self.id))
    }

    fn read<T>(&self, f: impl FnOnce(&RoView) -> Result<T>) -> Result<T> {
        guard(|| {
            let rtx = self.inner.db.begin_read()?;
            let root = read_snap(&rtx.open_table(SNAPSHOTS)?, self.id)?.root;
            f(&RoView {
                nodes: rtx.open_table(NODES)?,
                root,
            })
        })
    }

    /// Looks up a name in a directory. `.` and `..` resolve.
    pub fn lookup(&self, dir: Ino, name: &[u8]) -> Result<Attr> {
        self.read(|r| read::lookup(r, dir, name))
    }

    /// Attributes of an inode.
    pub fn getattr(&self, ino: Ino) -> Result<Attr> {
        self.read(|r| read::getattr(r, ino))
    }

    /// Lists up to `max` entries after `cookie` (0 starts a listing).
    ///
    /// Cookies belong to entries and never change, so a listing resumed after removals neither
    /// repeats nor skips surviving entries.
    pub fn readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir> {
        self.read(|r| read::readdir(r, dir, cookie, max))
    }

    /// Target of a symlink.
    pub fn readlink(&self, ino: Ino) -> Result<Vec<u8>> {
        self.read(|r| read::readlink(r, ino))
    }

    /// Chunk list of a regular file.
    pub fn chunks(&self, ino: Ino) -> Result<Vec<ChunkRef>> {
        self.read(|r| read::chunks(r, ino))
    }

    /// Value of an extended attribute.
    pub fn getxattr(&self, ino: Ino, name: &[u8]) -> Result<Vec<u8>> {
        self.read(|r| read::getxattr(r, ino, name))
    }

    /// Names of all extended attributes of an inode.
    pub fn listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>> {
        self.read(|r| read::listxattr(r, ino))
    }

    /// Walks the tree yielding every block id referenced by a chunk list.
    ///
    /// Subtrees whose root is already in `marker` are skipped, and each subtree is added to
    /// `marker` once fully walked. Sharing one `marker` across snapshots therefore costs only the
    /// nodes that differ. A block may be yielded more than once.
    pub fn live_blocks<'m>(&self, marker: &'m mut Marker) -> Result<LiveBlocks<'m>> {
        guard(|| {
            let rtx = self.inner.db.begin_read()?;
            let root = read_snap(&rtx.open_table(SNAPSHOTS)?, self.id)?.root;
            LiveBlocks::new(rtx.open_table(NODES)?, root, marker)
        })
    }

    /// Runs several operations in one transaction: all commit together or none do.
    pub fn batch<T>(&self, f: impl FnOnce(&mut Tx<'_>) -> Result<T>) -> Result<T> {
        let id = self.id;
        let node_max = self.inner.node_max;
        let mut user_panic = None;
        let res = self.inner.write(false, |wtx| {
            let mut snaps = wtx.open_table(SNAPSHOTS)?;
            let mut meta = wtx.open_table(META)?;
            let info = read_snap(&snaps, id)?;
            let next_ino = meta_get(&meta, "next_ino")?;
            let mut tx = Tx {
                w: NodeWriter::new(wtx.open_table(NODES)?, wtx.open_table(REFS)?),
                tree: MemTree::new(info.root, node_max),
                next_ino,
                now: Timestamp::now(),
            };
            let out = match catch_unwind(AssertUnwindSafe(|| f(&mut tx))) {
                Ok(r) => r?,
                Err(p) => {
                    user_panic = Some(p);
                    return Err(Error::Invalid("batch closure panicked"));
                }
            };
            if tx.tree.unchanged() {
                return Ok((out, false));
            }
            let new_root = tx.tree.flush(&mut tx.w)?;
            if new_root != info.root {
                tx.w.add_ref(new_root);
                tx.w.drop_ref(info.root);
            }
            tx.w.settle()?;
            if new_root != info.root {
                let updated = SnapshotInfo {
                    root: new_root,
                    ..info
                };
                snaps.insert(id.0, encode_snap(&updated).as_slice())?;
            }
            meta.insert("next_ino", tx.next_ino)?;
            Ok((out, true))
        });
        if let Some(p) = user_panic {
            resume_unwind(p);
        }
        res
    }
}
