use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue},
    middleware::Next,
    response::Response,
};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing::Instrument;
use uuid::Uuid;

/// Correlation header propagated by every RustO2 component and forwarded to backends.
pub const REQUEST_ID_HEADER: &str = "x-rusto2-request-id";

/// Request identifier stored in request extensions so handlers and middleware can reuse it.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// Install a JSON tracing subscriber. `RUST_LOG` overrides the default filter.
pub fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,rusto2_gw=debug,rusto2_is=debug")
    });

    let _ = tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_target(true)
        .with_env_filter(filter)
        .try_init();
}

/// Install the global Prometheus recorder and return the render handle.
///
/// Histogram buckets are expressed in seconds and tuned for a reverse proxy
/// (from 500 microseconds to 10 seconds).
pub fn install_metrics() -> anyhow::Result<PrometheusHandle> {
    let buckets = [
        0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
    ];

    let builder = PrometheusBuilder::new().set_buckets(&buckets)?;
    Ok(builder.install_recorder()?)
}

/// Assign a correlation id to every request, log a structured access line and
/// echo the id back to the caller.
///
/// A caller-supplied `X-RustO2-Request-Id` is honoured only when it looks like
/// an opaque id (printable ASCII, 8..=128 bytes); otherwise a UUIDv7 is minted.
pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| is_valid_request_id(value))
        .map(ToOwned::to_owned)
        .unwrap_or_else(new_request_id);

    let header_name = HeaderName::from_static(REQUEST_ID_HEADER);
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        request.headers_mut().insert(header_name.clone(), value);
    }
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let log_enabled = tracing::enabled!(tracing::Level::INFO);
    let span = log_enabled.then(|| {
        tracing::info_span!(
            "http.request",
            request_id = %request_id,
            method = %method,
            path = %path
        )
    });

    let started = Instant::now();
    let mut response = match &span {
        Some(span) => next.run(request).instrument(span.clone()).await,
        None => next.run(request).await,
    };
    let latency_ms = (started.elapsed().as_secs_f64() * 1_000_000.0).round() / 1_000.0;
    let status = response.status().as_u16();

    if let Some(span) = &span {
        tracing::info!(
            parent: span,
            status,
            latency_ms,
            "request completed"
        );
    }

    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(header_name, value);
    }

    response
}

fn is_valid_request_id(value: &str) -> bool {
    (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
}

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Time-ordered request id without a `getrandom` syscall on the hot path.
///
/// The high bits carry the Unix time in milliseconds, the middle bits the
/// process id, and the low bits an atomic per-process sequence. Correlation ids
/// only need to be unique within the tracing system and this is collision-safe
/// for a single process; a client-supplied id is still honoured upstream.
fn new_request_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = u128::from(std::process::id() & 0x0fff);
    let sequence =
        u128::from(REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)) & 0x3fff_ffff_ffff_ffff;

    let value = (millis << 80) | (0x7 << 76) | (pid << 64) | (0x2 << 62) | sequence;
    Uuid::from_u128(value).to_string()
}

/// Resolve on Ctrl+C or SIGTERM so containers shut down gracefully.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_uuid_and_rejects_junk() {
        assert!(is_valid_request_id("0192f0a1-7c3e-7b1a-9a2e-5f2b1c3d4e5f"));
        assert!(is_valid_request_id("req_12345678"));
        assert!(!is_valid_request_id("short"));
        assert!(!is_valid_request_id("bad value with spaces"));
        assert!(!is_valid_request_id("bad\"quote"));
        assert!(!is_valid_request_id(&"a".repeat(129)));
    }

    #[test]
    fn generated_request_ids_are_unique_and_time_ordered() {
        let first = new_request_id();
        let second = new_request_id();
        assert_ne!(first, second);
        assert_eq!(first.len(), 36);
        assert!(first < second, "later ids should sort after earlier ones");
    }
}
