use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;

/// In-memory `jti` blocklist with TTL. Revoking a token is effective until its
/// `exp` claim (capped by configuration) or until the process restarts.
pub struct RevocationStore {
    entries: DashMap<String, Instant>,
    max_ttl: Duration,
}

impl RevocationStore {
    pub fn new(max_ttl: Duration) -> Self {
        Self {
            entries: DashMap::new(),
            max_ttl,
        }
    }

    pub fn revoke(&self, jti: &str, exp: u64) -> bool {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let ttl_secs = exp
            .saturating_sub(now_secs)
            .max(1)
            .min(self.max_ttl.as_secs().max(1));
        self.entries.insert(
            jti.to_owned(),
            Instant::now() + Duration::from_secs(ttl_secs),
        );
        true
    }

    pub fn is_revoked(&self, jti: &str) -> bool {
        let Some(entry) = self.entries.get(jti) else {
            return false;
        };
        if *entry > Instant::now() {
            true
        } else {
            drop(entry);
            self.entries.remove(jti);
            false
        }
    }

    pub fn cleanup(&self) {
        let now = Instant::now();
        self.entries.retain(|_, expires| *expires > now);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

impl std::fmt::Debug for RevocationStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RevocationStore")
            .field("entries", &self.entries.len())
            .field("max_ttl", &self.max_ttl)
            .finish()
    }
}

/// Shared handle used by the HTTP handlers and the cleanup task.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revoked_jti_is_reported_until_expiry() {
        let store = RevocationStore::new(Duration::from_secs(60));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        assert!(!store.is_revoked("jti-1"));
        store.revoke("jti-1", now + 30);
        assert!(store.is_revoked("jti-1"));
        assert!(!store.is_revoked("jti-2"));
    }

    #[test]
    fn entries_are_capped_by_max_ttl() {
        let store = RevocationStore::new(Duration::from_secs(1));
        store.revoke("jti", u64::MAX);
        assert_eq!(store.len(), 1);
        std::thread::sleep(Duration::from_millis(1_100));
        assert!(!store.is_revoked("jti"));
    }
}
