//! Bandwidth limits.

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// Paces data to at most a fixed number of bytes per second, shared by all
/// streams of a transfer.
///
/// Each caller reserves the time its bytes take at the limit, after any
/// reservations made before it, and waits until its slot starts. Idle time
/// is not saved up, so a pause is never followed by a burst.
#[derive(Debug)]
pub(crate) struct RateLimit {
    bytes_per_sec: u64,
    /// When the next reservation may start.
    next: Mutex<Instant>,
}

impl RateLimit {
    /// A limit of `bytes_per_sec`, or `None` for no limit (0 means none too).
    pub(crate) fn new(bytes_per_sec: Option<u64>) -> Option<Self> {
        let bytes_per_sec = bytes_per_sec.filter(|&b| b > 0)?;
        Some(Self {
            bytes_per_sec,
            next: Mutex::new(Instant::now()),
        })
    }

    /// Waits until `bytes` more may pass.
    pub(crate) async fn take(&self, bytes: usize) {
        let cost = Duration::from_secs_f64(bytes as f64 / self.bytes_per_sec as f64);
        let start = {
            let Ok(mut next) = self.next.lock() else {
                return;
            };
            let start = (*next).max(Instant::now());
            *next = start + cost;
            start
        };
        tokio::time::sleep_until(start).await;
    }
}

/// Applies an optional limit.
pub(crate) async fn take(limit: Option<&RateLimit>, bytes: usize) {
    if let Some(limit) = limit {
        limit.take(bytes).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn paces_to_the_limit() {
        let limit = RateLimit::new(Some(100_000)).unwrap();
        let begin = Instant::now();
        // The first reservation starts at once; each later one waits for
        // the time the earlier ones take at 100 kB/s: 4 × 0.1 s.
        for _ in 0..5 {
            limit.take(10_000).await;
        }
        let took = begin.elapsed();
        assert!(took >= Duration::from_millis(390), "{took:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
    }

    #[tokio::test]
    async fn idle_time_is_not_saved_up() {
        let limit = RateLimit::new(Some(100_000)).unwrap();
        limit.take(10_000).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let begin = Instant::now();
        for _ in 0..3 {
            limit.take(10_000).await;
        }
        let took = begin.elapsed();
        assert!(took >= Duration::from_millis(190), "{took:?}");
    }

    #[test]
    fn zero_means_unlimited() {
        assert!(RateLimit::new(Some(0)).is_none());
        assert!(RateLimit::new(None).is_none());
    }
}
