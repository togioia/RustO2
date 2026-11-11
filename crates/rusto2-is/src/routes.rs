use axum::{
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{
        header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH},
        HeaderMap, HeaderValue, StatusCode,
    },
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;

use crate::{state::AppState, token};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route(
            "/oauth2/token",
            post(token::issue_token).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/oauth2/introspect",
            post(token::introspect).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/oauth2/revoke",
            post(token::revoke).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/.well-known/jwks.json", get(jwks))
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/healthz", get(health))
        .route("/metrics", get(metrics))
        .layer(middleware::from_fn(rusto2_common::request_id_middleware))
        .with_state(state)
}

async fn index() -> impl IntoResponse {
    Json(json!({
        "service": "rusto2-is",
        "version": env!("CARGO_PKG_VERSION"),
        "endpoints": {
            "token": "/oauth2/token",
            "introspect": "/oauth2/introspect",
            "revoke": "/oauth2/revoke",
            "jwks": "/.well-known/jwks.json",
            "discovery": "/.well-known/openid-configuration",
            "health": "/healthz",
            "metrics": "/metrics"
        }
    }))
}

async fn jwks(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(if_none_match) = headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        if if_none_match
            .split(',')
            .any(|candidate| candidate.trim() == state.key.jwks_etag)
        {
            return StatusCode::NOT_MODIFIED.into_response();
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/jwk-set+json")
        .header(CACHE_CONTROL, "public, max-age=300")
        .header(ETAG, state.key.jwks_etag.as_str())
        .body(Body::from(state.key.jwks_json.clone()))
        .expect("static JWKS response")
}

async fn discovery(State(state): State<AppState>) -> impl IntoResponse {
    let mut scopes: Vec<String> = state
        .config
        .enabled_clients()
        .flat_map(|client| client.scopes.iter().cloned())
        .collect();
    scopes.sort();
    scopes.dedup();

    Json(json!({
        "issuer": state.config.token.issuer,
        "token_endpoint": format!("{}/oauth2/token", state.config.token.issuer.trim_end_matches('/')),
        "introspection_endpoint": format!("{}/oauth2/introspect", state.config.token.issuer.trim_end_matches('/')),
        "revocation_endpoint": format!("{}/oauth2/revoke", state.config.token.issuer.trim_end_matches('/')),
        "jwks_uri": format!("{}/.well-known/jwks.json", state.config.token.issuer.trim_end_matches('/')),
        "grant_types_supported": ["client_credentials"],
        "response_types_supported": ["token"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
        "scopes_supported": scopes,
    }))
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "service": "rusto2-is",
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.started.elapsed().as_secs(),
        "issuer": state.config.token.issuer,
        "clients": state.clients.len(),
        "kid": state.key.kid,
        "revoked_tokens": state.revocations.len(),
    }))
}

async fn metrics(State(state): State<AppState>) -> Response {
    (
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        )],
        state.metrics.render(),
    )
        .into_response()
}
