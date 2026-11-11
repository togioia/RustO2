use std::time::Instant;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::{routes, security::is_internal_path, state::AppState};

/// Records request counters, latency histograms and in-flight gauge for every
/// path, including rejected requests. The scrape endpoint itself is excluded so
/// Prometheus traffic does not distort the numbers.
pub async fn metrics_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    if path == "/metrics" {
        return next.run(request).await;
    }

    let route_label = if is_internal_path(&path) {
        "internal".to_owned()
    } else {
        routes::find(&state.routes, &path)
            .map(|route| route.cfg.name.clone())
            .unwrap_or_else(|| "unmatched".to_owned())
    };
    let method = request.method().as_str().to_owned();

    metrics::gauge!("rusto2.gateway.inflight").increment(1.0);
    let started = Instant::now();
    let response = next.run(request).await;
    let elapsed = started.elapsed().as_secs_f64();
    metrics::gauge!("rusto2.gateway.inflight").decrement(1.0);

    let status = response.status().as_u16().to_string();
    metrics::counter!(
        "rusto2.http.requests.total",
        "route" => route_label.clone(),
        "method" => method,
        "status" => status
    )
    .increment(1);
    metrics::histogram!(
        "rusto2.http.request.duration.seconds",
        "route" => route_label
    )
    .record(elapsed);

    response
}
