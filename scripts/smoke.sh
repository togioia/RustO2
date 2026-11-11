#!/usr/bin/env bash
# End-to-end checks used locally and in CI.
set -euo pipefail

cd "$(dirname "$0")/.."

BASE_URL="${BASE_URL:-http://localhost:8080}"
IS_URL="${IS_URL:-http://localhost:9000}"
CLIENT_ID="${CLIENT_ID:-demo-client}"
CLIENT_SECRET="${CLIENT_SECRET:-demo-secret}"
READONLY_ID="${READONLY_ID:-readonly-client}"
READONLY_SECRET="${READONLY_SECRET:-readonly-secret}"
GATEWAY_SECRET="${GATEWAY_SECRET:-gateway-secret}"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok - $*"; }

extract_token() {
  sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p'
}

status() {
  curl -sS -o /dev/null -w '%{http_code}' "$@"
}

for _ in $(seq 1 60); do
  if curl -fsS "$IS_URL/healthz" >/dev/null 2>&1 && curl -fsS "$BASE_URL/readyz" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
curl -fsS "$BASE_URL/readyz" >/dev/null || fail "gateway is not ready"
pass "services are ready"

TOKEN=$(curl -sS -X POST "$IS_URL/oauth2/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET" \
  --data-urlencode "scope=read write" | extract_token)
[ -n "$TOKEN" ] || fail "token was not issued"
pass "token issued"

[ "$(status -H "Authorization: Bearer $TOKEN" "$BASE_URL/api/get")" = "200" ] \
  || fail "protected route did not return 200"
pass "valid JWT is accepted"

READONLY_TOKEN=$(curl -sS -X POST "$IS_URL/oauth2/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$READONLY_ID" \
  --data-urlencode "client_secret=$READONLY_SECRET" | extract_token)
[ -n "$READONLY_TOKEN" ] || fail "read-only token was not issued"

[ "$(status -H "Authorization: Bearer $READONLY_TOKEN" "$BASE_URL/admin/get")" = "403" ] \
  || fail "scope enforcement did not return 403"
pass "required scope is enforced"

[ "$(status -H "Authorization: Bearer $READONLY_TOKEN" "$BASE_URL/api/get")" = "200" ] \
  || fail "read scope was rejected on the read route"
pass "granted scope is accepted"

# WSO2 artifact compatibility: APIM api.yaml and a Synapse <api> with an
# in/out/fault sequence. The external path shape is /context/version/resource.
[ "$(status -H "Authorization: Bearer $TOKEN" "$BASE_URL/pizzashack/1.0.0/get")" = "200" ] \
  || fail "APIM API definition route did not return 200"
pass "APIM API definition route works"

[ "$(status -X POST -H "Authorization: Bearer $TOKEN" "$BASE_URL/pizzashack/1.0.0/get")" = "405" ] \
  || fail "APIM operation method allowlist was not enforced"
pass "APIM operation method is enforced"

[ "$(status -X POST -H "Authorization: Bearer $READONLY_TOKEN" "$BASE_URL/pizzashack/1.0.0/post")" = "403" ] \
  || fail "APIM operation scope was not enforced"
pass "APIM operation scope is enforced"

DEMO_BODY=$(curl -sS -H "Authorization: Bearer $TOKEN" "$BASE_URL/demo/1.0.0/items/42")
echo "$DEMO_BODY" | grep -q '"id":"42"' \
  || fail "Synapse payloadFactory did not receive the path parameter"
echo "$DEMO_BODY" | grep -q '"kind":"special"' \
  || fail "Synapse filter did not run the then branch"
echo "$DEMO_BODY" | grep -q '"hit":"answer"' \
  || fail "Synapse switch did not run the matching case"
pass "Synapse property, filter, switch, enrich and payloadFactory work"

