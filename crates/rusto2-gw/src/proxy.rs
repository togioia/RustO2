use std::{net::SocketAddr, sync::Arc, time::Instant};

use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::{
        header::{CONTENT_TYPE, HOST, RETRY_AFTER},
        HeaderMap, HeaderName, HeaderValue, StatusCode, Uri,
    },
    response::Response,
};
use rusto2_synapse::{
    MediationPlan, MessageContext, Payload, SynapseError, Transport, UpstreamRequest,
    UpstreamResponse,
};

use crate::{
    circuit_breaker::{Acquire, CircuitBreaker},
    http_util::{json_error, strip_hop_by_hop},
    rate_limit::Decision,
    routes::{CompiledRoute, MatchedRoute},
    security::AuthContext,
    state::AppState,
};

const X_FORWARDED_FOR: &str = "x-forwarded-for";
const X_FORWARDED_HOST: &str = "x-forwarded-host";
const X_FORWARDED_PROTO: &str = "x-forwarded-proto";
const X_REQUEST_ID: &str = "x-request-id";
const X_RUSTO2_REQUEST_ID: &str = "x-rusto2-request-id";
const X_RUSTO2_ROUTE: &str = "x-rusto2-route";
const X_RATELIMIT_LIMIT: &str = "x-ratelimit-limit";
const X_RATELIMIT_REMAINING: &str = "x-ratelimit-remaining";

