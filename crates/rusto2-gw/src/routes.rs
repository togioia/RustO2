use std::sync::Arc;

use anyhow::{bail, Context};
use http::Method;
use rusto2_synapse::{template::PathPattern, MediationPlan};

use crate::{
    artifact::ArtifactRoute,
    config::{GatewayConfig, RouteConfig},
};

/// A route with its policy compiled into runtime-friendly types.
#[derive(Debug)]
pub struct CompiledRoute {
    pub cfg: RouteConfig,
    pub methods: Vec<Method>,
    /// Synapse URI-template for routes imported from WSO2 artifacts. Plain YAML
    /// routes use prefix matching instead.
    pub pattern: Option<PathPattern>,
    /// Present on artifact routes: the resource must be executed by the
    /// Synapse mediation engine.
    pub mediation: Option<Arc<MediationPlan>>,
}

impl CompiledRoute {
    pub fn allows(&self, method: &Method) -> bool {
        self.methods.iter().any(|candidate| {
            candidate == method || (method == Method::HEAD && candidate == Method::GET)
        })
    }
}

#[derive(Clone)]
pub struct MatchedRoute {
    pub route: Arc<CompiledRoute>,
    pub captures: Vec<(String, String)>,
}

/// Result of matching a request against the route table.
pub enum RouteMatch<'a> {
    Found {
        route: &'a Arc<CompiledRoute>,
        captures: Vec<(String, String)>,
    },
    MethodNotAllowed {
        allow: String,
    },
    NotFound,
}

type BestMatch<'a> = (usize, &'a Arc<CompiledRoute>, Vec<(String, String)>);

/// Compile YAML routes and WSO2 artifact routes into one matcher. Artifact
/// templates carry a higher specificity than prefix routes, so a Synapse
/// resource such as `/pizzashack/1.0.0/menu/{id}` wins over a generic `/pizzashack`
/// prefix.
pub fn compile_all(
    config: &GatewayConfig,
    artifacts: &[ArtifactRoute],
) -> anyhow::Result<Vec<Arc<CompiledRoute>>> {
    let mut routes = Vec::with_capacity(config.routes.len() + artifacts.len());

    for route in &config.routes {
        let mut methods = Vec::with_capacity(route.methods.len());
        for method in &route.methods {
            let method = Method::from_bytes(method.to_uppercase().as_bytes())
                .with_context(|| format!("invalid HTTP method {method}"))?;
            methods.push(method);
        }
        if methods.is_empty() {
            bail!("route {} has no methods", route.name);
        }

        routes.push(Arc::new(CompiledRoute {
            cfg: route.clone(),
            methods,
            pattern: None,
            mediation: None,
        }));
    }

    for (index, route) in artifacts.iter().enumerate() {
        let method = Method::from_bytes(route.method.to_uppercase().as_bytes())
            .with_context(|| format!("artifact route {} has an invalid method", route.name))?;
        let pattern = PathPattern::parse(&route.full_path).with_context(|| {
            format!(
                "artifact route {} has an invalid URI template {}",
                route.name, route.full_path
            )
        })?;

        if routes.iter().any(|existing| {
            existing.cfg.name == route.name
                || existing
                    .pattern
                    .as_ref()
                    .is_some_and(|other| other.source() == pattern.source())
                    && existing.methods.contains(&method)
        }) {
            bail!(
                "artifact route {} ({}) is declared more than once",
                route.name,
                route.full_path
            );
        }

        let cfg = RouteConfig {
            name: format!("{}_{}", route.name, index),
            prefix: route.api_path.clone(),
            upstream: route.upstream.clone(),
            strip_prefix: true,
            methods: vec![route.method.clone()],
            auth: route.auth.clone(),
            rate_limit: route.rate_limit.clone(),
            circuit_breaker: None,
        };

        routes.push(Arc::new(CompiledRoute {
            cfg,
            methods: vec![method],
            pattern: Some(pattern),
            mediation: Some(route.plan.clone()),
        }));
    }

    // Specificity: artifact templates outrank prefix routes; among prefix
    // routes the longest one wins.
    routes.sort_by_key(|route| {
        std::cmp::Reverse(match &route.pattern {
            Some(pattern) => pattern.specificity(),
            None => route.cfg.prefix.len(),
        })
    });
    Ok(routes)
}

