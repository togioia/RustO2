# WSO2 parity matrix

RustO2 is not a generic API gateway with OAuth2. Its target is **behavioral
parity with the WSO2 API Manager Gateway/Key Manager core**, plus a practical
subset of Micro Integrator mediation. This document is the honest scoreboard:
what is already faithful, what is only inspired by WSO2, and what is missing.

Legend: ✅ faithful enough to use · 🟡 partial / different shape · ❌ missing ·
🚫 intentionally out of scope.

## Why this matters

A feature list can look impressive while missing the contracts that make WSO2
WSO2: Synapse XML, API definitions, mediation sequences, subscriptions,
throttling tiers, lifecycle states, backend JWTs and the key-manager grants. The
gateway must eventually consume the same artifacts and produce the same
observable decisions, not just expose similar knobs.

## Runtime and artifacts

| WSO2 capability | RustO2 status | Notes |
|---|---|---|
| Synapse XML `<api>`, `<resource>`, `<sequence>` | ❌ | Current configuration is YAML-native; no XML/Synapse parser yet |
| API definition (Publisher OpenAPI/APIM artifact) | ❌ | No import of `context`, `version`, resources, endpoint config or tiers |
| CAR package deployment | ❌ | No `.car` extraction or artifact lifecycle |
| Mediation sequence engine | ❌ | No `log`, `property`, `filter`, `switch`, `payloadFactory`, `enrich`, `call`, `send`, `respond`, `fault` interpreter |
| Custom class mediator | 🟡 | Java binary compatibility is out of scope; a native/WASM plugin ABI is the intended path |
| Resource URI templates | ❌ | Routes are prefix-based, not WSO2 resource templates |
| HTTP method allowlists | ✅ | Per-route allowlist with `405` |
| Endpoint HTTP backend | ✅ | Streaming proxy with pooling |
| Endpoint failover / load balance | ❌ | Single upstream per route |
| Fault sequence | ❌ | Gateway-generated JSON errors only |
| Request/response streaming | ✅ | End-to-end streaming, no buffering |
| CORS | ❌ | No APIM-style CORS configuration |
| API lifecycle enforcement | ❌ | No PUBLISHED / DEPRECATED / RETIRED / BLOCKED state |
| Response caching | ❌ | Not implemented |
| JSON/XML schema validation | ❌ | Not implemented |

## Security and API Manager semantics

| WSO2 capability | RustO2 status | Notes |
|---|---|---|
| OAuth2 client credentials | ✅ | Ed25519-signed JWT access tokens |
| Authorization code + PKCE | ❌ | Not implemented |
| Password grant | ❌ | Not implemented |
| Refresh tokens | ❌ | Not implemented |
| Dynamic client registration (DCR) | ❌ | Not implemented |
| Opaque access tokens | ❌ | Tokens are JWTs only |
| JWT access tokens | ✅ | EdDSA only; RS256/ES256 are missing |
| JWKS discovery | ✅ | JWKS with ETag and key-rotation refresh |
| Token introspection | ✅ | RFC 7662, in-memory blocklist, opt-in gateway check |
| Token revocation | ✅ | RFC 7009, `jti` TTL |
| Scopes | 🟡 | Enforced per route; not tied to APIM API resources |
| Subscription validation | ❌ | No applications, consumer keys or subscriptions |
| API keys | ❌ | No API-key security scheme |
| Mutual TLS | ❌ | Not implemented |
| Multiple key managers | ❌ | Single embedded issuer |
| Backend JWT generation | ❌ | No signed claims forwarded to the backend |
| Consumer-key/secret rotation | ❌ | Static client registry |
| Token endpoint throttling / Argon2 secrets | ❌ | SHA-256 digests and no token endpoint quota |

## Traffic management

| WSO2 capability | RustO2 status | Notes |
|---|---|---|
| Application / subscription / API / resource tiers | ❌ | No APIM tier model |
| Bronze / Silver / Gold / Unlimited presets | ❌ | Not defined |
| Advanced throttling policies | ❌ | Not implemented |
| Per-client / per-IP token bucket | ✅ | Route-level policy |
| Distributed quota store | ✅ | Redis Lua token bucket; fail-open policy |
| Traffic Manager parity | ❌ | No Traffic Manager RPC or policy language |
| Rate-limit headers | ✅ | `X-RateLimit-*`, `Retry-After` |
| Circuit breaker | ✅ | Per route, half-open recovery |
| Backend timeouts / suspend | 🟡 | Global connect/request timeouts; no endpoint suspend state |

## Identity server surface

