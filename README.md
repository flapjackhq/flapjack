# Flapjack

Self-hosted search engine with an Algolia-compatible API. Single binary, no dependencies.

[![CI](https://github.com/stuartcrobinson/flapjack202511/actions/workflows/ci.yml/badge.svg)](https://github.com/stuartcrobinson/flapjack202511/actions/workflows/ci.yml)
[![Release](https://github.com/stuartcrobinson/flapjack202511/actions/workflows/release.yml/badge.svg)](https://github.com/stuartcrobinson/flapjack202511/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Status: Beta](https://img.shields.io/badge/Status-Beta-orange)](https://github.com/stuartcrobinson/flapjack202511)

> **Beta:** Flapjack is under active development. The API is largely stable but breaking changes may still occur. Not yet recommended for production workloads without testing. Feedback and bug reports welcome.

- Works with [InstantSearch.js](https://github.com/algolia/instantsearch) and the [algoliasearch](https://github.com/algolia/algoliasearch-client-javascript) client — point them at your server instead of Algolia
- Typo-tolerant full-text search with faceting, geo-search, and custom ranking
- Single static binary, runs anywhere, data on local disk

**[Live Demo](https://flapjack-demo.pages.dev)** · **[Geo Demo](https://flapjack-demo.pages.dev/geo)** · **[API Docs](https://flapjack-demo.pages.dev/api-docs)**

---

## Quick Start

Download the latest binary from [Releases](https://github.com/flapjackhq/flapjack/releases/latest):

```bash
tar xzf flapjack-*.tar.gz
./flapjack-server

# Add documents
curl -X POST http://localhost:7701/1/indexes/movies/batch \
  -H "Content-Type: application/json" \
  -d '{"requests":[
    {"action":"addObject","body":{"objectID":"1","title":"The Matrix","year":1999}},
    {"action":"addObject","body":{"objectID":"2","title":"Inception","year":2010}}
  ]}'

# Search (typo-tolerant)
curl -X POST http://localhost:7701/1/indexes/movies/query \
  -H "Content-Type: application/json" \
  -d '{"query":"matrx"}'
```

No auth required in development mode. For production, set `FLAPJACK_ENV=production` and `FLAPJACK_ADMIN_KEY` — see [Configuration](#configuration). Data is stored in `FLAPJACK_DATA_DIR` (default `./data`). Mount this as a volume if running in Docker.

---

## Deployment

### Docker

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

### Systemd

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

### Build from Source

Requires Rust 1.70+:

```bash
cargo build --release
./target/release/flapjack-server
```

Release binaries available for Linux x86_64 (static musl), Linux ARM64, macOS Intel, and macOS Apple Silicon.

---

## Client Setup

Point the Algolia JavaScript client at your Flapjack server:

```javascript
import algoliasearch from 'algoliasearch';

const client = algoliasearch('your-app-id', 'your-api-key');
client.transporter.hosts = [{ url: 'localhost:7701', protocol: 'http' }];

// Everything else unchanged
```

InstantSearch.js widgets work out of the box — `SearchBox`, `Hits`, `RefinementList`, `Pagination`, `HierarchicalMenu`, `GeoSearch`, etc.

### Migrating from Algolia

One-click migration endpoint pulls settings, synonyms, rules, and all documents from an existing Algolia index:

```bash
curl -X POST http://localhost:7701/1/migrate-from-algolia \
  -H "Content-Type: application/json" \
  -d '{"appId":"YOUR_ALGOLIA_APP_ID","apiKey":"YOUR_ALGOLIA_API_KEY","sourceIndex":"products","targetIndex":"products"}'
```

---

## Features

| Feature | Status | Notes |
|---------|--------|-------|
| Full-text search | ✅ | Prefix matching, typo tolerance (Levenshtein ≤1/≤2) |
| Filters | ✅ | Numeric, string, boolean, date, `AND`/`OR`/`NOT` |
| Faceting | ✅ | Hierarchical, searchable, `filterOnly`, wildcard |
| Geo search | ✅ | `aroundLatLng`, `insideBoundingBox`, `insidePolygon`, auto-radius |
| Highlighting | ✅ | Typo-tolerant, nested objects, arrays |
| Custom ranking | ✅ | Multi-field, `asc`/`desc` |
| Synonyms | ✅ | One-way, multi-way, alternative corrections |
| Query rules | ✅ | Rewrite, pinning, hiding |
| Pagination | ✅ | `page`/`hitsPerPage`, `offset`/`length` |
| Distinct | ✅ | Deduplication by attribute |
| Stop words | ✅ | English built-in |
| Plurals | ✅ | `ignorePlurals` (English) |
| Batch operations | ✅ | Add, update, delete, clear, browse |
| API keys | ✅ | ACL, index patterns, TTL, secured (HMAC) |
| S3 backup/restore | ✅ | Scheduled snapshots, auto-restore on startup |
| InstantSearch.js | ✅ | v5, all standard widgets |
| REST API | ✅ | 45+ endpoints, Algolia v1 compatible |

**Not yet implemented:** Analytics API, A/B testing, query suggestions, vector/semantic search, multi-node replication.

---

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `FLAPJACK_DATA_DIR` | `./data` | Index storage directory |
| `FLAPJACK_BIND_ADDR` | `127.0.0.1:7701` | Listen address |
| `FLAPJACK_ADMIN_KEY` | — | Admin API key (enables auth) |
| `FLAPJACK_ENV` | `development` | Set to `production` to require auth |
| `FLAPJACK_S3_BUCKET` | — | S3 bucket for snapshots |
| `FLAPJACK_S3_REGION` | `us-west-1` | S3 region |
| `FLAPJACK_SNAPSHOT_INTERVAL` | — | Auto-snapshot interval (e.g. `6h`) |
| `FLAPJACK_SNAPSHOT_RETENTION` | — | Retention period (e.g. `30d`) |

---

## API Documentation

Browse the API at [flapjack-demo.pages.dev/api-docs](https://flapjack-demo.pages.dev/api-docs), or run the server locally and use the interactive Swagger UI at `http://localhost:7701/swagger-ui/`.

---

## Use as a Library

Flapjack's core can be embedded in Rust applications:

```toml
[dependencies]
flapjack = { version = "0.1", default-features = false }
```

See [LIB.md](LIB.md) for the full embedding guide, or run `cargo doc --open --no-deps` for API docs.

---

## Comparison

|  | Flapjack | Algolia | Typesense | Meilisearch |
|---|----------|---------|-----------|-------------|
| Deployment | Self-hosted | SaaS | Self-hosted or SaaS | Self-hosted or Cloud |
| License | MIT | Proprietary | GPL-3.0 | MIT |
| Language | Rust | Proprietary | C++ | Rust |
| InstantSearch.js | Compatible | Native | Via adapter | Via adapter |

---

## Architecture

Built on [Tantivy](https://github.com/stuartcrobinson/tantivy) (forked for edge-ngram prefix search). Axum + Tokio HTTP server. Multi-tenant — supports 600+ indexes per 4GB node.

```
flapjack/              # Core library (search engine, indexing, query execution)
flapjack-http/         # HTTP server (Axum handlers, middleware, routing)
flapjack-replication/  # Cluster coordination
flapjack-ssl/          # SSL/TLS (Let's Encrypt, ACME)
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

MIT — see [LICENSE](LICENSE)

---

Built with [Tantivy](https://github.com/stuartcrobinson/tantivy), [Axum](https://github.com/tokio-rs/axum), and [Tokio](https://tokio.rs/).
