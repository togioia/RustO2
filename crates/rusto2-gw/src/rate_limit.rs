use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Lock-free-ish token bucket. DashMap shards the map, so unrelated clients do
/// not contend on the same lock; each bucket is only touched by requests that
/// share the same rate-limit key.
pub struct RateLimiter {
    buckets: DashMap<String, Bucket>,
}

struct Bucket {
    tokens: f64,
    updated: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct Decision {
    pub allowed: bool,
    pub limit: u32,
    pub remaining: u32,
    pub retry_after_secs: Option<u64>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            buckets: DashMap::new(),
        }
    }

    pub fn check(&self, key: String, rps: u32, burst: u32) -> Decision {
        let now = Instant::now();
        let burst = burst.max(1);
        let mut bucket = self.buckets.entry(key).or_insert(Bucket {
            tokens: f64::from(burst),
            updated: now,
        });

        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * f64::from(rps)).min(f64::from(burst));
        bucket.updated = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Decision {
                allowed: true,
                limit: burst,
                remaining: bucket.tokens.floor().clamp(0.0, f64::from(burst)) as u32,
                retry_after_secs: None,
            }
        } else {
            let seconds = ((1.0 - bucket.tokens) / f64::from(rps.max(1))).ceil();
            Decision {
                allowed: false,
                limit: burst,
                remaining: 0,
                retry_after_secs: Some((seconds as u64).max(1)),
            }
        }
    }

    /// Drop idle buckets so a long-running gateway does not grow without bound.
    pub fn cleanup(&self, idle_for: Duration) {
        self.buckets
            .retain(|_, bucket| bucket.updated.elapsed() < idle_for);
    }

    pub fn len(&self) -> usize {
        self.buckets.len()
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforces_burst_then_refills() {
        let limiter = RateLimiter::new();

        assert!(limiter.check("client-a".into(), 10, 2).allowed);
        assert!(limiter.check("client-a".into(), 10, 2).allowed);

        let denied = limiter.check("client-a".into(), 10, 2);
        assert!(!denied.allowed);
        assert_eq!(denied.remaining, 0);
        assert!(denied.retry_after_secs.unwrap() >= 1);

        // A different key has an independent bucket.
        assert!(limiter.check("client-b".into(), 10, 2).allowed);
    }

    #[test]
    fn refills_over_time() {
        let limiter = RateLimiter::new();
        assert!(limiter.check("client".into(), 20, 1).allowed);
        assert!(!limiter.check("client".into(), 20, 1).allowed);

        std::thread::sleep(Duration::from_millis(60));
        assert!(limiter.check("client".into(), 20, 1).allowed);
    }

    #[test]
    fn cleanup_drops_idle_buckets() {
        let limiter = RateLimiter::new();
        let _ = limiter.check("client".into(), 1, 1);
        assert_eq!(limiter.len(), 1);
        limiter.cleanup(Duration::ZERO);
        assert_eq!(limiter.len(), 0);
    }
}
