mod config;
mod keys;
mod revocation;
mod routes;
mod state;
mod token;

use anyhow::Context;
use axum::serve::ListenerExt;
use tokio::net::TcpListener;
use tracing::info;

#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rusto2_common::init_tracing();

    let config_path =
        std::env::var("RUSTO2_CONFIG").unwrap_or_else(|_| "rusto2-is.yaml".to_owned());
    let config = config::Config::load(&config_path)?;
    let key = keys::load_or_generate(&config.keys)?;
    let metrics = rusto2_common::install_metrics()?;

    info!(
        config = %config_path,
        issuer = %config.token.issuer,
        audience = %config.token.audience,
        clients = config.enabled_clients().count(),
        kid = %key.kid,
        "rusto2-is starting"
    );

    let state = state::AppState::new(config, key, metrics)?;
    let listen = state.config.server.listen.clone();

    let revocations = state.revocations.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.tick().await;
        loop {
            interval.tick().await;
            revocations.cleanup();
        }
    });

    let app = routes::router(state);

    let listener = TcpListener::bind(&listen)
        .await
        .with_context(|| format!("cannot bind rusto2-is to {listen}"))?
        .tap_io(|stream| {
            if let Err(error) = stream.set_nodelay(true) {
                tracing::debug!(error = %error, "failed to set TCP_NODELAY");
            }
        });
    info!(listen = %listen, "rusto2-is listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(rusto2_common::shutdown_signal())
        .await
        .context("rusto2-is server failed")?;

    info!("rusto2-is stopped");
    Ok(())
}
