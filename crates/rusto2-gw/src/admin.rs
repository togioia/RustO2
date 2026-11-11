use axum::{
    extract::State,
    http::{header::CONTENT_TYPE, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::state::AppState;

pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let jwks = state.jwks.status();
    Json(json!({
        "status": "ok",
        "service": "rusto2-gw",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.started.elapsed().as_secs(),
        "routes": state.routes.len(),
        "jwks": {
            "keys": jwks.keys,
            "last_refresh_age_secs": jwks.last_refresh_age_secs,
        },
        "rate_limit": {
            "backend": state.limiter.backend_name(),
            "local_buckets": state.limiter.len(),
        },
        "revocation_cache_entries": state.revocations.as_ref().map(|cache| cache.len()).unwrap_or(0),
        "circuit_breakers": state.breakers.snapshot(),
    }))
}

pub async fn ready(State(state): State<AppState>) -> Response {
    if !state.jwks.has_keys() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "not_ready",
                "reason": "JWKS has not been loaded",
            })),
        )
            .into_response();
    }

    Json(json!({
        "status": "ready",
        "jwks_keys": state.jwks.status().keys,
    }))
    .into_response()
}

pub async fn metrics(State(state): State<AppState>) -> Response {
    (
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        )],
        state.metrics.render(),
    )
        .into_response()
}
