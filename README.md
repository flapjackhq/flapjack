# Flapjack

Self-hosted search engine with an Algolia-compatible API. Single binary, no dependencies.

[![CI](https://github.com/flapjackhq/flapjack/actions/workflows/ci.yml/badge.svg)](https://github.com/flapjackhq/flapjack/actions/workflows/ci.yml)
[![Release](https://github.com/flapjackhq/flapjack/actions/workflows/release.yml/badge.svg)](https://github.com/flapjackhq/flapjack/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Status: Beta](https://img.shields.io/badge/Status-Beta-orange)](https://github.com/flapjackhq/flapjack)

Drop-in replacement for [Algolia](https://algolia.com) — works with [InstantSearch.js](https://github.com/algolia/instantsearch) and the [algoliasearch](https://github.com/algolia/algoliasearch-client-javascript) client. Typo-tolerant full-text search with faceting, geo search, and custom ranking. Single static binary, runs anywhere, data stays on disk.

**[Live Demo](https://flapjack-demo.pages.dev)** · **[Geo Demo](https://flapjack-demo.pages.dev/geo)** · **[API Docs](https://flapjack-demo.pages.dev/api-docs)**

---

## Install

```bash
curl -fsSL https://install.flapjack.foo | sh
```

Or grab a binary from [Releases](https://github.com/flapjackhq/flapjack/releases/latest).

<details>
<summary>More install options</summary>

```bash
# Specific version
curl -fsSL https://install.flapjack.foo | sh -s -- v0.2.0

# Custom install directory
FLAPJACK_INSTALL=/opt/flapjack curl -fsSL https://install.flapjack.foo | sh

# Skip PATH modification
NO_MODIFY_PATH=1 curl -fsSL https://install.flapjack.foo | sh

# Uninstall
rm -rf ~/.flapjack
```

</details>

---

## Migrate from Algolia

Start the server and pull an existing index — settings, synonyms, rules, and all documents:

```bash
flapjack-server

curl -X POST http://localhost:7701/migrate \
  -d '{"appId":"YOUR_ALGOLIA_APP_ID","apiKey":"YOUR_ALGOLIA_ADMIN_KEY","sourceIndex":"products"}'
```

Find your Algolia credentials in the [Algolia dashboard](https://dashboard.algolia.com/account/api-keys/) under API Keys. You'll need the Admin API Key (not the Search-Only key).

Search immediately:

```bash
curl "http://localhost:7701/indexes/products/search?q=widget"
```

Then point your frontend at Flapjack instead of Algolia:

```javascript
import algoliasearch from 'algoliasearch';

// app-id can be any string, api-key is your FLAPJACK_ADMIN_KEY or a search key
const client = algoliasearch('my-app', 'your-flapjack-api-key');
client.transporter.hosts = [{ url: 'localhost:7701', protocol: 'http' }];

// Everything else stays the same
```

InstantSearch.js widgets work as-is — `SearchBox`, `Hits`, `RefinementList`, `Pagination`, `GeoSearch`, etc.

---

## Quick Start

If you're starting fresh, no SDK needed — just curl:

```bash
flapjack-server

# Add documents
curl -X POST http://localhost:7701/indexes/movies/documents \
  -d '[
    {"objectID":"1","title":"The Matrix","year":1999},
    {"objectID":"2","title":"Inception","year":2010}
  ]'

# Search (typo-tolerant)
curl "http://localhost:7701/indexes/movies/search?q=matrx"
```

No auth headers, no Content-Type header required in dev mode. For production, set `FLAPJACK_ENV=production` and `FLAPJACK_ADMIN_KEY` — see [Configuration](#configuration).

<details>
<summary>Full API reference</summary>

| Method | Endpoint | Description |
|--------|----------|-------------|
| `GET` | `/indexes` | List all indexes |
| `GET` | `/indexes/:name/search?q=...` | Search (query params) |
| `POST` | `/indexes/:name/search` | Search (JSON body) |
| `POST` | `/indexes/:name/documents` | Add/update documents |
| `GET` | `/indexes/:name/documents/:id` | Get a document |
| `DELETE` | `/indexes/:name/documents/:id` | Delete a document |
| `POST` | `/indexes/:name/documents/delete` | Bulk delete by ID array |
| `GET` | `/indexes/:name/settings` | Get index settings |
| `PUT` | `/indexes/:name/settings` | Update index settings |
| `DELETE` | `/indexes/:name` | Delete an index |
| `GET` | `/tasks/:taskId` | Check task status |
| `POST` | `/migrate` | Migrate from Algolia |

These are convenience endpoints with no auth required in dev mode. The full Algolia-compatible API lives under `/1/` with support for API keys, secured keys, filters, facets, and everything else.

</details>

---

## Features

| Feature | Details |
|---------|---------|
| Full-text search | Prefix matching, typo tolerance (Levenshtein ≤1/≤2) |
| Filters | Numeric, string, boolean, date — `AND`/`OR`/`NOT` |
| Faceting | Hierarchical, searchable, `filterOnly`, wildcard `*` |
| Geo search | `aroundLatLng`, `insideBoundingBox`, `insidePolygon`, auto-radius |
| Highlighting | Typo-aware, supports nested objects and arrays |
| Custom ranking | Multi-field, `asc`/`desc` |
| Synonyms | One-way, multi-way, alternative corrections |
| Query rules | Rewrite queries, pin/hide results |
| Pagination | `page`/`hitsPerPage` and `offset`/`length` |
| Distinct | Deduplication by attribute |
| Stop words & plurals | English built-in |
| Batch operations | Add, update, delete, clear, browse |
| API keys | ACL, index patterns, TTL, secured keys (HMAC) |
| S3 backup/restore | Scheduled snapshots, auto-restore on startup |

Algolia-compatible REST API under `/1/` — works with InstantSearch.js v5, the algoliasearch client, and [Laravel Scout](integrations/laravel-scout/).

**Not yet implemented:** analytics, A/B testing, query suggestions, vector search, multi-node replication.

---

## Deployment

```bash
cargo build --release
./target/release/flapjack-server
```

Requires Rust 1.70+. Pre-built binaries for Linux x86_64 (static musl), Linux ARM64, macOS Intel, and macOS Apple Silicon on the [releases page](https://github.com/flapjackhq/flapjack/releases/latest).

<details>
<summary>Docker</summary>

```yaml
# docker-compose.yml
services:
  flapjack:
    image: flapjack/flapjack:latest
    ports:
      - "7701:7701"
    volumes:
      - ./data:/data
    environment:
      FLAPJACK_DATA_DIR: /data
      FLAPJACK_ADMIN_KEY: ${ADMIN_KEY}
    restart: unless-stopped
```

</details>

<details>
<summary>Systemd</summary>

```ini
# /etc/systemd/system/flapjack.service
[Unit]
Description=Flapjack Search Server
After=network.target

[Service]
Type=simple
User=flapjack
WorkingDirectory=/var/lib/flapjack
Environment=FLAPJACK_DATA_DIR=/var/lib/flapjack/data
Environment=FLAPJACK_ADMIN_KEY=your-key
ExecStart=/usr/local/bin/flapjack-server
Restart=always

[Install]
WantedBy=multi-user.target
```

</details>

---

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `FLAPJACK_DATA_DIR` | `./data` | Index storage directory |
| `FLAPJACK_BIND_ADDR` | `127.0.0.1:7701` | Listen address |
| `FLAPJACK_ADMIN_KEY` | — | Admin API key (enables auth) |
| `FLAPJACK_ENV` | `development` | `production` requires auth on all endpoints |
| `FLAPJACK_S3_BUCKET` | — | S3 bucket for snapshots |
| `FLAPJACK_S3_REGION` | `us-west-1` | S3 region |
| `FLAPJACK_SNAPSHOT_INTERVAL` | — | Auto-snapshot interval (e.g. `6h`) |
| `FLAPJACK_SNAPSHOT_RETENTION` | — | Retention period (e.g. `30d`) |

Data stored in `FLAPJACK_DATA_DIR`. Mount as a volume in Docker.

---

## API Documentation

Browse at [flapjack-demo.pages.dev/api-docs](https://flapjack-demo.pages.dev/api-docs), or run locally and use the Swagger UI at `http://localhost:7701/swagger-ui/`.

---

## Use as a Library

Flapjack's core can be embedded directly:

```toml
[dependencies]
flapjack = { version = "0.1", default-features = false }
```

See [LIB.md](LIB.md) for the embedding guide.

---

## Architecture

Built on [Tantivy](https://github.com/stuartcrobinson/tantivy) (forked for edge-ngram prefix search). Axum + Tokio HTTP server. Supports 600+ indexes per 4GB node.

```
flapjack/              # Core library (search, indexing, query execution)
flapjack-http/         # HTTP server (Axum handlers, routing)
flapjack-replication/  # Cluster coordination
flapjack-ssl/          # TLS (Let's Encrypt, ACME)
flapjack-server/       # Binary entrypoint
```

---

## Development

```bash
cargo install cargo-nextest
cargo nextest run
```

---

## License

 [MIT](LICENSE)
