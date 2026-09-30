//! Small helpers: poison-tolerant locks and a sharded map with a per-shard epoch.

use std::collections::HashMap;
use std::hash::{BuildHasher, Hash, RandomState};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub(crate) trait MutexExt<T> {
    fn lk(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lk(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) trait RwExt<T> {
    fn rd(&self) -> RwLockReadGuard<'_, T>;
    fn wr(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwExt<T> for RwLock<T> {
    fn rd(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(|e| e.into_inner())
    }

    fn wr(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) const SHARDS: usize = 64;

#[derive(Debug)]
pub(crate) struct Shard<K, V> {
    pub map: HashMap<K, V>,
    /// Bumped by every mutation, commit and eviction: a fill that started before it is stale.
    pub epoch: u64,
}

/// A hash map split into shards, each behind its own mutex.
#[derive(Debug)]
pub(crate) struct ShardMap<K, V> {
    shards: Box<[Mutex<Shard<K, V>>]>,
    hasher: RandomState,
}

impl<K: Hash + Eq + Clone, V: Clone> ShardMap<K, V> {
    pub(crate) fn new() -> Self {
        Self {
            shards: (0..SHARDS)
                .map(|_| {
                    Mutex::new(Shard {
                        map: HashMap::new(),
                        epoch: 0,
                    })
                })
                .collect(),
            hasher: RandomState::new(),
        }
    }

    fn shard(&self, k: &K) -> MutexGuard<'_, Shard<K, V>> {
        let h = self.hasher.hash_one(k) as usize;
        self.shards[h % SHARDS].lk()
    }

    pub(crate) fn get(&self, k: &K) -> Option<V> {
        self.shard(k).map.get(k).cloned()
    }

    pub(crate) fn epoch(&self, k: &K) -> u64 {
        self.shard(k).epoch
    }

    /// Inserts unless present (the present value wins and is returned). `Err` if the shard
    /// changed since `epoch` was read, so the caller's data may be stale.
    pub(crate) fn insert_if(&self, k: K, v: V, epoch: u64) -> Result<V, ()> {
        let mut s = self.shard(&k);
        if let Some(cur) = s.map.get(&k) {
            return Ok(cur.clone());
        }
        if s.epoch != epoch {
            return Err(());
        }
        s.map.insert(k, v.clone());
        Ok(v)
    }

    pub(crate) fn upsert(&self, k: K, v: V) {
        let mut s = self.shard(&k);
        s.epoch += 1;
        s.map.insert(k, v);
    }

    /// Removes `k` when `pred` holds for its value, all under the shard lock.
    pub(crate) fn remove_if(&self, k: &K, pred: impl FnOnce(&V) -> bool) -> bool {
        let mut s = self.shard(k);
        let hit = s.map.get(k).is_some_and(pred);
        if hit {
            s.epoch += 1;
            s.map.remove(k);
        }
        hit
    }

    pub(crate) fn bump_all(&self) {
        for s in &*self.shards {
            s.lk().epoch += 1;
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lk().map.len()).sum()
    }

    pub(crate) fn shard_len(&self, k: &K) -> usize {
        self.shard(k).map.len()
    }

    /// Calls `f` on every entry, one shard at a time. `f` returns false to remove the entry.
    pub(crate) fn retain(&self, mut f: impl FnMut(&K, &V) -> bool) {
        for s in &*self.shards {
            let mut g = s.lk();
            let before = g.map.len();
            g.map.retain(|k, v| f(k, v));
            if g.map.len() != before {
                g.epoch += 1;
            }
        }
    }

    /// Shrinks the shard of `k` toward `target` entries by evicting entries `evictable` allows.
    pub(crate) fn shrink_shard_of(
        &self,
        k: &K,
        target: usize,
        mut evictable: impl FnMut(&K, &V) -> bool,
    ) {
        let mut s = self.shard(k);
        if s.map.len() <= target {
            return;
        }
        let mut over = s.map.len() - target;
        let before = over;
        s.map.retain(|k, v| {
            if over > 0 && evictable(k, v) {
                over -= 1;
                false
            } else {
                true
            }
        });
        if over != before {
            s.epoch += 1;
        }
    }
}
