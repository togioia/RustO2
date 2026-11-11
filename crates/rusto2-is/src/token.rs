use std::{
    collections::{BTreeSet, HashMap},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::State,
    http::{
        header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, PRAGMA, WWW_AUTHENTICATE},
        HeaderMap, StatusCode,
    },
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use bytes::Bytes;
use jsonwebtoken::{decode, encode, Algorithm, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::state::{AppState, RegisteredClient};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub iss: String,
    pub aud: String,
    pub exp: u64,
    pub nbf: u64,
    pub iat: u64,
    pub jti: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

struct OAuthError {
    status: StatusCode,
    code: &'static str,
    description: String,
    basic_challenge: bool,
}

impl OAuthError {
    fn invalid_request(description: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            description: description.into(),
            basic_challenge: false,
        }
    }

    fn invalid_client(description: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_client",
            description: description.into(),
            basic_challenge: true,
        }
    }

    fn invalid_scope(description: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_scope",
            description: description.into(),
            basic_challenge: false,
        }
    }

    fn insufficient_scope(description: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "insufficient_scope",
            description: description.into(),
            basic_challenge: false,
        }
    }

    fn unsupported_grant_type() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "unsupported_grant_type",
            description: "only grant_type=client_credentials is supported".to_owned(),
            basic_challenge: false,
        }
    }

    fn into_response_with_metrics(self, _state: &AppState) -> Response {
        metrics::counter!(
            "rusto2.identity.token.errors.total",
            "reason" => self.code.to_owned()
        )
        .increment(1);

        let mut response = (
            self.status,
            axum::Json(json!({
                "error": self.code,
                "error_description": self.description,
            })),
        )
            .into_response();
        response.headers_mut().insert(
            CACHE_CONTROL,
            "no-store".parse().expect("static header value"),
        );
        response
            .headers_mut()
            .insert(PRAGMA, "no-cache".parse().expect("static header value"));
        if self.basic_challenge {
            response.headers_mut().insert(
                WWW_AUTHENTICATE,
                "Basic realm=\"rusto2-is\""
                    .parse()
                    .expect("static header value"),
            );
        }
        response
    }
}

/// `POST /oauth2/token` implementing the OAuth2 client credentials grant.
pub async fn issue_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let params = match parse_params(&headers, &body) {
        Ok(params) => params,
        Err(description) => {
            return OAuthError::invalid_request(description).into_response_with_metrics(&state);
        }
    };

    let credentials = match extract_credentials(&headers, &params) {
        Ok(credentials) => credentials,
        Err(error) => return error.into_response_with_metrics(&state),
    };

    match params.get("grant_type").map(String::as_str) {
        Some("client_credentials") => {}
        Some(_) => {
            return OAuthError::unsupported_grant_type().into_response_with_metrics(&state);
        }
        None => {
            return OAuthError::invalid_request("grant_type is required")
                .into_response_with_metrics(&state);
        }
    }

    let Some(client) = state.clients.authenticate(&credentials.0, &credentials.1) else {
        tracing::warn!(client_id = %credentials.0, "client authentication failed");
        return OAuthError::invalid_client("invalid client credentials")
            .into_response_with_metrics(&state);
    };

    let scopes = match resolve_scopes(params.get("scope").map(String::as_str), &client) {
        Ok(scopes) => scopes,
        Err(unknown) => {
            return OAuthError::invalid_scope(format!(
                "scope not allowed for this client: {}",
                unknown.join(" ")
            ))
            .into_response_with_metrics(&state);
        }
    };

    let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(_) => {
            tracing::error!("system clock is before the UNIX epoch");
            return oauth_server_error();
        }
    };
    let Some(exp) = now.checked_add(state.config.token.ttl_secs) else {
        tracing::error!("token expiry overflows u64");
        return oauth_server_error();
    };

    let scope_claim = if scopes.is_empty() {
        None
    } else {
        Some(scopes.join(" "))
    };
    let claims = Claims {
        sub: client.client_id.clone(),
        iss: state.config.token.issuer.clone(),
        aud: state.config.token.audience.clone(),
        exp,
        nbf: now,
        iat: now,
        jti: Uuid::now_v7().to_string(),
        scope: scope_claim.clone(),
        client_id: Some(client.client_id.clone()),
    };

    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some(state.key.kid.clone());

    let token = match encode(&header, &claims, &state.key.encoding) {
        Ok(token) => token,
        Err(error) => {
            tracing::error!(error = %error, "failed to sign access token");
            return oauth_server_error();
        }
    };

    metrics::counter!(
        "rusto2.identity.tokens.issued.total",
        "client" => client.client_id.clone()
    )
    .increment(1);

    tracing::info!(
        client_id = %client.client_id,
        scopes = ?scopes,
        expires_in = state.config.token.ttl_secs,
        "issued access token"
    );

    let mut response = (
        StatusCode::OK,
        axum::Json({
            let mut payload = json!({
                "access_token": token,
                "token_type": "Bearer",
                "expires_in": state.config.token.ttl_secs,
            });
            if let Some(scope) = &scope_claim {
                payload["scope"] = json!(scope);
            }
            payload
        }),
    )
        .into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        "no-store".parse().expect("static header value"),
    );
    response
        .headers_mut()
        .insert(PRAGMA, "no-cache".parse().expect("static header value"));
    response
}

