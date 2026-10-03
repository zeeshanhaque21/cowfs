//! The reference gate: the one place a garbage collector can hold the reference side still.
//!
//! A *reader* is a thread that may acquire a reference to an existing block: it stores file data
//! or commits a chunk list. A *barrier* is the collector, which needs no reader to be inside for
//! as long as it holds the gate. Design and lock order: `docs/gc-core-integration.md`.
//!
//! This is a counter and a condvar, not a `RwLock`, because the collector's guard has to be
//! `Send + Sync` and a std write guard is not.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::util::MutexExt;

thread_local! {
    /// Gates this thread is inside, with how many times, so a nested enter is admitted.
    static INSIDE: RefCell<Vec<(usize, u32)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Open,
    Draining,
    Held,
}

#[derive(Debug)]
struct State {
    readers: usize,
    phase: Phase,
    waiting: usize,
}

/// See the module docs.
#[derive(Debug)]
pub(crate) struct Gate {
    st: Mutex<State>,
    cv: Condvar,
    /// Test seam, see `Core::set_gate_fault`: 1 makes `take` succeed without closing the gate.
    fault: AtomicU8,
}

/// Proof that the calling thread may acquire references. Not `Send`: it belongs to one thread.
#[derive(Debug)]
pub(crate) struct Entry<'g> {
    gate: &'g Gate,
    _thread: PhantomData<*const ()>,
}

/// The barrier, held. Dropping it reopens the gate.
#[derive(Debug)]
pub(crate) struct Hold {
    gate: Arc<Gate>,
    closed: bool,
}

impl Gate {
    pub(crate) fn new() -> Self {
        Self {
            st: Mutex::new(State {
                readers: 0,
                phase: Phase::Open,
                waiting: 0,
            }),
            cv: Condvar::new(),
            fault: AtomicU8::new(0),
        }
    }

    fn id(&self) -> usize {
        std::ptr::from_ref(self) as usize
    }

    /// Records one more entry on this thread. True when the thread was already inside.
    fn note_enter(&self) -> bool {
        INSIDE.with(|v| {
            let mut v = v.borrow_mut();
            match v.iter_mut().find(|(g, _)| *g == self.id()) {
                Some((_, n)) => {
                    *n += 1;
                    true
                }
                None => {
                    v.push((self.id(), 1));
                    false
                }
            }
        })
    }

    fn inside(&self) -> bool {
        INSIDE.with(|v| v.borrow().iter().any(|(g, _)| *g == self.id()))
    }

