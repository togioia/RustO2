use std::time::{Duration, Instant};

use async_trait::async_trait;
use dashmap::DashMap;
use redis::{aio::ConnectionManager, Script};
use thiserror::Error;

/// A rate-limit decision shared by the in-memory and distributed backends.
#[derive(Debug, Clone, Copy)]
pub struct Decision {
    pub allowed: bool,
    pub limit: u32,
    pub remaining: u32,
    pub retry_after_secs: Option<u64>,
}

#[derive(Debug, Error)]
pub enum RateLimitError {
    #[error("distributed rate limiter unavailable: {0}")]
    Backend(String),
}

/// Pluggable quota backend. The gateway only depends on this trait, so local
/// DashMap buckets and Redis-backed buckets are interchangeable.
#[async_trait]
pub trait RateLimitBackend: Send + Sync {
    async fn check(&self, key: &str, rps: u32, burst: u32) -> Result<Decision, RateLimitError>;

    /// Drop idle state. Redis uses key TTLs, so this is a no-op there.
    fn cleanup(&self, _idle_for: Duration) {}

    fn len(&self) -> usize;

    fn backend_name(&self) -> &'static str;
}

/// In-process token bucket. DashMap shards the map, so unrelated clients do not
/// contend on the same lock; each bucket is only touched by requests that share
/// the same rate-limit key.
pub struct MemoryRateLimiter {
    buckets: DashMap<String, Bucket>,
}

struct Bucket {
    tokens: f64,
    updated: Instant,
}

impl MemoryRateLimiter {
    pub fn new() -> Self {
        Self {
            buckets: DashMap::new(),
        }
    }
}

#[async_trait]
impl RateLimitBackend for MemoryRateLimiter {
    async fn check(&self, key: &str, rps: u32, burst: u32) -> Result<Decision, RateLimitError> {
        let now = Instant::now();
        let burst = burst.max(1);
        let mut bucket = self.buckets.entry(key.to_owned()).or_insert(Bucket {
            tokens: f64::from(burst),
            updated: now,
        });

        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * f64::from(rps)).min(f64::from(burst));
        bucket.updated = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(Decision {
                allowed: true,
                limit: burst,
                remaining: bucket.tokens.floor().clamp(0.0, f64::from(burst)) as u32,
                retry_after_secs: None,
            })
        } else {
            let seconds = ((1.0 - bucket.tokens) / f64::from(rps.max(1))).ceil();
            Ok(Decision {
                allowed: false,
                limit: burst,
                remaining: 0,
                retry_after_secs: Some((seconds as u64).max(1)),
            })
        }
    }

    fn cleanup(&self, idle_for: Duration) {
        self.buckets
            .retain(|_, bucket| bucket.updated.elapsed() < idle_for);
    }

    fn len(&self) -> usize {
        self.buckets.len()
    }

    fn backend_name(&self) -> &'static str {
        "memory"
    }
}

impl Default for MemoryRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Redis token bucket. The Lua script runs atomically inside Redis, so multiple
/// gateway replicas share exactly the same quota for a client.
pub struct RedisRateLimiter {
    manager: ConnectionManager,
    prefix: String,
    script: Script,
}

impl RedisRateLimiter {
    pub async fn connect(url: &str, prefix: &str) -> Result<Self, RateLimitError> {
        let client =
            redis::Client::open(url).map_err(|error| RateLimitError::Backend(error.to_string()))?;
        let manager = client
            .get_connection_manager()
            .await
            .map_err(|error| RateLimitError::Backend(error.to_string()))?;

        Ok(Self {
            manager,
            prefix: prefix.to_owned(),
            script: Script::new(REDIS_TOKEN_BUCKET),
        })
    }
}

const REDIS_TOKEN_BUCKET: &str = r#"
local key = KEYS[1]
local rps = tonumber(ARGV[1])
local burst = tonumber(ARGV[2])

local data = redis.call('HMGET', key, 'tokens', 'ts')
local tokens = tonumber(data[1])
local last = tonumber(data[2])

local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)

if tokens == nil or last == nil then
  tokens = burst
  last = now
end

local elapsed = math.max(0, now - last) / 1000.0
tokens = math.min(burst, tokens + elapsed * rps)

local allowed = 0
if tokens >= 1.0 then
  tokens = tokens - 1.0
  allowed = 1
end

redis.call('HSET', key, 'tokens', tokens, 'ts', now)
redis.call('PEXPIRE', key, 300000)

local retry_ms = 0
if allowed == 0 then
  retry_ms = math.ceil(((1.0 - tokens) / rps) * 1000)
end

return { allowed, math.floor(tokens), retry_ms }
"#;

#[async_trait]
impl RateLimitBackend for RedisRateLimiter {
    async fn check(&self, key: &str, rps: u32, burst: u32) -> Result<Decision, RateLimitError> {
        let mut connection = self.manager.clone();
        let redis_key = format!("{}{}", self.prefix, key);

        let values: Vec<i64> = self
            .script
            .key(redis_key)
            .arg(rps.max(1))
            .arg(burst.max(1))
            .invoke_async(&mut connection)
            .await
            .map_err(|error| RateLimitError::Backend(error.to_string()))?;

        let allowed = values.first().copied().unwrap_or(0) == 1;
        let remaining = values.get(1).copied().unwrap_or(0).max(0) as u32;
        let retry_ms = values.get(2).copied().unwrap_or(0).max(0) as u64;

        Ok(Decision {
            allowed,
            limit: burst.max(1),
            remaining,
            retry_after_secs: if allowed {
                None
            } else {
                Some(retry_ms.div_ceil(1_000).max(1))
            },
        })
    }

