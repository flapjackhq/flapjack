# Flapjack

Drop-in replacement for Algolia. Self-hosted, single binary, no dependencies.

[![CI](https://github.com/stuartcrobinson/flapjack/actions/workflows/ci.yml/badge.svg)](https://github.com/stuartcrobinson/flapjack/actions/workflows/ci.yml)
[![Release](https://github.com/stuartcrobinson/flapjack/actions/workflows/release.yml/badge.svg)](https://github.com/stuartcrobinson/flapjack/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Works with [InstantSearch.js](https://github.com/algolia/instantsearch), [algoliasearch](https://github.com/algolia/algoliasearch-client-javascript) (Algolia's JavaScript client), or plain HTTP.

**[Live Demo](https://flapjack-demo.pages.dev/)** — side-by-side comparison with Algolia, Typesense, and Meilisearch

---

## Quick Start

Download the latest binary from [Releases](https://github.com/stuartcrobinson/flapjack/releases/latest).

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

Or build from source (requires Rust 1.70+):

```bash
cargo build --release
./target/release/flapjack-server
```

---

## Client Setup

Point the Algolia JavaScript client at your Flapjack server:

```javascript
import algoliasearch from 'algoliasearch';

const client = algoliasearch('your-app-id', 'your-api-key');
client.transporter.hosts = [{ url: 'localhost:7701', protocol: 'http' }];

// Everything else unchanged
```

InstantSearch.js widgets work with Flapjack (SearchBox, Hits, RefinementList, Pagination, HierarchicalMenu, GeoSearch, etc.).

---

## Features

| Feature | Status | Notes |
|---------|--------|-------|
| **Search** | | |
| Full-text search | ✅ | `/1/indexes/*/query` endpoint |
| Filters | ✅ | Numeric, string, boolean, date comparisons |
| Faceting | ✅ | Hierarchical, searchable, filterOnly |
| Typo tolerance | ✅ | Levenshtein ≤1 (4-7 chars) / ≤2 (8+ chars) |
| Prefix search | ✅ | prefixLast, prefixAll, prefixNone |
| Geo-search | ✅ | aroundLatLng, boundingBox, polygon, auto-radius |
| Highlighting | ✅ | Typo-tolerant, nested objects, arrays |
| Custom ranking | ✅ | Multi-field ranking criteria |
| Pagination | ✅ | page, hitsPerPage, offset, length |
| Distinct | ✅ | Deduplication by attribute |
| Stop words | ✅ | Query-time removal (English) |
| Plurals | ✅ | ignorePlurals (English) |
| | | |
| **Indexing** | | |
| Batch operations | ✅ | Add, update, delete multiple documents |
| Single object CRUD | ✅ | Get, add, update, delete |
| Clear index | ✅ | Delete all documents |
| Delete by query | ✅ | Conditional deletion |
| Browse | ✅ | Iterate all documents |
| | | |
| **Configuration** | | |
| Index settings | ✅ | Searchable attributes, ranking, faceting |
| Synonyms | ✅ | One-way, multi-way, alternative corrections |
| Query rules | ✅ | Query rewrite, pinning, hiding |
| | | |
| **Security** | | |
| API keys | ✅ | ACL, index patterns, TTL |
| Secured API keys | ✅ | HMAC-based with embedded restrictions |
| | | |
| **Operations** | | |
| S3 backup/restore | ✅ | Automated snapshots + restore on startup |
| Multi-tenancy | ✅ | 600+ indexes per 4GB node |
| | | |
| **Client Libraries** | | |
| InstantSearch.js | ✅ | v5, all standard widgets |
| REST API | ✅ | Compatible with Algolia v1 clients |
| | | |
| **Planned** | | |
| Analytics API | ❌ | Search tracking, top queries |
| A/B testing | ❌ | |
| Query suggestions | ❌ | Autocomplete index |
| Vector search | ❌ | Semantic search |

---

## API Documentation

**Interactive Swagger UI** available at http://localhost:7700/swagger-ui when running the server.

- 45+ endpoints fully documented
- Request/response schemas
- "Try it out" functionality for testing
- OpenAPI 3.0 spec available at `/api-docs/openapi.json`


---

## Configuration

Configure via environment variables:

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

Example:

```bash
export FLAPJACK_ENV=production
export FLAPJACK_ADMIN_KEY="your-admin-key"
export FLAPJACK_BIND_ADDR="0.0.0.0:7701"
export FLAPJACK_DATA_DIR="/var/lib/flapjack"

./flapjack-server
```

---

## Use as a Library

Flapjack can be embedded in your Rust application:

```toml
[dependencies]
flapjack = "0.1"  # Core library only (no HTTP server)
# OR with optional features:
flapjack = { version = "0.1", features = ["axum-support", "s3-snapshots"] }
```

**Feature flags:**
- `axum-support` — HTTP error responses (IntoResponse trait)
- `s3-snapshots` — S3 backup/restore functionality
- `openapi` — OpenAPI schema generation (utoipa)

**Default features** include all of the above for convenience. Disable with `default-features = false` for minimal dependencies.

See [LIB.md](LIB.md) for the full embedding guide, or run `cargo doc --open --no-deps` for API documentation.

---

## Architecture

Built on [Tantivy](https://github.com/quickwit-oss/tantivy) (forked for edge-ngram prefix search). Single binary, Axum + Tokio HTTP server. Multi-tenant with DashMap — supports 600+ tenants per 4GB node. Operation log with S3 snapshots for durability.

**Tech stack:**
- Search engine: Tantivy 0.25
- HTTP server: Axum + Tokio
- Storage: Local filesystem
- Backup: rust-s3

---

## Development

### Run Tests

```bash
cargo install cargo-nextest

cargo nextest run           # Fast tests (~5s)
cargo nextest run -P slow   # Medium tests (~3 min)
cargo nextest run -P ci     # All tests
```

### Project Structure

```
flapjack/              # Core library (search engine, indexing, query execution)
  src/
  ├── index/           # Index, IndexManager, schema, write queue, oplog
  ├── query/           # Query parsing, execution, fuzzy matching, highlighting
  ├── tokenizer/       # CJK-aware tokenizer
  ├── types.rs         # Document, FieldValue, SearchResult, Filter, etc.
  └── error.rs         # FlapjackError
flapjack-http/         # HTTP server layer (Axum handlers, middleware, routing)
flapjack-replication/  # Cluster coordination (peer discovery, state sync)
flapjack-ssl/          # SSL/TLS management (Let's Encrypt, ACME)
flapjack-server/       # Binary entrypoint (CLI, config, main loop)
```

### CI & Releases

CI runs automatically on push/PR. Release builds are manual via GitHub CLI:

```bash
./s/release.sh 0.1.0         # Build + release
```

Release builds for: Linux x86_64 (static musl), Linux ARM64, macOS Intel, macOS Apple Silicon.

---

## Deployment

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

### Docker

```yaml
# docker-compose.yml
version: '3.8'
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

---

## Roadmap

**Phase 1: Core Search** ✅ Complete
- Full-text search, faceting, geo-search
- Synonyms, rules, API keys
- S3 backups

**Phase 2: Production Hardening** (Current)
- Analytics API
- Rate limiting
- Query suggestions

**Phase 3: Scale**
- Multi-node replication
- Horizontal scaling
- Cross-region support


---

## Comparison

|  | Flapjack | Algolia | Typesense | Meilisearch |
|---|----------|---------|-----------|-------------|
| Deployment | Self-hosted | SaaS | Self-hosted or SaaS | Self-hosted or Cloud |
| License | MIT | Proprietary | GPL-3.0 | MIT |
| Language | Rust | Proprietary | C++ | Rust |
| InstantSearch.js | Compatible | Native | Needs adapter | Needs adapter |

---

## Contributing

Contributions welcome. Please open an issue before starting work on features.

1. Fork and create feature branch
2. Write tests for new functionality
3. Run `cargo nextest run -P ci`
4. Submit pull request

---

## License

MIT — see [LICENSE](LICENSE)

---

Built with [Tantivy](https://github.com/quickwit-oss/tantivy), [Axum](https://github.com/tokio-rs/axum), and [Tokio](https://tokio.rs/).
