use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::Deserialize;
use thiserror::Error;

/// Short-lived cache in front of RFC 7662 introspection.
///
/// Local JWT validation remains the fast path. Routes that opt into
/// `revocation_check` ask the identity server at most once per cache window per
/// token, so revocation takes effect within `revocation_cache_secs` without
/// paying a network round trip on every request.
pub struct RevocationChecker {
    client: reqwest::Client,
    url: String,
    client_id: String,
    client_secret: String,
    cache_ttl: Duration,
    cache: DashMap<String, CacheEntry>,
}

struct CacheEntry {
    active: bool,
    expires: Instant,
}

#[derive(Debug, Error)]
pub enum RevocationError {
    #[error("introspection failed: {0}")]
    Backend(String),
}

#[derive(Deserialize)]
struct IntrospectionResponse {
    active: bool,
}

impl RevocationChecker {
    pub fn new(
        client: reqwest::Client,
        url: String,
        client_id: String,
        client_secret: String,
        cache_ttl: Duration,
    ) -> Self {
        Self {
            client,
            url,
            client_id,
            client_secret,
            cache_ttl,
            cache: DashMap::new(),
        }
    }

    /// Returns whether the token is still active according to the identity
    /// server. Cached values expire after the configured TTL.
    pub async fn is_active(&self, jti: &str, token: &str) -> Result<bool, RevocationError> {
        if let Some(entry) = self.cache.get(jti) {
            if entry.expires > Instant::now() {
                return Ok(entry.active);
            }
            drop(entry);
            self.cache.remove(jti);
        }

        let response = self
            .client
            .post(&self.url)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .timeout(Duration::from_secs(2))
            .form(&[("token", token), ("token_type_hint", "access_token")])
            .send()
            .await
            .map_err(|error| RevocationError::Backend(error.to_string()))?;

        if !response.status().is_success() {
            return Err(RevocationError::Backend(format!(
                "identity server returned {}",
                response.status()
            )));
        }

        let body: IntrospectionResponse = response
            .json()
            .await
            .map_err(|error| RevocationError::Backend(error.to_string()))?;

        self.cache.insert(
            jti.to_owned(),
            CacheEntry {
                active: body.active,
                expires: Instant::now() + self.cache_ttl,
            },
        );

        Ok(body.active)
    }

    pub fn cleanup(&self) {
        let now = Instant::now();
        self.cache.retain(|_, entry| entry.expires > now);
    }

    pub fn len(&self) -> usize {
        self.cache.len()
    }
}
