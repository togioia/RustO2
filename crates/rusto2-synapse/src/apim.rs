use std::collections::BTreeMap;

use serde::Deserialize;

use crate::error::SynapseError;

/// Security required by an API or one of its operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityScheme {
    None,
    OAuth2,
}

/// A normalized APIM API definition. This is the shape both the APIM `api.yaml`
/// format and an OpenAPI document with `x-wso2-*` extensions are converted to.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiDefinition {
    pub name: String,
    pub context: String,
    pub version: String,
    pub provider: Option<String>,
    pub lifecycle_status: Option<String>,
    pub security: SecurityScheme,
    pub throttling_policy: Option<String>,
    /// API-level backend endpoint. M1 supports a single HTTP(S) address; M3
    /// adds failover and load-balancing policy.
    pub endpoint: String,
    pub operations: Vec<OperationDefinition>,
}

/// One operation from an APIM definition.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationDefinition {
    pub target: String,
    pub verb: String,
    pub scopes: Vec<String>,
    /// `Some` overrides the API-level security for this operation.
    pub security: Option<SecurityScheme>,
    pub throttling_policy: Option<String>,
}

impl ApiDefinition {
    pub fn full_path(&self) -> String {
        let context = self.context.trim_end_matches('/');
        let version = self.version.trim();
        if version.is_empty() || context.ends_with(&format!("/{version}")) {
            context.to_owned()
        } else {
            format!("{context}/{version}")
        }
    }

    /// APIM serves PUBLISHED and DEPRECATED APIs. RETIRED and BLOCKED APIs
    /// return 404 at the gateway; the loader refuses them so a CAR cannot
    /// silently expose an API that should be offline.
    pub fn is_servable(&self) -> bool {
        matches!(
            self.lifecycle_status
                .as_deref()
                .map(str::trim)
                .map(str::to_ascii_uppercase)
                .as_deref(),
            None | Some("PUBLISHED") | Some("DEPRECATED")
        )
    }
}

pub fn parse_yaml(yaml: &str) -> Result<ApiDefinition, SynapseError> {
    let value: serde_yaml::Value = serde_yaml::from_str(yaml)
        .map_err(|error| SynapseError::ApiDefinition(error.to_string()))?;

    if value.get("openapi").is_some() || value.get("swagger").is_some() {
        return parse_openapi(value);
    }

    let data = if value.get("data").is_some() {
        value
            .get("data")
            .cloned()
            .ok_or_else(|| SynapseError::ApiDefinition("data is null".to_owned()))?
    } else {
        value
    };
    parse_apim_data(data)
}

fn parse_apim_data(value: serde_yaml::Value) -> Result<ApiDefinition, SynapseError> {
    let data: ApimData = serde_yaml::from_value(value)
        .map_err(|error| SynapseError::ApiDefinition(error.to_string()))?;

    if data.name.trim().is_empty() {
        return Err(SynapseError::ApiDefinition(
            "API name must not be empty".to_owned(),
        ));
    }
    if !data.context.starts_with('/') {
        return Err(SynapseError::ApiDefinition(format!(
            "API context must start with '/': {}",
            data.context
        )));
    }
    if let Some(header) = data.authorization_header.as_deref() {
        if !header.eq_ignore_ascii_case("Authorization") {
            return Err(SynapseError::UnsupportedSecurity(format!(
                "custom authorizationHeader '{header}' (only the standard Authorization header is supported)"
            )));
        }
    }

    let security = parse_security_schemes(data.security_scheme.as_deref())?;
    let endpoint = data
        .endpoint_config
        .as_ref()
        .ok_or_else(|| {
            SynapseError::ApiDefinition(format!(
                "API {} has no endpointConfig; RustO2 does not deploy registry-only endpoints yet",
                data.name
            ))
        })
        .and_then(endpoint_url)?;

    let operations = data
        .operations
        .into_iter()
        .map(|operation| {
            let verb = operation.verb.trim().to_ascii_uppercase();
            if verb.is_empty() {
                return Err(SynapseError::ApiDefinition(
                    "operation verb must not be empty".to_owned(),
                ));
            }
            let target = normalize_target(&operation.target)?;
            let operation_security = operation
                .auth_type
                .as_deref()
                .map(parse_auth_type)
                .transpose()?;
            Ok(OperationDefinition {
                target,
                verb,
                scopes: operation.scopes.unwrap_or_default(),
                security: operation_security,
                throttling_policy: operation.throttling_policy,
            })
        })
        .collect::<Result<Vec<_>, SynapseError>>()?;

    if operations.is_empty() {
        return Err(SynapseError::ApiDefinition(format!(
            "API {} has no operations",
            data.name
        )));
    }

    Ok(ApiDefinition {
        name: data.name,
        context: data.context,
        version: data.version.unwrap_or_default(),
        provider: data.provider,
        lifecycle_status: data.lifecycle_status,
        security,
        throttling_policy: data.throttling_policy,
        endpoint,
        operations,
    })
}

