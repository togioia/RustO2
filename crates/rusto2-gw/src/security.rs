use std::{collections::HashSet, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{
        header::{ALLOW, AUTHORIZATION, WWW_AUTHENTICATE},
        HeaderMap, HeaderValue, StatusCode,
    },
    middleware::Next,
    response::Response,
};
use jsonwebtoken::{decode, decode_header, Algorithm, Validation};
use serde::Deserialize;

use crate::{
    config::AuthMode,
    http_util::json_error,
    routes::{self, CompiledRoute, MatchedRoute, RouteMatch},
    state::AppState,
};

#[derive(Debug, Clone, Deserialize)]
pub struct Claims {
    pub sub: String,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub jti: Option<String>,
    /// OAuth2 space-delimited `scope` claim.
    #[serde(default)]
    pub scope: Option<String>,
    /// Optional array form used by some identity providers.
    #[serde(default)]
    pub scp: Option<Vec<String>>,
}

impl Claims {
    fn scope_set(&self) -> HashSet<&str> {
        let mut scopes = HashSet::new();
        if let Some(scope) = &self.scope {
            scopes.extend(scope.split_whitespace());
        }
        if let Some(scp) = &self.scp {
            scopes.extend(scp.iter().map(String::as_str));
        }
        scopes
    }
}

#[derive(Debug, Clone, Default)]
pub struct AuthContext {
    pub client_id: Option<String>,
}

#[derive(Debug)]
enum AuthFailure {
    MissingToken,
    MalformedToken,
    InvalidToken,
    UnknownKey,
    Revoked,
    RevocationUnavailable,
    InsufficientScope(String),
}

impl AuthFailure {
    fn reason(&self) -> &'static str {
        match self {
            Self::MissingToken => "missing_token",
            Self::MalformedToken => "malformed_token",
            Self::InvalidToken => "invalid_token",
            Self::UnknownKey => "unknown_key",
            Self::Revoked => "revoked_token",
            Self::RevocationUnavailable => "revocation_check_failed",
            Self::InsufficientScope(_) => "insufficient_scope",
        }
    }

    fn description(&self) -> String {
        match self {
            Self::MissingToken => "a Bearer access token is required".to_owned(),
            Self::MalformedToken => "the Authorization header is malformed".to_owned(),
            Self::InvalidToken => "the access token is invalid or expired".to_owned(),
            Self::UnknownKey => "the token signing key is not published by the issuer".to_owned(),
            Self::Revoked => "the access token has been revoked".to_owned(),
            Self::RevocationUnavailable => {
                "token revocation status could not be verified".to_owned()
            }
            Self::InsufficientScope(scope) => {
                format!("the access token does not grant the required scope: {scope}")
            }
        }
    }
}

/// Global security filter. It enforces the per-route method allowlist, validates
/// JWT signature/issuer/audience/expiry against the cached JWKS and checks the
/// scopes declared in `rusto2.yaml`.
pub async fn security_middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();

    if is_internal_path(&path) {
        return next.run(request).await;
    }

    let (route, captures) = match routes::find(&state.routes, &path, request.method()) {
        RouteMatch::Found { route, captures } => (Arc::clone(route), captures),
        RouteMatch::MethodNotAllowed { allow } => {
            let mut response = json_error(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "this method is not allowed by the route policy",
            );
            if let Ok(value) = HeaderValue::from_str(&allow) {
                response.headers_mut().insert(ALLOW, value);
            }
            return response;
        }
        RouteMatch::NotFound => {
            return json_error(
                StatusCode::NOT_FOUND,
                "not_found",
                "no route matches this path",
            );
        }
    };

    let auth = match route.cfg.auth.mode {
        AuthMode::None => AuthContext::default(),
        AuthMode::Jwt => match authenticate(&state, request.headers(), &route).await {
            Ok(context) => context,
            Err(failure) => {
                metrics::counter!(
                    "rusto2.auth.failures.total",
                    "reason" => failure.reason(),
                    "route" => route.cfg.name.clone()
                )
                .increment(1);
                tracing::debug!(
                    route = %route.cfg.name,
                    reason = failure.reason(),
                    "request rejected by security filter"
                );
                return auth_failure_response(failure);
            }
        },
    };

    request
        .extensions_mut()
        .insert(MatchedRoute { route, captures });
    request.extensions_mut().insert(auth);
    next.run(request).await
}

