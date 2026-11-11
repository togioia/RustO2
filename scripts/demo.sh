#!/usr/bin/env bash
# One-click demo: token issuance, JWKS discovery, protected route, scope
# enforcement and rate limiting.
set -euo pipefail

cd "$(dirname "$0")/.."

BASE_URL="${BASE_URL:-http://localhost:8080}"
IS_URL="${IS_URL:-http://localhost:9000}"
CLIENT_ID="${CLIENT_ID:-demo-client}"
CLIENT_SECRET="${CLIENT_SECRET:-demo-secret}"
READONLY_ID="${READONLY_ID:-readonly-client}"
READONLY_SECRET="${READONLY_SECRET:-readonly-secret}"
GATEWAY_SECRET="${GATEWAY_SECRET:-gateway-secret}"

bold() { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
ok() { printf '\033[1;32m%s\033[0m\n' "$*"; }

extract_token() {
  sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p'
}

wait_for() {
  local url="$1" name="$2"
  for _ in $(seq 1 60); do
    if curl -fsS "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "timed out waiting for $name at $url" >&2
  exit 1
}

bold "Waiting for RustO2"
wait_for "$IS_URL/healthz" "rusto2-is"
wait_for "$BASE_URL/readyz" "rusto2-gw"
ok "stack is ready"

bold "POST /oauth2/token (client_credentials)"
TOKEN_RESPONSE=$(curl -sS -X POST "$IS_URL/oauth2/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET" \
  --data-urlencode "scope=read write")
TOKEN=$(printf '%s' "$TOKEN_RESPONSE" | extract_token)
if [ -z "$TOKEN" ]; then
  echo "token endpoint returned: $TOKEN_RESPONSE" >&2
  exit 1
fi
printf 'access_token: %s...\n' "${TOKEN:0:48}"

bold "GET /.well-known/jwks.json"
curl -sS "$IS_URL/.well-known/jwks.json" | head -c 400
printf '\n'

bold "GET /api/get with a valid JWT"
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' \
  -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/api/get?demo=protected"

bold "GET /public/get without a token"
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' \
  "$BASE_URL/public/get?demo=anonymous"

bold "GET /api/get without a token (expect 401)"
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' "$BASE_URL/api/get"

bold "Scope enforcement: readonly token on /admin (expect 403)"
READONLY_TOKEN=$(curl -sS -X POST "$IS_URL/oauth2/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$READONLY_ID" \
  --data-urlencode "client_secret=$READONLY_SECRET" \
  | extract_token)
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' \
  -H "Authorization: Bearer $READONLY_TOKEN" \
  "$BASE_URL/admin/get"

bold "WSO2 APIM artifact: GET /pizzashack/1.0.0/get"
curl -sS -o /dev/null -w 'HTTP %{http_code}\n' \
  -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/pizzashack/1.0.0/get"

bold "WSO2 Synapse mediation: GET /demo/1.0.0/items/42"
curl -sS -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/demo/1.0.0/items/42"
printf '\n'

bold "WSO2 Synapse call + outSequence: GET /demo/1.0.0/anything/foo"
curl -sS -D - -o /dev/null -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/demo/1.0.0/anything/foo" | grep -iE '^(HTTP/|x-out-sequence:)'

bold "Rate limiting: 40 rapid calls on /public (limit is 10 rps, burst 10)"
blocked=0
for _ in $(seq 1 40); do
  status=$(curl -sS -o /dev/null -w '%{http_code}' "$BASE_URL/public/get")
  if [ "$status" = "429" ]; then
    blocked=$((blocked + 1))
  fi
done
printf 'requests blocked with 429: %s/40\n' "$blocked"

bold "Revocation: issue, introspect, revoke, introspect"
TMP_TOKEN=$(curl -sS -X POST "$IS_URL/oauth2/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET" \
  --data-urlencode "scope=write" | extract_token)
curl -sS -u gateway-client:"$GATEWAY_SECRET" \
  -d "token=$TMP_TOKEN" "$IS_URL/oauth2/introspect" | head -c 220
printf '\n'
curl -sS -o /dev/null -w 'revoke HTTP %{http_code}\n' \
  -u "$CLIENT_ID:$CLIENT_SECRET" -d "token=$TMP_TOKEN" "$IS_URL/oauth2/revoke"
curl -sS -u gateway-client:"$GATEWAY_SECRET" \
  -d "token=$TMP_TOKEN" "$IS_URL/oauth2/introspect"
printf '\n'

bold "Metrics snapshot"
curl -sS "$BASE_URL/metrics" | grep -E '^rusto2_(http_requests|auth_failures|rate_limited|circuit_open)' | head -n 12 || true

ok "Metrics snapshot above. Demo completed."
printf '\nGrafana: http://localhost:3000 (admin/admin) - Prometheus: http://localhost:9090\n'
