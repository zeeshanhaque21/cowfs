//! Duplicate request cache. A client that retransmits a non-idempotent call (REMOVE, RENAME,
//! CREATE, ...) must get the original reply, not the error a second execution would produce.
//!
//! A call is identified by the connection it arrived on, the client address, the xid and a hash
//! of the whole call. The connection matters: one client keeps several connections open and each
//! counts xids from its own start, so the same (xid, call) on another connection is a different
//! call, not a retransmission, and must be executed. See `PATCHES.md`.
//!
//! One call may still be replayed on another connection: if the connection that ran it had sent
//! nothing since, it never saw a reply, which is the only shape a post-reconnect resend can take.
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
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
    /// The highest xid each connection has sent, which says whether it moved on past a call.
    high: HashMap<u64, u32>,
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

    /// Notes the xid a connection just sent, and answers whether that connection has sent
    /// anything since a given xid. A connection that has moved on got the reply for that xid, so
    /// a later call with it on another connection is not its retransmission.
    pub fn note_xid(&self, conn: u64, xid: u32) {
        let mut g = self.lock();
        let high = g.high.entry(conn).or_insert(0);
        *high = (*high).max(xid);
        if g.high.len() > self.max_entries.saturating_mul(2).max(64) {
            let mut idle: Vec<(u64, u32)> = g.high.iter().map(|(c, h)| (*c, *h)).collect();
            idle.sort_unstable();
            for (c, _) in idle.into_iter().take(g.high.len() / 2) {
                g.high.remove(&c);
            }
        }
    }

    fn moved_on(&self, g: &Inner, conn: u64, xid: u32) -> bool {
        g.high.get(&conn).is_some_and(|h| *h > xid)
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
        // A call from another connection may be a retransmission of one the client sent before
        // it reconnected, and then must not run again. It can only be that if the connection
        // that ran it has sent nothing since, because a client that got the reply moved on.
        if let Some(&orig) = g.cross.get(&key.call()) {
            if orig.conn != key.conn && !self.moved_on(&g, orig.conn, key.xid) {
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
    fn a_call_from_another_connection_is_a_new_call_once_the_client_moved_on() {
        let c = cache(100, 60_000);
        c.note_xid(1, 7);
        assert!(matches!(c.begin(on(1, 7)), Begin::New));
        c.finish(on(1, 7), Some(vec![1]));
        c.note_xid(1, 8);
        assert!(
            matches!(c.begin(on(2, 7)), Begin::New),
            "the first connection sent xid 8, so it got the reply to 7"
        );
    }

    #[test]
    fn a_call_the_client_never_got_a_reply_to_replays_on_a_new_connection() {
        let c = cache(100, 60_000);
        c.note_xid(1, 7);
        assert!(matches!(c.begin(on(1, 7)), Begin::New));
        c.finish(on(1, 7), Some(vec![9]));
        match c.begin(on(2, 7)) {
            Begin::Replay(r) => assert_eq!(*r, vec![9]),
            other => panic!("{other:?}"),
        }
        assert!(
            matches!(c.begin(on(3, 7)), Begin::Replay(_)),
            "and every later connection gets the same answer"
        );
    }

    #[test]
    fn a_call_still_running_never_gets_a_reply_for_another_connection() {
        let c = cache(100, 60_000);
        c.note_xid(1, 7);
        assert!(matches!(c.begin(on(1, 7)), Begin::New));
        assert!(matches!(c.begin(on(2, 7)), Begin::InProgress));
    }

    #[test]
    fn the_connection_bookkeeping_is_bounded() {
        let c = cache(4, 60_000);
        for i in 0..10_000u64 {
            c.note_xid(i, i as u32);
        }
        let g = c.lock();
        assert!(g.high.len() <= 64, "{}", g.high.len());
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
