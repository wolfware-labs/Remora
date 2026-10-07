use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The daemon's only source of time, in milliseconds since the Unix epoch.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

/// The system's wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX)
    }
}

/// A clock that only moves when told to, for tests. Clones share the same time.
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<AtomicI64>);

impl ManualClock {
    pub fn new(now_ms: i64) -> Self {
        Self(Arc::new(AtomicI64::new(now_ms)))
    }

    pub fn set(&self, now_ms: i64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance(&self, by: Duration) {
        let ms = i64::try_from(by.as_millis()).unwrap_or(i64::MAX);
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Strictly increasing times for one project, even when the clock goes backwards:
/// `at_ms = max(now, last + 1)`.
#[derive(Debug)]
pub(crate) struct Monotonic {
    last: i64,
}

impl Monotonic {
    pub(crate) fn starting_after(last: i64) -> Self {
        Self { last }
    }

    pub(crate) fn next(&mut self, now_ms: i64) -> i64 {
        let at = now_ms.max(self.last.saturating_add(1));
        self.last = at;
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_never_repeat_or_go_backwards() {
        let mut times = Monotonic::starting_after(1_000);
        assert_eq!(
            times.next(500),
            1_001,
            "the clock is behind the last recorded event"
        );
        assert_eq!(times.next(1_001), 1_002, "same millisecond");
        assert_eq!(times.next(5_000), 5_000);
        assert_eq!(times.next(4_000), 5_001, "the clock jumped back");
    }

    #[test]
    fn a_manual_clock_is_shared_by_its_clones() {
        let clock = ManualClock::new(10);
        let other = clock.clone();
        clock.advance(Duration::from_millis(5));
        assert_eq!(other.now_ms(), 15);
    }
}
