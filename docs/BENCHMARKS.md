# RustO2 benchmark log

These are the raw measurements summarised in the main README. They are not a
vendor benchmark: the client, the gateway, the identity server and the backend
shared the same 8-vCPU WSL2 host.

## Environment

| Item | Value |
|---|---|
| Host | Windows 11 + WSL2 Ubuntu 22.04.5 LTS |
| CPU / RAM | 8 vCPU / 16 GB |
| Rust | 1.99.0 |
| Build | static musl binaries from the distroless images |
| Gateway | `rusto2-gw` with JWT validation, JWKS cache, metrics, mimalloc |
| Identity | `rusto2-is` with Ed25519 signing |
| Backend | `go-httpbin 2.26.0` |
| Load generator | `oha 1.16.0` |
| Protocol | HTTP/1.1 keep-alive |

The gateway was started with rate limiting disabled for throughput runs. The
default Compose stack uses Redis for distributed quotas; the benchmark overlay
pins the in-memory backend to isolate the proxy.

```bash
RUSTO2_RATE_LIMIT_RPS=0 RUST_LOG=warn \
MIMALLOC_PURGE_DELAY=0 MIMALLOC_ARENA_EAGER_COMMIT=0 \
./rusto2-gw
```

## 128 connections, 15 seconds, every request carries an Ed25519 JWT

```text
Summary:
  Success rate: 100.00%
  Total:        15008.2043 ms
  Average:      9.9234 ms
  Requests/sec: 12885.3523

Response time distribution:
  50.00% in 8.6164 ms
  99.00% in 29.0799 ms

Status code distribution:
  [200] 193268 responses
```

## 64 connections, 15 seconds

```text
Requests/sec: 12085.8799
  50.00% in 4.5852 ms
  99.00% in 15.3948 ms
  [200] 181287 responses
```

## Baseline: direct httpbin, 128 connections, 15 seconds

```text
Requests/sec: 30964.6939
  50.00% in 1.2405 ms
  99.00% in 31.1747 ms
  [200] 464593 responses
```

The baseline p99 is inflated by CPU contention on the shared host; the p50 is
the more useful comparison for the proxy overhead. Repeated runs of the gateway
benchmark ranged from about 8.9k to 12.9k req/s on this WSL2 host, so treat the
absolute throughput as indicative.

## Rate limiting active: 100 rps/client, 64 connections, 8 seconds

```text
Requests/sec: 33245.3582
  50.00% in 1.6331 ms
  99.00% in 6.2636 ms

Status code distribution:
  [429] 264885 responses
  [200] 1160 responses
```

The backend never received the 264,885 rejected requests. The limit headers on
an admitted response were:

```http
HTTP/1.1 200 OK
x-ratelimit-limit: 200
x-ratelimit-remaining: 57
```

`x-ratelimit-limit` is the token bucket capacity (`burst`); the sustained refill
rate is `rps`.

## Memory

`ps -o rss` after the throughput run:

```text
rusto2-gw   18,296 KiB
rusto2-is    4,284 KiB
go-httpbin  19,232 KiB
```

Idle RSS before the run: gateway 5,472 KiB, identity 4,216 KiB.

## Image size

```text
rusto2-gw:dev   8.75 MB
rusto2-is:dev   6.32 MB
```

Both images are based on `gcr.io/distroless/static-debian12:nonroot` and contain
a single statically linked musl binary.
