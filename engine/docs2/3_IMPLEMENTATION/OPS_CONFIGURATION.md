# Operational Configuration Reference

This document is the canonical source of truth for operator-facing environment
variables in the shipped Flapjack server stack.

- For deployment flow, host setup, and infrastructure runbooks, see
  [DEPLOYMENT.md](./DEPLOYMENT.md).
- For release/rollback discipline and operator triage, see
  [OPERATIONS.md](./OPERATIONS.md).
- For the public hardening baseline, see
  [SECURITY_BASELINE.md](./SECURITY_BASELINE.md).
- For feature-level status and roadmap context, see [FEATURES.md](../FEATURES.md).

## Verified Example Paths

- Single-node smoke + restart persistence: the repo's CLI smoke harness (public entrypoint: `./s/test --e2e`)
- 3-node nginx-routed topology: `engine/examples/ha-cluster/test_ha.sh`
- 2-node replication + analytics fan-out: `engine/examples/replication/test_replication.sh`
- Single-node S3 snapshots (MinIO): `engine/examples/s3-snapshot/test_snapshots.sh`

Not verified by these harnesses:

- Production AWS S3 compatibility beyond the repo's MinIO harness.

Verified separately from the harnesses above:

- Systemd runtime behavior on a real Linux VPS (completed on 2026-03-26; see
  `FEATURES.md` and `engine/examples/systemd/README.md`).

## Scope Notes

- Request correlation (`x-request-id`) is always on and has no env flag.
- Startup dependency summary logging is always on and has no env flag.

## Server

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_DATA_DIR` | Filesystem path | `./data` | Base data directory for indexes and runtime state. |
| `FLAPJACK_BIND_ADDR` | Socket address (`host:port`) | `127.0.0.1:7700` | HTTP bind address for the server process. |
| `FLAPJACK_PORT` | Integer port | unset | CLI-only bind helper; used when `FLAPJACK_BIND_ADDR` is not set. |
| `FLAPJACK_INSTALL` | Filesystem path | `$HOME/.flapjack` | Install root used by `flapjack uninstall`. |
| `FLAPJACK_ENV` | `development` or `production` | `development` | Server environment mode; production enforces stricter auth requirements. |
| `FLAPJACK_GEOIP_DB` | Filesystem path | `${FLAPJACK_DATA_DIR}/GeoLite2-City.mmdb` | Path to GeoIP database file for IP geolocation. |
| `FLAPJACK_SSL_EMAIL` | Email address | unset | Contact email for ACME/Let's Encrypt SSL automation. |
| `FLAPJACK_PUBLIC_IP` | IPv4/IPv6 address | unset | Public IP used for IP-based ACME certificate issuance. |
| `FLAPJACK_ACME_DIRECTORY` | HTTPS URL | `https://acme-v02.api.letsencrypt.org/directory` | ACME directory endpoint. |

## Auth

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_NO_AUTH` | `1` to enable | disabled | Explicit auth opt-out for local/dev bootstrap only; production startup rejects it fail-closed. |
| `FLAPJACK_ADMIN_KEY` | Non-empty string (production requires length `>=16`) | required in production; auto-generated in local dev if missing | Admin API key source for auth bootstrap and rotation. |

## Logging / Observability

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_LOG_FORMAT` | `text` or `json` | `text` | Selects structured JSON logs or human-readable text logs. |
| `RUST_LOG` | `tracing_subscriber` filter expression | `info` | Log level and target filtering. |
| `FLAPJACK_ALLOWED_ORIGINS` | Comma-separated origin URLs | loopback-only browser origins | CORS allowlist. Empty or invalid entries fall back to loopback-only mode (`localhost` / loopback IP origins); non-loopback browser origins require explicit allowlist configuration. |
| `FLAPJACK_SHUTDOWN_TIMEOUT_SECS` | Positive integer seconds | `30` | Graceful shutdown drain timeout. |
| `FLAPJACK_TRUSTED_PROXY_CIDRS` | Comma-separated CIDRs, or `off`/`none` | `127.0.0.0/8,::1/128` | Trusted proxy ranges for forwarded client IP handling. |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | OTLP/gRPC collector endpoint URL | unset | Enables OpenTelemetry trace export when the server is built with `--features otel`. |

