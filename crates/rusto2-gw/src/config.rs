use std::{collections::HashSet, env, path::Path};

use anyhow::{bail, Context};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayConfig {
    #[serde(default)]
    pub server: ServerConfig,
    pub identity: IdentityConfig,
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub trust_proxy: bool,
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_pool_idle")]
    pub pool_idle_timeout_secs: u64,
    #[serde(default = "default_pool_size")]
    pub pool_max_idle_per_host: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            trust_proxy: false,
            request_timeout_secs: default_request_timeout(),
            connect_timeout_secs: default_connect_timeout(),
            pool_idle_timeout_secs: default_pool_idle(),
            pool_max_idle_per_host: default_pool_size(),
        }
    }
}

fn default_listen() -> String {
    "0.0.0.0:8080".to_owned()
}
fn default_request_timeout() -> u64 {
    30
}
fn default_connect_timeout() -> u64 {
    5
}
fn default_pool_idle() -> u64 {
    90
}
fn default_pool_size() -> usize {
    256
}

#[derive(Debug, Clone, Deserialize)]
pub struct IdentityConfig {
    pub issuer: String,
    pub audience: String,
    pub jwks_url: String,
    #[serde(default = "default_refresh")]
    pub refresh_secs: u64,
    #[serde(default = "default_leeway")]
    pub leeway_secs: u64,
}

fn default_refresh() -> u64 {
    300
}
fn default_leeway() -> u64 {
    15
}

#[derive(Debug, Clone, Deserialize)]
pub struct RouteConfig {
    pub name: String,
    pub prefix: String,
    pub upstream: String,
    #[serde(default = "default_true")]
    pub strip_prefix: bool,
    /// HTTP methods allowed by the route policy. Anything else gets a 405
    /// before authentication or upstream traffic.
    #[serde(default = "default_methods")]
    pub methods: Vec<String>,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,
    #[serde(default)]
    pub circuit_breaker: Option<CircuitBreakerConfig>,
}

fn default_true() -> bool {
    true
}