/// `POST /oauth2/introspect` (RFC 7662).
pub async fn introspect(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let params = match parse_params(&headers, &body) {
        Ok(params) => params,
        Err(description) => {
            return OAuthError::invalid_request(description).into_response_with_metrics(&state);
        }
    };

    let client = match authenticate_client(&state, &headers, &params) {
        Ok(client) => client,
        Err(error) => return error.into_response_with_metrics(&state),
    };
    if !client.scopes.iter().any(|scope| scope == "introspect") {
        return OAuthError::insufficient_scope("the client is not authorised to introspect tokens")
            .into_response_with_metrics(&state);
    }

    let Some(token) = params.get("token").filter(|token| !token.is_empty()) else {
        return OAuthError::invalid_request("token is required").into_response_with_metrics(&state);
    };

    let active = match decode_claims(&state, token, true) {
        Ok(claims) => {
            if state.revocations.is_revoked(&claims.jti) {
                None
            } else {
                Some(claims)
            }
        }
        Err(_) => None,
    };

    metrics::counter!(
        "rusto2.identity.introspections.total",
        "result" => if active.is_some() { "active" } else { "inactive" }
    )
    .increment(1);

    let payload = match active {
        Some(claims) => json!({
            "active": true,
            "sub": claims.sub,
            "client_id": claims.client_id,
            "scope": claims.scope,
            "iss": claims.iss,
            "aud": claims.aud,
            "exp": claims.exp,
            "iat": claims.iat,
            "nbf": claims.nbf,
            "jti": claims.jti,
            "token_type": "Bearer",
        }),
        None => json!({ "active": false }),
    };

    no_store((StatusCode::OK, axum::Json(payload)).into_response())
}

/// `POST /oauth2/revoke` (RFC 7009). Only the client that owns the token can
/// revoke it. Invalid or unknown tokens still return 200 to avoid token oracle
/// behaviour, as required by the RFC.
pub async fn revoke(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let params = match parse_params(&headers, &body) {
        Ok(params) => params,
        Err(description) => {
            return OAuthError::invalid_request(description).into_response_with_metrics(&state);
        }
    };

    let client = match authenticate_client(&state, &headers, &params) {
        Ok(client) => client,
        Err(error) => return error.into_response_with_metrics(&state),
    };

    let Some(token) = params.get("token").filter(|token| !token.is_empty()) else {
        return OAuthError::invalid_request("token is required").into_response_with_metrics(&state);
    };

    // Revocation deliberately ignores `exp`: an expired token can still be
    // declared revoked without changing the observable result.
    if let Ok(claims) = decode_claims(&state, token, false) {
        if claims.sub == client.client_id {
            let jti = claims.jti.as_str();
            if !jti.is_empty() {
                state.revocations.revoke(jti, claims.exp);
                metrics::counter!("rusto2.identity.revocations.total").increment(1);
                tracing::info!(
                    client_id = %client.client_id,
                    jti = %jti,
                    exp = claims.exp,
                    "access token revoked"
                );
            }
        } else {
            tracing::debug!(
                client_id = %client.client_id,
                token_subject = %claims.sub,
                "ignoring revocation request for a token owned by another client"
            );
        }
    }

    no_store(StatusCode::OK.into_response())
}

fn authenticate_client(
    state: &AppState,
    headers: &HeaderMap,
    params: &HashMap<String, String>,
) -> Result<std::sync::Arc<RegisteredClient>, OAuthError> {
    let credentials = extract_credentials(headers, params)?;
    state
        .clients
        .authenticate(&credentials.0, &credentials.1)
        .ok_or_else(|| OAuthError::invalid_client("invalid client credentials"))
}

