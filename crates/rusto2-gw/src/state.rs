use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use metrics_exporter_prometheus::PrometheusHandle;
use reqwest::Client;

use crate::{
    artifact,
    circuit_breaker::BreakerRegistry,
    config::{GatewayConfig, RateLimitStore},
    jwks::JwksCache,
    rate_limit::{MemoryRateLimiter, RateLimitBackend, RedisRateLimiter},
    revocation::RevocationChecker,
    routes::{self, CompiledRoute},
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<GatewayConfig>,
    pub routes: Arc<Vec<Arc<CompiledRoute>>>,
    pub client: Client,
    pub jwks: Arc<JwksCache>,
    pub limiter: Arc<dyn RateLimitBackend>,
    pub revocations: Option<Arc<RevocationChecker>>,
    pub breakers: Arc<BreakerRegistry>,
    pub metrics: PrometheusHandle,
    pub started: Instant,
    pub artifact_api_count: usize,
    pub artifact_sources: Vec<String>,
    pub mediation_body_bytes: usize,
}

const _: fn() = || {
    fn assert_send_sync_clone<T: Send + Sync + Clone + 'static>() {}
    assert_send_sync_clone::<AppState>();
};

impl AppState {
    pub async fn build(config: GatewayConfig, metrics: PrometheusHandle) -> anyhow::Result<Self> {
        let artifacts = artifact::load(&config.artifacts)?;
        for ignored in &artifacts.ignored {
            tracing::warn!(artifact = %ignored, "unsupported WSO2 artifact was not loaded");
        }
        let routes = Arc::new(routes::compile_all(&config, &artifacts.routes)?);
        let mediation_body_bytes = config.artifacts.max_body_bytes;

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(config.server.connect_timeout_secs))
            .timeout(Duration::from_secs(config.server.request_timeout_secs))
            .pool_max_idle_per_host(config.server.pool_max_idle_per_host)
            .pool_idle_timeout(Duration::from_secs(config.server.pool_idle_timeout_secs))
            .tcp_nodelay(true)
            .tcp_keepalive(Duration::from_secs(60))
            .http2_adaptive_window(true)
            .build()
            .context("cannot build the upstream HTTP client")?;

        let jwks = Arc::new(JwksCache::new(
            client.clone(),
            config.identity.jwks_url.clone(),
            config.identity.refresh_secs,
        ));
        let breakers = Arc::new(BreakerRegistry::from_routes(&routes));

        let limiter: Arc<dyn RateLimitBackend> = match config.rate_limiting.backend {
            RateLimitStore::Memory => Arc::new(MemoryRateLimiter::new()),
            RateLimitStore::Redis => Arc::new(
                RedisRateLimiter::connect(
                    &config.rate_limiting.redis_url,
                    &config.rate_limiting.key_prefix,
                )
                .await
                .context("cannot initialise the Redis rate limiter")?,
            ),
        };

        let revocations = match (
            config.identity.introspection_url.as_ref(),
            config.identity.introspection_client_id.as_ref(),
            config.identity.introspection_client_secret.as_ref(),
        ) {
            (Some(url), Some(client_id), Some(client_secret)) => {
                Some(Arc::new(RevocationChecker::new(
                    client.clone(),
                    url.clone(),
                    client_id.clone(),
                    client_secret.clone(),
                    Duration::from_secs(config.identity.revocation_cache_secs),
                )))
            }
            _ => None,
        };

        Ok(Self {
            config: Arc::new(config),
            routes,
            client,
            jwks,
            limiter,
            revocations,
            breakers,
            metrics,
            started: Instant::now(),
            artifact_api_count: artifacts.api_count,
            artifact_sources: artifacts.sources,
            mediation_body_bytes,
        })
    }
}
