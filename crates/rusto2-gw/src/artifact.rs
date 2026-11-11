use std::sync::Arc;

use anyhow::Context;
use rusto2_synapse::{apim::SecurityScheme, ArtifactBundle, MediationPlan};

use crate::config::{ArtifactsConfig, AuthConfig, AuthMode, RateLimitConfig};

/// One WSO2 resource compiled into a gateway route. The plan owns the
/// in/out/fault sequences; the route owns auth, quota and path metadata.
#[derive(Debug, Clone)]
pub struct ArtifactRoute {
    pub name: String,
    pub api_name: String,
    pub api_path: String,
    pub full_path: String,
    pub method: String,
    pub upstream: String,
    pub auth: AuthConfig,
    pub rate_limit: Option<RateLimitConfig>,
    pub plan: Arc<MediationPlan>,
}

#[derive(Debug, Default)]
pub struct ArtifactRoutes {
    pub routes: Vec<ArtifactRoute>,
    pub api_count: usize,
    pub ignored: Vec<String>,
    pub sources: Vec<String>,
}

/// Load WSO2 artifacts and convert every resource/operation into a route.
///
/// Unsupported mediators fail here, at startup, with the mediator name and XML
/// location. A CAR that contains proxy services, tasks or message stores loads
/// the supported APIs and reports the rest in `ignored`.
pub fn load(config: &ArtifactsConfig) -> anyhow::Result<ArtifactRoutes> {
    if config.paths.is_empty() {
        return Ok(ArtifactRoutes::default());
    }

    let bundle = ArtifactBundle::load_paths(&config.paths)
        .context("cannot load WSO2 artifacts from artifacts.paths")?;
    let mut routes = Vec::new();
    let mut api_names = std::collections::BTreeSet::new();

    for api in &bundle.apis {
        api_names.insert(api.name.clone());
        let endpoint = api
            .endpoint
            .clone()
            .or_else(|| config.default_upstream.clone());
        if endpoint.is_none() {
            tracing::warn!(
                api = %api.name,
                "Synapse API has no endpoint; <call>/<send> must declare an address"
            );
        }
        let upstream = endpoint
            .clone()
            .unwrap_or_else(|| "http://unused.invalid".to_owned());

        for resource in &api.resources {
            let plan =
                MediationPlan::from_api(api, resource, &bundle, config.default_upstream.as_deref())
                    .with_context(|| {
                        format!(
                            "cannot compile mediation for {} {}",
                            api.name, resource.uri_template
                        )
                    })?;
            let full_path = join_path(&api.full_path(), &resource.uri_template);
            for method in &resource.methods {
                let route_name = format!(
                    "wso2_{}_{}_{}",
                    sanitize(&api.name),
                    method.to_ascii_lowercase(),
                    sanitize(&resource.uri_template)
                );
                routes.push(ArtifactRoute {
                    name: route_name,
                    api_name: api.name.clone(),
                    api_path: api.full_path(),
                    full_path: full_path.clone(),
                    method: method.clone(),
                    upstream: upstream.clone(),
                    auth: AuthConfig {
                        mode: config.auth,
                        scopes: config.scopes.clone(),
                        revocation_check: config.revocation_check,
                    },
                    rate_limit: None,
                    plan: Arc::new(plan.clone()),
                });
            }
        }
    }

    for api in &bundle.definitions {
        if !api.is_servable() {
            tracing::warn!(
                api = %api.name,
                status = ?api.lifecycle_status,
                "skipping WSO2 API that is not PUBLISHED or DEPRECATED"
            );
            continue;
        }
        api_names.insert(api.name.clone());
        if api
            .throttling_policy
            .as_deref()
            .is_some_and(|policy| !policy.eq_ignore_ascii_case("Unlimited"))
        {
            tracing::warn!(
                api = %api.name,
                policy = ?api.throttling_policy,
                "APIM tier policy is parsed but not enforced yet (M3); route has no quota"
            );
        }

        for operation in &api.operations {
            let plan = MediationPlan::from_definition(api, operation).with_context(|| {
                format!(
                    "cannot compile APIM operation {} {} {}",
                    api.name, operation.verb, operation.target
                )
            })?;
            let full_path = join_path(&api.full_path(), &operation.target);
            let route_name = format!(
                "apim_{}_{}_{}",
                sanitize(&api.name),
                operation.verb.to_ascii_lowercase(),
                sanitize(&operation.target)
            );
            routes.push(ArtifactRoute {
                name: route_name,
                api_name: api.name.clone(),
                api_path: api.full_path(),
                full_path,
                method: operation.verb.clone(),
                upstream: api.endpoint.clone(),
                auth: auth_for_operation(api, operation, config),
                rate_limit: None,
                plan: Arc::new(plan),
            });
        }
    }

    if routes.is_empty() {
        anyhow::bail!(
            "artifacts.paths produced no supported routes; check that the paths contain APIM API definitions or Synapse APIs"
        );
    }

    let mut seen = std::collections::HashSet::new();
    for route in &routes {
        if !seen.insert((route.full_path.clone(), route.method.to_ascii_uppercase())) {
            anyhow::bail!(
                "WSO2 artifacts declare {} {} more than once (API {})",
                route.method,
                route.full_path,
                route.api_name
            );
        }
    }

    for ignored in &bundle.ignored {
        tracing::warn!(artifact = %ignored, "WSO2 artifact type is not supported yet");
    }

    Ok(ArtifactRoutes {
        routes,
        api_count: api_names.len(),
        ignored: bundle.ignored,
        sources: bundle.sources,
    })
}

