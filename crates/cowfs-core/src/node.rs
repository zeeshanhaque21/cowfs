//! In-memory inodes.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use cowfs_vfs::{Attr, Error, FileKind, Ino};

use crate::file::FileData;

pub(crate) const DIR_SIZE: u64 = 4096;
const NO_GEN: u64 = u64::MAX;

/// The mutable part of a node. Attributes here are authoritative while the node is cached.
#[derive(Debug)]
pub(crate) struct NodeState {
    pub(crate) attr: Attr,
    /// Symlink target, loaded on first use.
    pub(crate) target: Option<Arc<[u8]>>,
    /// Regular file content, loaded on first use.
    pub(crate) file: Option<FileData>,
    /// Extended attributes, kept in memory only for orphans (meta has dropped their inode).
    pub(crate) xattrs: Option<BTreeMap<Vec<u8>, Vec<u8>>>,
    /// Exact number of entries, for directories created by this session.
    pub(crate) kids: Option<u32>,
}

impl NodeState {
    pub(crate) fn is_orphan(&self) -> bool {
        self.attr.nlink == 0
    }

    pub(crate) fn dirty_bytes(&self) -> usize {
        self.file.as_ref().map_or(0, FileData::dirty_bytes)
    }

    /// The attributes as reported to callers: `size` and `blocks` for the file content.
    pub(crate) fn report(&self) -> Attr {
        let mut a = self.attr;
        match a.kind {
            FileKind::Regular => {
                let used = match &self.file {
                    Some(f) => f.used_bytes(a.size),
                    None => a.size,
                };
                a.blocks = used.div_ceil(512);
            }
            FileKind::Directory => {
                a.size = DIR_SIZE;
                a.blocks = DIR_SIZE / 512;
            }
            FileKind::Symlink => a.blocks = 0,
            // FileKind is non_exhaustive: an unknown kind reports no blocks
            _ => a.blocks = 0,
        }
        a
    }
}

/// One live inode. Counters are atomics so that eviction checks need no lock.
#[derive(Debug)]
pub(crate) struct Node {
    pub(crate) ino: Ino,
    /// References handed out by `lookup`, `create`, `mkdir`, `symlink` and `link`.
    pub(crate) refs: AtomicU64,
    pub(crate) handles: AtomicU64,
    /// `seq` of the last mutation that meta has to learn about.
    pub(crate) seq: AtomicU64,
    /// `seq` of the last change to this directory's entries.
    pub(crate) ns_seq: AtomicU64,
    /// Queued operations that name this node structurally (as a parent, child or rename side).
    pub(crate) struct_ops: AtomicU32,
    /// Queue generation of the still queued create, if any.
    created_gen: AtomicU64,
    /// The create was cancelled before it reached meta, so nothing of this node is ever committed.
    pub(crate) elided: AtomicBool,
    /// Set when a flush of this file failed: its data cannot be chunked into the store, so every
    /// operation on it reports the error instead of writing back around the damage.
    pub(crate) poison: Mutex<Option<String>>,
    pub(crate) st: RwLock<NodeState>,
}

impl Node {
    pub(crate) fn new(ino: Ino, st: NodeState) -> Self {
        Self {
            ino,
            refs: AtomicU64::new(0),
            handles: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            ns_seq: AtomicU64::new(0),
            struct_ops: AtomicU32::new(0),
            created_gen: AtomicU64::new(NO_GEN),
            elided: AtomicBool::new(false),
            poison: Mutex::new(None),
            st: RwLock::new(st),
        }
    }

    pub(crate) fn add_ref(&self) {
        self.refs.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn pinned(&self) -> bool {
        self.refs.load(Ordering::Acquire) > 0 || self.handles.load(Ordering::Acquire) > 0
    }

    /// Records why this file cannot be written, and returns the error to report to callers.
    pub(crate) fn poison(&self, why: String) -> Error {
        *self.poison.lock().unwrap_or_else(|e| e.into_inner()) = Some(why.clone());
        Error::Corrupt(why)
    }

    pub(crate) fn poisoned(&self) -> Option<Error> {
        self.poison
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .map(Error::Corrupt)
    }

    pub(crate) fn set_created_gen(&self, gen: u64) {
        self.created_gen.store(gen, Ordering::Release);
    }

    pub(crate) fn created_gen(&self) -> Option<u64> {
        match self.created_gen.load(Ordering::Acquire) {
            NO_GEN => None,
            g => Some(g),
        }
    }
}
