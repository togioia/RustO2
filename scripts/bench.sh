#!/usr/bin/env bash
# Throughput benchmark with JWT validation enabled. Run against the bench
# overlay (rate limiting disabled) for the headline numbers:
#
#   docker compose -f docker-compose.yml -f docker-compose.bench.yml up -d --build
#   bash scripts/bench.sh
set -euo pipefail

cd "$(dirname "$0")/.."

BASE_URL="${BASE_URL:-http://localhost:8080}"
IS_URL="${IS_URL:-http://localhost:9000}"
CLIENT_ID="${CLIENT_ID:-demo-client}"
CLIENT_SECRET="${CLIENT_SECRET:-demo-secret}"
DURATION="${DURATION:-15s}"
CONNECTIONS="${CONNECTIONS:-128}"
OHA="${OHA:-oha}"
HTTPBIN_URL="${HTTPBIN_URL:-http://localhost:8081}"

if ! command -v "$OHA" >/dev/null 2>&1; then
  echo "oha not found. Install it with: bash scripts/install-oha.sh" >&2
  exit 1
fi

TOKEN=$(curl -sS -X POST "$IS_URL/oauth2/token" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode "grant_type=client_credentials" \
  --data-urlencode "client_id=$CLIENT_ID" \
  --data-urlencode "client_secret=$CLIENT_SECRET" \
  --data-urlencode "scope=read write" \
  | sed -n 's/.*"access_token":"\([^"]*\)".*/\1/p')

if [ -z "$TOKEN" ]; then
  echo "could not obtain a token from $IS_URL" >&2
  exit 1
fi

echo "Waiting for the gateway..."
for _ in $(seq 1 60); do
  if curl -fsS "$BASE_URL/readyz" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
curl -fsS "$BASE_URL/readyz" >/dev/null || {
  echo "gateway is not ready at $BASE_URL" >&2
  exit 1
}

echo "Warming up for 3s..."
"$OHA" -z 3s -c 32 --no-tui -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/api/get?bench=warmup" >/dev/null

echo "Benchmark: ${CONNECTIONS} connections for ${DURATION} (JWT validation + proxy)"
"$OHA" -z "$DURATION" -c "$CONNECTIONS" --no-tui \
  -H "Authorization: Bearer $TOKEN" \
  "$BASE_URL/api/get?bench=1"

echo
echo "Baseline: direct httpbin, no gateway (same ${CONNECTIONS} connections)"
"$OHA" -z "$DURATION" -c "$CONNECTIONS" --no-tui \
  "$HTTPBIN_URL/get?bench=baseline"

echo
echo "Gateway memory:"
GW_ID=$(docker compose ps -q rusto2-gw)
docker stats --no-stream --format '{{.Name}}: {{.MemUsage}} ({{.MemPerc}})' "$GW_ID"

echo
echo "Identity server memory:"
IS_ID=$(docker compose ps -q rusto2-is)
docker stats --no-stream --format '{{.Name}}: {{.MemUsage}} ({{.MemPerc}})' "$IS_ID"
