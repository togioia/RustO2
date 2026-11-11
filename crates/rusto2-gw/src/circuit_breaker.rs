use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::routes::CompiledRoute;

/// One circuit breaker per configured route.
pub struct BreakerRegistry {
    breakers: HashMap<String, Arc<CircuitBreaker>>,
}

impl BreakerRegistry {
    pub fn from_routes(routes: &[Arc<CompiledRoute>]) -> Self {
        let breakers = routes
            .iter()
            .filter_map(|route| {
                route.cfg.circuit_breaker.as_ref().map(|config| {
                    (
                        route.cfg.name.clone(),
                        Arc::new(CircuitBreaker::new(
                            config.failure_threshold,
                            Duration::from_secs(config.recovery_secs),
                        )),
                    )
                })
            })
            .collect();
        Self { breakers }
    }

    pub fn get(&self, route: &str) -> Option<Arc<CircuitBreaker>> {
        self.breakers.get(route).cloned()
    }

    pub fn snapshot(&self) -> HashMap<String, &'static str> {
        self.breakers
            .iter()
            .map(|(name, breaker)| (name.clone(), breaker.state_name()))
            .collect()
    }
}

struct Inner {
    state: State,
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    half_open_probe_inflight: bool,
    half_open_started_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Closed,
    Open,
    HalfOpen,
}

pub struct CircuitBreaker {
    failure_threshold: u32,
    recovery: Duration,
    inner: Mutex<Inner>,
}

#[derive(Debug, Clone, Copy)]
pub enum Acquire {
    Allowed,
    Rejected { retry_after_secs: u64 },
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, recovery: Duration) -> Self {
        Self {
            failure_threshold: failure_threshold.max(1),
            recovery,
            inner: Mutex::new(Inner {
                state: State::Closed,
                consecutive_failures: 0,
                opened_at: None,
                half_open_probe_inflight: false,
                half_open_started_at: None,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Ask permission to call the backend. While open, requests fail fast; after
    /// the recovery window a single half-open probe is allowed through.
    pub fn acquire(&self) -> Acquire {
        let mut inner = self.lock();
        match inner.state {
            State::Closed => Acquire::Allowed,
            State::Open => {
                let elapsed = inner
                    .opened_at
                    .map(|instant| instant.elapsed())
                    .unwrap_or(self.recovery);
                if elapsed >= self.recovery {
                    inner.state = State::HalfOpen;
                    inner.half_open_probe_inflight = true;
                    inner.half_open_started_at = Some(Instant::now());
                    Acquire::Allowed
                } else {
                    let remaining = self.recovery.saturating_sub(elapsed).as_secs().max(1);
                    Acquire::Rejected {
                        retry_after_secs: remaining,
                    }
                }
            }
            State::HalfOpen => {
                let stale_probe = inner
                    .half_open_started_at
                    .map(|instant| instant.elapsed() >= self.recovery)
                    .unwrap_or(true);
                if inner.half_open_probe_inflight && !stale_probe {
                    Acquire::Rejected {
                        retry_after_secs: 1,
                    }
                } else {
                    inner.half_open_probe_inflight = true;
                    inner.half_open_started_at = Some(Instant::now());
                    Acquire::Allowed
                }
            }
        }
    }

    /// Returns true when this failure caused the breaker to open.
    pub fn record_failure(&self) -> bool {
        let mut inner = self.lock();
        match inner.state {
            State::HalfOpen => {
                inner.state = State::Open;
                inner.opened_at = Some(Instant::now());
                inner.half_open_probe_inflight = false;
                inner.half_open_started_at = None;
                true
            }
            State::Closed => {
                inner.consecutive_failures = inner.consecutive_failures.saturating_add(1);
                if inner.consecutive_failures >= self.failure_threshold {
                    inner.state = State::Open;
                    inner.opened_at = Some(Instant::now());
                    true
                } else {
                    false
                }
            }
            State::Open => false,
        }
    }

    pub fn record_success(&self) {
        let mut inner = self.lock();
        // A success from a request that started before the circuit opened must
        // not close it; only a proper half-open probe can do that.
        if inner.state == State::Open {
            return;
        }
        inner.state = State::Closed;
        inner.consecutive_failures = 0;
        inner.opened_at = None;
        inner.half_open_probe_inflight = false;
        inner.half_open_started_at = None;
    }

    pub fn state_name(&self) -> &'static str {
        match self.lock().state {
            State::Closed => "closed",
            State::Open => "open",
            State::HalfOpen => "half_open",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;

    use super::*;

    #[test]
    fn opens_after_threshold_and_recovers() {
        let breaker = CircuitBreaker::new(2, Duration::from_millis(60));

        assert!(matches!(breaker.acquire(), Acquire::Allowed));
        assert!(!breaker.record_failure());
        assert!(breaker.record_failure());
        assert_eq!(breaker.state_name(), "open");

        assert!(matches!(
            breaker.acquire(),
            Acquire::Rejected {
                retry_after_secs: 1
            }
        ));

        sleep(Duration::from_millis(70));
        assert!(matches!(breaker.acquire(), Acquire::Allowed));
        assert_eq!(breaker.state_name(), "half_open");

        // A concurrent request is rejected while the probe is in flight.
        assert!(matches!(
            breaker.acquire(),
            Acquire::Rejected {
                retry_after_secs: 1
            }
        ));

        breaker.record_success();
        assert_eq!(breaker.state_name(), "closed");
        assert!(matches!(breaker.acquire(), Acquire::Allowed));
    }

    #[test]
    fn failed_probe_reopens_the_circuit() {
        let breaker = CircuitBreaker::new(1, Duration::from_millis(20));
        assert!(breaker.record_failure());
        sleep(Duration::from_millis(30));

        assert!(matches!(breaker.acquire(), Acquire::Allowed));
        assert!(breaker.record_failure());
        assert_eq!(breaker.state_name(), "open");
    }
}
