# Changelog

All notable changes to RustO2 are documented here.

## [0.2.0] - 2026-11-11

### Added

- Redis-backed distributed quota backend behind a `RateLimitBackend` trait,
  with an atomic Lua token bucket and configurable fail-open behavior.
- RFC 7662 token introspection, RFC 7009 revocation and a TTL-based `jti`
  blocklist, with opt-in gateway enforcement and a short introspection cache.
- Helm chart with probes, HPA, PDB, NetworkPolicy, ServiceMonitor and key
  persistence, plus a Kubernetes operations guide.
- Property-based tests for routing/env parsing/quota invariants and a cargo-fuzz
  harness for JWT header parsing.
- Supply-chain workflows: cargo-audit, cargo-deny, SBOM generation, keyless
  cosign signing and SLSA provenance, plus a release benchmark job.
- Prometheus alert rules for auth failures, quota backend errors, upstream
  errors, open circuits and introspection failures.

## [0.1.0] - 2026-11-11

### Added

- `rusto2-gw`: streaming HTTP reverse proxy built on axum + reqwest with
  connection pooling and hop-by-hop header filtering.
- Per-route declarative policies in `rusto2.yaml`: methods, JWT scopes,
  per-client/per-IP token buckets and circuit breakers.
- Correlation headers `X-RustO2-Request-Id` and `X-Request-Id`, plus
  `X-Forwarded-For`, `X-Forwarded-Host` and `X-Forwarded-Proto`.
- `rusto2-is`: OAuth2 `client_credentials` token endpoint, Ed25519 signed JWTs,
  JWKS and OpenID discovery endpoints.
- Gateway security middleware with cached JWKS, issuer/audience/expiry checks,
  scope enforcement and key-rotation refresh.
- DashMap token bucket rate limiter with `429`, `X-RateLimit-*` and
  `Retry-After` headers.
- Per-route circuit breaker with half-open probes and `503` fail-fast.
- Structured JSON logs, Prometheus metrics and Grafana dashboard provisioning.
- Multi-stage distroless Docker images and a one-command demo stack with
  httpbin, Prometheus and Grafana.
- Smoke tests, benchmark scripts and GitHub Actions CI.