/// Streaming reverse proxy. Request and response bodies are never buffered in
/// memory: the axum data stream is handed directly to reqwest and vice versa.
pub async fn proxy_handler(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    let started = Instant::now();

    let Some(matched) = request.extensions().get::<MatchedRoute>().cloned() else {
        return json_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "no route matches this path",
        );
    };
    let MatchedRoute { route, captures } = matched;
    let auth = request
        .extensions()
        .get::<AuthContext>()
        .cloned()
        .unwrap_or_default();
    let request_id = request
        .extensions()
        .get::<rusto2_common::RequestId>()
        .map(|id| id.0.clone());

    let method = request.method().clone();
    let uri = request.uri().clone();
    let headers = request.headers().clone();

    let client_ip = client_ip(&headers, peer, state.config.server.trust_proxy);

    let rate_decision = if let Some(policy) = &route.cfg.rate_limit {
        let key = match policy.key {
            crate::config::RateLimitKey::Client => auth
                .client_id
                .clone()
                .unwrap_or_else(|| client_ip.to_string()),
            crate::config::RateLimitKey::Ip => client_ip.to_string(),
        };
        let limiter_key = format!("{}:{}", route.cfg.name, key);

        match state
            .limiter
            .check(&limiter_key, policy.rps, policy.effective_burst())
            .await
        {
            Ok(decision) => {
                if !decision.allowed {
                    metrics::counter!(
                        "rusto2.rate.limited.total",
                        "route" => route.cfg.name.clone()
                    )
                    .increment(1);
                    tracing::debug!(
                        route = %route.cfg.name,
                        client = %key,
                        "request blocked by rate limiter"
                    );

                    let mut response = json_error(
                        StatusCode::TOO_MANY_REQUESTS,
                        "rate_limited",
                        "request rate limit exceeded for this client",
                    );
                    apply_rate_headers(&mut response, &decision, true);
                    return response;
                }
                Some(decision)
            }
            Err(error) => {
                metrics::counter!(
                    "rusto2.rate_limit.errors.total",
                    "route" => route.cfg.name.clone(),
                    "backend" => state.limiter.backend_name()
                )
                .increment(1);
                tracing::warn!(
                    route = %route.cfg.name,
                    backend = state.limiter.backend_name(),
                    error = %error,
                    fail_open = state.config.rate_limiting.fail_open,
                    "rate limiter backend failed"
                );

                if state.config.rate_limiting.fail_open {
                    None
                } else {
                    let mut response = json_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "rate_limiter_unavailable",
                        "the rate limit backend is unavailable",
                    );
                    response
                        .headers_mut()
                        .insert(RETRY_AFTER, HeaderValue::from_static("1"));
                    return response;
                }
            }
        }
    } else {
        None
    };

    let breaker = state.breakers.get(&route.cfg.name);
    if let Some(breaker) = &breaker {
        if let Acquire::Rejected { retry_after_secs } = breaker.acquire() {
            metrics::counter!(
                "rusto2.circuit.rejected.total",
                "route" => route.cfg.name.clone()
            )
            .increment(1);
            tracing::warn!(
                route = %route.cfg.name,
                retry_after_secs,
                "request rejected by open circuit breaker"
            );

            let mut response = json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "circuit_open",
                "backend circuit breaker is open",
            );
            if let Ok(value) = HeaderValue::from_str(&retry_after_secs.to_string()) {
                response.headers_mut().insert(RETRY_AFTER, value);
            }
            return response;
        }
    }

    if let Some(plan) = route.mediation.clone() {
        return mediated_proxy(
            state.clone(),
            route,
            plan,
            captures,
            method,
            uri,
            headers,
            request.into_body(),
            rate_decision,
            breaker,
            request_id,
            started,
        )
        .await;
    }

    let rest = if route.cfg.strip_prefix {
        uri.path()
            .strip_prefix(&route.cfg.prefix)
            .unwrap_or(uri.path())
    } else {
        uri.path()
    };
    let upstream_url = upstream_url(&route.cfg.upstream, rest, uri.query());

    let outbound = match build_outbound_headers(
        &headers,
        &state,
        &route.cfg.name,
        client_ip,
        request_id.as_deref(),
    ) {
        Ok(headers) => headers,
        Err(error) => {
            tracing::warn!(error = %error, "failed to build upstream headers");
            let mut response = json_error(
                StatusCode::BAD_REQUEST,
                "invalid_headers",
                "the request contains headers that cannot be forwarded",
            );
            if let Some(decision) = &rate_decision {
                apply_rate_headers(&mut response, decision, false);
            }
            return response;
        }
    };

    let upstream = state
        .client
        .request(method.clone(), &upstream_url)
        .headers(outbound)
        .body(reqwest::Body::wrap_stream(
            request.into_body().into_data_stream(),
        ))
        .send()
        .await;

    match upstream {
        Ok(upstream_response) => {
            let status = upstream_response.status();
            let mut response_headers = upstream_response.headers().clone();
            strip_hop_by_hop(&mut response_headers);

            if let Some(request_id) = &request_id {
                if let Ok(value) = HeaderValue::from_str(request_id) {
                    response_headers.insert(HeaderName::from_static(X_RUSTO2_REQUEST_ID), value);
                }
            }

            let mut response = Response::builder()
                .status(status)
                .body(Body::from_stream(upstream_response.bytes_stream()))
                .expect("valid upstream response");

            *response.headers_mut() = response_headers;

            if let Some(decision) = &rate_decision {
                apply_rate_headers(&mut response, decision, false);
            }

            if status.is_server_error() {
                metrics::counter!(
                    "rusto2.upstream.errors.total",
                    "route" => route.cfg.name.clone()
                )
                .increment(1);
                if let Some(breaker) = &breaker {
                    if breaker.record_failure() {
                        metrics::counter!(
                            "rusto2.circuit.open.total",
                            "route" => route.cfg.name.clone()
                        )
                        .increment(1);
                        tracing::warn!(
                            route = %route.cfg.name,
                            status = status.as_u16(),
                            "circuit breaker opened after consecutive 5xx responses"
                        );
                    }
                }
            } else if let Some(breaker) = &breaker {
                breaker.record_success();
            }

            tracing::debug!(
                route = %route.cfg.name,
                status = status.as_u16(),
                upstream = %upstream_url,
                latency_ms = started.elapsed().as_secs_f64() * 1_000.0,
                "proxied request"
            );
            response
        }
        Err(error) => {
            metrics::counter!(
                "rusto2.upstream.errors.total",
                "route" => route.cfg.name.clone()
            )
            .increment(1);
            if let Some(breaker) = &breaker {
                if breaker.record_failure() {
                    metrics::counter!(
                        "rusto2.circuit.open.total",
                        "route" => route.cfg.name.clone()
                    )
                    .increment(1);
                }
            }
            tracing::warn!(
                route = %route.cfg.name,
                upstream = %upstream_url,
                error = %error,
                "upstream request failed"
            );

            let mut response = json_error(
                StatusCode::BAD_GATEWAY,
                "bad_gateway",
                "the upstream service could not be reached",
            );
            if let Some(decision) = &rate_decision {
                apply_rate_headers(&mut response, decision, false);
            }
            response
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn mediated_proxy(
    state: AppState,
    route: Arc<CompiledRoute>,
    plan: Arc<MediationPlan>,
    captures: Vec<(String, String)>,
    method: axum::http::Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
    rate_decision: Option<Decision>,
    breaker: Option<Arc<CircuitBreaker>>,
    request_id: Option<String>,
    started: Instant,
) -> Response {
    let body = match axum::body::to_bytes(body, state.mediation_body_bytes).await {
        Ok(body) => body,
        Err(_) => {
            let mut response = json_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "the request body exceeds artifacts.max_body_bytes",
            );
            if let Some(decision) = &rate_decision {
                apply_rate_headers(&mut response, decision, false);
            }
            return response;
        }
    };

    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let mut context = MessageContext::new(
        method.as_str(),
        uri.path(),
        uri.query().map(ToOwned::to_owned),
        headers,
        Payload::from_bytes(content_type.as_deref(), body.to_vec()),
    );
    context.set_property(
        rusto2_synapse::PropertyScope::Default,
        "REST_URL_POSTFIX",
        rest_postfix(&route.cfg.prefix, uri.path()),
    );
    for (name, value) in captures {
        context.set_property(
            rusto2_synapse::PropertyScope::Default,
            format!("uri.var.{name}"),
            value,
        );
    }

    let transport = ReqwestTransport {
        client: state.client.clone(),
    };
    match rusto2_synapse::mediate::mediate(&plan, &transport, context).await {
        Ok(result) => {
            let mut response = Response::builder()
                .status(StatusCode::from_u16(result.status).unwrap_or(StatusCode::BAD_GATEWAY))
                .body(Body::from(result.body))
                .expect("valid mediated response");
            *response.headers_mut() = result.headers;
            strip_hop_by_hop(response.headers_mut());
            // Mediation may re-serialize the payload (for example JSON is
            // compacted after payloadFactory), so backend framing headers are
            // no longer valid. Hyper recomputes Content-Length.
            response
                .headers_mut()
                .remove(axum::http::header::CONTENT_LENGTH);
            response
                .headers_mut()
                .remove(axum::http::header::CONTENT_ENCODING);

            if let Some(request_id) = &request_id {
                if let Ok(value) = HeaderValue::from_str(request_id) {
                    response
                        .headers_mut()
                        .insert(HeaderName::from_static(X_RUSTO2_REQUEST_ID), value);
                }
            }
            if let Some(decision) = &rate_decision {
                apply_rate_headers(&mut response, decision, false);
            }

            if result.status >= 500 {
                metrics::counter!(
                    "rusto2.upstream.errors.total",
                    "route" => route.cfg.name.clone()
                )
                .increment(1);
                if let Some(breaker) = &breaker {
                    if breaker.record_failure() {
                        metrics::counter!(
                            "rusto2.circuit.open.total",
                            "route" => route.cfg.name.clone()
                        )
                        .increment(1);
                    }
                }
            } else if let Some(breaker) = &breaker {
                breaker.record_success();
            }

            tracing::debug!(
                route = %route.cfg.name,
                plan = %plan.name,
                status = result.status,
                latency_ms = started.elapsed().as_secs_f64() * 1_000.0,
                "mediated request completed"
            );
            response
        }
        Err(error) => {
            metrics::counter!(
                "rusto2.upstream.errors.total",
                "route" => route.cfg.name.clone()
            )
            .increment(1);
            if let Some(breaker) = &breaker {
                if breaker.record_failure() {
                    metrics::counter!(
                        "rusto2.circuit.open.total",
                        "route" => route.cfg.name.clone()
                    )
                    .increment(1);
                }
            }
            tracing::warn!(
                route = %route.cfg.name,
                plan = %plan.name,
                error = %error,
                "mediation failed"
            );
            let mut response = json_error(
                StatusCode::BAD_GATEWAY,
                "mediation_failed",
                "the WSO2 mediation flow could not be completed",
            );
            if let Some(decision) = &rate_decision {
                apply_rate_headers(&mut response, decision, false);
            }
            response
        }
    }
}

fn rest_postfix(prefix: &str, path: &str) -> String {
    if path == prefix {
        return "/".to_owned();
    }
    path.strip_prefix(prefix)
        .filter(|rest| rest.starts_with('/'))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.to_owned())
}

