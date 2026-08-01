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
    marks: Mutex<Vec<ActiveMark>>,
}

impl OwnCopyGuard {
    pub fn mark_pending(&self, normalized: &str) -> u64 {
        let mut next_id = self.next_id.lock();
        *next_id += 1;
        let id = *next_id;
        let mut marks = self.marks.lock();
        marks.retain(|m| m.created_at.elapsed() < Duration::from_secs(10));
        marks.push(ActiveMark {
            id,
            hash: content_hash(normalized),
            created_at: Instant::now(),
            state: MarkState::Pending,
        });
        id
    }

    pub fn commit(&self, token: u64) {
        let mut marks = self.marks.lock();
        if let Some(mark) = marks.iter_mut().find(|m| m.id == token) {
            mark.state = MarkState::Committed;
        }
    }

    pub fn cancel(&self, token: u64) {
        let mut marks = self.marks.lock();
        marks.retain(|m| m.id != token);
    }

    #[allow(dead_code)]
    pub fn mark(&self, normalized: &str) {
        let token = self.mark_pending(normalized);
        self.commit(token);
    }

    pub fn should_suppress(&self, normalized: &str) -> bool {
        let mut marks = self.marks.lock();
        let target_hash = content_hash(normalized);
        marks.retain(|m| m.created_at.elapsed() < Duration::from_secs(10));
        if let Some(pos) = marks
            .iter()
            .position(|m| m.created_at.elapsed() < Duration::from_secs(2) && m.hash == target_hash)
        {
            marks.remove(pos);
            true
        } else {
            false
        }
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

    #[test]
    fn suppresses_multiple_fast_consecutive_copies() {
        let guard = OwnCopyGuard::default();
        let token1 = guard.mark_pending("card1");
        let token2 = guard.mark_pending("card2");
        guard.commit(token1);
        guard.commit(token2);
        assert!(guard.should_suppress("card1"));
        assert!(guard.should_suppress("card2"));
        assert!(!guard.should_suppress("card1"));
        assert!(!guard.should_suppress("card2"));
    }
}
