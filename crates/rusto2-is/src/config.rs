use std::{collections::HashSet, env, path::PathBuf};

use anyhow::{bail, Context};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    pub token: TokenConfig,
    pub keys: KeysConfig,
    pub clients: Vec<ClientConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
        }
    }
}

fn default_listen() -> String {
    "0.0.0.0:9000".to_owned()
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenConfig {
    pub issuer: String,
    pub audience: String,
    #[serde(default = "default_ttl")]
    pub ttl_secs: u64,
}

fn default_ttl() -> u64 {
    3_600
}

#[derive(Debug, Clone, Deserialize)]
pub struct KeysConfig {
    pub private_key_path: PathBuf,
    #[serde(default = "default_true")]
    pub generate_if_missing: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read identity configuration at {path}"))?;
        let expanded = rusto2_common::env::expand_env(&raw)?;
        let mut config: Self =
            serde_yaml::from_str(&expanded).with_context(|| format!("invalid YAML in {path}"))?;
        if let Ok(key_path) = env::var("RUSTO2_KEY_PATH") {
            config.keys.private_key_path = PathBuf::from(key_path);
        }
        if let Ok(listen) = env::var("RUSTO2_IS_LISTEN") {
            config.server.listen = listen;
        }
        if let Ok(issuer) = env::var("RUSTO2_ISSUER") {
            config.token.issuer = issuer;
        }
        if let Ok(audience) = env::var("RUSTO2_AUDIENCE") {
            config.token.audience = audience;
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.token.issuer.trim().is_empty() {
            bail!("token.issuer must not be empty");
        }
        if self.token.audience.trim().is_empty() {
            bail!("token.audience must not be empty");
        }
        if self.token.ttl_secs == 0 {
            bail!("token.ttl_secs must be greater than zero");
        }
        if self.clients.is_empty() {
            bail!("at least one client must be configured");
        }

        let mut ids = HashSet::new();
        for client in self.clients.iter().filter(|client| client.enabled) {
            if client.client_id.trim().is_empty() {
                bail!("client_id must not be empty");
            }
            if !ids.insert(client.client_id.clone()) {
                bail!("duplicate client_id: {}", client.client_id);
            }
            if client
                .client_secret
                .as_deref()
                .map(str::is_empty)
                .unwrap_or(true)
            {
                bail!(
                    "client {} has no client_secret (use an environment placeholder)",
                    client.client_id
                );
            }
        }
        Ok(())
    }

    pub fn enabled_clients(&self) -> impl Iterator<Item = &ClientConfig> {
        self.clients.iter().filter(|client| client.enabled)
    }
}
