//! The dentry cache: positive and negative entries, bounded, exact under this process's mutations.

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::sync::Mutex;

use cowfs_vfs::{FileKind, Ino};

use crate::ino::snap_of;
use crate::util::{MutexExt, SHARDS};

/// What a name resolves to: `None` is a cached "no such name".
pub(crate) type Target = Option<(Ino, FileKind)>;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Dent {
    pub(crate) target: Target,
    /// `seq` of the mutation that wrote this entry; 0 for entries read from meta.
    pub(crate) seq: u64,
}

#[derive(Debug, Default)]
struct DShard {
    dirs: HashMap<Ino, HashMap<Box<[u8]>, Dent>>,
    count: usize,
    epoch: u64,
}

#[derive(Debug)]
pub(crate) struct DCache {
    shards: Box<[Mutex<DShard>]>,
    hasher: RandomState,
    cap_per_shard: usize,
}

impl DCache {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            shards: (0..SHARDS).map(|_| Mutex::new(DShard::default())).collect(),
            hasher: RandomState::new(),
            cap_per_shard: (cap / SHARDS).max(16),
        }
    }

    fn shard(&self, dir: Ino) -> std::sync::MutexGuard<'_, DShard> {
        self.shards[self.hasher.hash_one(dir) as usize % SHARDS].lk()
    }

    pub(crate) fn get(&self, dir: Ino, name: &[u8]) -> Option<Dent> {
        self.shard(dir).dirs.get(&dir)?.get(name).copied()
    }

    pub(crate) fn epoch(&self, dir: Ino) -> u64 {
        self.shard(dir).epoch
    }

    /// Caches what meta said, unless the shard changed since `epoch` was read or an entry exists.
    pub(crate) fn fill(&self, dir: Ino, name: &[u8], target: Target, epoch: u64) {
        let mut s = self.shard(dir);
        if s.epoch != epoch {
            return;
        }
        let d = s.dirs.entry(dir).or_default();
        if d.contains_key(name) {
            return;
        }
        d.insert(name.into(), Dent { target, seq: 0 });
        s.count += 1;
    }

    /// Like `fill` for a whole page of positive entries from one directory read.
    pub(crate) fn fill_many(&self, dir: Ino, entries: &[(&[u8], Target)], epoch: u64) {
        let mut s = self.shard(dir);
        if s.epoch != epoch {
            return;
        }
        let mut added = 0;
        let d = s.dirs.entry(dir).or_default();
        for (name, target) in entries {
            if !d.contains_key(*name) {
                d.insert(
                    (*name).into(),
                    Dent {
                        target: *target,
                        seq: 0,
                    },
                );
                added += 1;
            }
        }
        s.count += added;
    }

    /// Writes an entry that reflects a mutation (`seq` above the snapshot's `flushed` while
    /// uncommitted).
    pub(crate) fn put(&self, dir: Ino, name: &[u8], target: Target, seq: u64) {
        let mut s = self.shard(dir);
        s.epoch += 1;
        let d = s.dirs.entry(dir).or_default();
        if d.insert(name.into(), Dent { target, seq }).is_none() {
            s.count += 1;
        }
    }

    pub(crate) fn bump_all(&self) {
        for s in &*self.shards {
            s.lk().epoch += 1;
        }
    }

    pub(crate) fn over_cap(&self, dir: Ino) -> bool {
        self.shard(dir).count > self.cap_per_shard
    }

    pub(crate) fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lk().count).sum()
    }

    /// Drops every entry of snapshot `snap`.
    pub(crate) fn purge_snapshot(&self, snap: u64) {
        for s in &*self.shards {
            let mut g = s.lk();
            g.dirs.retain(|d, _| snap_of(*d) != Some(snap));
            g.count = g.dirs.values().map(HashMap::len).sum();
            g.epoch += 1;
        }
    }

    /// Removes every clean entry.
    pub(crate) fn shrink_all(&self, flushed: &dyn Fn(Ino) -> u64) {
        for sh in &*self.shards {
            let mut s = sh.lk();
            let mut removed = 0;
            for (d, names) in s.dirs.iter_mut() {
                let fl = flushed(*d);
                let before = names.len();
                names.retain(|_, e| e.seq > fl);
                removed += before - names.len();
            }
            s.dirs.retain(|_, n| !n.is_empty());
            s.count -= removed;
            s.epoch += 1;
        }
    }

    /// Evicts clean entries (`seq` at most the snapshot's flushed value) from `dir`'s shard while
    /// it is over its bound.
    pub(crate) fn shrink(&self, dir: Ino, flushed: &dyn Fn(Ino) -> u64) {
        let mut s = self.shard(dir);
        if s.count <= self.cap_per_shard {
            return;
        }
        let target = self.cap_per_shard * 3 / 4;
        let mut over = s.count - target;
        let mut removed = 0;
        for (d, names) in s.dirs.iter_mut() {
            if over == 0 {
                break;
            }
            let fl = flushed(*d);
            names.retain(|_, e| {
                if over > 0 && e.seq <= fl {
                    over -= 1;
                    removed += 1;
                    false
                } else {
                    true
                }
            });
        }
        s.dirs.retain(|_, n| !n.is_empty());
        s.count -= removed;
        s.epoch += 1;
    }
}