fn auth_for_operation(
    api: &rusto2_synapse::apim::ApiDefinition,
    operation: &rusto2_synapse::apim::OperationDefinition,
    config: &ArtifactsConfig,
) -> AuthConfig {
    match operation.security.unwrap_or(api.security) {
        SecurityScheme::OAuth2 => AuthConfig {
            mode: AuthMode::Jwt,
            scopes: operation.scopes.clone(),
            revocation_check: config.revocation_check,
        },
        SecurityScheme::None => AuthConfig {
            mode: AuthMode::None,
            scopes: Vec::new(),
            revocation_check: false,
        },
    }
}

fn join_path(base: &str, resource: &str) -> String {
    let base = base.trim_end_matches('/');
    let resource = resource.trim();
    if resource.is_empty() || resource == "/" {
        return if base.is_empty() {
            "/".to_owned()
        } else {
            base.to_owned()
        };
    }
    let resource = resource.strip_prefix('/').unwrap_or(resource);
    if base.is_empty() {
        format!("/{resource}")
    } else {
        format!("{base}/{resource}")
    }
}

fn sanitize(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
        } else if !output.ends_with('_') {
            output.push('_');
        }
    }
    let trimmed = output.trim_matches('_');
    if trimmed.is_empty() {
        "api".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_api_context_and_resource() {
        assert_eq!(
            join_path("/pizzashack/1.0.0", "/menu"),
            "/pizzashack/1.0.0/menu"
        );
        assert_eq!(join_path("/pizzashack/1.0.0", "/"), "/pizzashack/1.0.0");
        assert_eq!(join_path("/", "/menu"), "/menu");
    }

    #[test]
    fn sanitizes_resource_names() {
        assert_eq!(sanitize("Pizza Shack/1.0"), "pizza_shack_1_0");
        assert_eq!(sanitize("///"), "api");
    }

    #[test]
    fn loads_the_demo_artifacts() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/wso2");
        let config = ArtifactsConfig {
            paths: vec![
                root.join("apim").display().to_string(),
                root.join("synapse").display().to_string(),
            ],
            ..Default::default()
        };
        let loaded = load(&config).expect("artifacts");

        assert_eq!(loaded.api_count, 2);
        assert_eq!(loaded.routes.len(), 5);
        assert!(loaded
            .routes
            .iter()
            .any(|route| route.full_path == "/pizzashack/1.0.0/post" && route.method == "POST"));
        let demo = loaded
            .routes
            .iter()
            .find(|route| route.full_path == "/demo/1.0.0/items/{id}")
            .expect("mediation demo route");
        assert!(demo.plan.named_sequences.contains_key("CommonFault"));
        assert!(demo.plan.endpoints.contains_key("HttpbinEndpoint"));
    }
}
