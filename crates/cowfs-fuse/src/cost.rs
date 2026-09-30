//! Learns how long each class of request takes, so cheap ones run on the request loop thread
//! and only slow ones pay for a hand-off to a worker lane. Pure, no kernel access.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Kinds of request that are worth telling apart, because their cost differs by orders of
/// magnitude for a real backend (a cache hit against a disk read, a flush against an fsync).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Class {
    Read,
    Write,
    Meta,
    Close,
    Fsync,
    Dir,
    Open,
}

const CLASSES: usize = 7;

/// A running average of the time each class takes.
#[derive(Debug)]
pub(crate) struct Cost {
    threshold_ns: u64,
    average_ns: [AtomicU64; CLASSES],
}

impl Cost {
    /// Requests whose class averages under `threshold` run inline; zero sends everything to a
    /// lane. A class is treated as slow until it has been seen to be fast, so a slow first
    /// request never blocks the loop.
    pub(crate) fn new(threshold: Duration) -> Self {
        let threshold_ns = u64::try_from(threshold.as_nanos()).unwrap_or(u64::MAX);
        let start = threshold_ns.saturating_add(1);
        Self {
            threshold_ns,
            average_ns: std::array::from_fn(|_| AtomicU64::new(start)),
        }
    }

    /// True when requests of this class have recently been cheap enough to run inline.
    pub(crate) fn cheap(&self, class: Class) -> bool {
        self.average_ns[class as usize].load(Ordering::Relaxed) < self.threshold_ns
    }

    /// Folds one observed duration in. The average moves half way to each sample, so a single
    /// slow request sends the class to the lanes and a few fast ones bring it back.
    pub(crate) fn record(&self, class: Class, took: Duration) {
        let sample = u64::try_from(took.as_nanos()).unwrap_or(u64::MAX);
        let a = &self.average_ns[class as usize];
        let old = a.load(Ordering::Relaxed);
        a.store(old / 2 + sample / 2, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: Duration = Duration::from_micros(1);

    #[test]
    fn a_class_starts_on_the_lanes_and_moves_inline_once_seen_to_be_fast() {
        let c = Cost::new(100 * US);
        assert!(!c.cheap(Class::Read));
        for _ in 0..4 {
            c.record(Class::Read, 5 * US);
        }
        assert!(c.cheap(Class::Read));
        assert!(!c.cheap(Class::Write), "classes are independent");
    }

    #[test]
    fn one_slow_request_sends_a_class_back_to_the_lanes_and_fast_ones_bring_it_home() {
        let c = Cost::new(100 * US);
        for _ in 0..8 {
            c.record(Class::Meta, 5 * US);
        }
        assert!(c.cheap(Class::Meta));
        c.record(Class::Meta, Duration::from_millis(50));
        assert!(!c.cheap(Class::Meta));
        for _ in 0..16 {
            c.record(Class::Meta, 5 * US);
        }
        assert!(c.cheap(Class::Meta));
    }

    #[test]
    fn a_consistently_slow_class_never_runs_inline() {
        let c = Cost::new(100 * US);
        for _ in 0..100 {
            c.record(Class::Fsync, Duration::from_millis(1));
        }
        assert!(!c.cheap(Class::Fsync));
    }

    #[test]
    fn zero_threshold_means_always_lane_and_huge_means_inline() {
        let never = Cost::new(Duration::ZERO);
        for _ in 0..10 {
            never.record(Class::Dir, Duration::ZERO);
        }
        assert!(!never.cheap(Class::Dir));
        let always = Cost::new(Duration::from_secs(3600));
        always.record(Class::Dir, Duration::ZERO);
        always.record(Class::Dir, Duration::ZERO);
        assert!(always.cheap(Class::Dir));
    }
}