/// Match a path and method. Artifact templates match exactly (with path
/// parameters), prefix routes keep the original segment-boundary semantics.
pub fn find<'a>(routes: &'a [Arc<CompiledRoute>], path: &str, method: &Method) -> RouteMatch<'a> {
    let mut best: Option<BestMatch<'a>> = None;
    let mut allow = std::collections::BTreeSet::new();

    for route in routes {
        let (matched, captures, score) = match &route.pattern {
            Some(pattern) => match pattern.matches(path) {
                Some(captures) => (true, captures, pattern.specificity()),
                None => (false, Vec::new(), 0),
            },
            None => (
                prefix_matches(&route.cfg.prefix, path),
                Vec::new(),
                route.cfg.prefix.len(),
            ),
        };
        if !matched {
            continue;
        }

        if route.allows(method) {
            if best
                .as_ref()
                .is_none_or(|(best_score, ..)| score > *best_score)
            {
                best = Some((score, route, captures));
            }
        } else {
            for allowed in &route.methods {
                allow.insert(allowed.as_str().to_owned());
            }
            if route.methods.contains(&Method::GET) {
                allow.insert(Method::HEAD.as_str().to_owned());
            }
        }
    }

    match best {
        Some((_, route, captures)) => RouteMatch::Found { route, captures },
        None if !allow.is_empty() => RouteMatch::MethodNotAllowed {
            allow: allow.into_iter().collect::<Vec<_>>().join(", "),
        },
        None => RouteMatch::NotFound,
    }
}

fn prefix_matches(prefix: &str, path: &str) -> bool {
    prefix == "/"
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(name: &str, prefix: &str) -> Arc<CompiledRoute> {
        Arc::new(CompiledRoute {
            cfg: RouteConfig {
                name: name.to_owned(),
                prefix: prefix.to_owned(),
                upstream: "http://backend".to_owned(),
                strip_prefix: true,
                methods: vec!["GET".to_owned()],
                auth: Default::default(),
                rate_limit: None,
                circuit_breaker: None,
            },
            methods: vec![Method::GET],
            pattern: None,
            mediation: None,
        })
    }

    fn matched<'a>(routes: &'a [Arc<CompiledRoute>], path: &str) -> Option<&'a Arc<CompiledRoute>> {
        match find(routes, path, &Method::GET) {
            RouteMatch::Found { route, .. } => Some(route),
            _ => None,
        }
    }

    #[test]
    fn matches_segment_boundaries_only() {
        let routes = vec![route("api", "/api")];
        assert!(matched(&routes, "/api").is_some());
        assert!(matched(&routes, "/api/get").is_some());
        assert!(matched(&routes, "/apix").is_none());
        assert!(matched(&routes, "/other").is_none());
    }

    #[test]
    fn longest_prefix_wins() {
        let routes = vec![route("api", "/api"), route("v2", "/api/v2")];
        // compile() sorts, so emulate the sort here.
        let mut routes = routes;
        routes.sort_by_key(|route| std::cmp::Reverse(route.cfg.prefix.len()));
        let matched = matched(&routes, "/api/v2/users").expect("match");
        assert_eq!(matched.cfg.name, "v2");
    }

    #[test]
    fn method_mismatch_reports_allow() {
        let routes = vec![route("api", "/api")];
        match find(&routes, "/api/get", &Method::POST) {
            RouteMatch::MethodNotAllowed { allow } => assert_eq!(allow, "GET, HEAD"),
            _ => panic!("expected method not allowed"),
        }
    }

    #[test]
    fn uri_template_routes_outrank_prefix_routes() {
        use std::collections::HashMap;

        let mut config = crate::config::GatewayConfig {
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
            routes: Vec::new(),
        };
        config.routes.push(RouteConfig {
            name: "prefix".to_owned(),
            prefix: "/pizzashack".to_owned(),
            upstream: "http://backend".to_owned(),
            strip_prefix: true,
            methods: vec!["GET".to_owned()],
            auth: Default::default(),
            rate_limit: None,
            circuit_breaker: None,
        });

        let artifact = ArtifactRoute {
            name: "menu".to_owned(),
            api_name: "PizzaShackAPI".to_owned(),
            api_path: "/pizzashack/1.0.0".to_owned(),
            full_path: "/pizzashack/1.0.0/menu/{id}".to_owned(),
            method: "GET".to_owned(),
            upstream: "http://backend".to_owned(),
            auth: Default::default(),
            rate_limit: None,
            plan: Arc::new(MediationPlan {
                name: "PizzaShackAPI /menu/{id}".to_owned(),
                api_path: "/pizzashack/1.0.0".to_owned(),
                default_upstream: Some("http://backend".to_owned()),
                in_sequence: rusto2_synapse::Sequence::default(),
                out_sequence: rusto2_synapse::Sequence::default(),
                fault_sequence: rusto2_synapse::Sequence::default(),
                named_sequences: HashMap::new(),
                endpoints: HashMap::new(),
            }),
        };
        let routes = compile_all(&config, &[artifact]).expect("compile");

        match find(&routes, "/pizzashack/1.0.0/menu/7", &Method::GET) {
            RouteMatch::Found { route, captures } => {
                assert!(route.pattern.is_some());
                assert_eq!(captures, vec![("id".to_owned(), "7".to_owned())]);
            }
            _ => panic!("expected the URI-template route"),
        }
        match find(&routes, "/pizzashack/1.0.0/menu/7", &Method::POST) {
            RouteMatch::MethodNotAllowed { allow } => assert_eq!(allow, "GET, HEAD"),
            _ => panic!("expected method not allowed"),
        }
    }

    #[test]
    fn head_is_allowed_when_get_is_allowed() {
        let route = route("api", "/api");
        assert!(route.allows(&Method::HEAD));
        assert!(!route.allows(&Method::POST));
    }
}

