use super::content_hash;
use parking_lot::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum MarkState {
    Pending,
    Committed,
}

struct ActiveMark {
    id: u64,
    hash: String,
    created_at: Instant,
    state: MarkState,
}

#[derive(Default)]
pub struct OwnCopyGuard {
    next_id: Mutex<u64>,
    active: Mutex<Option<ActiveMark>>,
}

impl OwnCopyGuard {
    pub fn mark_pending(&self, normalized: &str) -> u64 {
        let mut next_id = self.next_id.lock();
        *next_id += 1;
        let id = *next_id;
        *self.active.lock() = Some(ActiveMark {
            id,
            hash: content_hash(normalized),
            created_at: Instant::now(),
            state: MarkState::Pending,
        });
        id
    }

    pub fn commit(&self, token: u64) {
        let mut active = self.active.lock();
        if let Some(ref mut mark) = *active {
            if mark.id == token {
                mark.state = MarkState::Committed;
            }
        }
    }

    pub fn cancel(&self, token: u64) {
        let mut active = self.active.lock();
        if active.as_ref().is_some_and(|mark| mark.id == token) {
            *active = None;
        }
    }

    #[allow(dead_code)]
    pub fn mark(&self, normalized: &str) {
        let token = self.mark_pending(normalized);
        self.commit(token);
    }

    pub fn should_suppress(&self, normalized: &str) -> bool {
        let mut active = self.active.lock();
        let matches = active.as_ref().is_some_and(|mark| {
            mark.created_at.elapsed() < Duration::from_secs(2)
                && mark.hash == content_hash(normalized)
        });
        if matches {
            *active = None;
        }
        matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppresses_exactly_one_recent_matching_copy() {
        let guard = OwnCopyGuard::default();
        let token = guard.mark_pending("hello");
        guard.commit(token);
        assert!(!guard.should_suppress("other"));
        assert!(guard.should_suppress("hello"));
        assert!(!guard.should_suppress("hello"));
    }

    #[test]
    fn suppresses_pending_mark_before_commit() {
        let guard = OwnCopyGuard::default();
        let _token = guard.mark_pending("hello");
        assert!(guard.should_suppress("hello"));
        assert!(!guard.should_suppress("hello"));
    }

    #[test]
    fn cancel_removes_pending_mark() {
        let guard = OwnCopyGuard::default();
        let token = guard.mark_pending("hello");
        guard.cancel(token);
        assert!(!guard.should_suppress("hello"));
    }
}

