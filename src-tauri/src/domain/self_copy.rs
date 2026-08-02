use super::content_hash;
use parking_lot::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ClipboardEventOrigin {
    #[allow(dead_code)]
    External,
    KitsuPin,
}

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
    #[allow(dead_code)]
    origin: ClipboardEventOrigin,
    expected_owner: Option<u32>,
}

#[derive(Default)]
pub struct OwnCopyGuard {
    next_id: Mutex<u64>,
    marks: Mutex<Vec<ActiveMark>>,
}

impl OwnCopyGuard {
    pub fn mark_pending_with_details(
        &self,
        normalized: &str,
        origin: ClipboardEventOrigin,
        expected_owner: Option<u32>,
    ) -> u64 {
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
            origin,
            expected_owner,
        });
        id
    }

    pub fn mark_pending(&self, normalized: &str) -> u64 {
        self.mark_pending_with_details(normalized, ClipboardEventOrigin::KitsuPin, None)
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

    pub fn should_suppress_with_owner(&self, normalized: &str, actual_owner: Option<u32>) -> bool {
        let mut marks = self.marks.lock();
        let target_hash = content_hash(normalized);
        marks.retain(|m| m.created_at.elapsed() < Duration::from_secs(10));
        if let Some(pos) = marks.iter().position(|m| {
            if m.created_at.elapsed() >= Duration::from_secs(2) || m.hash != target_hash {
                return false;
            }
            if let (Some(owner), Some(exp_owner)) = (actual_owner, m.expected_owner) {
                if owner != exp_owner {
                    return false;
                }
            }
            true
        }) {
            marks.remove(pos);
            true
        } else {
            false
        }
    }

    #[allow(dead_code)]
    pub fn should_suppress(&self, normalized: &str) -> bool {
        self.should_suppress_with_owner(normalized, None)
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

    #[test]
    fn owner_mismatch_does_not_suppress_external_copy() {
        let guard = OwnCopyGuard::default();
        let token = guard.mark_pending_with_details(
            "same_content",
            ClipboardEventOrigin::KitsuPin,
            Some(1001),
        );
        guard.commit(token);
        // External copy from window 2002 with same content
        assert!(!guard.should_suppress_with_owner("same_content", Some(2002)));
        // Internal event from expected window 1001
        assert!(guard.should_suppress_with_owner("same_content", Some(1001)));
    }
}
