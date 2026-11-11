<p align="center">
  <img src="docs/assets/logo.svg" width="110" alt="RustO2 logo" />
</p>

<h1 align="center">RustO2</h1>

<p align="center">
  <strong>Reimplementazione in Rust del core di WSO2: gateway, OAuth2, JWT, rate limiting e resilienza.</strong>
</p>

RustO2 è un esperimento ingegneristico indipendente che riprende i concetti
chiave di WSO2 (pipeline di mediazione, policy per API, throttling, circuit
breaker, osservabilità) e li ricostruisce su un runtime Rust moderno, senza
application server, OSGi o management console. Non contiene codice WSO2.

L'obiettivo è la **parità funzionale con il core Gateway/Key Manager di WSO2 API
Manager** e con un sottoinsieme pratico del Micro Integrator. Oggi RustO2 importa
`api.yaml` di APIM, API/sequence Synapse e pacchetti `.car`, ed esegue un
sottoinsieme crescente di mediator. La matrice onesta è in
[`docs/WSO2_PARITY.md`](docs/WSO2_PARITY.md); il subset Synapse supportato è in
[`docs/SYNAPSE.md`](docs/SYNAPSE.md).

> La documentazione completa, i benchmark e la guida curl sono in
> **[README.md](README.md)** (inglese).

## Cosa c'è dentro

| Componente | Ruolo |
|---|---|
| `rusto2-gw` | Proxy HTTP streaming su axum + reqwest, con policy per rotta |
| `rusto2-synapse` | Parser artefatti WSO2 (APIM, Synapse, CAR) e motore di mediazione |
| `rusto2-is` | OAuth2 `client_credentials`, JWT Ed25519, JWKS e discovery |
| `rusto2-common` | Log JSON, correlation ID, metriche e helper condivisi |

Funzionalità principali:

- import di API definition APIM, API Synapse con URI-template, sequence
  in/out/fault, endpoint e sequence nominali, pacchetti `.car`;
- motore di mediazione con `log`, `property`, `filter`, `switch`,
  `payloadFactory`, `enrich`, `call`, `send`, `respond`, `header`, `drop`;
- rotte dichiarative in `rusto2.yaml` con metodi consentiti, scope richiesti,
  quota e circuit breaker;
- rate limiting con backend in memoria o **Redis** (token bucket Lua atomico)
  per condividere le quote tra repliche del gateway;
- introspezione RFC 7662 e revoca RFC 7009 con blocklist `jti` e cache breve nel
  gateway;
- propagazione di `X-Forwarded-For`, `X-Forwarded-Proto`, `X-RustO2-Request-Id`
  e `X-Request-Id`;
- validazione JWT locale tramite JWKS in cache, con refresh per rotazione chiavi;
- token bucket su `DashMap` per `client_id` o IP, con `429` e header
  `X-RateLimit-*` / `Retry-After`;
- circuit breaker per rotta con stato `closed` / `open` / `half_open`;
- log strutturati JSON, metriche Prometheus e dashboard Grafana;
- Helm chart Kubernetes con probe, HPA, PDB, NetworkPolicy e ServiceMonitor;
- immagini Docker multi-stage distroless e stack demo con httpbin, Redis,
  Prometheus e Grafana.

## Avvio rapido

```bash
git clone https://github.com/togioia/RustO2.git
cd RustO2
docker compose up -d --build
./scripts/demo.sh
```

Servizi esposti:

| Servizio | URL |
|---|---|
| Gateway | http://localhost:8080 |
| Identity Server | http://localhost:9000 |
| Prometheus | http://localhost:9090 |
| Grafana (`admin` / `admin`) | http://localhost:3000 |
| httpbin diretto | http://localhost:8081 |

## Esempio curl

```bash
TOKEN=$(curl -s -X POST http://localhost:9000/oauth2/token \
  -d "grant_type=client_credentials" \
  -d "client_id=demo-client" \
  -d "client_secret=demo-secret" \
  -d "scope=read write" | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')

curl -i -H "Authorization: Bearer $TOKEN" \
  http://localhost:8080/api/get

curl -i http://localhost:8080/public/get     # rotta anonima
curl -i http://localhost:8080/api/get        # 401, token mancante

# Artefatto APIM: path /context/version/resource
curl -i -H "Authorization: Bearer $TOKEN" \
  http://localhost:8080/pizzashack/1.0.0/get

# Artefatto Synapse: property -> filter -> switch -> payloadFactory -> outSequence
curl -i -H "Authorization: Bearer $TOKEN" \
  http://localhost:8080/demo/1.0.0/items/42

# call verso endpoint nominale + outSequence
curl -i -H "Authorization: Bearer $TOKEN" \
  http://localhost:8080/demo/1.0.0/anything/foo
```

Gli artefatti demo sono in `examples/wso2/`; `examples/wso2/build-car.sh`
produce un `.car` equivalente. I mediator non supportati falliscono
all'avvio con nome e posizione XML, invece di essere ignorati.

## Test

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
./scripts/smoke.sh
```

Sono inclusi property test (`proptest`), un harness `cargo-fuzz` documentato in
`docs/FUZZING.md`, il chart Helm in `deploy/helm/rusto2`, la policy supply
chain in `docs/SUPPLY_CHAIN.md` e il subset Synapse in `docs/SYNAPSE.md`.

## Nota sul progetto

RustO2 è un proof of concept e un portfolio project, non un prodotto pronto per
la produzione. Le credenziali demo sono pubbliche e vanno sostituite. Vedi
[SECURITY.md](SECURITY.md).

Licenza: [Apache-2.0](LICENSE). WSO2 è un marchio di WSO2 LLC; RustO2 non è
affiliato a WSO2.
