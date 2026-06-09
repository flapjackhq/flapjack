# Using Flapjack as a Library

Flapjack's core search functionality is available as a standalone Rust library. Embed it in desktop apps, CLI tools, mobile apps (via FFI), or custom web servers.

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
# Minimal - core search only
flapjack = { version = "1.0", default-features = false }

# OR with HTTP support
flapjack = { version = "1.0", default-features = false, features = ["axum-support"] }

# OR with S3 backups
flapjack = { version = "1.0", default-features = false, features = ["s3-snapshots"] }

# OR everything (default)
flapjack = "1.0"
```

## Feature Flags

| Feature | Dependencies | Use Case |
|---------|--------------|----------|
| `axum-support` | axum | Axum web framework integration (IntoResponse trait) |
| `s3-snapshots` | rust-s3, flate2, tar | S3 backup/restore, snapshot export/import |
| `openapi` | utoipa | OpenAPI schema generation for API docs |
| `analytics` | datafusion, arrow, parquet | Analytics query engine and query-suggestion exports |
| `vector-search` | usearch | Vector index/search support with external embeddings |
| `vector-search-local` | usearch, fastembed | Vector index/search with local embedding runtime |
| `decompound` | (none) | Compound-word splitting in query processing |
| `memory-stats` | tikv-jemalloc-ctl, sysinfo | Memory statistics instrumentation |

**Default features**: `axum-support`, `s3-snapshots`, `openapi`, `analytics`, `vector-search-local`, and `decompound` are enabled by default. Add `memory-stats` explicitly when needed, or opt out entirely with `default-features = false`.

## Basic Usage

### Create an Index

```rust
use flapjack::index::{Index, schema::Schema};

// Create index with default schema (creates directory if needed)
let index = Index::create_in_dir("./my_index")?;

// OR with explicit schema
let schema = Schema::builder().build();
let index = Index::create("./my_index", schema)?;
```

### Add Documents (Simple API)

The simple API accepts JSON with `objectID` (Algolia-compatible) or `_id`, auto-commits, and refreshes the reader immediately.

```rust
use serde_json::json;

let docs = vec![
    json!({
        "objectID": "1",
        "title": "MacBook Pro",
        "description": "Powerful laptop",
        "price": 2399,
        "category": "Electronics"
    }),
    json!({
        "objectID": "2",
        "title": "iPhone 15",
        "description": "Latest smartphone",
        "price": 999,
        "category": "Electronics"
    }),
];

// Auto-commit + reader refresh — documents are immediately searchable
index.add_documents_simple(&docs)?;
```

### Add Documents (Manual Writer)

For fine-grained control, use the `Document` type with an explicit writer:

```rust
use flapjack::types::{Document, FieldValue};
use std::collections::HashMap;

let doc = Document {
    id: "3".to_string(),
    fields: HashMap::from([
        ("title".to_string(), FieldValue::Text("iPad Air".to_string())),
        ("price".to_string(), FieldValue::Integer(599)),
    ]),
};

let mut writer = index.writer()?;
index.add_document(&mut writer, doc)?;
writer.commit()?;
// Note: call index.reader().reload()? to see committed data immediately
```

### Search via IndexManager

`IndexManager` provides multi-tenant search with query parsing, fuzzy matching, facets, synonyms, and rules.

```rust
use flapjack::IndexManager;
use flapjack::types::{Document, FieldValue};
use std::collections::HashMap;

let manager = IndexManager::new("./data");  // Returns Arc<IndexManager>
manager.create_tenant("products")?;

// Add documents (async, batched via write queue)
let docs = vec![Document {
    id: "1".to_string(),
    fields: HashMap::from([
        ("title".to_string(), FieldValue::Text("MacBook Pro laptop".to_string())),
        ("price".to_string(), FieldValue::Integer(2399)),
    ]),
}];
manager.add_documents_sync("products", docs).await?;

