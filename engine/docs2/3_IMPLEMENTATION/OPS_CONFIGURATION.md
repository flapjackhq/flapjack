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

## Migration spool payload retention

The shipped MIG-9 owner parses `FLAPJACK_MIGRATION_SPOOL_GC_INTERVAL_SECS`
once at startup as a positive-integer scheduling interval with default `300`.
Absent, invalid, or zero values fall back to `300` to prevent a busy loop.
This variable controls only how often the periodic pass runs.

Payload retention eligibility starts from durable terminal state, not from the
manifest admission timestamp. A terminal disposition is one of `Succeeded`,
`Failed`, or `Cancelled` with `MigrationPhaseRecord::terminal_at` present, and
payloads become reclaimable only when
`now >= terminal_at + SpoolLimits::retention_seconds`. The
`SpoolLimits::retention_seconds` value remains the sole retention duration and
keeps the existing `86,400`-second default. `Running`, a missing `terminal_at`,
a missing or corrupt `migration_phase.json`, or any disposition/timestamp
inconsistency fails closed and is never permission to reclaim payloads.

`expires_at` remains legacy manifest metadata written from admission time. It
must not become a second retention knob, status-duration knob, or scheduler
origin. Existing workflow-local cleanup paths may delete export payloads after
successful activation or cancellation, while failures retain payloads until the
future retention pass becomes eligible; those paths are not the future retention
scheduler and do not define an alternate retention policy.

The payload reclamation transaction removes manifest-listed visible artifacts,
documents/rules/synonyms completion sidecars, and zeroes the manifest payload
accounting while preserving the job directory. It keeps the durable control
files `async_migration.json` and `migration_phase.json`: authenticated status
ownership depends on `async_migration.json`, and the terminal response depends
on `migration_phase.json`. Payload reclamation therefore introduces no
time-based status 404.

`collect_garbage` now performs temp cleanup, retention-based payload
reclamation for eligible terminal jobs, and tombstoning of already-deleted
manifests. `write_tombstone` and `recover` remain part of that manifest
lifecycle. `recover_async_admissions` is separate because it removes only
interrupted async admissions without a committed phase record, not retained
terminal payloads or reclaimed jobs.

## `flapjack ingest` Beta Operations

`flapjack ingest` is a packaged CLI import path for cron, systemd timers, and
one-shot backfills. It does not run an embedded server and does not write index
files directly; every mutation is sent to the authenticated
`POST /1/indexes/{indexName}/batch` endpoint.

Example cron entry:

```cron
15 * * * * /usr/local/bin/flapjack ingest --endpoint http://127.0.0.1:7700 --index products --source /var/lib/flapjack/imports/products.ndjson --api-key-file /etc/flapjack/ingest.key --report-json >>/var/log/flapjack-ingest.log 2>&1
```

Example systemd service:

```ini
[Service]
Type=oneshot
Environment=FLAPJACK_INGEST_API_KEY_FILE=/etc/flapjack/ingest.key
ExecStart=/usr/local/bin/flapjack ingest --endpoint http://127.0.0.1:7700 --index products --source /var/lib/flapjack/imports/products.ndjson --api-key-file ${FLAPJACK_INGEST_API_KEY_FILE} --report-json
```

Operational bounds:

- Credential input is explicit: use exactly one of `--api-key-env`,
  `--api-key-file`, or `--api-key-stdin`. There is no `--api-key` flag.
- `--batch-size` bounds parser batches and the JSON report's
  `queue_high_watermark` shows the largest queued envelope size observed.
- The retry policy is intentionally small: transport failures and HTTP `429` or
  `503` are retried with a capped `Retry-After`; other `4xx` responses are
  permanent failures.
- Reports distinguish `confirmed_committed` from `outcome_unknown`. If
  `outcome_unknown` is non-zero, inspect/search the destination and rerun the
  same source with stable `objectID` values. Do not count unknown records as
  committed.
- Upsert mode preserves target-only records. Source omissions are not delete
  propagation; send explicit `_action:"delete"` or `_action:"deleteObject"`
  records to remove objects.
- `--mode replace` is currently a typed zero-mutation refusal with
  `failure_classification:"replace_not_supported"`.

## `flapjack migrate`

`flapjack migrate` copies an Algolia, Meilisearch, or Typesense index into an
existing Flapjack server. It is a pure authenticated HTTP client: it submits
`POST /1/migrations/{provider}`, polls the returned durable job to a terminal
state, and never starts a server or binds a listener.

