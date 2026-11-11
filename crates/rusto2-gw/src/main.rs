mod admin;
mod circuit_breaker;
mod config;
mod http_util;
mod jwks;
mod observability;
mod proxy;
mod rate_limit;
mod revocation;
mod routes;
mod security;
mod state;

use std::{sync::Arc, time::Duration};

use anyhow::Context;
use axum::{middleware, routing::get, serve::ListenerExt, Router};
use tokio::net::TcpListener;
use tracing::{info, warn};

#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rusto2_common::init_tracing();

    let config_path = std::env::var("RUSTO2_CONFIG").unwrap_or_else(|_| "rusto2.yaml".to_owned());
    let config = config::GatewayConfig::load(&config_path)?;
    let metrics = rusto2_common::install_metrics()?;
    let state = state::AppState::build(config, metrics).await?;
    let listen = state.config.server.listen.clone();
    let routes = state.routes.len();
    let issuer = state.config.identity.issuer.clone();

    info!(
        config = %config_path,
        listen = %listen,
        routes,
        issuer = %issuer,
        "rusto2-gw starting"
    );

    // Load JWKS eagerly, but keep serving even if the identity server is still
    // booting: the background refresher will retry.
    match tokio::time::timeout(Duration::from_secs(5), state.jwks.refresh()).await {
        Ok(Ok(keys)) => info!(keys, "initial JWKS loaded"),
        Ok(Err(error)) => warn!(error = %error, "initial JWKS load failed; will retry"),
        Err(_) => warn!("initial JWKS load timed out; will retry"),
    }

    tokio::spawn(Arc::clone(&state.jwks).run());

    let limiter = Arc::clone(&state.limiter);
    let revocations = state.revocations.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        interval.tick().await;
        loop {
            interval.tick().await;
            limiter.cleanup(Duration::from_secs(300));
            if let Some(cache) = &revocations {
                cache.cleanup();
            }
        }
    });

    let metrics_handle = state.metrics.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            metrics_handle.run_upkeep();
        }
    });

    let app = Router::new()
        .route("/healthz", get(admin::health))
        .route("/readyz", get(admin::ready))
        .route("/metrics", get(admin::metrics))
        .fallback(proxy::proxy_handler)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security::security_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            observability::metrics_middleware,
        ))
        .layer(middleware::from_fn(rusto2_common::request_id_middleware))
        .with_state(state);

    let listener = TcpListener::bind(&listen)
        .await
        .with_context(|| format!("cannot bind rusto2-gw to {listen}"))?
        .tap_io(|stream| {
            if let Err(error) = stream.set_nodelay(true) {
                tracing::debug!(error = %error, "failed to set TCP_NODELAY");
            }
        });
    info!(listen = %listen, "rusto2-gw listening");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(rusto2_common::shutdown_signal())
    .await
    .context("rusto2-gw server failed")?;

    info!("rusto2-gw stopped");
    Ok(())
}
