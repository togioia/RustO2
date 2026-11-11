# Security policy

RustO2 is a reference implementation and a benchmark playground, not a
hardened production gateway out of the box.

## Reporting a vulnerability

Please open a private security advisory on GitHub:
<https://github.com/togioia/RustO2/security/advisories/new>

Do not disclose the issue publicly before a fix is available.

## Demo credentials

The credentials in `rusto2-is.yaml` and `docker-compose.yml` are **demo
credentials**. They are deliberately public and must be replaced through the
`RUSTO2_DEMO_SECRET` / `RUSTO2_READONLY_SECRET` environment variables.

## Before exposing RustO2 to untrusted traffic

- Terminate TLS in front of the gateway and set `server.trust_proxy` only when
  a trusted load balancer sets `X-Forwarded-For`.
- Replace demo clients with secrets stored in a secret manager; rotate them.
  Client secrets are compared in constant time, but they are stored as SHA-256
  digests, not Argon2 hashes; use high-entropy secrets.
- Keep `/.well-known/jwks.json` public, but restrict `/metrics`, `/healthz` and
  `/readyz` to the monitoring network.
- Persist and back up the Ed25519 private key. Use `RUSTO2_KEY_PATH` to point to
  a mounted secret; the key file is created with mode `0600` when generated.
- Run behind a body-size limit appropriate for your APIs. RustO2 streams bodies,
  so the limit belongs at the edge or in the upstream service.
- Review scopes per route; `auth.mode: none` routes are public by design.
- The identity server implements only `client_credentials`; it does not support
  authorization code, PKCE, refresh tokens or token revocation.
