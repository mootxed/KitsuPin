use super::content_hash;
use parking_lot::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct OwnCopyGuard {
    last: Mutex<Option<(String, Instant)>>,
}

impl OwnCopyGuard {
    pub fn mark(&self, normalized: &str) {
        *self.last.lock() = Some((content_hash(normalized), Instant::now()));
    }

    pub fn should_suppress(&self, normalized: &str) -> bool {
        let mut last = self.last.lock();
        let matches = last.as_ref().is_some_and(|(hash, at)| {
            at.elapsed() < Duration::from_secs(2) && hash == &content_hash(normalized)
        });
        if matches {
            *last = None;
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
        guard.mark("hello");
        assert!(!guard.should_suppress("other"));
        assert!(guard.should_suppress("hello"));
        assert!(!guard.should_suppress("hello"));
    }
}
