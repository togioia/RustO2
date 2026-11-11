use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use metrics_exporter_prometheus::PrometheusHandle;
use reqwest::Client;

use crate::{
    circuit_breaker::BreakerRegistry,
    config::GatewayConfig,
    jwks::JwksCache,
    rate_limit::RateLimiter,
    routes::{self, CompiledRoute},
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<GatewayConfig>,
    pub routes: Arc<Vec<Arc<CompiledRoute>>>,
    pub client: Client,
    pub jwks: Arc<JwksCache>,
    pub limiter: Arc<RateLimiter>,
    pub breakers: Arc<BreakerRegistry>,
    pub metrics: PrometheusHandle,
    pub started: Instant,
}

const _: fn() = || {
    fn assert_send_sync_clone<T: Send + Sync + Clone + 'static>() {}
    assert_send_sync_clone::<AppState>();
};

impl AppState {
    pub fn build(config: GatewayConfig, metrics: PrometheusHandle) -> anyhow::Result<Self> {
        let routes = Arc::new(routes::compile(&config)?);

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

        Ok(Self {
            config: Arc::new(config),
            routes,
            client,
            jwks,
            limiter: Arc::new(RateLimiter::new()),
            breakers,
            metrics,
            started: Instant::now(),
        })
    }
}
