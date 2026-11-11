# Running RustO2 on Kubernetes

This guide deploys RustO2 with the Helm chart in `deploy/helm/rusto2`. The
chart runs the same two binaries as the Compose demo:

| Component | Workload | Container port | Health endpoints |
|---|---|---|---|
| `rusto2-gw` | Gateway: routing, JWT validation, rate limiting, metrics | 8080 | `/healthz`, `/readyz`, `/metrics` |
| `rusto2-is` | Identity server: OAuth2 `client_credentials`, Ed25519, JWKS | 9000 | `/healthz`, `/metrics` |

Both containers run as UID/GID `65532` with a read-only root filesystem. The
identity server is the only workload that needs a writable path: `/data`, where
the Ed25519 private key lives.

## Prerequisites

- Kubernetes 1.25 or newer (the chart uses `autoscaling/v2`,
  `policy/v1`, and `networking.k8s.io/v1`).
- Helm 3.14 or newer.
- Container images available to the cluster. The chart defaults to
  `rusto2-gw:dev` and `rusto2-is:dev`, matching `docker compose`.
- Optional: Metrics Server for the HPAs.
- Optional: Prometheus Operator CRDs for `serviceMonitor.enabled`.
- Optional: a CNI that enforces NetworkPolicy for `networkPolicy.enabled`.

## Build and load images

Build the distroless images from the repository root:

```bash
docker build -f docker/gateway.Dockerfile  -t rusto2-gw:dev .
docker build -f docker/identity.Dockerfile -t rusto2-is:dev .
```

For a local cluster, load them instead of pushing to a registry:

```bash
# kind
kind load docker-image rusto2-gw:dev --name <cluster>
kind load docker-image rusto2-is:dev --name <cluster>

# minikube
minikube image load rusto2-gw:dev
minikube image load rusto2-is:dev
```

For a shared cluster, tag and push to your registry and override the
repositories at install time. The chart supports tags and digests:

```yaml
gateway:
  image:
    repository: ghcr.io/example/rusto2-gw
    tag: "0.1.0"
    digest: ""          # when set, takes precedence over tag
identity:
  image:
    repository: ghcr.io/example/rusto2-is
    tag: "0.1.0"
```

Use `imagePullSecrets` for private registries:

```yaml
imagePullSecrets:
  - name: regcred
```

## Install

```bash
helm upgrade --install rusto2 deploy/helm/rusto2 \
  --namespace rusto2 --create-namespace
```

With a release named `rusto2`, the chart creates these stable names:

| Resource | Name |
|---|---|
| Gateway Service | `rusto2-gw` (port 8080) |
| Identity Service | `rusto2-is` (port 9000) |
| Gateway Deployment | `rusto2-gw` |
| Identity Deployment | `rusto2-is` |
| Client credentials Secret (chart-managed) | `rusto2-is-secrets` |
| Signing key PVC (chart-managed) | `rusto2-is-data` |
| ServiceAccount | `rusto2` |

The gateway ConfigMap points at `http://rusto2-is:9000/.well-known/jwks.json`.
If you use a different release name or `fullnameOverride`, the helper adjusts
both the Service name and the JWKS URL together.

Check the rollout:

```bash
kubectl -n rusto2 get pods,svc,hpa,pdb,pvc
kubectl -n rusto2 rollout status deployment/rusto2-is
kubectl -n rusto2 rollout status deployment/rusto2-gw
```

Gateway readiness (`/readyz`) returns `503` until the first JWKS fetch succeeds,
so a gateway pod is not added to the Service before it can verify tokens. The
identity server binds its listener after loading or generating the key, so its
`/healthz` is also a useful readiness signal and it becomes ready quickly.

### Demo backend

The default routes point at a Service named `httpbin` on port 8080, mirroring
the Compose demo. The chart does not deploy a backend. For a quick smoke test:

```bash
kubectl -n rusto2 create deployment httpbin \
  --image=mccutchen/go-httpbin:2.26.0 --port=8080
kubectl -n rusto2 expose deployment httpbin --port=8080
```

In production, replace `gateway.config.routes` with your own upstreams and
adjust `networkPolicy.gateway.egress.backend` accordingly.

## Get a token and call the gateway

### Port-forward

In two terminals:

```bash
kubectl -n rusto2 port-forward svc/rusto2-is 9000:9000
kubectl -n rusto2 port-forward svc/rusto2-gw 8080:8080
```

When `identity.secrets.*` is empty, the chart generates random demo client
secrets on first install and preserves them on upgrade. Read them from the
chart-managed Secret:

```bash
DEMO_SECRET=$(kubectl -n rusto2 get secret rusto2-is-secrets \
  -o jsonpath='{.data.RUSTO2_DEMO_SECRET}' | base64 -d)
READONLY_SECRET=$(kubectl -n rusto2 get secret rusto2-is-secrets \
  -o jsonpath='{.data.RUSTO2_READONLY_SECRET}' | base64 -d)
```