fn decode_claims(
    state: &AppState,
    token: &str,
    validate_exp: bool,
) -> Result<Claims, jsonwebtoken::errors::Error> {
    let mut validation = Validation::new(Algorithm::EdDSA);
    let required: &[&str] = if validate_exp {
        &["exp", "iss", "aud", "sub"]
    } else {
        &["iss", "aud", "sub"]
    };
    validation.set_required_spec_claims(required);
    validation.set_issuer(&[state.config.token.issuer.as_str()]);
    validation.set_audience(&[state.config.token.audience.as_str()]);
    validation.validate_exp = validate_exp;
    validation.validate_nbf = validate_exp;

    decode::<Claims>(token, &state.key.decoding, &validation).map(|data| data.claims)
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        CACHE_CONTROL,
        "no-store".parse().expect("static header value"),
    );
    response
        .headers_mut()
        .insert(PRAGMA, "no-cache".parse().expect("static header value"));
    response
}

fn parse_params(headers: &HeaderMap, body: &[u8]) -> Result<HashMap<String, String>, String> {
    let is_json = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.starts_with("application/json"))
        .unwrap_or(false);

    if is_json {
        serde_json::from_slice(body).map_err(|_| "invalid JSON body".to_owned())
    } else {
        serde_urlencoded::from_bytes(body).map_err(|_| "invalid form body".to_owned())
    }
}

fn extract_credentials(
    headers: &HeaderMap,
    params: &HashMap<String, String>,
) -> Result<(String, String), OAuthError> {
    if let Some(value) = headers.get(AUTHORIZATION) {
        let value = value
            .to_str()
            .map_err(|_| OAuthError::invalid_client("malformed Authorization header"))?;
        let Some((scheme, encoded)) = value.split_once(' ') else {
            return Err(OAuthError::invalid_client("malformed Authorization header"));
        };
        if !scheme.eq_ignore_ascii_case("basic") {
            return Err(OAuthError::invalid_client(
                "only HTTP Basic client authentication is supported",
            ));
        }
        let decoded = STANDARD
            .decode(encoded.trim())
            .map_err(|_| OAuthError::invalid_client("invalid Basic credentials"))?;
        let decoded = String::from_utf8(decoded)
            .map_err(|_| OAuthError::invalid_client("invalid Basic credentials"))?;
        let Some((client_id, client_secret)) = decoded.split_once(':') else {
            return Err(OAuthError::invalid_client("invalid Basic credentials"));
        };
        return Ok((client_id.to_owned(), client_secret.to_owned()));
    }

    let client_id = params
        .get("client_id")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OAuthError::invalid_client("client_id is required"))?;
    let client_secret = params
        .get("client_secret")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OAuthError::invalid_client("client_secret is required"))?;

    Ok((client_id.clone(), client_secret.clone()))
}

/// Requested scopes must be a subset of the client's grant. An omitted scope
/// grants every scope assigned to the client.
fn resolve_scopes(
    requested: Option<&str>,
    client: &RegisteredClient,
) -> Result<Vec<String>, Vec<String>> {
    let requested: Vec<String> = requested
        .unwrap_or_default()
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect();

    if requested.is_empty() {
        return Ok(client.scopes.clone());
    }

    let allowed: BTreeSet<&str> = client.scopes.iter().map(String::as_str).collect();
    let unknown: Vec<String> = requested
        .iter()
        .filter(|scope| !allowed.contains(scope.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(unknown);
    }

    let mut deduped = Vec::new();
    for scope in requested {
        if !deduped.contains(&scope) {
            deduped.push(scope);
        }
    }
    Ok(deduped)
}

fn oauth_server_error() -> Response {
    metrics::counter!("rusto2.identity.token.errors.total", "reason" => "server_error")
        .increment(1);
    let mut response = (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(json!({
            "error": "server_error",
            "error_description": "unable to issue token",
        })),
    )
        .into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        "no-store".parse().expect("static header value"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(scopes: &[&str]) -> RegisteredClient {
        RegisteredClient {
            client_id: "demo-client".to_owned(),
            secret_hash: [0u8; 32],
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        }
    }

    #[test]
    fn omitted_scope_grants_all_client_scopes() {
        let scopes = resolve_scopes(None, &client(&["read", "write"])).expect("allowed");
        assert_eq!(scopes, vec!["read".to_owned(), "write".to_owned()]);
    }

    #[test]
    fn requested_scope_must_be_a_subset() {
        let scopes = resolve_scopes(Some("read"), &client(&["read", "write"])).expect("allowed");
        assert_eq!(scopes, vec!["read".to_owned()]);

        let unknown = resolve_scopes(Some("read admin"), &client(&["read", "write"]))
            .expect_err("must reject");
        assert_eq!(unknown, vec!["admin".to_owned()]);
    }

    #[test]
    fn duplicate_scopes_are_collapsed() {
        let scopes = resolve_scopes(Some("read read"), &client(&["read"])).expect("allowed");
        assert_eq!(scopes, vec!["read".to_owned()]);
    }
}
