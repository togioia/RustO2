use std::sync::Arc;

use anyhow::{bail, Context};
use http::Method;

use crate::config::{GatewayConfig, RouteConfig};

/// A route with its policy compiled into runtime-friendly types.
#[derive(Debug)]
pub struct CompiledRoute {
    pub cfg: RouteConfig,
    pub methods: Vec<Method>,
    pub allow_header: String,
}

impl CompiledRoute {
    pub fn allows(&self, method: &Method) -> bool {
        self.methods.iter().any(|candidate| {
            candidate == method || (method == Method::HEAD && candidate == Method::GET)
        })
    }
}

#[derive(Clone)]
pub struct MatchedRoute(pub Arc<CompiledRoute>);

pub fn compile(config: &GatewayConfig) -> anyhow::Result<Vec<Arc<CompiledRoute>>> {
    let mut routes = Vec::with_capacity(config.routes.len());

    for route in &config.routes {
        let mut methods = Vec::with_capacity(route.methods.len());
        for method in &route.methods {
            let method = Method::from_bytes(method.to_uppercase().as_bytes())
                .with_context(|| format!("invalid HTTP method {method}"))?;
            methods.push(method);
        }
        let allow_header = methods
            .iter()
            .map(Method::as_str)
            .collect::<Vec<_>>()
            .join(", ");

        if methods.is_empty() {
            bail!("route {} has no methods", route.name);
        }

        routes.push(Arc::new(CompiledRoute {
            cfg: route.clone(),
            methods,
            allow_header,
        }));
    }

    // Longest prefix wins, so /api/v2 is selected before /api.
    routes.sort_by_key(|route| std::cmp::Reverse(route.cfg.prefix.len()));
    Ok(routes)
}

pub fn find<'a>(routes: &'a [Arc<CompiledRoute>], path: &str) -> Option<&'a Arc<CompiledRoute>> {
    routes.iter().find(|route| {
        let prefix = route.cfg.prefix.as_str();
        if prefix == "/" {
            return true;
        }
        if path == prefix {
            return true;
        }
        path.strip_prefix(prefix)
            .map(|rest| rest.starts_with('/'))
            .unwrap_or(false)
    })
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
            allow_header: "GET".to_owned(),
        })
    }

    #[test]
    fn matches_segment_boundaries_only() {
        let routes = vec![route("api", "/api")];
        assert!(find(&routes, "/api").is_some());
        assert!(find(&routes, "/api/get").is_some());
        assert!(find(&routes, "/apix").is_none());
        assert!(find(&routes, "/other").is_none());
    }

    #[test]
    fn longest_prefix_wins() {
        let routes = vec![route("api", "/api"), route("v2", "/api/v2")];
        // compile() sorts, so emulate the sort here.
        let mut routes = routes;
        routes.sort_by_key(|route| std::cmp::Reverse(route.cfg.prefix.len()));
        let matched = find(&routes, "/api/v2/users").expect("match");
        assert_eq!(matched.cfg.name, "v2");
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
            allow_header: "GET, POST".to_owned(),
        })
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
            let actual = find(&routes, &path).is_some();
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

            match find(&routes, &path) {
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
                find(&routes, &path).is_some(),
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
            let routes = compile(&config_with(prefixes.clone())).expect("compile");

            prop_assert_eq!(routes.len(), prefixes.len());
            for pair in routes.windows(2) {
                prop_assert!(pair[0].cfg.prefix.len() >= pair[1].cfg.prefix.len());
            }
            for route in &routes {
                prop_assert!(route.allows(&Method::GET));
                prop_assert!(route.allows(&Method::POST));
                prop_assert_eq!(route.allow_header.as_str(), "GET, POST");
            }
        }

        /// The full compile + find path still obeys longest-prefix matching.
        #[test]
        fn compiled_routes_select_the_longest_prefix(
            prefixes in prop::collection::vec(any::<String>(), 1..10),
            path in any::<String>(),
        ) {
            let routes = compile(&config_with(prefixes)).expect("compile");

            match find(&routes, &path) {
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