If `identity.existingSecret` is set, the chart does not create that Secret;
read the same keys from your Secret instead. It must also contain
`RUSTO2_GATEWAY_SECRET` for the gateway's introspection client.

### Revocation and introspection

The default `/admin` route has `revocation_check: true`. The gateway uses the
`RUSTO2_GATEWAY_SECRET` from the chart Secret to call `/oauth2/introspect`;
revoked `jti` values are rejected after `revocation_cache_secs` (5 s by
default). `/api` keeps the local-JWKS fast path. Revocation state is held in the
identity server's memory, so keep `identity.replicaCount: 1` unless you add a
shared revocation store.

### Distributed quotas with Redis

The chart does not deploy Redis. To share quotas across gateway replicas, point
the gateway at an external or operator-managed Redis:

```yaml
gateway:
  config:
    rate_limiting:
      backend: redis
      redis_url: redis://rusto2-redis:6379/0
      fail_open: true
```

If Redis is unavailable and `fail_open` is true the gateway keeps serving
without strict quota; set it to false only if availability is less important
than the limit. Extend the NetworkPolicy to allow egress to Redis.

Request an access token:

```bash
TOKEN=$(curl -s -X POST http://localhost:9000/oauth2/token \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=demo-client" \
  --data-urlencode "client_secret=${DEMO_SECRET}" \
  --data-urlencode "scope=read write" \
  | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')

echo "${TOKEN:0:40}..."
```

Call a protected route:

```bash
curl -i -H "Authorization: Bearer ${TOKEN}" \
  "http://localhost:8080/api/get?hello=rusto2"
```

`/api` requires the `read` scope, `/admin` requires `write`, and `/public` is
anonymous. Method allowlists are enforced before authentication.

### From inside the cluster

Run a throwaway curl pod in the same namespace:

```bash
kubectl -n rusto2 run curl --rm -it \
  --image=curlimages/curl:8.10.1 --restart=Never -- sh
```

Inside the pod, export the secret you retrieved above
(`export DEMO_SECRET='...'`) and use the Service DNS names:

```sh
TOKEN=$(curl -s -X POST http://rusto2-is:9000/oauth2/token \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "grant_type=client_credentials" \
  -d "client_id=demo-client" \
  -d "client_secret=${DEMO_SECRET}" \
  -d "scope=read write" \
  | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')

curl -i -H "Authorization: Bearer ${TOKEN}" \
  http://rusto2-gw:8080/api/get?hello=rusto2
```

With NetworkPolicy disabled (the default), any pod in the namespace can reach
`http://rusto2-is:9000` and `http://rusto2-gw:8080` directly. If you enable
NetworkPolicy, add an `identity.extraIngress` rule for any debug pod that must
request tokens.

## Configuration examples

### Gateway HPA

```yaml
gateway:
  autoscaling:
    enabled: true
    minReplicas: 2
    maxReplicas: 10
    targetCPUUtilizationPercentage: 70
```

```bash
helm upgrade --install rusto2 deploy/helm/rusto2 -n rusto2 \
  --set gateway.autoscaling.enabled=true \
  --set gateway.autoscaling.minReplicas=2 \
  --set gateway.autoscaling.maxReplicas=10
```

When the HPA is enabled the Deployment omits `replicas`; `gateway.replicaCount`
is used only while autoscaling is disabled. Metrics Server must be installed or
the HPA stays in `unknown` state.

### Identity replicas and HPA

Key generation is a **single-writer** operation. The default is one identity
replica, `Recreate` updates, and an HPA disabled. If you need more than one
replica, give every replica the same pre-generated key as a read-only Secret:

```bash
openssl genpkey -algorithm ED25519 -out ed25519.pem
kubectl -n rusto2 create secret generic rusto2-signing-key \
  --from-file=ed25519.pem
```

```yaml
identity:
  replicaCount: 2
  persistence:
    enabled: false          # the key comes from the Secret, not a PVC
  keys:
    existingSecret: rusto2-signing-key
    secretKey: ed25519.pem  # generate_if_missing is forced to false
  autoscaling:
    enabled: true
    minReplicas: 2
    maxReplicas: 4
```

```bash
helm upgrade --install rusto2 deploy/helm/rusto2 -n rusto2 \
  --set identity.persistence.enabled=false \
  --set identity.keys.existingSecret=rusto2-signing-key \
  --set identity.replicaCount=2 \
  --set identity.autoscaling.enabled=true \
  --set identity.autoscaling.minReplicas=2 \
  --set identity.autoscaling.maxReplicas=4
```