Set the two credential environment variables in the calling environment, then
submit:

```bash
flapjack migrate \
  --endpoint http://127.0.0.1:7700 \
  --application-id acme-store \
  --api-key-env FLAPJACK_ADMIN_KEY \
  --source-provider meilisearch \
  --source-endpoint https://tenant.meilisearch.io \
  --source-key-env SOURCE_PROVIDER_ADMIN_KEY \
  --source-index products \
  --target-index products_v2 \
  --overwrite \
  --poll-interval 500ms \
  --timeout 1h \
  --json
```

The bare `flapjack migrate` invocation is both submit and monitor: after
admission it polls until the job succeeds, fails, is cancelled, or reaches the
client timeout. There is no `status` subcommand. For out-of-band monitoring,
send an authenticated `GET /1/migrations/{provider}/{job_id}` request to the
same Flapjack server, using the provider selected when the job was submitted.

Cancellation is cooperative. A successful cancel request prints the
server-returned status:

```bash
flapjack migrate \
  --endpoint http://127.0.0.1:7700 \
  --application-id acme-store \
  --api-key-env FLAPJACK_ADMIN_KEY \
  --source-provider meilisearch \
  cancel --job-id 01890f8e-8b28-78e8-b542-8cfdcb2d4f24
```

After a job is terminal, acknowledge it with:

```bash
flapjack migrate \
  --endpoint http://127.0.0.1:7700 \
  --application-id acme-store \
  --api-key-env FLAPJACK_ADMIN_KEY \
  --source-provider meilisearch \
  ack --job-id 01890f8e-8b28-78e8-b542-8cfdcb2d4f24
```

### Credentials

The Flapjack owner key must have the `admin` ACL. Supply it on submit, cancel,
and ack through exactly one of `--api-key-env`, `--api-key-file`, or
`--api-key-stdin`. There is deliberately no flag that accepts the key value:
command-line arguments can be visible to other users of the host.

For any non-loopback Flapjack endpoint, use `https://`. The CLI rejects
cleartext remote `http://` endpoints so the Flapjack owner key and source
provider key are not sent without TLS. Plain HTTP is accepted only for
`localhost` and loopback IPs such as `127.0.0.1` during local development.

Submission also requires the source-provider key through exactly one of
`--source-key-env`, `--source-key-file`, or `--source-key-stdin`. The shipped
`--algolia-key-env`, `--algolia-key-file`, and `--algolia-key-stdin` names are
aliases for those canonical flags. Source-key flags are submit-only. The CLI
rejects all submit-only flags on `cancel` and `ack`, and rejects a submit that
combines `--api-key-stdin` with `--source-key-stdin` (including its Algolia
alias) because both would consume the same stream.

For Algolia specifically, the Admin API key is the simple supported choice;
keep it confidential. For a least-privilege Algolia source key, match the
permissions to the requests the importer actually issues:

| Importer request | When issued | Required Algolia ACL |
|---|---|---|
| `GET /1/indexes` | Always, including source-quiescence checks | [`listIndexes`](https://www.algolia.com/doc/rest-api/search/list-indices) |
| `GET /1/indexes/{index}/settings` | Always for the source, and for referenced replicas | [`settings`](https://www.algolia.com/doc/rest-api/search/get-settings) |
| `POST /1/indexes/{index}/browse` | Always to export records | [`browse`](https://www.algolia.com/doc/rest-api/search/browse) |
| `POST /1/indexes/{index}/synonyms/search` | Always to export synonyms | [`settings`](https://www.algolia.com/doc/rest-api/search/search-synonyms) |
| `POST /1/indexes/{index}/rules/search` | Always to export rules | [`settings`](https://www.algolia.com/doc/rest-api/search/search-rules) |
| `GET /1/keys/{key}` | Only when source settings contain non-empty `unretrievableAttributes` | [`search`](https://www.algolia.com/doc/rest-api/search/get-api-key) |

Therefore `listIndexes`, `settings`, and `browse` are always required. If the
source has `unretrievableAttributes`, the key also needs `search` so the
importer can inspect that key's permissions, plus
[`seeUnretrievableAttributes`](https://www.algolia.com/doc/guides/security/api-keys)
so browse responses can include those attributes. The migration fails rather
than silently omitting protected attributes.

### Application IDs and job ownership

The similarly named ID flags belong to different systems:

| Flag | Identity | Where it is sent | Validation/default |
|---|---|---|---|
| `--application-id` | Flapjack tenant and job owner namespace | `x-algolia-application-id` request header | Defaults to `flapjack` |
| `--app-id` | Source Algolia application | `appId` submission body field | Algolia submissions only; required and restricted to ASCII letters and digits |

Job ownership combines the Flapjack application ID with a SHA-256 digest of the
submitting Flapjack key. Use the same `--application-id` and the same Flapjack
key when polling, cancelling, acknowledging, or sending the out-of-band status
GET. A different Flapjack key yields HTTP 404, not 403; this intentionally does
not reveal that another owner has the job.

### Output and exit status

Without `--json`, a status is one space-delimited line. The first three fields
are always `job_id`, `phase`, and `disposition`; available target, topology,
settings, and import counts follow. Each warning is printed on its own
`warning=<json-value>` line. For example:

```text
job_id=01890f8e-8b28-78e8-b542-8cfdcb2d4f24 phase=activating disposition=succeeded target_index=products_v2 topology=single_node_only settings_applied=true objects_imported=3 synonyms_imported=2 rules_imported=1
```

With `--json`, the CLI prints the server status object in camelCase. Optional
progress, timestamp, count, and warning fields appear only when supplied by the
server:

```json
{"jobId":"01890f8e-8b28-78e8-b542-8cfdcb2d4f24","phase":"activating","disposition":"succeeded","targetIndex":"products_v2","topology":"single_node_only","exportProgress":{"completed":10,"total":10},"createdAt":"2026-07-29T16:00:00Z","updatedAt":"2026-07-29T16:00:03Z","terminalAt":"2026-07-29T16:00:03Z","settingsApplied":true,"objectsImported":{"imported":3},"synonymsImported":{"imported":2},"rulesImported":{"imported":1}}
```

Successful acknowledgement output is exactly
`job_id=<job_id> acknowledged=true`, or with `--json`:

```json
{"jobId":"01890f8e-8b28-78e8-b542-8cfdcb2d4f24","acknowledged":true}
```

JSON failures without a status use
`{"errorType":"<label>","message":"<message>","exitCode":<code>}`. If a status
is available, those three failure fields are added to the status object.

| Exit code | `errorType` label | Operator meaning |
|---:|---|---|
| 2 | `config` | Invalid arguments, credential source, endpoint, or local configuration |
| 3 | `http_rejection` | Transport failure, rejected HTTP response, or incompatible server response |
| 4 | `timeout` | A request timed out or the polling deadline expired |
| 5 | `failed_job` | The job reached the `failed` disposition |
| 6 | `cancelled_job` | The monitored job reached the `cancelled` disposition |
| 7 | `cancel_too_late` | Cancellation reached the server after its commit boundary |
| 8 | `migration_ack_too_early` | Acknowledgement was attempted before the job became terminal |

A successful terminal migration or acknowledgement exits 0. Every
non-success terminal state exits nonzero.

Resume is not available. An interrupted migration must be run again from the
start; the CLI exposes no resume flag.

Operational bounds:

- `--source-provider <algolia|meilisearch|typesense>` selects the route and
  payload shape and defaults to `algolia`. An Algolia submission requires
  `--app-id` and rejects `--source-endpoint`; a Meilisearch submission requires
  `--source-endpoint` and sends it as `endpoint`; a Typesense submission
  requires `--source-endpoint` and sends it as `node`. Meilisearch and
  Typesense reject `--app-id`.
- `--endpoint` is required and must be an absolute HTTP or HTTPS URL. Remote
  endpoints must use HTTPS; plain HTTP is accepted only for `localhost` and
  loopback IPs. `--poll-interval` accepts a positive whole duration with an
  `ms`, `s`, `m`, or `h` suffix, defaults to `250ms`, and is capped at 60
  seconds. `--timeout` uses the same syntax, defaults to `1h`, and is capped at
  24 hours.
- `--target-index` defaults to `--source-index`.
- Without `--overwrite`, publication is create-only. Work can be exported and
  staged, but publication fails if the target already exists.
- With `--overwrite`, publication uses replacement semantics and captures the
  target's staging baseline. The target does not have to exist: a missing
  target has a compatibility baseline sequence of zero. Durable async
  migrations accept `--overwrite` through this same admission path.
- A node with configured replication peers refuses migration imports with HTTP
  503 and code `migration_ha_unsupported`.

## Server

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_DATA_DIR` | Filesystem path | `./data` | Base data directory for indexes and runtime state. |
| `FLAPJACK_BIND_ADDR` | Socket address (`host:port`) | `127.0.0.1:7700` | HTTP bind address for the server process. |
| `FLAPJACK_PORT` | Integer port | unset | CLI-only bind helper; used when `FLAPJACK_BIND_ADDR` is not set. |
| `FLAPJACK_INSTALL` | Filesystem path | `$HOME/.flapjack` | Install root used by `flapjack uninstall`. |
| `FLAPJACK_ENV` | `development` or `production` | `development` | Server environment mode; production enforces stricter auth requirements. |
| `FLAPJACK_CONTENT_SECURITY_POLICY` | HTTP `Content-Security-Policy` header value | `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'` | Overrides the global response CSP for API, dashboard, and Swagger surfaces. Unset, empty, or invalid HTTP header values fail closed to the strict default. |
| `FLAPJACK_REQUEST_TIMEOUT_SECS` | Positive integer seconds | `300` | Bounds admitted request execution. Unset, empty, invalid, or non-positive values fall back to `300`. The clock starts only after global concurrency admission, so queue wait is not bounded. |
| `FLAPJACK_MAX_CONCURRENT_REQUESTS` | Positive integer request count | `1024` | Sets one global admitted-request cap shared across routes. Unset, empty, invalid, or non-positive values fall back to `1024`; excess requests queue in `poll_ready` rather than being shed. |
| `FLAPJACK_GEOIP_DB` | Filesystem path | `${FLAPJACK_DATA_DIR}/GeoLite2-City.mmdb` | Path to GeoIP database file for IP geolocation. |
| `--ssl-cert-path` / `FLAPJACK_SSL_CERT_PATH` | Filesystem path to PEM certificate chain | unset | Optional in-binary TLS certificate chain. Must be configured together with `--ssl-key-path` / `FLAPJACK_SSL_KEY_PATH`; when both are unset the server binds plaintext HTTP. The file is loaded once at startup, so replacing it requires a server restart. Unreadable, empty, malformed, or key-mismatched material fails startup before serving. |
| `--ssl-key-path` / `FLAPJACK_SSL_KEY_PATH` | Filesystem path to PEM private key | unset | Optional in-binary TLS private key matching the configured certificate chain. Must be configured together with `--ssl-cert-path` / `FLAPJACK_SSL_CERT_PATH`; when both are set the startup banner reports `https`. The key file may contain one supported PEM private key and is loaded only at startup. |
| `FLAPJACK_SSL_EMAIL` | Email address | unset | Contact email for ACME/Let's Encrypt SSL automation. |
| `FLAPJACK_PUBLIC_IP` | IPv4/IPv6 address | unset | Public IP used for IP-based ACME certificate issuance. |
| `FLAPJACK_SSL_DOMAIN` | DNS name | unset | Domain used for DNS-based ACME issuance when `FLAPJACK_PUBLIC_IP` is unset. Values are trimmed and validated during configuration loading. |
| `FLAPJACK_ACME_DIRECTORY` | HTTPS URL | `https://acme-v02.api.letsencrypt.org/directory` | ACME directory endpoint. |
| `FLAPJACK_ACME_MATERIAL_DIR` | Filesystem path | `${FLAPJACK_DATA_DIR}/ssl/acme` | Directory where ACME automation publishes the currently visible `fullchain.pem` and owner-private `privkey.pem` pair. The service also requires write access to its parent directory, where owner-scoped atomic generations are staged and retained; configure both paths on the same filesystem. Platforms without Unix-style atomic symlink replacement support first publication but reject replacement of existing material before mutation. |
| `FLAPJACK_ACME_ROOT_CA_PEM` | Filesystem path to PEM CA certificate | unset | Optional ACME trust-anchor override, read during SSL manager initialization. Missing or unreadable PEM fails initialization before account creation. |

Production recommendation: keep terminating TLS at a reverse proxy or load
balancer unless you have a specific reason to run the optional static-file
in-binary listener. ACME automation configured by `FLAPJACK_SSL_EMAIL`,
`FLAPJACK_PUBLIC_IP` or `FLAPJACK_SSL_DOMAIN`, and `FLAPJACK_ACME_DIRECTORY`
publishes `fullchain.pem` plus owner-private `privkey.pem` under
`FLAPJACK_ACME_MATERIAL_DIR`. The static in-binary TLS listener loads material
once at startup, so replacing the ACME-published pair still requires a server
restart until served rotation is wired separately. Any ACME-backed in-binary
design must keep HTTP-01 reachable over plaintext port 80 while the HTTPS
listener serves TLS.

## Auth

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_NO_AUTH` | `1` to enable | disabled | Explicit auth opt-out for local/dev bootstrap only; production startup rejects it fail-closed. |
| `FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND` | `1` to enable | disabled | Development-only override that permits `FLAPJACK_NO_AUTH=1` with a resolved non-loopback IP or hostname bind address. Production still rejects no-auth startup. |
| `FLAPJACK_ADMIN_KEY` | Non-empty string (production requires length `>=16`) | required in production; auto-generated in local dev if missing | Admin API key source for auth bootstrap and rotation. |
| `FLAPJACK_DISABLE_DASHBOARD` | `1` to enable | disabled | Removes unauthenticated dashboard, Swagger UI, and OpenAPI JSON exposure by not mounting `/dashboard`, `/swagger-ui`, or `/api-docs` routes. |

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
| `FLAPJACK_SNAPSHOT_KEY_FILE` | Filesystem path to a file containing exactly 64 hex characters or 32 raw bytes, with at most one trailing newline | unset (plaintext snapshot bytes) | Enables optional AES-256-GCM-SIV encryption of portable snapshot bytes (envelope `FJSNAPE1`). **Every node that produces, consumes, replicates, or auto-restores snapshot bytes — including S3 auto-restore — must use the same key file**, or an encrypted producer feeding an unkeyed consumer fails with an error naming this env var. On Unix, any group/other permission bit (`mode & 0o077 != 0`) emits a warning; Flapjack does not `chmod` the file, because read-only secret mounts are valid. |

## Replication

| Name | Type / Values | Default | Description |
|---|---|---|---|
| `FLAPJACK_NODE_ID` | Non-empty string | hostname fallback | Node identity for replication and cluster status. |
| `FLAPJACK_ADVERTISE_ADDR` | HTTP(S) origin | unset | Address this node publishes to peers. A fresh seed node with an advertised address starts replication even when its peer list is empty. |
| `FLAPJACK_PEERS` | Comma-separated `id=addr` pairs | empty | Static full membership for mesh replication. Use this when the complete peer set is known at startup; it takes precedence over bootstrap join. |
| `FLAPJACK_BOOTSTRAP_PEER` | HTTP(S) origin | unset | Single running member used by a fresh node to join an HA cluster when no static peer list is configured. |
| `FLAPJACK_REPLICATION_API_KEY` | Non-empty string | unset | Outbound peer credential for configured replication. Trimmed whitespace-only values are treated as unset; the value is never auto-generated, is not stored in `KeyStore`, and is never returned by `/1/keys`. Startup validation is owned by `engine/flapjack-http/src/startup.rs::validate_replication_peer_credential`; transport validation is owned by `engine/flapjack-replication/src/config.rs::NodeConfig::validate_peer_transport`. See [Replication peer credential](#replication-peer-credential). |
| `FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS` | `1` only | unset / off | Temporary rolling-upgrade escape hatch. When unset or set to anything except `1`, any configured replication intent without `FLAPJACK_REPLICATION_API_KEY` fails startup. Owned by `engine/flapjack-http/src/startup.rs::validate_replication_peer_credential`. |
| `FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS` | `1` only | unset / off | Temporary cleartext transport escape hatch. When unset or set to anything except `1`, `http://` peer origins are refused whenever an outbound credential could be attached. Server startup fails instead of dropping a rejected static, persisted, or bootstrap peer and serving with reduced topology. This includes caller-supplied API keys forwarded by analytics queries even when server authentication is disabled, plus the admin credential used for bootstrap membership. Owned by `engine/flapjack-replication/src/config.rs::NodeConfig::{load_for_server_startup,validate_peer_transport,validate_credentialed_peer_transport}` and `engine/flapjack-http/src/analytics_cluster.rs::validate_authenticated_query_peer_transport`, including static peers, persisted `node.json`, bootstrap peers, and runtime `POST /internal/cluster/peers`. |
| `FLAPJACK_STARTUP_CATCHUP_TIMEOUT_SECS` | Integer seconds | `30` | Startup catch-up timeout before serving. |
| `FLAPJACK_SYNC_INTERVAL_SECS` | Integer seconds | `60` | Periodic replication catch-up interval. |
| `FLAPJACK_AUTOHEAL_ENABLED` | `true` or `false` | `false` | Enables quorum-preserving auto-heal eviction from the replication health-probe loop after three consecutive unreachable observations. Values are trimmed and matched ASCII-case-insensitively; invalid values warn and behave as `false`. See [Dead-node auto-heal](./OPERATIONS.md#scenario-dead-node-auto-heal). |

Topology source precedence is owned by `NodeConfig::load_or_default`. An
existing `${FLAPJACK_DATA_DIR}/node.json` wins over topology environment
variables. Without `node.json`, `FLAPJACK_PEERS` supplies static full
membership; `FLAPJACK_BOOTSTRAP_PEER` is considered only for a fresh node with
no static peer list.

Replication addresses are normalized by `NodeConfig::normalize_peer_addr`.
`FLAPJACK_BOOTSTRAP_PEER`, `FLAPJACK_ADVERTISE_ADDR`, and peer addresses must be
safe HTTP(S) origins; unsafe loopback, wildcard, metadata, non-HTTP(S), or
non-origin values are rejected by that owner.

Bootstrap join is fail-loud and requires admin auth. `server_init::bootstrap_join_with_client`
registers the joining node with a running replication-enabled member via the
admin-only `/internal/cluster/peers` mutation, fetches cluster status, persists
the learned membership to `node.json`, and fails startup rather than serving as
a silent single-node fallback when auth, registration, status, or
advertised-origin resolution fails.

### Replication peer credential

Configured replication now requires an outbound peer identity by default. The
configured topology sources are `node.json`, `FLAPJACK_PEERS`,
`FLAPJACK_BOOTSTRAP_PEER`, `FLAPJACK_ADVERTISE_ADDR`, and runtime
`POST /internal/cluster/peers`; they must not be treated as unchanged when
`FLAPJACK_REPLICATION_API_KEY` is absent. Startup refuses static, persisted,
bootstrap, and advertised-address replication intent unless the peer key is set
or `FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS=1` is present for a
temporary rolling upgrade.

The startup refusal text is:

```text
replication is configured (<intent>) but FLAPJACK_REPLICATION_API_KEY is unset; set FLAPJACK_REPLICATION_API_KEY so this node presents a peer identity on outbound replication and analytics rollup traffic, or set FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS=1 to start unauthenticated
```

When the peer credential is set, startup trims it and rejects values that
cannot be encoded as an HTTP header. Outbound replication and analytics rollup
fan-out use the accepted value with
`x-algolia-application-id: flapjack-replication`.
`engine/flapjack-http/src/analytics_cluster.rs::AnalyticsClusterClient::query_peers`
forwards inbound credentials, while
`engine/flapjack-http/src/analytics_cluster.rs::AnalyticsClusterClient::push_rollup_to_peers`
uses the configured outbound peer credential for background rollup pushes.

Credentialed `http://` peer origins are refused by default because the
credential would be sent in plaintext. A rejected startup peer fails
configuration loading; it is never silently removed in a way that lets the
node serve standalone or as an empty seed. Analytics query fan-out forwards any
caller-supplied API key even when server authentication is disabled, so every
`http://` peer origin is refused if the dedicated peer credential is disabled.
Bootstrap join also uses the admin API key for its admin-only membership mutation, so an
`http://` bootstrap origin is refused when auth is enabled, even if
`FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS=1` disables the dedicated
peer credential. Re-permitting either path requires the separate, explicit
`FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1` override. This policy is shared by static
`FLAPJACK_PEERS`, persisted `node.json` peers, `FLAPJACK_BOOTSTRAP_PEER`, and
runtime `POST /internal/cluster/peers`.

The cleartext refusal text is:

```text
Refusing replication peer <node_id> at <addr>: FLAPJACK_REPLICATION_API_KEY is set and the peer origin is cleartext http://, which would send the peer credential in plaintext. Move the peer to https://, or set FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1 to keep the cleartext peer.
```

`https://` peer origins were already supported before this lane:
`engine/flapjack-replication/src/config.rs::NodeConfig::normalize_peer_addr`
accepts HTTP(S) origins, config tests construct
`https://peer-a.example.com:7700`, and the replication crate builds `reqwest`
with `rustls-tls`. This lane changes default refusal of credentialed
cleartext, not TLS feature availability.

The peer-allowed internal route set is owned by
`engine/flapjack-http/src/auth/route_acl.rs::required_acl_for_route`: `GET
/internal/status`, `GET /internal/cluster/status`, `GET
/internal/snapshots/capability`, `GET /internal/ops`, `GET /internal/tenants`,
`GET /internal/snapshot/:tenant_id`, `POST /internal/replicate`, and `POST
/internal/analytics-rollup`. Everything else under `/internal/` is admin-only,
including `POST /internal/cluster/peers`, `DELETE
/internal/cluster/peers/:node_id`, `POST /internal/rotate-admin-key`, and any
unmapped `/internal/*`.

The admin key remains accepted on peer-allowed routes unconditionally, as owned
by `engine/flapjack-http/src/auth/middleware.rs`. URL-query credentials are
refused for both internal tiers.

Rollout order:

1. Generate a peer secret, set `FLAPJACK_REPLICATION_API_KEY` to the same value
   on every node before or during the binary rollout, and keep
   `FLAPJACK_ADMIN_KEY` configured.
2. Prefer `https://` peer origins. Use
   `FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS=1` or
   `FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1` only as temporary, logged
   compatibility overrides, then remove them after every node converges.
3. Verify with `engine/tests/replication_peer_auth_http_probe.sh`.
4. After every node is confirmed on the peer secret, remove both temporary
   overrides. Outbound replication never substitutes `FLAPJACK_ADMIN_KEY` for
   a missing peer key; the unauthenticated override therefore sends no identity
   until `FLAPJACK_REPLICATION_API_KEY` is configured.

Scope boundaries: the public `/1/keys` ACL vocabulary is unchanged and
Algolia-compatible, with no `"replication"` ACL. Bootstrap join via
`FLAPJACK_BOOTSTRAP_PEER` remains admin-only, runtime add/remove membership and
admin-key rotation remain admin-only, and the peer credential cannot join a
cluster or authorize `POST /internal/cluster/peers`, `DELETE
/internal/cluster/peers/:node_id`, or `POST /internal/rotate-admin-key`.

Runtime membership is restart-durable through the existing `node.json` owner.
`ReplicationManager::{add_peer,remove_peer,replace_peers}` persist membership
mutations, and a restarted node reloads peers from `node.json` without requiring
`FLAPJACK_PEERS` or `FLAPJACK_BOOTSTRAP_PEER`.

Auto-heal is disabled unless `FLAPJACK_AUTOHEAL_ENABLED=true` is set at startup.
When enabled, the existing replication health-probe supervisor observes one
membership-scoped pass at a time and evicts at most one peer after the fixed
three-observation sustained-unreachable threshold, only when the Stage 1 quorum
rule remains satisfied with `N = peer_count_at_observation_start + 1`. A
two-node cluster therefore refuses to evict its only peer, and simultaneous peer
loss that may indicate local isolation is recorded as an indeterminate refusal
rather than a removal. Operators should inspect `/internal/cluster/status`
`autoheal_enabled` and `autoheal_peers` for live lifecycle state; the day-2
procedure is owned by [Dead-node auto-heal](./OPERATIONS.md#scenario-dead-node-auto-heal).

Auto-heal persists readable decision state at
`${FLAPJACK_DATA_DIR}/autoheal_decisions.jsonl`. The journal records decision
IDs, timestamps, membership snapshots, candidates, exact refusal or eviction
decisions, and action intent/outcome records. It does not persist observation
counters; every process restart begins a fresh observation window. A restart
after a synced eviction intent but before a synced outcome records
`outcome_unknown` and waits for fresh probe evidence instead of retrying the
ambiguous action. Healthy returning auto-heal candidates are retained in the
journal and can be readmitted by the survivor-side health probe through the
existing `ReplicationManager::add_peer` path; startup catch-up still runs before
authoritative reads on the returning node.

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
| `FLAPJACK_MAX_BATCH_SIZE` | Integer operation count | `10000` | Max object operations per batch request. Exceeding this limit returns HTTP 413. |
| `FLAPJACK_MAX_RECORD_BYTES` | Integer bytes | `102400` | Max serialized size per individual record. Exceeding this limit returns HTTP 413. |
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