struct ReqwestTransport {
    client: reqwest::Client,
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn send(&self, request: UpstreamRequest) -> Result<UpstreamResponse, SynapseError> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|error| SynapseError::Transport(error.to_string()))?;
        let response = self
            .client
            .request(method, &request.url)
            .headers(request.headers)
            .body(request.body)
            .send()
            .await
            .map_err(|error| SynapseError::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response
            .bytes()
            .await
            .map_err(|error| SynapseError::Transport(error.to_string()))?
            .to_vec();
        Ok(UpstreamResponse {
            status,
            headers,
            body,
        })
    }
}

fn build_outbound_headers(
    headers: &HeaderMap,
    state: &AppState,
    route_name: &str,
    client_ip: std::net::IpAddr,
    request_id: Option<&str>,
) -> anyhow::Result<HeaderMap> {
    let mut outbound = headers.clone();
    strip_hop_by_hop(&mut outbound);
    outbound.remove(HOST);

    if let Some(host) = headers.get(HOST).cloned() {
        outbound.insert(HeaderName::from_static(X_FORWARDED_HOST), host);
    }

    let forwarded_for = match headers
        .get(X_FORWARDED_FOR)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
    {
        Some(existing) => format!("{existing}, {client_ip}"),
        None => client_ip.to_string(),
    };
    outbound.insert(
        HeaderName::from_static(X_FORWARDED_FOR),
        HeaderValue::from_str(&forwarded_for)?,
    );

    let forwarded_proto = if state.config.server.trust_proxy {
        headers.get(X_FORWARDED_PROTO).cloned()
    } else {
        None
    }
    .unwrap_or_else(|| HeaderValue::from_static("http"));
    outbound.insert(HeaderName::from_static(X_FORWARDED_PROTO), forwarded_proto);

    if let Some(request_id) = request_id {
        let value = HeaderValue::from_str(request_id)?;
        outbound.insert(HeaderName::from_static(X_RUSTO2_REQUEST_ID), value.clone());
        outbound.insert(HeaderName::from_static(X_REQUEST_ID), value);
    }

    outbound.insert(
        HeaderName::from_static(X_RUSTO2_ROUTE),
        HeaderValue::from_str(route_name)?,
    );

    Ok(outbound)
}