| WSO2 capability | RustO2 status | Notes |
|---|---|---|
| OAuth2 authorization server | 🟡 | Client credentials only |
| OIDC discovery / JWKS | 🟡 | Issuer, token, introspection, revocation; no full OIDC |
| User / role stores | ❌ | No users, roles or SCIM |
| SAML / WS-Federation | ❌ | Out of scope for the gateway-core experiment |
| Adaptive authentication | ❌ | Out of scope |
| Token exchange | ❌ | Not implemented |
| Key rotation with overlap | ❌ | One active key; no grace window |

## Micro Integrator subset

| WSO2 capability | RustO2 status | Notes |
|---|---|---|
| Synapse mediation runtime | ❌ | Highest-priority fidelity gap |
| Data services | ❌ | Not implemented |
| Scheduled tasks | ❌ | Not implemented |
| Message stores / processors | ❌ | Not implemented |
| Inbound endpoints | ❌ | HTTP proxy only |
| Connectors (REST/SOAP) | ❌ | Not implemented |
| Hot deployment of artifacts | ❌ | Process restart / no artifact watcher |

## Observability, deployment and operations

| Capability | RustO2 status | Notes |
|---|---|---|
| Structured JSON logs | ✅ | Correlation ID propagated to backends |
| Prometheus metrics | ✅ | Requests, auth, quotas, revocation, latency |
| Grafana dashboards and alerts | ✅ | Provisioned dashboard + Prometheus rules |
| OpenTelemetry OTLP traces | ❌ | Roadmap; JSON correlation only today |
| WSO2 analytics events / ELK | ❌ | Not implemented |
| Helm chart | ✅ | Probes, HPA, PDB, NetworkPolicy, ServiceMonitor |
| APIM distributed profiles (GW/KM/TM/Publisher/DevPortal) | ❌ | Two binaries only |
| Supply-chain evidence | ✅ | SBOM, cosign, SLSA provenance, cargo-deny |
| Hot config reload | ❌ | SIGHUP/file-watch roadmap |

## Fidelity roadmap (ordered)

### M1 — WSO2 artifact compatibility
Read real WSO2 artifacts without hand-written YAML:

1. Parse an APIM API definition (`context`, `version`, resources, endpoint
   configuration, security, throttling policy).
2. Parse Synapse `<api>` XML and `<resource>` URI templates.
3. Extract `.car` archives and map API definitions to internal route snapshots.
4. Fail closed or warn on unsupported mediators instead of silently ignoring
   them.

**Exit criteria:** a sample WSO2 API passes through the gateway with the same
external path shape (`/context/version/resource`) and method/security decisions.

### M2 — Mediation engine
Introduce a `rusto2-synapse` crate with a sequence interpreter and a message
context compatible with Synapse semantics:

- `log`, `property`, `filter`, `switch`, `payloadFactory`, `enrich`, `call`,
  `send`, `respond`, `header`, `fault`.
- In/out/fault sequences, named sequences and endpoint references.
- JSON and XML payload support, properties and transport headers.
- A native mediator plugin ABI (Rust/WASM); Java class mediators are out of
  scope.

**Exit criteria:** the WSO2 quick-start mediation samples run unchanged (except
for class mediators) and produce equivalent responses.

### M3 — API Manager policy layer
- Applications, subscriptions and consumer keys.
- API/resource scopes and API keys.
- Lifecycle states enforced at the gateway.
- Bronze/Silver/Gold/Unlimited and custom tier policies.
- Backend JWT with WSO2-compatible claims.
- CORS and fault payloads matching APIM behavior.

### M4 — Key manager parity
- Authorization code + PKCE, password grant, refresh tokens, DCR.
- Opaque tokens and token exchange.
- Multiple key managers and key rotation with overlapping keys.
- Persistent revocation store; token endpoint throttling.

### M5 — Traffic Manager parity
- Tier policy model and advanced throttling rules.
- Distributed counters and policy reload.
- Traffic Manager-compatible semantics, not only a Redis bucket.

### M6 — Micro Integrator subset and operations
- Data services, tasks, message stores/processors, inbound endpoints.
- CAR hot deployment and artifact lifecycle.
- OpenTelemetry/Tempo and WSO2-style analytics events.
- Helm profiles for gateway/key-manager/traffic-manager roles.

## Non-goals

- Reimplementing Carbon/OSGi, the management console, Publisher or Developer
  Portal UI.
- Byte-compatible Java class mediators.
- SAML, adaptive authentication and the full Identity Server feature set.
- Claiming “WSO2-compatible” before the M1/M2/M3 exit criteria are met.

## Current position

RustO2 today is a **gateway/key-manager prototype with several APIM-shaped
building blocks** (JWT, quotas, revocation, resilience, observability), but the
Synapse mediation runtime and WSO2 artifact contracts are still missing. The
platform-hardening items (OpenTelemetry, hot reload, distributed revocation,
supply chain) matter, but they do not close the main fidelity gap.
