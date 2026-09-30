use std::collections::{hash_map, HashMap};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

/// `TransactionTracker` tracks the state of transactions to detect retransmissions.
pub struct TransactionTracker {
    retention_period: Duration,
    transactions: Mutex<HashMap<(u32, String), TransactionState>>,
    last_housekeeping: Mutex<SystemTime>,
}

impl TransactionTracker {
    pub fn new(retention_period: Duration) -> Self {
        Self {
            retention_period,
            transactions: Mutex::new(HashMap::new()),
            last_housekeeping: Mutex::new(SystemTime::now()),
        }
    }

    /// Checks if the transaction is a retransmission.
    /// If it's a new transaction, it is marked as `InProgress`.
    ///
    /// Returns `true` if the transaction is a retransmission, `false` otherwise.
    pub fn is_retransmission(&self, xid: u32, client_addr: &str) -> bool {
        let key = (xid, client_addr.to_string());
        let mut transactions = self
            .transactions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // full-map scan per request was O(n) under a global lock; amortize to once a second
        let now = SystemTime::now();
        let mut last = self
            .last_housekeeping
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if now.duration_since(*last).unwrap_or_default() >= Duration::from_secs(1) {
            *last = now;
            housekeeping(&mut transactions, self.retention_period);
        }
        drop(last);
        if let hash_map::Entry::Vacant(e) = transactions.entry(key) {
            e.insert(TransactionState::InProgress);
            false
        } else {
            true
        }
    }

    /// Marks the transaction as processed.
    pub fn mark_processed(&self, xid: u32, client_addr: &str) {
        let key = (xid, client_addr.to_string());
        let completion_time = SystemTime::now();
        let mut transactions = self
            .transactions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(tx) = transactions.get_mut(&key) {
            *tx = TransactionState::Completed(completion_time);
        }
    }
}

fn housekeeping(transactions: &mut HashMap<(u32, String), TransactionState>, max_age: Duration) {
    let mut cutoff = SystemTime::now() - max_age;
    transactions.retain(|_, v| match v {
        TransactionState::InProgress => true,
        TransactionState::Completed(completion_time) => completion_time >= &mut cutoff,
    });
}

pub enum TransactionState {
    InProgress,
    Completed(SystemTime),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_xid_from_one_client_is_a_retransmission() {
        let t = TransactionTracker::new(Duration::from_secs(60));
        assert!(!t.is_retransmission(7, "a"));
        assert!(t.is_retransmission(7, "a"));
        assert!(!t.is_retransmission(7, "b"), "another client");
        assert!(!t.is_retransmission(8, "a"));
        t.mark_processed(7, "a");
        assert!(
            t.is_retransmission(7, "a"),
            "still remembered once completed"
        );
    }

    #[test]
    fn old_completed_transactions_are_forgotten() {
        let t = TransactionTracker::new(Duration::ZERO);
        assert!(!t.is_retransmission(1, "a"));
        t.mark_processed(1, "a");
        *t.last_housekeeping.lock().unwrap() = SystemTime::now() - Duration::from_secs(5);
        std::thread::sleep(Duration::from_millis(5));
        assert!(!t.is_retransmission(2, "a"));
        assert!(!t.is_retransmission(1, "a"), "expired entry was swept");
    }
}
