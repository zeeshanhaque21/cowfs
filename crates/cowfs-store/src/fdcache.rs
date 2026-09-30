//! Bounded cache of read-only pack file handles, so thousands of packs need few descriptors.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, PoisonError, RwLock, RwLockWriteGuard};

use crate::pack;

#[derive(Debug)]
struct Slot {
    file: Arc<File>,
    used: AtomicU64,
}

#[derive(Debug)]
pub(crate) struct FdCache {
    dir: PathBuf,
    cap: usize,
    tick: AtomicU64,
    map: RwLock<HashMap<u32, Slot>>,
}

impl FdCache {
    pub(crate) fn new(dir: PathBuf, cap: usize) -> Self {
        Self {
            dir,
            cap: cap.max(1),
            tick: AtomicU64::new(0),
            map: RwLock::new(HashMap::new()),
        }
    }

    fn write(&self) -> RwLockWriteGuard<'_, HashMap<u32, Slot>> {
        self.map.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// A shared read handle for pack `id`, opening it if it is not cached. A hit takes only a read lock.
    pub(crate) fn get(&self, id: u32) -> io::Result<Arc<File>> {
        {
            let g = self.map.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(slot) = g.get(&id) {
                slot.used.store(self.tick.fetch_add(1, Relaxed), Relaxed);
                return Ok(Arc::clone(&slot.file));
            }
        }
        let path = pack::pack_path(&self.dir, id);
        let file = match File::open(&path) {
            Ok(f) => f,
            // ENFILE and EMFILE are 23 and 24 on Linux and macOS: drop cached handles and retry once.
            Err(e) if matches!(e.raw_os_error(), Some(23 | 24)) => {
                self.write().clear();
                File::open(&path)?
            }
            Err(e) => return Err(e),
        };
        let mut g = self.write();
        let used = self.tick.fetch_add(1, Relaxed);
        let out = Arc::clone(
            &g.entry(id)
                .or_insert_with(|| Slot {
                    file: Arc::new(file),
                    used: AtomicU64::new(used),
                })
                .file,
        );
        while g.len() > self.cap {
            let oldest = g
                .iter()
                .filter(|(k, _)| **k != id)
                .min_by_key(|(_, s)| s.used.load(Relaxed))
                .map(|(k, _)| *k);
            match oldest {
                Some(k) => g.remove(&k),
                None => break,
            };
        }
        Ok(out)
    }

    #[cfg(test)]
    pub(crate) fn cached(&self) -> usize {
        self.map
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_holds_more_than_cap() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(pack::pack_dir(dir.path())).unwrap();
        for i in 0..10 {
            std::fs::write(pack::pack_path(dir.path(), i), b"x").unwrap();
        }
        let c = FdCache::new(dir.path().to_path_buf(), 3);
        for i in 0..10 {
            c.get(i).unwrap();
            assert!(c.cached() <= 3);
        }
        assert!(c.get(99).is_err());
    }
}