DEMO_HEADERS=$(curl -sS -D - -o /dev/null -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/demo/1.0.0/items/42")
echo "$DEMO_HEADERS" | grep -qi '^x-mediated: synapse' \
  || fail "Synapse outSequence header was not applied"
pass "Synapse outSequence is applied"

[ "$(status -H "Authorization: Bearer $TOKEN" "$BASE_URL/demo/1.0.0/anything/foo")" = "200" ] \
  || fail "Synapse call/outSequence route did not return 200"
DEMO_HEADERS=$(curl -sS -D - -o /dev/null -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/demo/1.0.0/anything/foo")
echo "$DEMO_HEADERS" | grep -qi '^x-out-sequence: true' \
  || fail "Synapse outSequence header was not applied after call"
pass "Synapse call, named endpoint and outSequence work"

# Introspection and revocation. Use a throw-away token so the main token can
# still be used by the circuit-breaker test below.
REVOKE_TOKEN=$(curl -sS -X POST "$IS_URL/oauth2/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET" \
  --data-urlencode "scope=write" | extract_token)
[ -n "$REVOKE_TOKEN" ] || fail "token for revocation test was not issued"

INTROSPECTION=$(curl -sS -u gateway-client:"$GATEWAY_SECRET" \
  -d "token=$REVOKE_TOKEN" "$IS_URL/oauth2/introspect")
echo "$INTROSPECTION" | grep -q '"active":true' \
  || fail "introspection did not report an active token"

[ "$(status -H "Authorization: Bearer $REVOKE_TOKEN" "$BASE_URL/admin/get")" = "200" ] \
  || fail "admin route did not accept the active token"

curl -sS -o /dev/null -u "$CLIENT_ID:$CLIENT_SECRET" \
  -d "token=$REVOKE_TOKEN" "$IS_URL/oauth2/revoke"
INTROSPECTION=$(curl -sS -u gateway-client:"$GATEWAY_SECRET" \
  -d "token=$REVOKE_TOKEN" "$IS_URL/oauth2/introspect")
echo "$INTROSPECTION" | grep -q '"active":false' \
  || fail "introspection did not report the revoked token as inactive"
pass "RFC 7662 introspection and RFC 7009 revocation work"

sleep 6
[ "$(status -H "Authorization: Bearer $REVOKE_TOKEN" "$BASE_URL/admin/get")" = "401" ] \
  || fail "gateway did not enforce revocation after the cache window"
pass "gateway enforces revoked jti after the cache window"

[ "$(status "$BASE_URL/api/get")" = "401" ] \
  || fail "missing token did not return 401"
pass "missing token is rejected"

[ "$(status -H "Authorization: Bearer not-a-jwt" "$BASE_URL/api/get")" = "401" ] \
  || fail "invalid token did not return 401"
pass "invalid token is rejected"

[ "$(status -X POST "$BASE_URL/public/get")" = "405" ] \
  || fail "method allowlist did not return 405"
pass "method allowlist is enforced"

[ "$(status "$BASE_URL/public/get")" = "200" ] \
  || fail "anonymous route did not return 200"
pass "anonymous route works"

for _ in $(seq 1 40); do
  code=$(status "$BASE_URL/public/get")
  if [ "$code" = "429" ]; then
    pass "rate limiter returned 429"
    break
  fi
done
[ "${code:-}" = "429" ] || fail "rate limiter never returned 429"
pass "rate limiting is enforced"

for _ in $(seq 1 6); do
  code=$(status -H "Authorization: Bearer $TOKEN" "$BASE_URL/api/status/500")
done
[ "$code" = "503" ] || fail "circuit breaker did not open (last code $code)"
pass "circuit breaker opens on consecutive 5xx"

sleep 11
[ "$(status -H "Authorization: Bearer $TOKEN" "$BASE_URL/api/get")" = "200" ] \
  || fail "circuit breaker did not recover"
pass "circuit breaker recovers after the backoff window"

curl -fsS "$BASE_URL/metrics" | grep -q 'rusto2_http_requests_total' \
  || fail "Prometheus metrics are missing"
pass "metrics endpoint exposes RustO2 counters"

echo "All smoke tests passed."