## Storage / Snapshots

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_S3_BUCKET` | S3 bucket name | unset | Enables S3 snapshot integration when set. |
| `FLAPJACK_S3_REGION` | AWS region string | `us-east-1` | S3 region for snapshot operations. |
| `FLAPJACK_S3_ENDPOINT` | URL | unset | Optional custom S3-compatible endpoint. |
| `FLAPJACK_SNAPSHOT_INTERVAL` | Integer seconds | `0` | Scheduled S3 snapshot interval; `0` disables. |
| `FLAPJACK_SNAPSHOT_RETENTION` | Integer count | `24` | Number of snapshots retained per index/tenant. |
| `FLAPJACK_OPLOG_RETENTION` | Integer operation count | `1000` | Retention window for committed oplog entries. |

## Replication

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_NODE_ID` | Non-empty string | hostname fallback | Node identity for replication and cluster status. |
| `FLAPJACK_PEERS` | Comma-separated `id=addr` pairs | empty | Peer list for mesh replication. |
| `FLAPJACK_STARTUP_CATCHUP_TIMEOUT_SECS` | Integer seconds | `30` | Startup catch-up timeout before serving. |
| `FLAPJACK_SYNC_INTERVAL_SECS` | Integer seconds | `60` | Periodic replication catch-up interval. |