fn apply_rate_headers(response: &mut Response, decision: &Decision, include_retry: bool) {
    if let Ok(value) = HeaderValue::from_str(&decision.limit.to_string()) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(X_RATELIMIT_LIMIT), value);
    }
    if let Ok(value) = HeaderValue::from_str(&decision.remaining.to_string()) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(X_RATELIMIT_REMAINING), value);
    }
    if include_retry {
        if let Some(seconds) = decision.retry_after_secs {
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(RETRY_AFTER, value);
            }
        }
    }
}

fn client_ip(headers: &HeaderMap, peer: SocketAddr, trust_proxy: bool) -> std::net::IpAddr {
    if trust_proxy {
        if let Some(ip) = headers
            .get(X_FORWARDED_FOR)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .and_then(|value| value.trim().parse().ok())
        {
            return ip;
        }
    }
    peer.ip()
}

fn upstream_url(base: &str, rest: &str, query: Option<&str>) -> String {
    let mut url = base.trim_end_matches('/').to_owned();
    if rest.is_empty() {
        url.push('/');
    } else if rest.starts_with('/') {
        url.push_str(rest);
    } else {
        url.push('/');
        url.push_str(rest);
    }
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes;

    #[test]
    fn joins_upstream_paths_and_queries() {
        assert_eq!(
            upstream_url("http://backend:8080", "/get", None),
            "http://backend:8080/get"
        );
        assert_eq!(
            upstream_url("http://backend:8080/", "/get", Some("y=2")),
            "http://backend:8080/get?y=2"
        );
        assert_eq!(
            upstream_url("http://backend:8080", "", None),
            "http://backend:8080/"
        );
        assert_eq!(
            upstream_url("http://backend/base", "get", Some("a=1")),
            "http://backend/base/get?a=1"
        );
    }

    #[test]
    fn forwarded_for_appends_the_peer() {
        let mut headers = HeaderMap::new();
        headers.insert(X_FORWARDED_FOR, "203.0.113.7".parse().unwrap());
        let peer: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        assert_eq!(
            client_ip(&headers, peer, false),
            "127.0.0.1".parse::<std::net::IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(&headers, peer, true),
            "203.0.113.7".parse::<std::net::IpAddr>().unwrap()
        );
    }

    #[test]
    fn route_module_is_exercised() {
        let routes = routes::compile_all(
            &crate::config::GatewayConfig {
                server: Default::default(),
                identity: crate::config::IdentityConfig {
                    issuer: "http://issuer".into(),
                    audience: "rusto2".into(),
                    jwks_url: "http://issuer/jwks".into(),
                    refresh_secs: 300,
                    leeway_secs: 15,
                    introspection_url: None,
                    introspection_client_id: None,
                    introspection_client_secret: None,
                    revocation_cache_secs: 5,
                    revocation_fail_open: true,
                },
                rate_limiting: Default::default(),
                artifacts: Default::default(),
                routes: vec![crate::config::RouteConfig {
                    name: "api".into(),
                    prefix: "/api".into(),
                    upstream: "http://backend".into(),
                    strip_prefix: true,
                    methods: vec!["GET".into()],
                    auth: Default::default(),
                    rate_limit: None,
                    circuit_breaker: None,
                }],
            },
            &[],
        )
        .expect("compile");
        assert_eq!(routes.len(), 1);
    }
}
