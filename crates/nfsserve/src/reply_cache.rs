//! Duplicate request cache. A client that retransmits a non-idempotent call (REMOVE, RENAME,
//! CREATE, ...) must get the original reply, not the error a second execution would produce.
//!
//! A call is identified by the connection it arrived on, the client address, the xid and a hash
//! of the whole call. The connection matters: one client keeps several connections open and each
//! counts xids from its own start, so the same (xid, call) on another connection is a different
//! call, not a retransmission, and must be executed. See `PATCHES.md`.
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// What identifies one call on one connection: retransmissions repeat all four.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub client: IpAddr,
    pub conn: u64,
    pub xid: u32,
    pub fingerprint: u64,
}

impl CacheKey {
    /// The same call on any connection.
    fn call(&self) -> (IpAddr, u32, u64) {
        (self.client, self.xid, self.fingerprint)
    }
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
    /// The call each remembered one answers to when it arrives on another connection.
    cross: HashMap<(IpAddr, u32, u64), CacheKey>,
    /// Connections that have gone away, oldest first.
    closed: VecDeque<u64>,
    closed_set: HashSet<u64>,
    bytes: usize,
}

/// See the module docs.
#[derive(Debug)]
pub struct ReplyCache {
    max_entries: usize,
    max_bytes: usize,
    max_age: Duration,
    inner: Mutex<Inner>,
    next_conn: AtomicU64,
}

impl ReplyCache {
    pub fn new(max_entries: usize, max_bytes: usize, max_age: Duration) -> Self {
        Self {
            max_entries,
            max_bytes,
            max_age,
            inner: Mutex::new(Inner::default()),
            next_conn: AtomicU64::new(1),
        }
    }

    /// A connection identity, unique for the life of the server.
    pub fn open_conn(&self) -> u64 {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    /// Notes that a connection is gone. A call it ran may then be replayed for another
    /// connection, which is what a client that lost its connection and reconnected does.
    pub fn close_conn(&self, conn: u64) {
        let mut g = self.lock();
        if g.closed_set.insert(conn) {
            g.closed.push_back(conn);
        }
        while g.closed.len() > self.max_entries.saturating_mul(2).max(64) {
            match g.closed.pop_front() {
                Some(old) => {
                    g.closed_set.remove(&old);
                }
                None => break,
            }
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
        // A call that arrived on a connection which is gone may be a retransmission of one the
        // client sent before it reconnected, and then must not run again. A call from a
        // connection that is still open is a new call: the client runs several at once and each
        // counts xids from its own start.
        if let Some(&orig) = g.cross.get(&key.call()) {
            if orig.conn != key.conn && g.closed_set.contains(&orig.conn) {
                return match &g.map.get(&orig).map(|e| &e.state) {
                    Some(State::InProgress) | None => Begin::InProgress,
                    Some(State::Done(r)) => Begin::Replay(r.clone()),
                };
            }
        }
        g.map.insert(
            key,
            Entry {
                state: State::InProgress,
                at: now,
            },
        );
        g.order.push_back(key);
        g.cross.insert(key.call(), key);
        Begin::New
    }

    /// Records the reply of a finished call, or forgets the call if it produced none.
    pub fn finish(&self, key: CacheKey, reply: Option<Vec<u8>>) {
        let mut g = self.lock();
        if reply.is_none() {
            g.cross.remove(&key.call());
        }
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
            if g.cross.get(&key.call()) == Some(&key) {
                g.cross.remove(&key.call());
            }
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
            conn: 1,
            xid,
            fingerprint: 9,
        }
    }

    fn on(conn: u64, xid: u32) -> CacheKey {
        CacheKey { conn, ..key(xid) }
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
    fn the_same_call_on_a_live_connection_is_not_a_retransmission() {
        let c = cache(100, 60_000);
        assert!(matches!(c.begin(on(1, 7)), Begin::New));
        c.finish(on(1, 7), Some(vec![1]));
        assert!(
            matches!(c.begin(on(2, 7)), Begin::New),
            "another open connection is a new call"
        );
    }

    #[test]
    fn the_same_call_after_a_reconnect_replays() {
        let c = cache(100, 60_000);
        c.close_conn(1);
        assert!(matches!(c.begin(on(1, 7)), Begin::New));
        c.finish(on(1, 7), Some(vec![9]));
        match c.begin(on(2, 7)) {
            Begin::Replay(r) => assert_eq!(*r, vec![9]),
            other => panic!("{other:?}"),
        }
        c.close_conn(2);
        assert!(
            matches!(c.begin(on(3, 7)), Begin::Replay(_)),
            "and every later connection gets the same answer"
        );
    }

    #[test]
    fn a_call_still_running_never_gets_a_reply_for_another_connection() {
        let c = cache(100, 60_000);
        assert!(matches!(c.begin(on(1, 7)), Begin::New));
        c.close_conn(1);
        assert!(matches!(c.begin(on(2, 7)), Begin::InProgress));
    }

    #[test]
    fn closed_connections_are_forgotten_in_bounded_time() {
        let c = cache(4, 60_000);
        for i in 0..1000 {
            c.close_conn(i);
        }
        let g = c.lock();
        assert!(g.closed.len() <= 64, "{}", g.closed.len());
        assert!(!g.closed_set.contains(&0), "the oldest are dropped");
    }

    #[test]
    fn a_failed_call_is_forgotten() {
        let c = cache(10, 60_000);
        assert!(matches!(c.begin(key(1)), Begin::New));
        c.finish(key(1), None);
        assert!(matches!(c.begin(key(1)), Begin::New));
    }

    #[test]
    fn the_cross_index_does_not_outlive_its_entries() {
        let c = cache(50, 60_000);
        for i in 0..5000u32 {
            c.begin(key(i));
            c.finish(key(i), Some(vec![0; 16]));
        }
        let g = c.lock();
        assert!(
            g.cross.len() <= g.map.len(),
            "{} vs {}",
            g.cross.len(),
            g.map.len()
        );
        for call in g.cross.keys() {
            assert!(g.map.contains_key(&g.cross[call]));
        }
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
        assert!(g.cross.capacity() <= 4 * 100 + 64 + 100);
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