#[cfg(test)]
mod property_tests {
    use proptest::prelude::*;

    use super::*;
    use crate::config::{AuthConfig, IdentityConfig};

    fn route_config(prefix: &str) -> RouteConfig {
        RouteConfig {
            name: "route".to_owned(),
            prefix: prefix.to_owned(),
            upstream: "http://backend".to_owned(),
            strip_prefix: true,
            methods: vec!["get".to_owned(), "POST".to_owned()],
            auth: AuthConfig::default(),
            rate_limit: None,
            circuit_breaker: None,
        }
    }

    fn compiled(prefix: &str) -> Arc<CompiledRoute> {
        Arc::new(CompiledRoute {
            cfg: route_config(prefix),
            methods: vec![Method::GET, Method::POST],
            pattern: None,
            mediation: None,
        })
    }

    fn matched<'a>(routes: &'a [Arc<CompiledRoute>], path: &str) -> Option<&'a Arc<CompiledRoute>> {
        match find(routes, path, &Method::GET) {
            RouteMatch::Found { route, .. } => Some(route),
            _ => None,
        }
    }

    /// Reproduce the ordering that `compile` applies: longest prefix first.
    fn sorted(prefixes: Vec<String>) -> Vec<Arc<CompiledRoute>> {
        let mut routes: Vec<_> = prefixes
            .into_iter()
            .map(|prefix| compiled(prefix.as_str()))
            .collect();
        routes.sort_by_key(|route| std::cmp::Reverse(route.cfg.prefix.len()));
        routes
    }

    /// Specification of the matcher: a path matches a prefix when it is the
    /// prefix itself or continues on a segment boundary. `/` is a catch-all.
    fn reference_matches(prefix: &str, path: &str) -> bool {
        prefix == "/"
            || path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    }

    fn config_with(prefixes: Vec<String>) -> GatewayConfig {
        GatewayConfig {
            server: Default::default(),
            identity: IdentityConfig {
                issuer: "https://issuer.test".to_owned(),
                audience: "rusto2".to_owned(),
                jwks_url: "https://issuer.test/.well-known/jwks.json".to_owned(),
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
            routes: prefixes
                .into_iter()
                .enumerate()
                .map(|(index, prefix)| RouteConfig {
                    name: format!("route{index}"),
                    ..route_config(&prefix)
                })
                .collect(),
        }
    }

    proptest! {
        /// A single route matches exactly the paths equal to its prefix or
        /// continuing with `/`; anything else is rejected.
        #[test]
        fn matches_only_prefix_or_slash_continuation(
            prefix in any::<String>(),
            path in any::<String>(),
        ) {
            let routes = vec![compiled(&prefix)];
            let actual = matched(&routes, &path).is_some();
            let expected = reference_matches(&prefix, &path);
            prop_assert_eq!(
                actual,
                expected,
                "find disagreed for prefix {:?} and path {:?}",
                prefix,
                path,
            );
        }

        /// When several prefixes match, the longest one must be selected.
        #[test]
        fn longest_matching_prefix_wins(
            prefixes in prop::collection::vec(any::<String>(), 1..10),
            path in any::<String>(),
        ) {
            let routes = sorted(prefixes);

            match matched(&routes, &path) {
                Some(matched) => {
                    let matched_len = matched.cfg.prefix.len();
                    prop_assert!(
                        reference_matches(&matched.cfg.prefix, &path),
                        "selected route {} does not match path {:?}",
                        matched.cfg.prefix,
                        path,
                    );
                    for route in &routes {
                        if route.cfg.prefix.len() > matched_len {
                            prop_assert!(
                                !reference_matches(&route.cfg.prefix, &path),
                                "route {} is longer than {} but also matches {:?}",
                                route.cfg.prefix,
                                matched.cfg.prefix,
                                path,
                            );
                        }
                    }
                }
                None => {
                    for route in &routes {
                        prop_assert!(
                            !reference_matches(&route.cfg.prefix, &path),
                            "route {} matches {:?} but find returned None",
                            route.cfg.prefix,
                            path,
                        );
                    }
                }
            }
        }

        /// The root prefix is a catch-all regardless of the other routes.
        #[test]
        fn root_prefix_matches_every_path(
            others in prop::collection::vec(any::<String>(), 0..5),
            path in any::<String>(),
        ) {
            let mut prefixes = others;
            prefixes.push("/".to_owned());
            let routes = sorted(prefixes);
            prop_assert!(
                matched(&routes, &path).is_some(),
                "root route did not match path {:?}",
                path,
            );
        }

        /// `compile` keeps every route, sorts longest-first, and normalises
        /// methods into `Method` values and an `Allow` header.
        #[test]
        fn compile_sorts_routes_and_normalises_methods(
            prefixes in prop::collection::vec(any::<String>(), 1..12),
        ) {
            let routes = compile_all(&config_with(prefixes.clone()), &[]).expect("compile");

            prop_assert_eq!(routes.len(), prefixes.len());
            for pair in routes.windows(2) {
                prop_assert!(pair[0].cfg.prefix.len() >= pair[1].cfg.prefix.len());
            }
            for route in &routes {
                prop_assert!(route.allows(&Method::GET));
                prop_assert!(route.allows(&Method::POST));
                prop_assert_eq!(route.methods.len(), 2);
            }
        }

        /// The full compile + find path still obeys longest-prefix matching.
        #[test]
        fn compiled_routes_select_the_longest_prefix(
            prefixes in prop::collection::vec(any::<String>(), 1..10),
            path in any::<String>(),
        ) {
            let routes = compile_all(&config_with(prefixes), &[]).expect("compile");

            match matched(&routes, &path) {
                Some(matched) => {
                    let matched_len = matched.cfg.prefix.len();
                    prop_assert!(reference_matches(&matched.cfg.prefix, &path));
                    for route in &routes {
                        if route.cfg.prefix.len() > matched_len {
                            prop_assert!(!reference_matches(&route.cfg.prefix, &path));
                        }
                    }
                }
                None => {
                    for route in &routes {
                        prop_assert!(!reference_matches(&route.cfg.prefix, &path));
                    }
                }
            }
        }
    }
}
