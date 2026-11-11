# Synapse compatibility subset

`rusto2-synapse` is the WSO2-facing part of RustO2. It reads WSO2 artifacts,
builds a route snapshot and executes the resource's Synapse sequences against a
`MessageContext`. The design rule is: **execute what is supported, reject the
rest explicitly**. A mediator that is not implemented never becomes a no-op.

```text
api.yaml / OpenAPI ─┐
Synapse <api> XML ──┼─> ArtifactBundle ─> ArtifactRoute ─> MediationPlan
<sequence>/<endpoint>┤                                      │
.car (ZIP) ─────────┘                                      v
                                                   mediate::mediate()
                                                   ├─ inSequence
                                                   ├─ outSequence
                                                   └─ faultSequence
```

## Artifact formats

### APIM `api.yaml` (API Manager 4.x)

Supported fields:

- `data.name`, `context`, `version`, `provider`
- `lifeCycleStatus`: `PUBLISHED` and `DEPRECATED` are served; other states are
  skipped with a warning
- `securityScheme: [oauth2]` and per-operation `authType`
- `throttlingPolicy` (parsed and reported; enforcement is M3)
- `endpointConfig` with `endpoint_type: http` or `address` and
  `production_endpoints` / `sandbox_endpoints`
- `operations[].target`, `verb`, `scopes`, `authType`, `throttlingPolicy`

Rejected at load time:

- `securityScheme` without OAuth2 (`api_key`, `basic_auth`, `mutual_ssl`)
- custom `authorizationHeader` values other than `Authorization`
- `endpoint_type: failover` or `load_balance` (M3)
- `endpointConfig.endpoint_security` (backend OAuth2/basic auth, M3)
- APIs with no endpoint or no operations

APIM 3.x exports sometimes store `endpointConfig` as a JSON string; both forms
are accepted. The external path is `/context/version/<target>`.

### OpenAPI 3 with WSO2 extensions

The parser also accepts an OpenAPI document with:

- `x-wso2-basePath`
- `x-wso2-production-endpoints.urls[0]` or a `servers[].url`
- `paths` and standard HTTP operation keys
- operation `security` requirements and `x-wso2-disable-security`
- `components.securitySchemes` of type `oauth2` (or `apiKey`, which is rejected)

### Synapse `<api>`

```xml
<api xmlns="http://ws.apache.org/ns/synapse"
     name="OrderAPI" context="/orders" version="1.0.0">
  <resource methods="GET" uri-template="/orders/{id}">
    <inSequence>...</inSequence>
    <outSequence>...</outSequence>
    <faultSequence key="CommonFault"/>
  </resource>
</api>
```

- `resource@methods` is split on spaces or commas and uppercased.
- `resource@uri-template` supports literals, `{name}` parameters and a final
  `*` wildcard. Parameters become `uri.var.<name>` properties.
- API-level `inSequence`/`outSequence`/`faultSequence` are defaults for
  resources that do not override them.
- A sequence can be inline or `key="name"`; named sequences and endpoints are
  resolved against the loaded bundle.
- Unsupported child elements (`handlers`, `class`, `script`, ...) fail loading.

### Named sequences and endpoints

Standalone Synapse files are accepted:

```xml
<definitions xmlns="http://ws.apache.org/ns/synapse">
  <endpoint name="Backend"><address uri="http://backend:8080"/></endpoint>
  <sequence name="CommonFault"><payloadFactory .../><respond/></sequence>
</definitions>
```

`<endpoint>` supports `<address uri="...">`, `<http uri="...">` and
`<default><address .../></default>`. WSDL and endpoint aliases are rejected.

### CAR packages

A CAR is a ZIP. RustO2 scans every `.xml`, `.yaml` and `.yml` entry and parses
the supported artifact types. Typical layout:

```text
artifacts/PizzaShackAPI/api.yaml
synapse-configs/default/api/MediationDemo.xml
synapse-configs/default/sequences/CommonSequences.xml
synapse-configs/default/endpoints/Backend.xml
```

Limits are 4,096 entries, 16 MiB per entry and 64 MiB uncompressed. Files under
`/api/` or `/sequences/` must parse as the expected type. Other artifact types
(proxy services, tasks, message stores, inbound endpoints) are collected in
`ignored` and logged; they do not stop the supported APIs from loading.

Build the demo archive with:

```bash
./examples/wso2/build-car.sh
# then set artifacts.paths to examples/wso2/Rusto2Demo.car
```

## Route mapping

For an APIM operation or Synapse resource:

```text
external path = /<context>/<version>/<uri-template>
```

Examples:

| Artifact | External path | Backend path (`REST_URL_POSTFIX`) |
|---|---|---|
| `context: /pizzashack`, `version: 1.0.0`, target `/get` | `/pizzashack/1.0.0/get` | `/get` |
| `context: /demo`, version `1.0.0`, `uri-template: /items/{id}` | `/demo/1.0.0/items/42` | `/items/42` |
| `uri-template: /anything/*` | `/demo/1.0.0/anything/foo/bar` | `/anything/foo/bar` |

Matching is segment-based and more specific than native prefix routes. A
request whose path matches but whose method does not returns `405` with the
union of allowed methods.

## Mediators

