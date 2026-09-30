//! Duplicate request cache. A client that retransmits a non-idempotent call (REMOVE, RENAME,
//! CREATE, ...) must get the original reply, not the error a second execution would produce.
//! Entries are keyed by client address, xid and a hash of the whole call, and are bounded in
//! count, bytes and age.
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// What identifies one call: retransmissions repeat all three.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub client: IpAddr,
    pub xid: u32,
    pub fingerprint: u64,
}

/// What `ReplyCache::begin` decided.
#[derive(Debug)]
pub enum Begin {
    /// First time this call is seen: execute it and report the outcome with `finish`.
    New,
    /// The original is still running: drop the retransmission, its reply will come.
    InProgress,
    /// The original finished: send this reply again.
    Replay(Arc<Vec<u8>>),
}

#[derive(Debug)]
enum State {
    InProgress,
    Done(Arc<Vec<u8>>),
}

#[derive(Debug)]
struct Entry {
    state: State,
    at: Instant,
}

#[derive(Debug, Default)]
struct Inner {
    map: HashMap<CacheKey, Entry>,
    order: VecDeque<CacheKey>,
    bytes: usize,
}

/// See the module docs.
#[derive(Debug)]
pub struct ReplyCache {
    max_entries: usize,
    max_bytes: usize,
    max_age: Duration,
    inner: Mutex<Inner>,
}

impl ReplyCache {
    pub fn new(max_entries: usize, max_bytes: usize, max_age: Duration) -> Self {
        Self {
            max_entries,
            max_bytes,
            max_age,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().map.len()
    }

    pub fn begin(&self, key: CacheKey) -> Begin {
        let now = Instant::now();
        let mut g = self.lock();
        self.evict(&mut g, now);
        if let Some(e) = g.map.get(&key) {
            return match &e.state {
                State::InProgress => Begin::InProgress,
                State::Done(r) => Begin::Replay(r.clone()),
            };
        }
        g.map.insert(
            key,
            Entry {
                state: State::InProgress,
                at: now,
            },
        );
        g.order.push_back(key);
        Begin::New
    }

    /// Records the reply of a finished call, or forgets the call if it produced none.
    pub fn finish(&self, key: CacheKey, reply: Option<Vec<u8>>) {
        let mut g = self.lock();
        match reply {
            Some(r) => {
                let len = r.len();
                if let Some(e) = g.map.get_mut(&key) {
                    e.state = State::Done(Arc::new(r));
                    e.at = Instant::now();
                    g.bytes += len;
                }
            }
            None => {
                g.map.remove(&key);
            }
        }
        self.evict(&mut g, Instant::now());
    }

    fn evict(&self, g: &mut Inner, now: Instant) {
        while let Some(key) = g.order.front().copied() {
            let stale_key = !g.map.contains_key(&key);
            let expired = g
                .map
                .get(&key)
                .is_some_and(|e| now.duration_since(e.at) >= self.max_age);
            let over = g.map.len() > self.max_entries
                || g.bytes > self.max_bytes
                || g.order.len() > self.max_entries * 2;
            if !(stale_key || expired || over) {
                break;
            }
            g.order.pop_front();
            if let Some(Entry {
                state: State::Done(r),
                ..
            }) = g.map.remove(&key)
            {
                g.bytes = g.bytes.saturating_sub(r.len());
            }
        }
        if g.map.capacity() > 4 * g.map.len() + 64 {
            g.map.shrink_to_fit();
        }
        if g.order.capacity() > 4 * g.order.len() + 64 {
            g.order.shrink_to_fit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(xid: u32) -> CacheKey {
        CacheKey {
            client: IpAddr::from([127, 0, 0, 1]),
            xid,
            fingerprint: 9,
        }
    }

    fn cache(entries: usize, age_ms: u64) -> ReplyCache {
        ReplyCache::new(entries, 1 << 20, Duration::from_millis(age_ms))
    }

    #[test]
    fn a_finished_call_replays_and_an_unfinished_one_is_dropped() {
        let c = cache(10, 60_000);
        assert!(matches!(c.begin(key(1)), Begin::New));
        assert!(matches!(c.begin(key(1)), Begin::InProgress));
        c.finish(key(1), Some(vec![1, 2, 3]));
        match c.begin(key(1)) {
            Begin::Replay(r) => assert_eq!(*r, vec![1, 2, 3]),
            other => panic!("{other:?}"),
        }
        assert!(matches!(c.begin(key(2)), Begin::New), "another xid");
        let other_args = CacheKey {
            fingerprint: 10,
            ..key(1)
        };
        assert!(
            matches!(c.begin(other_args), Begin::New),
            "same xid, other call"
        );
    }

    #[test]
    fn a_failed_call_is_forgotten() {
        let c = cache(10, 60_000);
        assert!(matches!(c.begin(key(1)), Begin::New));
        c.finish(key(1), None);
        assert!(matches!(c.begin(key(1)), Begin::New));
    }

    #[test]
    fn size_is_bounded_and_shrinks() {
        let c = cache(100, 60_000);
        for i in 0..50_000 {
            assert!(matches!(c.begin(key(i)), Begin::New));
            c.finish(key(i), Some(vec![0; 64]));
        }
        assert!(c.len() <= 100, "{}", c.len());
        let g = c.lock();
        assert!(
            g.map.capacity() <= 4 * 100 + 64 + 100,
            "{}",
            g.map.capacity()
        );
        assert!(g.bytes <= 100 * 64);
    }

    #[test]
    fn old_entries_expire() {
        let c = cache(100, 20);
        assert!(matches!(c.begin(key(1)), Begin::New));
        c.finish(key(1), Some(vec![1]));
        std::thread::sleep(Duration::from_millis(60));
        assert!(matches!(c.begin(key(1)), Begin::New));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn bytes_are_bounded() {
        let c = ReplyCache::new(1000, 1000, Duration::from_secs(60));
        for i in 0..100 {
            c.begin(key(i));
            c.finish(key(i), Some(vec![0; 100]));
        }
        assert!(c.lock().bytes <= 1000);
    }
}