fn parse_security_schemes(schemes: Option<&[String]>) -> Result<SecurityScheme, SynapseError> {
    let Some(schemes) = schemes else {
        return Ok(SecurityScheme::OAuth2);
    };
    if schemes.is_empty() {
        return Ok(SecurityScheme::None);
    }

    let mut oauth = false;
    let mut unsupported = Vec::new();
    for scheme in schemes {
        match scheme.trim().to_ascii_lowercase().as_str() {
            "oauth2" | "oauth-2" | "oauth" => oauth = true,
            "" => {}
            other => unsupported.push(other.to_owned()),
        }
    }
    if oauth {
        return Ok(SecurityScheme::OAuth2);
    }
    Err(SynapseError::UnsupportedSecurity(format!(
        "{} (only OAuth2 is enforced today; API keys, basic auth and mTLS are M3)",
        unsupported.join(", ")
    )))
}

fn parse_auth_type(value: &str) -> Result<SecurityScheme, SynapseError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Ok(SecurityScheme::None),
        "application" | "application user" | "application & application user" | "any" => {
            Ok(SecurityScheme::OAuth2)
        }
        other => Err(SynapseError::UnsupportedSecurity(format!(
            "operation authType '{other}'"
        ))),
    }
}

fn normalize_target(target: &str) -> Result<String, SynapseError> {
    let target = target.trim();
    if target.is_empty() || target == "/" {
        return Ok("/".to_owned());
    }
    if target.starts_with('/') {
        Ok(target.to_owned())
    } else {
        Ok(format!("/{target}"))
    }
}

#[derive(Debug, Deserialize)]
struct ApimData {
    name: String,
    context: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(rename = "lifeCycleStatus", default)]
    lifecycle_status: Option<String>,
    #[serde(rename = "securityScheme", default)]
    security_scheme: Option<Vec<String>>,
    #[serde(rename = "authorizationHeader", default)]
    authorization_header: Option<String>,
    #[serde(rename = "throttlingPolicy", default)]
    throttling_policy: Option<String>,
    #[serde(rename = "endpointConfig", default)]
    endpoint_config: Option<serde_yaml::Value>,
    #[serde(default)]
    operations: Vec<ApimOperation>,
}