All replicas publish the same JWKS. With a PVC instead of a Secret, the claim
must be provisioned as `ReadWriteMany`, pre-populated with `ed25519.pem`, and
mounted with `identity.keys.generateIfMissing=false` before scaling; a
`ReadWriteOnce` volume cannot be attached by pods on different nodes:

```yaml
identity:
  persistence:
    enabled: true
    existingClaim: rusto2-keys   # must be a ReadWriteMany claim
  keys:
    generateIfMissing: false
  updateStrategy:
    type: RollingUpdate
```

See [Limitations](#limitations).

### Gateway Ingress

```yaml
ingress:
  enabled: true
  className: nginx
  annotations:
    cert-manager.io/cluster-issuer: letsencrypt-prod
    nginx.ingress.kubernetes.io/proxy-body-size: 10m
  hosts:
    - host: gateway.example.com
      paths:
        - path: /
          pathType: Prefix
  tls:
    - secretName: rusto2-gateway-tls
      hosts:
        - gateway.example.com

gateway:
  config:
    server:
      trust_proxy: true   # only behind a trusted proxy that overwrites X-Forwarded-For
```

TLS terminates at the Ingress; the RustO2 listener itself is plain HTTP.

### Existing client Secret

Create a Secret whose keys match `identity.config.clients[].secretEnv`:

```bash
kubectl -n rusto2 create secret generic rusto2-clients \
  --from-literal=RUSTO2_DEMO_SECRET='replace-me' \
  --from-literal=RUSTO2_READONLY_SECRET='replace-me-too'

helm upgrade --install rusto2 deploy/helm/rusto2 -n rusto2 \
  --set identity.existingSecret=rusto2-clients
```

The chart then creates no client Secret. If you add a client with
`secretEnv: RUSTO2_PARTNER_SECRET`, that key must also exist in the Secret.
Values are expanded into a double-quoted YAML scalar, so avoid double quotes
and backslashes in the secret (generated alphanumeric secrets are safe).
Changes to an externally managed Secret are not observed by Helm; restart the
identity Deployment after rotating credentials:

```bash
kubectl -n rusto2 rollout restart deployment/rusto2-is
```

### Signing key PVC

The default values persist the generated key on a `ReadWriteOnce` PVC:

```yaml
identity:
  persistence:
    enabled: true
    size: 1Gi
    storageClass: ""        # cluster default
    accessModes: [ReadWriteOnce]
    keep: true              # helm.sh/resource-policy: keep
```

To mount a claim you manage yourself:

```yaml
identity:
  persistence:
    enabled: true
    existingClaim: rusto2-keys
  keys:
    generateIfMissing: false  # fail loudly if ed25519.pem is missing
```

The claim must contain `ed25519.pem` at its root, or be writable by UID/GID
`65532` so the identity server can generate it. `keep: true` means
`helm uninstall` leaves the PVC behind; delete it manually after rotating keys:

```bash
kubectl -n rusto2 delete pvc rusto2-is-data
```

### NetworkPolicy

NetworkPolicy is disabled by default so the chart works on any CNI:

```yaml
networkPolicy:
  enabled: true
  monitoring:
    namespaceSelector:
      matchLabels:
        kubernetes.io/metadata.name: monitoring
  gateway:
    ingress:
      from:
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: ingress-nginx
    egress:
      backend:
        allowAllNamespaces: false
        cidrs:
          - 10.0.0.0/8
        ports: [8080, 443]
```

The generated policies allow:

- ingress to the gateway on 8080 (from `gateway.ingress.from`, or any source
  when the list is empty) and from the monitoring namespace for metrics;
- gateway to the identity Service on 9000 and to backend ports for the
  configured namespaces/CIDRs;
- ingress to identity on 9000 only from gateway pods, the monitoring namespace,
  and any `identity.extraIngress` rules;
- DNS egress to `kube-system` for both workloads.

### ServiceMonitor

```yaml
serviceMonitor:
  enabled: true
  interval: 30s
  labels:
    release: kube-prometheus-stack   # match your Prometheus selector
```

The ServiceMonitor selects both Services and scrapes `/metrics` on their `http`
port. It requires the Prometheus Operator CRDs; enabling it without them makes
the release fail.

### Topology spread / anti-affinity

```yaml
gateway:
  topologySpreadConstraints:
    - maxSkew: 1
      topologyKey: topology.kubernetes.io/zone
      whenUnsatisfiable: ScheduleAnyway
      labelSelector:
        matchLabels:
          app.kubernetes.io/name: rusto2
          app.kubernetes.io/component: gateway
```

`identity.topologySpreadConstraints` and per-component `affinity`,
`nodeSelector`, and `tolerations` are also supported. If you change
`nameOverride`, update the label selector accordingly.

## Operations

### Upgrade and rollback

```bash
helm upgrade rusto2 deploy/helm/rusto2 -n rusto2 \
  --set gateway.image.tag=0.1.1

helm history rusto2 -n rusto2
helm rollback rusto2 1 -n rusto2
```

ConfigMap and chart-managed Secret content are hashed into the pod template, so
changing routes, clients, or generated values triggers a rollout. Changes to an
external `identity.existingSecret` or to the key Secret/PVC are not hashed;
restart the identity Deployment after rotating them.

### Disruptions

The gateway PDB keeps `minAvailable: 1`; the identity PDB uses
`maxUnavailable: 1` so a single-replica node drain can complete instead of
blocking forever. Adjust `gateway.podDisruptionBudget` and
`identity.podDisruptionBudget` if you run different replica counts.

### Uninstall

```bash
helm uninstall rusto2 -n rusto2
```

The chart-managed PVC is retained because of `helm.sh/resource-policy: keep`.
The generated client Secret is deleted with the release; switch to
`identity.existingSecret` if credentials must survive reinstalls. Remove the
retained PVC manually when it is no longer needed. A retained PVC can also block
a later reinstall under the same release name; delete it first or let Helm adopt
it with `--take-ownership` (Helm 3.17+).

### Key rotation

1. Generate a new Ed25519 key (`openssl genpkey -algorithm ED25519`).
2. Update the key Secret or PVC.
3. Restart `rusto2-is`.

`rusto2-is` publishes one active key at a time and `rusto2-gw` refreshes JWKS
on rotation, but there is no overlapping-key grace period: tokens signed with
the previous key fail immediately. Rotate during a low-traffic window or accept
re-authentication. The gateway keeps its last known-good JWKS if the identity
server is briefly unavailable.

## Limitations

- **One signing key writer.** `rusto2-is` generates a key only when the PEM is
  missing, writes it with mode `0600`, and publishes one active key. Do not let
  two pods generate keys independently: tokens signed by one pod would fail
  verification against the JWKS cached from another. The chart defaults to one
  replica, `Recreate` updates for PVC-backed keys, and a disabled identity HPA.
- **Multi-replica identity needs a shared key.** Use
  `identity.keys.existingSecret` (recommended, read-only), or a `ReadWriteMany`
  claim that is pre-populated before scaling. A `ReadWriteOnce` PVC is suitable
  for exactly one writer and usually one node at a time.
- **No TLS in the applications.** The gateway and identity listeners are plain
  HTTP. Terminate TLS at an Ingress, load balancer, or service mesh. The chart
  only exposes an optional Ingress for the gateway; expose the identity server
  separately if external clients need its discovery or token endpoints.
- **Issuer consistency.** `identity.issuer` is written into tokens, discovery,
  and the gateway's `RUSTO2_ISSUER`. The internal JWKS URL always points at the
  identity Service. If you publish the identity server under an external URL,
  set `identity.issuer` to that URL in the same release, otherwise the gateway
  rejects the issuer. Changing the issuer invalidates previously issued tokens.
- **Quotas depend on the backend.** With `backend: memory` (the chart default)
  each gateway pod enforces its own quota, so an HPA multiplies the effective
  limit. Configure `rate_limiting.backend: redis` with an external Redis to
  share quotas; circuit-breaker state remains per pod.
- **Revocation is in-memory in the identity server.** The `jti` blocklist is
  local to a single `rusto2-is` process and is opt-in per route. A multi-replica
  identity deployment needs a shared revocation store.
- **Demo credentials.** Chart-generated client secrets are convenient demos,
  not production credentials. Use `identity.existingSecret` and rotate them.
  GitOps renderers such as Argo CD and Flux evaluate the chart without cluster
  access, so the `lookup` that preserves generated secrets returns empty and
  every render produces a new value; use an External Secrets/Sealed Secrets
  workflow with `identity.existingSecret` instead.
- **Gateway readiness depends on JWKS.** If the identity Service is unreachable
  for a long time, new gateway pods never become ready. Existing pods keep
  serving with cached keys.
- **Default routes target `httpbin`.** Replace `gateway.config.routes` with real
  upstreams; the chart does not deploy an application backend.
- **ServiceMonitor and HPA prerequisites.** Missing Prometheus Operator CRDs or
  Metrics Server will leave those optional features non-functional or fail the
  release.

## Chart validation

```bash
helm lint --strict deploy/helm/rusto2
helm template rusto2 deploy/helm/rusto2 --namespace rusto2
helm template rusto2 deploy/helm/rusto2 --namespace rusto2 \
  --set gateway.replicaCount=3 \
  --set gateway.autoscaling.enabled=true \
  --set ingress.enabled=true
```

A live cluster is not required for these commands. `helm template` renders the
manifests locally (the Secret `lookup` returns empty when no API is reachable).
`helm install --dry-run` may still contact the API server, so prefer
`helm template` for offline validation.
