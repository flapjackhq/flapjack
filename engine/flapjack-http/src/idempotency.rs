//! Stub summary for /Users/stuart/parallel_development/flapjack_dev/may31_12pm_4_idempotency_cache_durability/flapjack_dev/engine/flapjack-http/src/idempotency.rs.
use axum::body::{Body, Bytes};
use axum::http::StatusCode;
use axum::response::Response;
use dashmap::DashMap;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub const IDEMPOTENCY_HEADER: &str = "x-flapjack-idempotency-key";
pub const BATCH_INDEX_WILDCARD: &str = "*";

const DEFAULT_TTL_SECS: u64 = 300;
const MIN_TTL_SECS: u64 = 1;
const IDENTITY_WILDCARD: &str = "*";
const PERSIST_ENV_CANONICAL: &str = "FLAPJACK_IDEMPOTENCY_PERSISTENT";
const PERSIST_ENV_COMPAT: &str = "FLAPJACK_IDEMPOTENCY_PERSIST";

#[derive(Clone)]
pub struct IdempotencyRecord {
    pub status: u16,
    pub body: Bytes,
}

impl IdempotencyRecord {
    pub fn json(status: StatusCode, body: Bytes) -> Self {
        Self {
            status: status.as_u16(),
            body,
        }
    }

    pub fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::OK);
        Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .header("x-flapjack-idempotency-replayed", "true")
            .body(Body::from(self.body))
            .expect("static idempotency response headers are valid")
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct CompositeKey {
    application_id: String,
    index_segment: String,
    idempotency_key: String,
}

impl CompositeKey {
    fn new(application_id: &str, index_segment: &str, idempotency_key: &str) -> Self {
        Self {
            application_id: application_id.to_owned(),
            index_segment: index_segment.to_owned(),
            idempotency_key: idempotency_key.to_owned(),
        }
    }
}

#[derive(Clone)]
struct TimedRecord {
    record: IdempotencyRecord,
    inserted_at_unix_ms: i64,
}

#[derive(Debug, Error)]
pub enum IdempotencyStoreError {
    #[error("idempotency store unavailable: mutex poisoned")]
    MutexPoisoned,
    #[error("idempotency sqlite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

trait IdempotencyStore: Send + Sync {
    fn lookup(
        &self,
        key: &CompositeKey,
        ttl: Duration,
    ) -> Result<Option<IdempotencyRecord>, IdempotencyStoreError>;
    fn store(
        &self,
        key: CompositeKey,
        value: TimedRecord,
        ttl: Duration,
    ) -> Result<(), IdempotencyStoreError>;
    fn trim_count(&self) -> u64;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
}

struct MemoryStore {
    entries: DashMap<CompositeKey, TimedRecord>,
    last_trim_millis: AtomicU64,
    trim_count: AtomicU64,
}

impl MemoryStore {
    fn new() -> Self {
        let now_millis = now_unix_ms() as u64;
        Self {
            entries: DashMap::new(),
            last_trim_millis: AtomicU64::new(now_millis),
            trim_count: AtomicU64::new(0),
        }
    }

    fn maybe_trim(&self, ttl: Duration) {
        let interval_millis = ttl.as_millis() as u64;
        let now_millis = now_unix_ms() as u64;
        let last = self.last_trim_millis.load(Ordering::Relaxed);
        if now_millis.saturating_sub(last) < interval_millis {
            return;
        }
        if self
            .last_trim_millis
            .compare_exchange(last, now_millis, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let cutoff = now_millis as i64 - ttl.as_millis() as i64;
        self.entries
            .retain(|_, timed| timed.inserted_at_unix_ms >= cutoff);
        self.trim_count.fetch_add(1, Ordering::Relaxed);
    }
}

impl IdempotencyStore for MemoryStore {
    fn lookup(
        &self,
        key: &CompositeKey,
        ttl: Duration,
    ) -> Result<Option<IdempotencyRecord>, IdempotencyStoreError> {
        self.maybe_trim(ttl);
        let Some(entry) = self.entries.get(key) else {
            return Ok(None);
        };
        let now = now_unix_ms();
        let age_ms = now.saturating_sub(entry.inserted_at_unix_ms);
        let record = entry.record.clone();
        drop(entry);
        if age_ms > ttl.as_millis() as i64 {
            let ttl_ms = ttl.as_millis() as i64;
            self.entries.remove_if(key, |_, timed| {
                now_unix_ms().saturating_sub(timed.inserted_at_unix_ms) > ttl_ms
            });
            return Ok(None);
        }
        Ok(Some(record))
    }