// Search: (tenant, query_text, filter, sort, limit) -> SearchResult
let results = manager.search("products", "laptop", None, None, 10)?;
println!("Found {} hits", results.total);
for hit in &results.documents {
    println!("  {} (score: {})", hit.document.id, hit.score);
}
```

## Multi-Tenant Manager

```rust
use flapjack::IndexManager;

let manager = IndexManager::new("./data");  // Arc<IndexManager>

// Create tenants (indexes)
manager.create_tenant("products")?;
manager.create_tenant("customers")?;

// Tenants are fully isolated
let results = manager.search("products", "laptop", None, None, 20)?;

// Async document operations with batched writes
manager.add_documents_sync("products", docs).await?;
manager.delete_documents_sync("products", vec!["old-id".to_string()]).await?;

// Get a single document by ID
let doc = manager.get_document("products", "1")?;
```

## Desktop App Example

```rust
use flapjack::index::Index;
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let index_path = dirs::data_dir()
        .unwrap()
        .join("my-app")
        .join("search-index");

    // create_in_dir creates the directory if needed
    let index = if index_path.exists() {
        Index::open(&index_path)?
    } else {
        Index::create_in_dir(&index_path)?
    };

    index.add_documents_simple(&[
        json!({"objectID": "1", "title": "Document 1"}),
        json!({"objectID": "2", "title": "Document 2"}),
    ])?;

    Ok(())
}
```

## Custom Web Server Example

Build your own HTTP API using the core library with Axum (requires `features = ["axum-support"]`):

```rust
use axum::{Router, routing::get, extract::{State, Path, Query}, Json};
use flapjack::IndexManager;
use std::sync::Arc;
use serde::Deserialize;

#[derive(Deserialize)]
struct SearchParams {
    q: String,
    limit: Option<usize>,
}

async fn search_handler(
    State(manager): State<Arc<IndexManager>>,
    Path(tenant): Path<String>,
    Query(params): Query<SearchParams>,
) -> Result<Json<serde_json::Value>, flapjack::error::FlapjackError> {
    let limit = params.limit.unwrap_or(20);
    let results = manager.search(&tenant, &params.q, None, None, limit)?;
    Ok(Json(serde_json::json!({
        "hits": results.documents.iter().map(|d| d.document.to_json()).collect::<Vec<_>>(),
        "nbHits": results.total,
    })))
}

