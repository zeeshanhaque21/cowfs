//! Bounded cache of read-only pack file handles, so thousands of packs need few descriptors.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use crate::pack;

#[derive(Debug)]
pub(crate) struct FdCache {
    dir: PathBuf,
    cap: usize,
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    tick: u64,
    map: HashMap<u32, (Arc<File>, u64)>,
}

impl FdCache {
    pub(crate) fn new(dir: PathBuf, cap: usize) -> Self {
        Self {
            dir,
            cap: cap.max(1),
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A shared read handle for pack `id`, opening it if it is not cached.
    pub(crate) fn get(&self, id: u32) -> io::Result<Arc<File>> {
        {
            let mut g = self.lock();
            g.tick += 1;
            let tick = g.tick;
            if let Some((f, t)) = g.map.get_mut(&id) {
                *t = tick;
                return Ok(Arc::clone(f));
            }
        }
        let path = pack::pack_path(&self.dir, id);
        let file = match File::open(&path) {
            Ok(f) => f,
            // ENFILE and EMFILE are 23 and 24 on Linux and macOS: drop cached handles and retry once.
            Err(e) if matches!(e.raw_os_error(), Some(23 | 24)) => {
                self.lock().map.clear();
                File::open(&path)?
            }
            Err(e) => return Err(e),
        };
        let file = Arc::new(file);
        let mut g = self.lock();
        g.tick += 1;
        let tick = g.tick;
        let entry = g.map.entry(id).or_insert((file, tick));
        entry.1 = tick;
        let out = Arc::clone(&entry.0);
        while g.map.len() > self.cap {
            let oldest = g.map.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| *k);
            match oldest {
                Some(k) if k != id => g.map.remove(&k),
                _ => break,
            };
        }
        Ok(out)
    }

    #[cfg(test)]
    pub(crate) fn cached(&self) -> usize {
        self.lock().map.len()
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
