use std::{collections::HashMap, sync::Arc, time::Instant};

use anyhow::bail;
use metrics_exporter_prometheus::PrometheusHandle;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{config::Config, keys::KeyMaterial, revocation::RevocationStore};

/// A client whose secret is stored only as a SHA-256 digest. The raw secret is
/// never kept in memory after startup.
pub struct RegisteredClient {
    pub client_id: String,
    pub secret_hash: [u8; 32],
    pub scopes: Vec<String>,
}

impl RegisteredClient {
    fn verify(&self, digest: &[u8]) -> bool {
        bool::from(digest.ct_eq(&self.secret_hash))
    }
}

pub struct ClientRegistry {
    clients: HashMap<String, Arc<RegisteredClient>>,
}

impl ClientRegistry {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let mut clients = HashMap::new();
        for client in config.enabled_clients() {
            let Some(secret) = client.client_secret.as_deref() else {
                bail!("client {} has no secret", client.client_id);
            };
            let digest = Sha256::digest(secret.as_bytes());
            clients.insert(
                client.client_id.clone(),
                Arc::new(RegisteredClient {
                    client_id: client.client_id.clone(),
                    secret_hash: digest.into(),
                    scopes: client.scopes.clone(),
                }),
            );
        }
        Ok(Self { clients })
    }

    /// Constant-time credential check. Unknown clients still pay for a hash and
    /// a fixed comparison to reduce the timing signal.
    pub fn authenticate(&self, client_id: &str, secret: &str) -> Option<Arc<RegisteredClient>> {
        let provided = Sha256::digest(secret.as_bytes());
        match self.clients.get(client_id) {
            Some(client) if client.verify(provided.as_slice()) => Some(Arc::clone(client)),
            Some(_) => None,
            None => {
                let _ = provided.as_slice().ct_eq(&[0x5au8; 32]);
                None
            }
        }
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub clients: Arc<ClientRegistry>,
    pub key: Arc<KeyMaterial>,
    pub revocations: Arc<RevocationStore>,
    pub metrics: PrometheusHandle,
    pub started: Instant,
}

impl AppState {
    pub fn new(
        config: Config,
        key: KeyMaterial,
        metrics: PrometheusHandle,
    ) -> anyhow::Result<Self> {
        let clients = ClientRegistry::new(&config)?;
        let revocations = Arc::new(RevocationStore::new(std::time::Duration::from_secs(
            config.revocation.max_ttl_secs,
        )));
        Ok(Self {
            config: Arc::new(config),
            clients: Arc::new(clients),
            key: Arc::new(key),
            revocations,
            metrics,
            started: Instant::now(),
        })
    }
}