#[tokio::main]
async fn main() {
    let manager = IndexManager::new("./data");
    let app = Router::new()
        .route("/search/{tenant}", get(search_handler))
        .with_state(manager);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
```

## S3 Backups (Feature-Gated)

Enable with `features = ["s3-snapshots"]`:

```rust
use flapjack::index::s3::{self, S3Config};
use flapjack::index::snapshot;

// Export to S3
let s3_config = S3Config::from_env().expect("S3 config from env");
let snapshot_bytes = snapshot::export_to_bytes(&index_path)?;
let s3_key = s3::upload_snapshot(&s3_config, "my-index", &snapshot_bytes).await?;

// Restore from S3
let (key, data) = s3::download_latest_snapshot(&s3_config, "my-index").await?;
snapshot::import_from_bytes(&data, &restore_path)?;
```

Environment variables:
- `FLAPJACK_S3_BUCKET` — S3 bucket name
- `FLAPJACK_S3_REGION` — AWS region (default: us-east-1)
- `FLAPJACK_S3_ENDPOINT` — Custom endpoint (for MinIO, LocalStack, etc.)

## Memory Management

Control memory usage for large-scale indexing:

```rust
use flapjack::index::memory::{MemoryBudget, MemoryBudgetConfig};
use std::sync::Arc;

let config = MemoryBudgetConfig {
    max_buffer_mb: 50,            // 50MB per writer
    max_concurrent_writers: 10,   // Max 10 writers
    max_doc_mb: 5,                // 5MB per document
};

let budget = Arc::new(MemoryBudget::new(config));
let schema = flapjack::index::schema::Schema::builder().build();
let index = flapjack::index::Index::create_with_budget("./index", schema, budget)?;
```

Or configure via environment variables:
- `FLAPJACK_MAX_BUFFER_MB` (default: 31)
- `FLAPJACK_MAX_CONCURRENT_WRITERS` (default: 40)
- `FLAPJACK_MAX_DOC_MB` (default: 3)

## Testing

```rust
use flapjack::index::Index;
use serde_json::json;
use tempfile::TempDir;

#[test]
fn test_search() {
    let temp_dir = TempDir::new().unwrap();
    let index = Index::create_in_dir(temp_dir.path()).unwrap();

    index.add_documents_simple(&[
        json!({"objectID": "1", "title": "test document"}),
    ]).unwrap();

    // Documents are immediately available after add_documents_simple
    let reader = index.reader();
    let searcher = reader.searcher();
    let count: usize = searcher.segment_readers()
        .iter().map(|r| r.num_docs() as usize).sum();
    assert_eq!(count, 1);
}
```

## Error Handling

```rust
use flapjack::error::FlapjackError;
use flapjack::IndexManager;

fn search(manager: &IndexManager) -> flapjack::Result<()> {
    match manager.search("products", "laptop", None, None, 10) {
        Ok(results) => {
            println!("Found {} hits", results.total);
            Ok(())
        }
        Err(FlapjackError::TenantNotFound(tenant)) => {
            eprintln!("Index '{}' does not exist", tenant);
            Err(FlapjackError::TenantNotFound(tenant))
        }
        Err(FlapjackError::InvalidQuery(msg)) => {
            eprintln!("Invalid query: {}", msg);
            Err(FlapjackError::InvalidQuery(msg))
        }
        Err(e) => Err(e),
    }
}
```

All errors implement `std::error::Error` and can be converted with `?`.

## Performance Tips

1. **Write throughput (beta)**: When write throughput matters, batch documents per call (for example, `index.add_documents_simple(vec![doc1, doc2, doc3, doc4])?;`) instead of sending one document per request. The post-PL-13 durable single-doc path (`IndexManager::add_documents_durable` -> `wait_for_write_durable` -> `await_task_terminal`) is roughly `5.290291 req/s`, while retained batched/sustained evidence is `831.652577/s` and `921.538969/s` using the write queue batch seam (`WRITE_QUEUE_BATCH_SIZE`). Do not treat parallel-writer fan-out as the throughput path; use batching. See `loadtest/BENCHMARKS.md` and `docs/reference/research/pl13_stage3_throughput_delta.md`.
2. **Batch writes**: Use `add_documents_simple` for bulk loading (one commit per call)
3. **Reuse writers**: For manual writes, create a writer once and commit multiple times
4. **Index warming**: Call `index.searchable_paths()` after loading to warm caches
5. **Memory tuning**: Increase `max_buffer_mb` for large batch imports
6. **Concurrent searches**: `IndexManager` is `Send + Sync`, share with `Arc` (it already returns `Arc<Self>`)

## Architecture

```
flapjack/              # Core library (search engine, indexing, query execution)
flapjack-http/         # Axum HTTP layer (routing, middleware, handlers)
flapjack-replication/  # Replication peer coordination
flapjack-ssl/          # TLS/ACME support
flapjack-server/       # Binary entrypoint (CLI/env bootstrap)
```

**When embedding**, you typically only need the core `flapjack` crate. The HTTP and server crates are for running a standalone service.

## Migration from Server to Library

Already using `flapjack-server`? Migrate gradually:

```rust
use flapjack::IndexManager;

// IndexManager reads the same on-disk format as flapjack-server
let manager = IndexManager::new("/var/lib/flapjack/data");

// Existing indexes are loaded on first access
let results = manager.search("my-index", "query", None, None, 20)?;
```

Data format is identical. No re-indexing needed.

## Further Reading

- [ARCHITECTURE.md](docs2/3_IMPLEMENTATION/ARCHITECTURE.md) — Core design decisions
- [Integration Tests](tests/) — Real-world usage examples (`test_query.rs`, `test_index_ops.rs`)
- **API Docs**: Run `cargo doc --open --no-deps` for generated API documentation

## License

MIT