| Mediator | Implemented behavior | Notes |
|---|---|---|
| `log` | `simple`, `custom`, `full`, `trace`/`debug`/`info`/`warn`/`error`; named properties and expressions | `full` includes up to 4 KiB of payload |
| `property` | `set`/`remove`, `value`/`expression`, default/axis2/transport/synapse/operation scopes, STRING/INTEGER/BOOLEAN/DOUBLE/FLOAT/LONG/SHORT/OM/JSON types | `HTTP_SC` is the response status |
| `filter` | `source` + `regex`, or `xpath`; `then`/`else` | XPath subset below |
| `switch` | `source`, `case@regex`, `case@value`, `default` | first match wins |
| `payloadFactory` | JSON, XML and text formats with `$1..$n` arguments | JSON/XML are validated before use |
| `enrich` | body/envelope replace/remove, body append/prepend for JSON arrays or text/XML, property and header set/remove | `child` and custom-XPath sources are rejected |
| `call` | blocking HTTP call; response becomes the current message | `blocking="false"` is rejected |
| `send` | same HTTP transport path as `call` | kept distinct so out-sequence semantics stay explicit |
| `respond` | stops mediation and returns the current payload/status/headers | |
| `header` | transport header set/remove | in-sequence mutates the backend request; out/fault mutates the client response |
| `drop` | returns an empty `200` | Synapse drops the connection; HTTP needs a response |
| `sequence` | named sequence reference | resolved recursively, depth limit 32 |

Everything else is a load error. That includes `class`, `script`, `xslt`,
`iterate`, `clone`, `aggregate`, `foreach`, `dbreport`, `dblookup`,
`transaction`, `entitlement`, `oauthService`, `rule`, `event`, `enqueue`,
`makefault` and `bam`.

## Expressions

| Expression | Meaning |
|---|---|
| `get-property('name')` | default-scope property |
| `get-property('axis2','HTTP_SC')` | scoped property |
| `$ctx:name`, `$trp:name`, `$axis2:name`, `$synapse:name`, `$operation:name` | scoped shorthand |
| `$body` | entire payload as text |
| `json-eval($.a.b[0])` | JSONPath subset: object keys, array indexes, quoted keys |
| `/a/b`, `//name`, `//name/text()` | XML text extraction subset |
| anything else | literal string |

JSONPath wildcards, filters and recursive descent are rejected. XPath
predicates, attributes, functions and wildcards are rejected. The goal is to
fail clearly rather than evaluate a different expression than Synapse would.

## Sequences and faults

The execution order is:

1. `inSequence`
2. if it did not `respond`/`drop`, `outSequence`
3. if any step fails, `faultSequence`

`call` and `send` replace the current payload, status and response headers with
the backend response. Properties survive across the call, which is what
`get-property('ERROR_CODE')` and similar fault patterns rely on.

The default plan for an APIM operation (and for a Synapse resource with no
`inSequence`) is:

```xml
<inSequence><call/></inSequence>
<outSequence><respond/></outSequence>
```

The default endpoint is the API definition endpoint (APIM) or
`artifacts.default_upstream`. A `<call>`/`<send>` without an endpoint uses that
default. An inline/named endpoint is a base URL; `REST_URL_POSTFIX` and the
query string are appended.

Fault handling sets:

- `ERROR_MESSAGE` (default scope)
- `ERROR_CODE=500`
- `HTTP_SC=500` (axis2 scope)

If no fault sequence is configured, the gateway returns a generic JSON `500`.
Fault details are logged, not echoed to the client.

## Properties

The engine initializes:

| Property | Scope | Value |
|---|---|---|
| `HTTP_METHOD` | default | request method |
| `REQUEST_URI` | default | request path |
| `REST_URL_POSTFIX` | default | path after `/context/version` |
| `HTTP_SC` | axis2 | initial status, updated by `call` and property mediators |
| `uri.var.<name>` | default | URI-template parameters |

## Configuration

```yaml
artifacts:
  paths:
    - ./examples/wso2/apim
    - ./examples/wso2/synapse
    - ./examples/wso2/Rusto2Demo.car
  auth: jwt                 # security for plain Synapse APIs
  scopes: []
  revocation_check: false
  max_body_bytes: 2097152
  default_upstream: http://httpbin:8080
```

APIM definitions carry their own security and scopes. `auth`/`scopes` apply
only to Synapse `<api>` resources, because Synapse XML does not declare APIM
security metadata.

## Tests

```bash
cargo test -p rusto2-synapse
cargo test -p rusto2-gw artifact
./scripts/smoke.sh
```

The smoke script checks:

- `/pizzashack/1.0.0/get` from an APIM definition
- operation method and scope enforcement
- `/demo/1.0.0/items/42` through property/filter/switch/enrich/payloadFactory
- `/demo/1.0.0/anything/foo` through `call`, a named endpoint and outSequence

## Known gaps

- Class mediators and Java binary compatibility are out of scope. A native or
  WASM plugin ABI is the intended extension.
- XML enrichment is text-level; there is no AXIOM tree.
- `call blocking="false"` and callback-based mediation are rejected.
- Failover/load-balanced endpoints, endpoint security and backend JWT are M3.
- APIM subscriptions, applications, API keys and throttling tiers are M3.
- Mediated routes buffer the request body; native routes remain streaming.