## Analytics

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_ANALYTICS_ENABLED` | `false`/`0` disables; anything else enables | enabled | Master switch for analytics collection/retention loops. |
| `FLAPJACK_ANALYTICS_DIR` | Filesystem path | `${FLAPJACK_DATA_DIR}/analytics` | Analytics storage directory. |
| `FLAPJACK_ANALYTICS_FLUSH_INTERVAL` | Integer seconds | `60` | Flush interval for analytics writer. |
| `FLAPJACK_ANALYTICS_FLUSH_SIZE` | Integer event count | `10000` | Flush batch size threshold. |
| `FLAPJACK_ANALYTICS_RETENTION_DAYS` | Integer days | `90` | Retention window for analytics data. |
| `FLAPJACK_ROLLUP_INTERVAL_SECS` | Integer seconds | `300` | Cluster rollup broadcast interval when analytics cluster is active. |
| `FLAPJACK_USAGE_ALERT_THRESHOLD_SEARCHES` | Integer count (`0` disables) | `0` | Search-count threshold for alerts. |
| `FLAPJACK_USAGE_ALERT_THRESHOLD_WRITES` | Integer count (`0` disables) | `0` | Write-count threshold for alerts. |
| `FLAPJACK_TRENDING_WINDOW_DAYS` | Positive integer days | `7` | Trending recommendation lookback window. |
| `FLAPJACK_RECOMMEND_MAX_RESULTS` | Integer, clamped `1..30` | `30` | Default recommendation response size. |

## Limits

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_MAX_BODY_MB` | Integer MB | `100` | Global HTTP request body size limit. |
| `FLAPJACK_MAX_BATCH_SIZE` | Integer operation count | `10000` | Max object operations per batch request. |
| `FLAPJACK_MAX_RECORD_BYTES` | Integer bytes | `102400` | Max serialized size per individual record. |
| `FLAPJACK_MAX_BUFFER_MB` | Integer MB | `31` | In-memory write buffer budget. |
| `FLAPJACK_MAX_CONCURRENT_WRITERS` | Integer | `40` | Max concurrent writer tasks per tenant manager. |
| `FLAPJACK_WRITE_QUEUE_BATCH_SIZE` | Positive integer operation count | `32` | Runtime write-queue commit threshold. Invalid values (non-integer or `<=0`) fall back to `32`. Higher values usually improve sustained write throughput by amortizing commit cost, while lower values generally reduce per-op flush latency. |
| `FLAPJACK_MAX_DOC_MB` | Integer MB | `3` | Max document payload size admitted by memory-budget controls. |
| `FLAPJACK_MEMORY_HIGH_WATERMARK` | Integer percent | `80` | Elevated pressure threshold. |
| `FLAPJACK_MEMORY_CRITICAL` | Integer percent | `90` | Critical pressure threshold. |
| `FLAPJACK_MEMORY_LIMIT_MB` | Integer MB | auto-detected | Explicit memory-limit override for pressure calculations. |
| `FLAPJACK_IDEMPOTENCY_PERSISTENT` | Boolean (`1`/`true`/`yes`/`on`) | disabled | Enables node-local SQLite persistence for idempotency replay state at `${FLAPJACK_DATA_DIR}/_idempotency/cache.db`. Canonical flag. |
| `FLAPJACK_IDEMPOTENCY_TTL_SECS` | Integer seconds | `300` | TTL for the per-node `X-Flapjack-Idempotency-Key` response cache. See [`OPERATIONS.md` — Idempotency contract](./OPERATIONS.md#idempotency-contract). Minimum effective value is `1`. |

### Idempotency Restart Durability Proof

- Canonical SQLite path: `${FLAPJACK_DATA_DIR}/_idempotency/cache.db`
- Compatibility alias: `FLAPJACK_IDEMPOTENCY_PERSIST` is still accepted when `FLAPJACK_IDEMPOTENCY_PERSISTENT` is unset; canonical flag takes precedence when both are set.
- TTL behavior: idempotency entries older than `FLAPJACK_IDEMPOTENCY_TTL_SECS` are treated as expired and are trimmed on lookup/store.

Proof command:

```bash
cd engine && cargo test -p flapjack-server --test idempotency_restart_durability_test
```

Persistent-mode probe command:

```bash
cd engine && FLAPJACK_IDEMPOTENCY_PERSISTENT=true cargo test -p flapjack-server --test idempotency_restart_durability_test -- --nocapture
```

Measured baseline from the probe at HEAD (2026-05-31):

- `iterations=300`
- `store_avg_us=2010.69`, `store_p95_us=7845.58`, `store_p99_us=13000.46`
- `lookup_avg_us=63.48`, `lookup_p95_us=236.92`, `lookup_p99_us=752.17`

## Email / Alerts

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_SES_ENABLED` | `1`/`true` enables | disabled | Enables AWS SES notification sender. |
| `FLAPJACK_SES_FROM_EMAIL` | Email address | unset | Required sender address when SES is enabled. |
| `FLAPJACK_SES_ALERT_RECIPIENTS` | Comma-separated email list | unset | Required alert recipients when SES is enabled. |
| `FLAPJACK_SES_COOLDOWN_MINUTES` | Integer minutes | `60` | Cooldown interval between repeated alerts per key. |

## AI

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_AI_BASE_URL` | URL | unset | Fallback AI provider base URL; index-level settings override env. |
| `FLAPJACK_AI_API_KEY` | API key string | unset | Fallback AI provider API key; index-level settings override env. |
| `FLAPJACK_AI_MODEL` | Model name string | `gpt-4o-mini` | Fallback AI model when request/index does not provide one. |
| `FASTEMBED_CACHE_DIR` | Filesystem path | library default cache path | Optional cache directory for local `fastembed` model artifacts. |

## Configuration Recipes

### JSON logging

```bash
FLAPJACK_LOG_FORMAT=json \
RUST_LOG=info \
flapjack-server
```

### CORS lockdown

```bash
FLAPJACK_ALLOWED_ORIGINS=https://app.example.com \
flapjack-server
```

### Shutdown tuning

```bash
FLAPJACK_SHUTDOWN_TIMEOUT_SECS=60 \
flapjack-server
```

### OpenTelemetry export (feature-gated)

```bash
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317 \
cargo run -p flapjack-server --features otel
```

If you run a prebuilt binary, it must be compiled with the `otel` feature for
this env var to take effect.
