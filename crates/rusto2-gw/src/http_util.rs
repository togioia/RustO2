use axum::{
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Consistent JSON error envelope used by the gateway.
pub fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        [(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        Json(json!({
            "error": code,
            "message": message,
        })),
    )
        .into_response()
}

/// Hop-by-hop headers must not be forwarded by a proxy (RFC 9110 §7.6.1).
pub fn strip_hop_by_hop(headers: &mut axum::http::HeaderMap) {
    // Headers nominated by the Connection header are hop-by-hop as well.
    let nominated: Vec<String> = headers
        .get("connection")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();

    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }

    for token in nominated {
        headers.remove(token);
    }
}