fn default_methods() -> Vec<String> {
    ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AuthConfig {
    #[serde(default)]
    pub mode: AuthMode,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    None,
    Jwt,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitConfig {
    /// Sustained requests per second for this key.
    pub rps: u32,
    /// Token bucket capacity. Defaults to 2x the sustained rate.
    #[serde(default)]
    pub burst: Option<u32>,
    #[serde(default)]
    pub key: RateLimitKey,
}

impl RateLimitConfig {
    pub fn effective_burst(&self) -> u32 {
        self.burst
            .unwrap_or_else(|| self.rps.saturating_mul(2))
            .max(1)
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitKey {
    #[default]
    Client,
    Ip,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CircuitBreakerConfig {
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
    #[serde(default = "default_recovery")]
    pub recovery_secs: u64,
}

fn default_failure_threshold() -> u32 {
    5
}
fn default_recovery() -> u64 {
    10
}

impl GatewayConfig {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        if !Path::new(path).exists() {
            bail!("gateway configuration not found at {path}");
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read gateway configuration at {path}"))?;
        let expanded = rusto2_common::env::expand_env(&raw)?;
        let mut config: Self =
            serde_yaml::from_str(&expanded).with_context(|| format!("invalid YAML in {path}"))?;
        config.apply_env_overrides()?;
        config.validate()?;
        Ok(config)
    }

    fn apply_env_overrides(&mut self) -> anyhow::Result<()> {
        if let Ok(value) = env::var("RUSTO2_GW_LISTEN") {
            self.server.listen = value;
        }
        if let Ok(value) = env::var("RUSTO2_ISSUER") {
            self.identity.issuer = value;
        }
        if let Ok(value) = env::var("RUSTO2_JWKS_URL") {
            self.identity.jwks_url = value;
        }
        if let Ok(value) = env::var("RUSTO2_RATE_LIMIT_RPS") {
            let rps: u32 = value
                .parse()
                .context("RUSTO2_RATE_LIMIT_RPS must be an integer")?;
            for route in &mut self.routes {
                if rps == 0 {
                    route.rate_limit = None;
                } else if let Some(policy) = &mut route.rate_limit {
                    policy.rps = rps;
                    policy.burst = Some(policy.burst.unwrap_or_else(|| rps.saturating_mul(2)));
                }
            }
        }
        if let Ok(value) = env::var("RUSTO2_RATE_LIMIT_BURST") {
            let burst: u32 = value
                .parse()
                .context("RUSTO2_RATE_LIMIT_BURST must be an integer")?;
            for route in &mut self.routes {
                if let Some(policy) = &mut route.rate_limit {
                    policy.burst = Some(burst);
                }
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.server.request_timeout_secs == 0 {
            bail!("server.request_timeout_secs must be greater than zero");
        }
        if self.server.connect_timeout_secs == 0 {
            bail!("server.connect_timeout_secs must be greater than zero");
        }
        if self.routes.is_empty() {
            bail!("at least one route must be configured");
        }

        let issuer = reqwest::Url::parse(&self.identity.issuer)
            .context("identity.issuer must be an absolute URL")?;
        if !matches!(issuer.scheme(), "http" | "https") {
            bail!("identity.issuer must use http or https");
        }
        let jwks = reqwest::Url::parse(&self.identity.jwks_url)
            .context("identity.jwks_url must be an absolute URL")?;
        if !matches!(jwks.scheme(), "http" | "https") {
            bail!("identity.jwks_url must use http or https");
        }
        if self.identity.audience.trim().is_empty() {
            bail!("identity.audience must not be empty");
        }
        if self.identity.refresh_secs == 0 {
            bail!("identity.refresh_secs must be greater than zero");
        }

        let mut names = HashSet::new();
        let mut prefixes = HashSet::new();
        for route in &self.routes {
            if route.name.trim().is_empty() {
                bail!("route name must not be empty");
            }
            if !route
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            {
                bail!(
                    "route {} name must contain only letters, digits, '-' or '_'",
                    route.name
                );
            }
            if !names.insert(route.name.clone()) {
                bail!("duplicate route name: {}", route.name);
            }
            if !route.prefix.starts_with('/') {
                bail!("route {} prefix must start with '/'", route.name);
            }
            if route.prefix.len() > 1 && route.prefix.ends_with('/') {
                bail!("route {} prefix must not end with '/'", route.name);
            }
            if !prefixes.insert(route.prefix.clone()) {
                bail!("duplicate route prefix: {}", route.prefix);
            }
            let upstream = reqwest::Url::parse(&route.upstream)
                .with_context(|| format!("route {} has an invalid upstream URL", route.name))?;
            if !matches!(upstream.scheme(), "http" | "https") || upstream.host().is_none() {
                bail!(
                    "route {} upstream must be an absolute http(s) URL",
                    route.name
                );
            }
            if route.methods.is_empty() {
                bail!("route {} must allow at least one method", route.name);
            }
            for method in &route.methods {
                http::Method::from_bytes(method.to_uppercase().as_bytes()).with_context(|| {
                    format!("route {} has an invalid method: {method}", route.name)
                })?;
            }
            if let Some(policy) = &route.rate_limit {
                if policy.rps == 0 {
                    bail!(
                        "route {} rate_limit.rps must be greater than zero",
                        route.name
                    );
                }
                if let Some(burst) = policy.burst {
                    if burst == 0 {
                        bail!(
                            "route {} rate_limit.burst must be greater than zero",
                            route.name
                        );
                    }
                }
            }
            if let Some(breaker) = &route.circuit_breaker {
                if breaker.failure_threshold == 0 {
                    bail!(
                        "route {} circuit_breaker.failure_threshold must be greater than zero",
                        route.name
                    );
                }
                if breaker.recovery_secs == 0 {
                    bail!(
                        "route {} circuit_breaker.recovery_secs must be greater than zero",
                        route.name
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_yaml() -> &'static str {
        r#"
identity:
  issuer: https://issuer.test
  audience: rusto2
  jwks_url: https://issuer.test/.well-known/jwks.json
routes:
  - name: api
    prefix: /api
    upstream: http://backend:8080
    auth:
      mode: jwt
      scopes: [read]
    rate_limit:
      rps: 10
    circuit_breaker:
      failure_threshold: 3
"#
    }

    #[test]
    fn parses_a_valid_configuration() {
        let config: GatewayConfig = serde_yaml::from_str(base_yaml()).expect("yaml");
        config.validate().expect("valid");
        assert_eq!(
            config.routes[0]
                .rate_limit
                .as_ref()
                .unwrap()
                .effective_burst(),
            20
        );
    }

    #[test]
    fn rejects_duplicate_prefixes() {
        let mut config: GatewayConfig = serde_yaml::from_str(base_yaml()).expect("yaml");
        let mut duplicate = config.routes[0].clone();
        duplicate.name = "api2".to_owned();
        config.routes.push(duplicate);
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_zero_rate_limit() {
        let mut config: GatewayConfig = serde_yaml::from_str(base_yaml()).expect("yaml");
        config.routes[0].rate_limit.as_mut().unwrap().rps = 0;
        assert!(config.validate().is_err());
    }
}