async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    route: &CompiledRoute,
) -> Result<AuthContext, AuthFailure> {
    let token = bearer_token(headers)?.to_owned();
    let claims = validate_token(state, &token).await?;
    let scopes = claims.scope_set();

    for required in &route.cfg.auth.scopes {
        if !scopes.contains(required.as_str()) {
            return Err(AuthFailure::InsufficientScope(required.clone()));
        }
    }

    if route.cfg.auth.revocation_check {
        let Some(jti) = claims.jti.as_deref() else {
            return Err(AuthFailure::InvalidToken);
        };
        let Some(checker) = state.revocations.as_ref() else {
            tracing::error!(
                route = %route.cfg.name,
                "revocation_check is enabled but no introspection client is configured"
            );
            return Err(AuthFailure::RevocationUnavailable);
        };

        match checker.is_active(jti, &token).await {
            Ok(true) => {}
            Ok(false) => return Err(AuthFailure::Revoked),
            Err(error) => {
                metrics::counter!(
                    "rusto2.revocation.errors.total",
                    "route" => route.cfg.name.clone()
                )
                .increment(1);
                tracing::warn!(
                    route = %route.cfg.name,
                    error = %error,
                    fail_open = state.config.identity.revocation_fail_open,
                    "introspection check failed"
                );
                if !state.config.identity.revocation_fail_open {
                    return Err(AuthFailure::RevocationUnavailable);
                }
            }
        }
    }

    Ok(AuthContext {
        client_id: claims
            .client_id
            .clone()
            .or_else(|| Some(claims.sub.clone())),
    })
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, AuthFailure> {
    let value = headers
        .get(AUTHORIZATION)
        .ok_or(AuthFailure::MissingToken)?
        .to_str()
        .map_err(|_| AuthFailure::MalformedToken)?;

    let Some((scheme, token)) = value.split_once(' ') else {
        return Err(AuthFailure::MalformedToken);
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(AuthFailure::MalformedToken);
    }

    let token = token.trim();
    if token.is_empty() {
        return Err(AuthFailure::MalformedToken);
    }
    Ok(token)
}

async fn validate_token(state: &AppState, token: &str) -> Result<Claims, AuthFailure> {
    let header = decode_header(token).map_err(|_| AuthFailure::MalformedToken)?;
    if header.alg != Algorithm::EdDSA {
        return Err(AuthFailure::InvalidToken);
    }

    let mut candidates = state.jwks.keys_for(header.kid.as_deref());
    if candidates.is_empty() {
        // A rotation may have introduced a new kid. This path is rate-limited
        // inside the cache and cannot be used to amplify requests.
        state.jwks.refresh_after_miss().await;
        candidates = state.jwks.keys_for(header.kid.as_deref());
    }
    if candidates.is_empty() {
        return Err(AuthFailure::UnknownKey);
    }

    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.set_issuer(&[state.config.identity.issuer.as_str()]);
    validation.set_audience(&[state.config.identity.audience.as_str()]);
    validation.leeway = state.config.identity.leeway_secs;
    validation.validate_nbf = true;

    let mut last_error = None;
    for key in &candidates {
        match decode::<Claims>(token, key, &validation) {
            Ok(data) => {
                if data.claims.sub.trim().is_empty() {
                    return Err(AuthFailure::InvalidToken);
                }
                return Ok(data.claims);
            }
            Err(error) => last_error = Some(error),
        }
    }

    tracing::debug!(error = ?last_error, "JWT validation failed");
    Err(AuthFailure::InvalidToken)
}

fn auth_failure_response(failure: AuthFailure) -> Response {
    let status = match failure {
        AuthFailure::InsufficientScope(_) => StatusCode::FORBIDDEN,
        AuthFailure::RevocationUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::UNAUTHORIZED,
    };

    if status == StatusCode::SERVICE_UNAVAILABLE {
        let mut response = json_error(status, failure.reason(), &failure.description());
        response.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            HeaderValue::from_static("1"),
        );
        return response;
    }

    let challenge = match &failure {
        AuthFailure::InsufficientScope(scope) => format!(
            "Bearer error=\"insufficient_scope\", scope=\"{}\"",
            scope.replace('"', "")
        ),
        _ => format!(
            "Bearer error=\"invalid_token\", error_description=\"{}\"",
            failure.description().replace('"', "'")
        ),
    };

    let mut response = json_error(status, failure.reason(), &failure.description());
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response.headers_mut().insert(WWW_AUTHENTICATE, value);
    }
    response
}

pub fn is_internal_path(path: &str) -> bool {
    matches!(path, "/healthz" | "/readyz" | "/metrics")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(scope: Option<&str>, scp: Option<&[&str]>) -> Claims {
        Claims {
            sub: "client".to_owned(),
            client_id: None,
            jti: None,
            scope: scope.map(ToOwned::to_owned),
            scp: scp.map(|values| values.iter().map(|value| (*value).to_owned()).collect()),
        }
    }

    #[test]
    fn merges_scope_and_scp_claims() {
        let claims = claims(Some("read write"), Some(&["admin"]));
        let set = claims.scope_set();
        assert!(set.contains("read"));
        assert!(set.contains("write"));
        assert!(set.contains("admin"));
    }

    #[test]
    fn bearer_parsing_is_case_insensitive() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "bearer abc.def.ghi".parse().unwrap());
        assert_eq!(bearer_token(&headers).unwrap(), "abc.def.ghi");
    }

    #[test]
    fn rejects_non_bearer_authorization() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, "Basic Zm9vOmJhcg==".parse().unwrap());
        assert!(matches!(
            bearer_token(&headers),
            Err(AuthFailure::MalformedToken)
        ));
    }
}