    fn store(
        &self,
        key: CompositeKey,
        value: TimedRecord,
        ttl: Duration,
    ) -> Result<(), IdempotencyStoreError> {
        self.maybe_trim(ttl);
        self.entries.insert(key, value);
        Ok(())
    }

    fn trim_count(&self) -> u64 {
        self.trim_count.load(Ordering::Relaxed)
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

struct SqliteStore {
    conn: Mutex<Connection>,
    path: PathBuf,
    last_trim_millis: AtomicU64,
    trim_count: AtomicU64,
}

impl SqliteStore {
    fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS idempotency_cache (
                application_id TEXT NOT NULL,
                index_segment TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                status INTEGER NOT NULL,
                body BLOB NOT NULL,
                inserted_at_unix_ms INTEGER NOT NULL,
                PRIMARY KEY (application_id, index_segment, idempotency_key)
            );
            CREATE INDEX IF NOT EXISTS idx_idempotency_inserted_at
                ON idempotency_cache(inserted_at_unix_ms);",
        )?;

        let now_millis = now_unix_ms() as u64;
        Ok(Self {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
            last_trim_millis: AtomicU64::new(now_millis),
            trim_count: AtomicU64::new(0),
        })
    }

    fn maybe_trim(&self, ttl: Duration) -> Result<(), IdempotencyStoreError> {
        let interval_millis = ttl.as_millis() as u64;
        let now_millis = now_unix_ms() as u64;
        let last = self.last_trim_millis.load(Ordering::Relaxed);
        if now_millis.saturating_sub(last) < interval_millis {
            return Ok(());
        }
        if self
            .last_trim_millis
            .compare_exchange(last, now_millis, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return Ok(());
        }
        let cutoff = now_millis as i64 - ttl.as_millis() as i64;
        let conn = self
            .conn
            .lock()
            .map_err(|_| IdempotencyStoreError::MutexPoisoned)?;
        conn.execute(
            "DELETE FROM idempotency_cache WHERE inserted_at_unix_ms < ?1",
            params![cutoff],
        )?;
        self.trim_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl IdempotencyStore for SqliteStore {
    fn lookup(
        &self,
        key: &CompositeKey,
        ttl: Duration,
    ) -> Result<Option<IdempotencyRecord>, IdempotencyStoreError> {
        self.maybe_trim(ttl)?;
        let conn = self
            .conn
            .lock()
            .map_err(|_| IdempotencyStoreError::MutexPoisoned)?;
        let mut stmt = conn.prepare(
            "SELECT status, body, inserted_at_unix_ms
                 FROM idempotency_cache
                 WHERE application_id = ?1 AND index_segment = ?2 AND idempotency_key = ?3",
        )?;
        let row = match stmt.query_row(
            params![key.application_id, key.index_segment, key.idempotency_key],
            |row| {
                Ok((
                    row.get::<_, u16>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        ) {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(err) => return Err(IdempotencyStoreError::Sqlite(err)),
        };
        let age_ms = now_unix_ms().saturating_sub(row.2);
        if age_ms > ttl.as_millis() as i64 {
            conn.execute(
                "DELETE FROM idempotency_cache
                 WHERE application_id = ?1 AND index_segment = ?2 AND idempotency_key = ?3",
                params![key.application_id, key.index_segment, key.idempotency_key],
            )?;
            return Ok(None);
        }
        Ok(Some(IdempotencyRecord {
            status: row.0,
            body: Bytes::from(row.1),
        }))
    }

    fn store(
        &self,
        key: CompositeKey,
        value: TimedRecord,
        ttl: Duration,
    ) -> Result<(), IdempotencyStoreError> {
        self.maybe_trim(ttl)?;
        let conn = self
            .conn
            .lock()
            .map_err(|_| IdempotencyStoreError::MutexPoisoned)?;
        conn.execute(
            "INSERT INTO idempotency_cache(
                    application_id, index_segment, idempotency_key, status, body, inserted_at_unix_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(application_id, index_segment, idempotency_key)
                 DO UPDATE SET
                    status = excluded.status,
                    body = excluded.body,
                    inserted_at_unix_ms = excluded.inserted_at_unix_ms",
                params![
                    key.application_id,
                    key.index_segment,
                    key.idempotency_key,
                    value.record.status,
                    value.record.body.to_vec(),
                    value.inserted_at_unix_ms,
                ],
            )?;
        Ok(())
    }

    fn trim_count(&self) -> u64 {
        self.trim_count.load(Ordering::Relaxed)
    }

    fn len(&self) -> usize {
        if let Ok(conn) = self.conn.lock() {
            conn.query_row("SELECT COUNT(*) FROM idempotency_cache", [], |row| {
                row.get(0)
            })
            .unwrap_or(0)
        } else {
            0
        }
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub struct IdempotencyCache {
    store: Box<dyn IdempotencyStore>,
    ttl: Duration,
    sqlite_path: Option<PathBuf>,
}

impl IdempotencyCache {
    pub fn new(ttl: Duration) -> Self {
        Self::memory(ttl)
    }

    pub fn memory(ttl: Duration) -> Self {
        Self {
            store: Box::new(MemoryStore::new()),
            ttl,
            sqlite_path: None,
        }
    }

    pub fn persistent(ttl: Duration, db_path: &Path) -> Result<Self, rusqlite::Error> {
        let store = SqliteStore::open(db_path)?;
        let persisted_path = store.path().to_path_buf();
        Ok(Self {
            store: Box::new(store),
            ttl,
            sqlite_path: Some(persisted_path),
        })
    }

    pub fn persistent_under_data_dir(
        ttl: Duration,
        data_dir: &Path,
    ) -> Result<Self, rusqlite::Error> {
        Self::persistent(ttl, &Self::canonical_db_path(data_dir))
    }

    pub fn from_env() -> Self {
        Self::from_env_with_data_dir(Path::new("."))
    }

    pub fn from_env_with_data_dir(data_dir: &Path) -> Self {
        let ttl_secs = std::env::var("FLAPJACK_IDEMPOTENCY_TTL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TTL_SECS)
            .max(MIN_TTL_SECS);
        let ttl = Duration::from_secs(ttl_secs);
        if persistent_mode_enabled_from_env() {
            match Self::persistent_under_data_dir(ttl, data_dir) {
                Ok(cache) => cache,
                Err(err) => {
                    tracing::warn!(error = %err, "failed to initialize persistent idempotency cache; falling back to memory");
                    Self::memory(ttl)
                }
            }
        } else {
            Self::memory(ttl)
        }
    }

    pub fn canonical_db_path(data_dir: &Path) -> PathBuf {
        data_dir.join("_idempotency").join("cache.db")
    }

    pub fn persistence_path(&self) -> Option<&Path> {
        self.sqlite_path.as_deref()
    }

    pub fn lookup_scoped(
        &self,
        application_id: &str,
        index_segment: &str,
        idempotency_key: &str,
    ) -> Result<Option<IdempotencyRecord>, IdempotencyStoreError> {
        let key = CompositeKey::new(application_id, index_segment, idempotency_key);
        self.store.lookup(&key, self.ttl)
    }

    pub fn store_scoped(
        &self,
        application_id: &str,
        index_segment: &str,
        idempotency_key: &str,
        record: IdempotencyRecord,
    ) -> Result<(), IdempotencyStoreError> {
        let key = CompositeKey::new(application_id, index_segment, idempotency_key);
        self.store.store(
            key,
            TimedRecord {
                record,
                inserted_at_unix_ms: now_unix_ms(),
            },
            self.ttl,
        )
    }

    pub fn lookup(&self, key: &str) -> Result<Option<IdempotencyRecord>, IdempotencyStoreError> {
        self.lookup_scoped(IDENTITY_WILDCARD, IDENTITY_WILDCARD, key)
    }

    pub fn store(
        &self,
        key: String,
        record: IdempotencyRecord,
    ) -> Result<(), IdempotencyStoreError> {
        self.store_scoped(IDENTITY_WILDCARD, IDENTITY_WILDCARD, &key, record)
    }

    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    pub fn trim_count(&self) -> u64 {
        self.store.trim_count()
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn persistent_mode_enabled_from_env() -> bool {
    if std::env::var_os(PERSIST_ENV_CANONICAL).is_some() {
        return env_flag_enabled(PERSIST_ENV_CANONICAL);
    }
    if std::env::var_os(PERSIST_ENV_COMPAT).is_some() {
        tracing::warn!(
            "{} is deprecated; prefer {}",
            PERSIST_ENV_COMPAT,
            PERSIST_ENV_CANONICAL
        );
        return env_flag_enabled(PERSIST_ENV_COMPAT);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_returns_none_when_key_missing() {
        let cache = IdempotencyCache::new(Duration::from_secs(60));
        assert!(cache.lookup("absent").expect("lookup").is_none());
    }

    #[test]
    fn store_then_lookup_returns_record() {
        let cache = IdempotencyCache::new(Duration::from_secs(60));
        let record = IdempotencyRecord::json(StatusCode::CREATED, Bytes::from_static(b"{}"));
        cache.store("k1".into(), record).expect("store");
        let hit = cache.lookup("k1").expect("lookup").expect("hit");
        assert_eq!(hit.status, 201);
        assert_eq!(hit.body, Bytes::from_static(b"{}"));
    }

    #[test]
    fn expired_entry_is_evicted_on_lookup() {
        let cache = IdempotencyCache::new(Duration::from_nanos(1));
        cache
            .store(
                "k1".into(),
                IdempotencyRecord::json(StatusCode::OK, Bytes::from_static(b"{}")),
            )
            .expect("store");
        std::thread::sleep(Duration::from_millis(2));
        assert!(cache.lookup("k1").expect("lookup").is_none());
        assert!(cache.is_empty(), "expired entry must be evicted");
    }

    #[test]
    fn lookups_within_trim_interval_do_not_resweep() {
        let cache = IdempotencyCache::new(Duration::from_secs(60));
        cache
            .store(
                "k1".into(),
                IdempotencyRecord::json(StatusCode::OK, Bytes::from_static(b"{}")),
            )
            .expect("store");
        let trims_before = cache.trim_count();
        for _ in 0..1_000 {
            let _ = cache.lookup("k1").expect("lookup");
        }
        assert_eq!(cache.trim_count(), trims_before);
    }

    #[test]
    fn stores_within_trim_interval_do_not_resweep() {
        let cache = IdempotencyCache::new(Duration::from_secs(60));
        let trims_before = cache.trim_count();
        for i in 0..1_000 {
            cache
                .store(
                    format!("k{i}"),
                    IdempotencyRecord::json(StatusCode::OK, Bytes::from_static(b"{}")),
                )
                .expect("store");
        }
        assert_eq!(cache.trim_count(), trims_before);
        assert_eq!(cache.len(), 1_000);
    }

    #[test]
    fn from_env_clamps_zero_ttl_to_one_second() {
        // Hold ENV_MUTEX while mutating process-global env vars and route
        // cache construction through from_env_with_data_dir(temp) so a
        // concurrent test that has flipped the persistence flag cannot
        // cause this test to create `_idempotency/cache.db` under the
        // crate source tree.
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = crate::test_helpers::with_env_var("FLAPJACK_IDEMPOTENCY_TTL_SECS", "0");
        let cache = IdempotencyCache::from_env_with_data_dir(temp.path());
        let trims_before = cache.trim_count();
        for i in 0..100 {
            cache
                .store(
                    format!("k{i}"),
                    IdempotencyRecord::json(StatusCode::OK, Bytes::from_static(b"{}")),
                )
                .expect("store");
        }
        assert_eq!(cache.trim_count(), trims_before);
    }

    #[test]
    fn canonical_persistent_env_enables_persistence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = crate::test_helpers::with_env_var(PERSIST_ENV_CANONICAL, "1");
        let cache = IdempotencyCache::from_env_with_data_dir(temp.path());
        assert!(cache.persistence_path().is_some());
    }

    #[test]
    fn compatibility_persistent_env_still_enables_persistence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let _guard = crate::test_helpers::with_env_var(PERSIST_ENV_COMPAT, "1");
        let cache = IdempotencyCache::from_env_with_data_dir(temp.path());
        assert!(cache.persistence_path().is_some());
    }

    #[test]
    fn canonical_persistent_env_takes_precedence_over_compat_alias() {
        let temp = tempfile::tempdir().expect("tempdir");
        // Acquire ENV_MUTEX once and layer per-var restore guards underneath
        // so both vars are mutated atomically with respect to other tests.
        let _lock = crate::test_helpers::ENV_MUTEX
            .lock()
            .expect("env mutex poisoned");
        let _canonical = crate::test_helpers::EnvVarRestoreGuard::set(PERSIST_ENV_CANONICAL, "0");
        let _compat = crate::test_helpers::EnvVarRestoreGuard::set(PERSIST_ENV_COMPAT, "1");
        let cache = IdempotencyCache::from_env_with_data_dir(temp.path());
        assert!(
            cache.persistence_path().is_none(),
            "canonical flag must win when both flags are set"
        );
    }

    #[test]
    fn into_response_marks_replay() {
        let record = IdempotencyRecord::json(StatusCode::CREATED, Bytes::from_static(b"{\"a\":1}"));
        let response = record.into_response();
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response
                .headers()
                .get("x-flapjack-idempotency-replayed")
                .and_then(|v| v.to_str().ok()),
            Some("true"),
        );
    }

    #[test]
    fn composite_key_isolates_same_idempotency_key_across_application_and_index() {
        let cache = IdempotencyCache::new(Duration::from_secs(60));
        let record_a = IdempotencyRecord::json(StatusCode::OK, Bytes::from_static(b"{\"a\":1}"));
        let record_b =
            IdempotencyRecord::json(StatusCode::CREATED, Bytes::from_static(b"{\"b\":2}"));
        cache
            .store_scoped("app-a", "products", "same-key", record_a)
            .expect("store app-a");
        cache
            .store_scoped("app-b", "products", "same-key", record_b)
            .expect("store app-b");
        assert_eq!(
            cache
                .lookup_scoped("app-a", "products", "same-key")
                .expect("lookup app-a")
                .expect("app-a hit")
                .status,
            StatusCode::OK.as_u16()
        );
        assert_eq!(
            cache
                .lookup_scoped("app-b", "products", "same-key")
                .expect("lookup app-b")
                .expect("app-b hit")
                .status,
            StatusCode::CREATED.as_u16()
        );
        assert!(cache
            .lookup_scoped("app-a", "orders", "same-key")
            .expect("lookup app-a orders")
            .is_none());
    }

    #[test]
    fn memory_ttl_uses_wall_clock_elapsed_time() {
        let cache = IdempotencyCache::new(Duration::from_millis(25));
        cache
            .store_scoped(
                "app-a",
                "products",
                "k1",
                IdempotencyRecord::json(StatusCode::OK, Bytes::from_static(b"{}")),
            )
            .expect("store");
        assert!(cache
            .lookup_scoped("app-a", "products", "k1")
            .expect("lookup")
            .is_some());
        std::thread::sleep(Duration::from_millis(40));
        assert!(cache
            .lookup_scoped("app-a", "products", "k1")
            .expect("lookup")
            .is_none());
    }

    #[test]
    fn sqlite_restart_survives_then_expires_by_wall_clock_ttl() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("cache.db");
        let ttl = Duration::from_millis(250);
        let cache = IdempotencyCache::persistent(ttl, &db_path).expect("persistent cache");
        cache
            .store_scoped(
                "app-a",
                "products",
                "k1",
                IdempotencyRecord::json(StatusCode::OK, Bytes::from_static(b"{\"ok\":true}")),
            )
            .expect("store");
        drop(cache);

        let reopened = IdempotencyCache::persistent(ttl, &db_path).expect("reopen cache");
        assert!(reopened
            .lookup_scoped("app-a", "products", "k1")
            .expect("lookup")
            .is_some());
        std::thread::sleep(Duration::from_millis(320));
        assert!(reopened
            .lookup_scoped("app-a", "products", "k1")
            .expect("lookup")
            .is_none());
    }

    #[test]
    fn persistent_mode_uses_canonical_cache_db_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cache =
            IdempotencyCache::persistent_under_data_dir(Duration::from_secs(5), temp.path())
                .expect("persistent cache");
        let expected = temp.path().join("_idempotency").join("cache.db");
        assert_eq!(cache.persistence_path(), Some(expected.as_path()));
        assert!(expected.exists());
    }

    #[test]
    fn sqlite_store_surfaces_write_failures() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("cache.db");
        let cache =
            IdempotencyCache::persistent(Duration::from_secs(60), &db_path).expect("cache open");

        let conn = Connection::open(&db_path).expect("direct sqlite open");
        conn.execute("DROP TABLE idempotency_cache", [])
            .expect("drop cache table");

        let result = cache.store_scoped(
            "app-a",
            "products",
            "k1",
            IdempotencyRecord::json(StatusCode::OK, Bytes::from_static(b"{}")),
        );
        assert!(result.is_err(), "store must surface sqlite write failures");
    }

    #[test]
    fn sqlite_store_surfaces_expired_delete_failures() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("cache.db");
        let cache =
            IdempotencyCache::persistent(Duration::from_millis(1), &db_path).expect("cache open");
        cache
            .store_scoped(
                "app-a",
                "products",
                "k1",
                IdempotencyRecord::json(StatusCode::OK, Bytes::from_static(b"{}")),
            )
            .expect("initial store");
        std::thread::sleep(Duration::from_millis(3));

        let conn = Connection::open(&db_path).expect("direct sqlite open");
        conn.execute("DROP TABLE idempotency_cache", [])
            .expect("drop cache table");

        let result = cache.lookup_scoped("app-a", "products", "k1");
        assert!(
            result.is_err(),
            "lookup must surface sqlite delete failures when expiring stale records"
        );
    }
}
