//! Verified block reads through a two-generation cache in front of the store.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use cowfs_store::{BlockId, Store, MAX_BLOCK_LEN};
use cowfs_vfs::Result;

use crate::error::from_store;
use crate::util::MutexExt;

#[derive(Debug, Default)]
struct Gens {
    cur: HashMap<BlockId, Arc<Vec<u8>>>,
    prev: HashMap<BlockId, Arc<Vec<u8>>>,
    cur_bytes: usize,
}

/// The block store plus a cache of verified blocks.
///
/// A block enters the cache only after `Store::get` verified it or after this process hashed
/// it on the way in, so a hit is never unverified data.
#[derive(Debug)]
pub(crate) struct Blocks {
    pub(crate) store: Arc<Store>,
    half: usize,
    gens: Mutex<Gens>,
}

impl Blocks {
    pub(crate) fn new(store: Arc<Store>, cache_bytes: usize) -> Self {
        Self {
            store,
            half: (cache_bytes / 2).max(MAX_BLOCK_LEN),
            gens: Mutex::new(Gens::default()),
        }
    }

    fn insert(&self, id: BlockId, data: Arc<Vec<u8>>) {
        let mut g = self.gens.lk();
        g.cur_bytes += data.len();
        g.cur.insert(id, data);
        if g.cur_bytes > self.half {
            g.prev = std::mem::take(&mut g.cur);
            g.cur_bytes = 0;
        }
    }

    /// The uncompressed bytes of block `id`, hash-verified.
    pub(crate) fn get(&self, id: BlockId) -> Result<Arc<Vec<u8>>> {
        {
            let mut g = self.gens.lk();
            if let Some(b) = g.cur.get(&id) {
                return Ok(b.clone());
            }
            if let Some(b) = g.prev.get(&id).cloned() {
                g.cur_bytes += b.len();
                g.cur.insert(id, b.clone());
                return Ok(b);
            }
        }
        let data = Arc::new(self.store.get(id).map_err(from_store)?);
        self.insert(id, data.clone());
        Ok(data)
    }

    /// Stores `data` (at most 256 KiB) and remembers it for reads.
    pub(crate) fn put(&self, data: &[u8]) -> Result<BlockId> {
        let id = self.store.put(data).map_err(from_store)?;
        self.insert(id, Arc::new(data.to_vec()));
        Ok(id)
    }

    pub(crate) fn clear(&self) {
        let mut g = self.gens.lk();
        g.cur.clear();
        g.prev.clear();
        g.cur_bytes = 0;
    }
}
