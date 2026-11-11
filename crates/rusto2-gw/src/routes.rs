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
