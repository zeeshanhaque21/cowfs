//! Per-snapshot state: the operation queue and the counters that tell what is committed.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cowfs_meta::Snapshot;
use cowfs_vfs::Ino;

use crate::file::Chunks;
use crate::node::Node;

#[derive(Clone, Debug)]
pub(crate) enum Create {
    File,
    Dir,
    Symlink(Arc<[u8]>),
}

/// One deferred meta operation, replayed in queue order inside one batch.
#[derive(Debug)]
pub(crate) enum Op {
    Create {
        parent: Ino,
        name: Box<[u8]>,
        mode: u32,
        child: Ino,
        what: Create,
    },
    Link {
        ino: Ino,
        parent: Ino,
        name: Box<[u8]>,
    },
    Unlink {
        parent: Ino,
        name: Box<[u8]>,
    },
    Rmdir {
        parent: Ino,
        name: Box<[u8]>,
    },
    Rename {
        from: Ino,
        from_name: Box<[u8]>,
        to: Ino,
        to_name: Box<[u8]>,
    },
    Content {
        ino: Ino,
        chunks: Arc<Chunks>,
        size: u64,
    },
}

impl Op {
    /// The inode a create or content operation is for.
    fn subject(&self) -> Option<Ino> {
        match self {
            Op::Create { child, .. } => Some(*child),
            Op::Content { ino, .. } => Some(*ino),
            _ => None,
        }
    }
}

/// What a flush takes out of the queue.
#[derive(Debug, Default)]
pub(crate) struct Batch {
    pub(crate) ops: Vec<Op>,
    pub(crate) touched: Vec<Ino>,
    pub(crate) elided: HashSet<Ino>,
    pub(crate) seq: u64,
}

impl Batch {
    /// Operations that reach meta: everything except those cancelled by elision.
    pub(crate) fn applied(&self) -> usize {
        self.ops
            .iter()
            .filter(|o| o.subject().is_none_or(|i| !self.elided.contains(&i)))
            .count()
    }
}

#[derive(Debug, Default)]
pub(crate) struct Queue {
    pub(crate) seq: u64,
    pub(crate) gen: u64,
    ops: Vec<Op>,
    content_idx: HashMap<Ino, usize>,
    touched: HashSet<Ino>,
    elided: HashSet<Ino>,
    dirty_files: HashSet<Ino>,
    oldest: Option<Instant>,
}

impl Queue {
    fn note(&mut self) {
        if self.oldest.is_none() {
            self.oldest = Some(Instant::now());
        }
    }

    pub(crate) fn op_count(&self) -> usize {
        self.ops.len()
    }

    pub(crate) fn pending(&self) -> bool {
        !self.ops.is_empty() || !self.touched.is_empty() || !self.dirty_files.is_empty()
    }

    pub(crate) fn age(&self) -> Option<std::time::Duration> {
        self.oldest.map(|t| t.elapsed())
    }

    /// Appends `op`. `structural` nodes are named by it (for elision), `touch` nodes have their
    /// mode and times rewritten when it commits. Returns the mutation's `seq`.
    pub(crate) fn push(&mut self, op: Op, structural: &[&Node], touch: &[&Node]) -> u64 {
        self.seq += 1;
        let s = self.seq;
        self.ops.push(op);
        for n in structural {
            n.struct_ops.fetch_add(1, Ordering::AcqRel);
        }
        for n in touch {
            n.seq.store(s, Ordering::Release);
            self.touched.insert(n.ino);
        }
        self.note();
        s
    }

    /// Records a mutation that has no operation of its own (mode or times).
    pub(crate) fn touch(&mut self, n: &Node) -> u64 {
        self.seq += 1;
        n.seq.store(self.seq, Ordering::Release);
        self.touched.insert(n.ino);
        self.note();
        self.seq
    }

    /// Queues the file's chunk list and size, replacing an earlier queued one for the same inode.
    pub(crate) fn set_content(&mut self, n: &Node, chunks: Arc<Chunks>, size: u64) -> u64 {
        self.seq += 1;
        let op = Op::Content {
            ino: n.ino,
            chunks,
            size,
        };
        match self.content_idx.get(&n.ino) {
            Some(&i) => self.ops[i] = op,
            None => {
                self.content_idx.insert(n.ino, self.ops.len());
                self.ops.push(op);
            }
        }
        n.seq.store(self.seq, Ordering::Release);
        self.touched.insert(n.ino);
        self.note();
        self.seq
    }

    pub(crate) fn add_dirty_file(&mut self, ino: Ino) {
        self.dirty_files.insert(ino);
        self.note();
    }

    pub(crate) fn take_dirty_files(&mut self) -> Vec<Ino> {
        self.dirty_files.drain().collect()
    }

    /// Cancels a create that has not been taken by a flush yet, together with everything queued
    /// for that inode. Only valid when nothing else in the queue names the node.
    pub(crate) fn try_elide(&mut self, n: &Node) -> bool {
        if n.created_gen() == Some(self.gen) && n.struct_ops.load(Ordering::Acquire) == 1 {
            self.elided.insert(n.ino);
            self.content_idx.remove(&n.ino);
            true
        } else {
            false
        }
    }

    pub(crate) fn drain(&mut self, sc: &SnapCtx) -> Batch {
        sc.drained.store(self.seq, Ordering::Release);
        self.gen += 1;
        self.content_idx.clear();
        self.oldest = None;
        Batch {
            ops: std::mem::take(&mut self.ops),
            touched: self.touched.drain().collect(),
            elided: std::mem::take(&mut self.elided),
            seq: self.seq,
        }
    }

    /// Puts a batch that failed to commit back in front of what was queued since.
    pub(crate) fn restore(&mut self, b: Batch) {
        let mut ops = b.ops;
        ops.append(&mut self.ops);
        self.ops = ops;
        self.gen += 1;
        self.content_idx.clear();
        self.touched.extend(b.touched);
        self.elided.extend(b.elided);
        self.note();
    }
}

/// A mounted snapshot.
#[derive(Debug)]
pub(crate) struct SnapCtx {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) snap: Snapshot,
    /// Held by operations that change directory entries.
    pub(crate) ns: Mutex<()>,
    /// Held by whoever commits a batch.
    pub(crate) flush: Mutex<()>,
    pub(crate) q: Mutex<Queue>,
    /// `seq` at the last time the queue was taken.
    pub(crate) drained: AtomicU64,
    /// `seq` at the last committed batch: entities with a `seq` above it are uncommitted.
    pub(crate) flushed: AtomicU64,
    pub(crate) removed: AtomicBool,
    pub(crate) open_handles: AtomicU64,
}

impl SnapCtx {
    pub(crate) fn new(id: u64, name: String, snap: Snapshot) -> Self {
        Self {
            id,
            name,
            snap,
            ns: Mutex::new(()),
            flush: Mutex::new(()),
            q: Mutex::new(Queue::default()),
            drained: AtomicU64::new(0),
            flushed: AtomicU64::new(0),
            removed: AtomicBool::new(false),
            open_handles: AtomicU64::new(0),
        }
    }

    pub(crate) fn flushed(&self) -> u64 {
        self.flushed.load(Ordering::Acquire)
    }

    pub(crate) fn drained(&self) -> u64 {
        self.drained.load(Ordering::Acquire)
    }
}