#[derive(Debug, Deserialize)]
struct ApimOperation {
    target: String,
    verb: String,
    #[serde(rename = "authType", default)]
    auth_type: Option<String>,
    #[serde(rename = "throttlingPolicy", default)]
    throttling_policy: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

fn endpoint_url(value: &serde_yaml::Value) -> Result<String, SynapseError> {
    // APIM 3.x exports endpointConfig as a JSON string; APIM 4.x uses a map.
    if let Some(text) = value.as_str() {
        let parsed: serde_yaml::Value = serde_yaml::from_str(text).map_err(|error| {
            SynapseError::ApiDefinition(format!("invalid endpointConfig string: {error}"))
        })?;
        return endpoint_url(&parsed);
    }

    let mapping = value.as_mapping().ok_or_else(|| {
        SynapseError::ApiDefinition("endpointConfig must be a map or a JSON string".to_owned())
    })?;

    if mapping_get(mapping, "endpoint_security").is_some()
        || mapping_get(mapping, "endpointSecurity").is_some()
    {
        return Err(SynapseError::UnsupportedEndpoint(
            "endpoint_security (backend OAuth2/basic auth) is M3".to_owned(),
        ));
    }

    let endpoint_type = mapping_get(mapping, "endpoint_type")
        .or_else(|| mapping_get(mapping, "endpointType"))
        .and_then(|value| value.as_str())
        .unwrap_or("http")
        .trim()
        .to_ascii_lowercase();

    match endpoint_type.as_str() {
        "http" | "address" => {}
        "failover" | "load_balance" | "loadbalance" => {
            return Err(SynapseError::UnsupportedEndpoint(format!(
                "endpoint_type '{endpoint_type}' requires M3 failover/load-balancing policy"
            )));
        }
        other => {
            return Err(SynapseError::UnsupportedEndpoint(format!(
                "endpoint_type '{other}'"
            )));
        }
    }

    for key in [
        "production_endpoints",
        "sandbox_endpoints",
        "production_failovers",
        "sandbox_failovers",
    ] {
        if let Some(candidate) = mapping_get(mapping, key) {
            if let Some(url) = endpoint_value_url(candidate) {
                validate_http_url(&url)?;
                return Ok(url);
            }
        }
    }
    if let Some(url) = mapping_get(mapping, "url").and_then(endpoint_value_url) {
        validate_http_url(&url)?;
        return Ok(url);
    }

    Err(SynapseError::UnsupportedEndpoint(
        "endpointConfig has no production or sandbox URL".to_owned(),
    ))
}

fn endpoint_value_url(value: &serde_yaml::Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    if let Some(mapping) = value.as_mapping() {
        return mapping_get(mapping, "url")
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned);
    }
    if let Some(sequence) = value.as_sequence() {
        return sequence.first().and_then(endpoint_value_url);
    }
    None
}

fn mapping_get<'a>(mapping: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    mapping.get(serde_yaml::Value::String(key.to_owned()))
}