    /// Waits until no barrier is draining or held, then enters. Call with no node, namespace or
    /// flush lock held: it blocks for as long as a barrier is held.
    pub(crate) fn enter(&self) -> Entry<'_> {
        if !self.inside() {
            let mut s = self.st.lk();
            if s.phase != Phase::Open {
                s.waiting += 1;
                while s.phase != Phase::Open {
                    s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner());
                }
                s.waiting -= 1;
            }
            s.readers += 1;
        }
        self.note_enter();
        self.entry()
    }

    /// Enters if no barrier is draining or held, and never blocks. For a caller that holds a node
    /// lock and can defer its work.
    pub(crate) fn try_enter(&self) -> Option<Entry<'_>> {
        if !self.inside() {
            let mut s = self.st.lk();
            if s.phase != Phase::Open {
                return None;
            }
            s.readers += 1;
        }
        self.note_enter();
        Some(self.entry())
    }

    fn entry(&self) -> Entry<'_> {
        Entry {
            gate: self,
            _thread: PhantomData,
        }
    }

    /// Closes the gate: waits up to `patience` for every reader to leave, then holds it. `None`
    /// when the readers did not leave in time, another barrier is held, or the caller is itself
    /// inside; the gate is left open in every one of those cases.
    pub(crate) fn take(self: &Arc<Self>, patience: Duration) -> Option<Hold> {
        if self.inside() {
            return None;
        }
        if self.fault.load(Ordering::Acquire) == 1 {
            return Some(Hold {
                gate: Arc::clone(self),
                closed: false,
            });
        }
        let deadline = Instant::now() + patience;
        let mut s = self.st.lk();
        // Hand off to parked readers first. A collector re-takes the barrier immediately between
        // packs, and a reader that was already waiting loses the race for the mutex every time, so
        // it can wait out a whole sweep instead of one pack. Letting the queue drain before closing
        // again gives them their turn; each entry is one commit, so this is short. The wait is
        // strictly bounded and never abandons the sweep: if the queue is still busy after the small
        // handoff slice, the barrier closes anyway and the collector makes progress. This is
        // fairness only, not a safety gate, so it must not turn a busy reader into a lost pack.
        const HANDOFF: Duration = Duration::from_millis(50);
        let handoff = Instant::now() + HANDOFF;
        if s.waiting > 0 {
            self.cv.notify_all();
        }
        while s.waiting > 0 && Instant::now() < handoff {
            let left = handoff.saturating_duration_since(Instant::now());
            s = self
                .cv
                .wait_timeout(s, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if s.phase != Phase::Open {
            return None;
        }
        s.phase = Phase::Draining;
        while s.readers > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                s.phase = Phase::Open;
                self.cv.notify_all();
                return None;
            }
            s = self
                .cv
                .wait_timeout(s, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        s.phase = Phase::Held;
        Some(Hold {
            gate: Arc::clone(self),
            closed: true,
        })
    }

    /// Threads parked in `enter`. For tests.
    pub(crate) fn waiting(&self) -> usize {
        self.st.lk().waiting
    }

    pub(crate) fn set_fault(&self, kind: u8) {
        self.fault.store(kind, Ordering::Release);
    }
}

impl Drop for Entry<'_> {
    fn drop(&mut self) {
        let last = INSIDE.with(|v| {
            let mut v = v.borrow_mut();
            let Some(i) = v.iter().position(|(g, _)| *g == self.gate.id()) else {
                return false;
            };
            v[i].1 -= 1;
            if v[i].1 == 0 {
                v.swap_remove(i);
                true
            } else {
                false
            }
        });
        if last {
            let mut s = self.gate.st.lk();
            s.readers -= 1;
            if s.readers == 0 && s.phase == Phase::Draining {
                self.gate.cv.notify_all();
            }
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if self.closed {
            self.gate.st.lk().phase = Phase::Open;
            self.gate.cv.notify_all();
        }
    }
}

impl cowfs_gc::Held for Hold {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn is_send_sync<T: Send + Sync>() {}

    #[test]
    fn the_hold_can_cross_threads() {
        is_send_sync::<Hold>();
    }

    #[test]
    fn a_reader_inside_makes_take_give_up_and_leaves_the_gate_open() {
        let g = Arc::new(Gate::new());
        let e = g.enter();
        let g2 = Arc::clone(&g);
        let r = std::thread::spawn(move || g2.take(Duration::from_millis(50)).is_none());
        assert!(
            r.join().unwrap(),
            "take must time out while a reader is inside"
        );
        drop(e);
        drop(g.try_enter().expect("the gate is open again"));
        assert!(g.take(Duration::from_millis(50)).is_some());
    }

    #[test]
    fn an_entrant_parks_while_held_and_runs_when_the_hold_drops() {
        let g = Arc::new(Gate::new());
        let hold = g
            .take(Duration::from_secs(1))
            .expect("no reader, so it holds");
        assert!(
            g.try_enter().is_none(),
            "try_enter never blocks and never gets in"
        );
        let g2 = Arc::clone(&g);
        let t = std::thread::spawn(move || {
            let _e = g2.enter();
        });
        let start = Instant::now();
        while g.waiting() == 0 {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "the entrant never parked"
            );
            std::thread::yield_now();
        }
        drop(hold);
        t.join().unwrap();
        assert_eq!(g.waiting(), 0);
    }

    #[test]
    fn a_nested_enter_is_admitted_while_a_barrier_drains() {
        let g = Arc::new(Gate::new());
        let outer = g.enter();
        let g2 = Arc::clone(&g);
        let t = std::thread::spawn(move || g2.take(Duration::from_secs(5)).is_some());
        let start = Instant::now();
        while g.st.lk().phase != Phase::Draining {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "take never started"
            );
            std::thread::yield_now();
        }
        drop(g.enter());
        assert!(
            g.try_enter().is_some(),
            "a thread already inside is admitted"
        );
        drop(outer);
        assert!(
            t.join().unwrap(),
            "the barrier gets in once the reader leaves"
        );
    }

    #[test]
    fn a_second_barrier_does_not_wait_for_the_first() {
        let g = Arc::new(Gate::new());
        let _first = g.take(Duration::from_secs(1)).unwrap();
        assert!(g.take(Duration::from_secs(1)).is_none());
    }

    #[test]
    fn a_re_take_hands_the_gate_to_a_parked_reader_first() {
        // A collector drops one hold and takes the next immediately. A reader parked in `enter`
        // must get in during that gap, not starve for the whole sweep.
        let g = Arc::new(Gate::new());
        let first = g.take(Duration::from_secs(1)).unwrap();
        let g2 = Arc::clone(&g);
        let entered = Arc::new(AtomicBool::new(false));
        let e2 = Arc::clone(&entered);
        let t = std::thread::spawn(move || {
            let _e = g2.enter();
            e2.store(true, Ordering::SeqCst);
        });
        let start = Instant::now();
        while g.waiting() == 0 {
            assert!(start.elapsed() < Duration::from_secs(10), "never parked");
            std::thread::yield_now();
        }
        drop(first);
        let second = g.take(Duration::from_secs(5)).expect("still holds");
        assert!(
            entered.load(Ordering::SeqCst),
            "the parked reader was admitted before the next barrier closed"
        );
        drop(second);
        t.join().unwrap();
    }

    #[test]
    fn the_fault_seam_takes_without_closing() {
        let g = Arc::new(Gate::new());
        g.set_fault(1);
        let hold = g
            .take(Duration::from_millis(10))
            .expect("faulted take succeeds");
        assert!(g.try_enter().is_some(), "and the gate stays open");
        drop(hold);
        g.set_fault(0);
        assert!(g.try_enter().is_some());
    }
}
