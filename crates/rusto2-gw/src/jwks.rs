use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use anyhow::{bail, Context};
use jsonwebtoken::{
    jwk::{AlgorithmParameters, JwkSet, KeyAlgorithm, PublicKeyUse},
    DecodingKey,
};
use reqwest::Client;

/// In-memory JWKS cache refreshed in the background. A failed refresh keeps the
/// last known good keys so a transient identity-server outage does not take the
/// gateway down.
pub struct JwksCache {
    client: Client,
    url: String,
    refresh_secs: u64,
    keys: RwLock<HashMap<String, DecodingKey>>,
    last_success: RwLock<Option<Instant>>,
    last_attempt: Mutex<Option<Instant>>,
    refresh_lock: tokio::sync::Mutex<()>,
}

#[derive(Debug, Clone, Copy)]
pub struct JwksStatus {
    pub keys: usize,
    pub last_refresh_age_secs: Option<u64>,
}

impl JwksCache {
    pub fn new(client: Client, url: String, refresh_secs: u64) -> Self {
        Self {
            client,
            url,
            refresh_secs,
            keys: RwLock::new(HashMap::new()),
            last_success: RwLock::new(None),
            last_attempt: Mutex::new(None),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Fetch and replace the key set. Old keys stay cached if anything fails.
    pub async fn refresh(&self) -> anyhow::Result<usize> {
        let _guard = self.refresh_lock.lock().await;
        self.fetch_and_store().await
    }

    /// Called when a token references an unknown `kid`. Rate-limited to at most
    /// one refresh every five seconds and skipped when another refresh is in
    /// flight, so forged `kid` values cannot amplify traffic.
    pub async fn refresh_after_miss(&self) {
        {
            let mut last_attempt = self
                .last_attempt
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(instant) = *last_attempt {
                if instant.elapsed() < Duration::from_secs(5) {
                    return;
                }
            }
            *last_attempt = Some(Instant::now());
        }

        if let Ok(_guard) = self.refresh_lock.try_lock() {
            if let Err(error) = self.fetch_and_store().await {
                tracing::warn!(error = %error, "JWKS refresh after unknown kid failed");
            }
        }
    }

    async fn fetch_and_store(&self) -> anyhow::Result<usize> {
        {
            let mut last_attempt = self
                .last_attempt
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *last_attempt = Some(Instant::now());
        }

        let response = self
            .client
            .get(&self.url)
            .header(
                reqwest::header::ACCEPT,
                "application/jwk-set+json, application/json",
            )
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .with_context(|| format!("cannot fetch JWKS from {}", self.url))?
            .error_for_status()
            .context("JWKS endpoint returned an error status")?;

        let set: JwkSet = response.json().await.context("invalid JWKS document")?;
        let mut keys = HashMap::new();

        for jwk in set.keys {
            if jwk.common.key_algorithm != Some(KeyAlgorithm::EdDSA) {
                continue;
            }
            if !matches!(jwk.algorithm, AlgorithmParameters::OctetKeyPair(_)) {
                continue;
            }
            if matches!(jwk.common.public_key_use, Some(PublicKeyUse::Encryption)) {
                continue;
            }
            let Some(kid) = jwk.common.key_id.clone() else {
                continue;
            };
            match DecodingKey::from_jwk(&jwk) {
                Ok(key) => {
                    keys.insert(kid, key);
                }
                Err(error) => {
                    tracing::warn!(kid = %kid, error = %error, "ignoring unusable JWK");
                }
            }
        }

        if keys.is_empty() {
            bail!("JWKS contained no usable Ed25519 signing keys");
        }

        let count = keys.len();
        *self
            .keys
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = keys;
        *self
            .last_success
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());

        tracing::info!(keys = count, url = %self.url, "JWKS refreshed");
        Ok(count)
    }

    pub fn keys_for(&self, kid: Option<&str>) -> Vec<DecodingKey> {
        let keys = self
            .keys
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match kid {
            Some(kid) => keys.get(kid).cloned().into_iter().collect(),
            None => keys.values().cloned().collect(),
        }
    }

    pub fn has_keys(&self) -> bool {
        !self
            .keys
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    pub fn status(&self) -> JwksStatus {
        let keys = self
            .keys
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        let age = self
            .last_success
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .map(|instant| instant.elapsed().as_secs());
        JwksStatus {
            keys,
            last_refresh_age_secs: age,
        }
    }

    /// Background refresh loop. While the cache is empty (identity server not
    /// ready yet) it retries with exponential backoff instead of sleeping for
    /// the whole refresh interval.
    pub async fn run(self: Arc<Self>) {
        let base = Duration::from_secs(self.refresh_secs.max(5));
        let mut delay = if self.has_keys() {
            base
        } else {
            Duration::from_secs(2)
        };

        loop {
            tokio::time::sleep(delay).await;
            match self.refresh().await {
                Ok(_) => delay = base,
                Err(error) => {
                    tracing::warn!(error = %error, "scheduled JWKS refresh failed");
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        }
    }
}