fn validate_http_url(url: &str) -> Result<(), SynapseError> {
    let parsed = url::Url::parse(url).map_err(|error| {
        SynapseError::ApiDefinition(format!("invalid endpoint URL '{url}': {error}"))
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
        return Err(SynapseError::ApiDefinition(format!(
            "endpoint URL must be absolute http(s): {url}"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// OpenAPI 3 / Swagger 2 with WSO2 extensions
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct OpenApiDoc {
    info: OpenApiInfo,
    #[serde(default)]
    servers: Option<Vec<OpenApiServer>>,
    #[serde(default)]
    paths: BTreeMap<String, BTreeMap<String, serde_yaml::Value>>,
    #[serde(rename = "x-wso2-basePath", default)]
    base_path: Option<String>,
    #[serde(rename = "x-wso2-production-endpoints", default)]
    production_endpoints: Option<serde_yaml::Value>,
    #[serde(default)]
    components: Option<serde_yaml::Value>,
    #[serde(default)]
    security: Option<Vec<BTreeMap<String, Vec<String>>>>,
}

#[derive(Debug, Deserialize)]
struct OpenApiInfo {
    title: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct OpenApiServer {
    url: String,
}

fn parse_openapi(value: serde_yaml::Value) -> Result<ApiDefinition, SynapseError> {
    let document: OpenApiDoc = serde_yaml::from_value(value).map_err(|error| {
        SynapseError::ApiDefinition(format!("invalid OpenAPI document: {error}"))
    })?;

    let base_path = document
        .base_path
        .clone()
        .or_else(|| {
            document
                .servers
                .as_ref()
                .and_then(|servers| servers.first())
                .and_then(|server| url::Url::parse(&server.url).ok())
                .map(|url| url.path().trim_end_matches('/').to_owned())
        })
        .filter(|path| path.starts_with('/'))
        .unwrap_or_else(|| "/".to_owned());

    let (context, version) = split_base_path(&base_path, &document.info.version);
    let security = openapi_security(&document)?;
    let endpoint = openapi_endpoint(&document)?;

    let mut operations = Vec::new();
    for (target, methods) in &document.paths {
        for (method, operation) in methods {
            let method = method.to_ascii_lowercase();
            if !matches!(
                method.as_str(),
                "get" | "post" | "put" | "patch" | "delete" | "head" | "options" | "trace"
            ) {
                continue;
            }
            let operation_security = operation_security(operation, security);
            let scopes = operation_scopes(operation, &document);
            operations.push(OperationDefinition {
                target: normalize_target(target)?,
                verb: method.to_ascii_uppercase(),
                scopes,
                security: Some(operation_security),
                throttling_policy: None,
            });
        }
    }

    if operations.is_empty() {
        return Err(SynapseError::ApiDefinition(
            "OpenAPI document has no operations".to_owned(),
        ));
    }

    Ok(ApiDefinition {
        name: document.info.title,
        context,
        version,
        provider: None,
        lifecycle_status: Some("PUBLISHED".to_owned()),
        security,
        throttling_policy: None,
        endpoint,
        operations,
    })
}

fn split_base_path(base_path: &str, version: &str) -> (String, String) {
    let base_path = base_path.trim_end_matches('/');
    if let Some(context) = base_path.strip_suffix(&format!("/{version}")) {
        if !context.is_empty() {
            return (context.to_owned(), version.to_owned());
        }
    }
    if base_path.is_empty() {
        ("/".to_owned(), version.to_owned())
    } else {
        (base_path.to_owned(), version.to_owned())
    }
}

fn openapi_security(document: &OpenApiDoc) -> Result<SecurityScheme, SynapseError> {
    let has_oauth = document
        .components
        .as_ref()
        .and_then(|components| components.get("securitySchemes"))
        .and_then(|schemes| schemes.as_mapping())
        .map(|schemes| {
            schemes.values().any(|scheme| {
                scheme
                    .get("type")
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case("oauth2"))
            })
        })
        .unwrap_or(false);

    if has_oauth {
        return Ok(SecurityScheme::OAuth2);
    }

    let uses_api_key = document
        .components
        .as_ref()
        .and_then(|components| components.get("securitySchemes"))
        .and_then(|schemes| schemes.as_mapping())
        .map(|schemes| {
            schemes.values().any(|scheme| {
                scheme
                    .get("type")
                    .and_then(|value| value.as_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case("apiKey"))
            })
        })
        .unwrap_or(false);
    if uses_api_key {
        return Err(SynapseError::UnsupportedSecurity(
            "OpenAPI apiKey scheme (M3 feature)".to_owned(),
        ));
    }

    Ok(SecurityScheme::OAuth2)
}

fn operation_security(
    operation: &serde_yaml::Value,
    api_security: SecurityScheme,
) -> SecurityScheme {
    if operation
        .get("x-wso2-disable-security")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return SecurityScheme::None;
    }
    match operation
        .get("security")
        .and_then(|value| value.as_sequence())
    {
        Some(requirements) if requirements.is_empty() => SecurityScheme::None,
        Some(_) => SecurityScheme::OAuth2,
        None => api_security,
    }
}

fn operation_scopes(operation: &serde_yaml::Value, document: &OpenApiDoc) -> Vec<String> {
    let requirements = operation
        .get("security")
        .and_then(|value| value.as_sequence())
        .cloned()
        .or_else(|| {
            document.security.as_ref().map(|security| {
                security
                    .iter()
                    .map(|requirement| {
                        let mut mapping = serde_yaml::Mapping::new();
                        for (name, scopes) in requirement {
                            mapping.insert(
                                serde_yaml::Value::String(name.clone()),
                                serde_yaml::Value::Sequence(
                                    scopes
                                        .iter()
                                        .map(|scope| serde_yaml::Value::String(scope.clone()))
                                        .collect(),
                                ),
                            );
                        }
                        serde_yaml::Value::Mapping(mapping)
                    })
                    .collect()
            })
        })
        .unwrap_or_default();

    let mut scopes = Vec::new();
    for requirement in requirements {
        let Some(mapping) = requirement.as_mapping() else {
            continue;
        };
        for value in mapping.values() {
            if let Some(sequence) = value.as_sequence() {
                for scope in sequence {
                    if let Some(scope) = scope.as_str() {
                        if !scopes.iter().any(|existing| existing == scope) {
                            scopes.push(scope.to_owned());
                        }
                    }
                }
            }
        }
    }
    scopes
}

fn openapi_endpoint(document: &OpenApiDoc) -> Result<String, SynapseError> {
    if let Some(endpoints) = &document.production_endpoints {
        if let Some(urls) = endpoints.get("urls").and_then(|value| value.as_sequence()) {
            if let Some(url) = urls.first().and_then(|value| value.as_str()) {
                validate_http_url(url)?;
                return Ok(url.to_owned());
            }
        }
        if let Some(url) = endpoints.get("url").and_then(|value| value.as_str()) {
            validate_http_url(url)?;
            return Ok(url.to_owned());
        }
    }
    if let Some(server) = document
        .servers
        .as_ref()
        .and_then(|servers| servers.first())
    {
        validate_http_url(&server.url)?;
        return Ok(server.url.trim_end_matches('/').to_owned());
    }
    Err(SynapseError::UnsupportedEndpoint(
        "OpenAPI document has no x-wso2-production-endpoints or servers URL".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_apim_api_yaml() {
        let api = parse_yaml(
            r#"
type: api
version: v4.2.0
data:
  name: PizzaShackAPI
  context: /pizzashack
  version: 1.0.0
  provider: admin
  lifeCycleStatus: PUBLISHED
  securityScheme: [oauth2]
  throttlingPolicy: Unlimited
  endpointConfig:
    endpoint_type: http
    production_endpoints:
      url: http://httpbin:8080
  operations:
    - target: /menu
      verb: GET
      authType: Application & Application User
      throttlingPolicy: Unlimited
      scopes: [read]
    - target: /order/{orderId}
      verb: POST
      authType: Application
      scopes: []
"#,
        )
        .expect("api");

        assert_eq!(api.name, "PizzaShackAPI");
        assert_eq!(api.full_path(), "/pizzashack/1.0.0");
        assert_eq!(api.security, SecurityScheme::OAuth2);
        assert_eq!(api.operations.len(), 2);
        assert_eq!(api.operations[0].scopes, vec!["read"]);
        assert!(api.is_servable());
    }

    #[test]
    fn parses_json_string_endpoint_config() {
        let api = parse_yaml(
            r#"
name: Legacy
context: /legacy
version: 1.0.0
securityScheme: []
endpointConfig: '{"endpoint_type":"http","production_endpoints":{"url":"http://legacy:8080"}}'
operations:
  - target: /items
    verb: GET
"#,
        )
        .expect("api");
        assert_eq!(api.endpoint, "http://legacy:8080");
        assert_eq!(api.security, SecurityScheme::None);
    }

    #[test]
    fn rejects_api_key_security_explicitly() {
        let error = parse_yaml(
            r#"
name: Keyed
context: /keyed
version: 1.0.0
securityScheme: [api_key]
endpointConfig:
  endpoint_type: http
  production_endpoints: {url: "http://backend:8080"}
operations:
  - target: /x
    verb: GET
"#,
        )
        .expect_err("api_key unsupported");
        assert!(matches!(error, SynapseError::UnsupportedSecurity(_)));
    }

    #[test]
    fn parses_openapi_with_wso2_extensions() {
        let api = parse_yaml(
            r#"
openapi: 3.0.1
info:
  title: PetStore
  version: 2.0.0
servers:
  - url: http://petstore:8080
x-wso2-basePath: /petstore/2.0.0
x-wso2-production-endpoints:
  urls: [http://petstore:8080]
components:
  securitySchemes:
    oauth2:
      type: oauth2
paths:
  /pets/{id}:
    get:
      security:
        - oauth2: [read]
      responses:
        "200": {description: ok}
"#,
        )
        .expect("openapi");
        assert_eq!(api.name, "PetStore");
        assert_eq!(api.full_path(), "/petstore/2.0.0");
        assert_eq!(api.operations[0].verb, "GET");
        assert_eq!(api.operations[0].scopes, vec!["read"]);
    }
}