    fn len(&self) -> usize {
        0
    }

    fn backend_name(&self) -> &'static str {
        "redis"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn enforces_burst_then_refills() {
        let limiter = MemoryRateLimiter::new();

        assert!(limiter.check("client-a", 10, 2).await.unwrap().allowed);
        assert!(limiter.check("client-a", 10, 2).await.unwrap().allowed);

        let denied = limiter.check("client-a", 10, 2).await.unwrap();
        assert!(!denied.allowed);
        assert_eq!(denied.remaining, 0);
        assert!(denied.retry_after_secs.unwrap() >= 1);

        // A different key has an independent bucket.
        assert!(limiter.check("client-b", 10, 2).await.unwrap().allowed);
    }

    #[tokio::test]
    async fn refills_over_time() {
        let limiter = MemoryRateLimiter::new();
        assert!(limiter.check("client", 20, 1).await.unwrap().allowed);
        assert!(!limiter.check("client", 20, 1).await.unwrap().allowed);

        std::thread::sleep(Duration::from_millis(60));
        assert!(limiter.check("client", 20, 1).await.unwrap().allowed);
    }

    #[test]
    fn cleanup_drops_idle_buckets() {
        let limiter = MemoryRateLimiter::new();
        let _ = limiter.buckets.insert(
            "client".into(),
            Bucket {
                tokens: 1.0,
                updated: Instant::now(),
            },
        );
        assert_eq!(RateLimitBackend::len(&limiter), 1);
        limiter.cleanup(Duration::ZERO);
        assert_eq!(RateLimitBackend::len(&limiter), 0);
    }

    #[tokio::test]
    async fn redis_backend_enforces_shared_bucket_when_configured() {
        let Ok(url) = std::env::var("RUSTO2_TEST_REDIS_URL") else {
            // CI without Redis still runs the memory tests above.
            return;
        };
        let prefix = format!(
            "rusto2:test:{}:{}:",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let limiter = RedisRateLimiter::connect(&url, &prefix)
            .await
            .expect("connect redis");
        let key = "client";

        assert!(limiter.check(key, 5, 2).await.unwrap().allowed);
        assert!(limiter.check(key, 5, 2).await.unwrap().allowed);
        assert!(!limiter.check(key, 5, 2).await.unwrap().allowed);
    }
}

#[cfg(test)]
mod property_tests {
    use proptest::prelude::*;

    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime")
    }

    /// Drain a fresh bucket and return the first rejected decision.
    async fn exhaust(limiter: &MemoryRateLimiter, key: &str, burst: u32) -> Decision {
        // rps = 0 disables time-based refill, so the burst bound below is
        // deterministic even on a busy CI machine. The limiter guards its
        // Retry-After division with `rps.max(1)`.
        for _ in 0..burst {
            let decision = limiter.check(key, 0, burst).await.expect("check");
            assert!(decision.allowed, "burst {burst} rejected a token too early");
        }
        limiter.check(key, 0, burst).await.expect("check")
    }

    proptest! {
        /// A burst-sized bucket can never admit more than `burst` calls in a
        /// tight loop.
        #[test]
        fn tight_loop_never_allows_more_than_burst(burst in 1u32..=20) {
            let allowed = runtime().block_on(async {
                let limiter = MemoryRateLimiter::new();
                let mut allowed = 0u32;
                for _ in 0..burst.saturating_mul(2).saturating_add(5) {
                    if limiter
                        .check("client", 0, burst)
                        .await
                        .expect("check")
                        .allowed
                    {
                        allowed += 1;
                    }
                }
                allowed
            });

            prop_assert!(
                allowed <= burst,
                "limiter allowed {allowed} calls for burst {burst}",
            );
        }

        /// Once a bucket is drained, the rejection carries a Retry-After of at
        /// least one second and reports no remaining tokens.
        #[test]
        fn exhausted_bucket_reports_retry_after(burst in 1u32..=20) {
            let decision = runtime().block_on(async {
                let limiter = MemoryRateLimiter::new();
                exhaust(&limiter, "client", burst).await
            });

            prop_assert!(!decision.allowed);
            prop_assert_eq!(decision.remaining, 0);
            prop_assert_eq!(decision.limit, burst);
            prop_assert!(
                decision.retry_after_secs.is_some_and(|seconds| seconds >= 1),
                "429 decision had no valid Retry-After: {:?}",
                decision.retry_after_secs,
            );
        }

        /// Buckets are keyed independently: draining one key must not consume
        /// tokens from another.
        #[test]
        fn keys_have_independent_buckets(burst in 1u32..=20) {
            let (first_denied, second_allowed) = runtime().block_on(async {
                let limiter = MemoryRateLimiter::new();
                let first = exhaust(&limiter, "key-a", burst).await;
                let second = limiter.check("key-b", 0, burst).await.expect("check");
                (!first.allowed, second.allowed)
            });

            prop_assert!(first_denied, "first key was not exhausted");
            prop_assert!(second_allowed, "second key lost its independent bucket");
        }
    }
}
