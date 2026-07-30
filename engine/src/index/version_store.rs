use rusqlite::{params, types::ValueRef, Connection, OptionalExtension, Row};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Tenant-generation directory containing the durable replication version database
/// and any SQLite companion files created beside it.
pub const VERSION_STORE_DIR: &str = "version_store";

const VERSION_STORE_DATABASE: &str = "versions.sqlite3";

#[derive(Debug, Error)]
pub enum VersionStoreError {
    #[error("failed to prepare version-store directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("version-store SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("version-store injected failure: {0}")]
    Injected(String),
}

pub type Result<T> = std::result::Result<T, VersionStoreError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionRecord {
    pub timestamp_ms: u64,
    pub node_id: String,
    pub tombstone: bool,
    pub oplog_seq: u64,
}

impl VersionRecord {
    pub fn new(
        timestamp_ms: u64,
        node_id: impl Into<String>,
        tombstone: bool,
        oplog_seq: u64,
    ) -> Self {
        Self {
            timestamp_ms,
            node_id: node_id.into(),
            tombstone,
            oplog_seq,
        }
    }

    fn tuple_is_newer_than(&self, other: &Self) -> bool {
        tuple_is_strictly_newer(
            (self.timestamp_ms, self.node_id.as_str()),
            (other.timestamp_ms, other.node_id.as_str()),
        )
    }

    fn with_oplog_seq(&self, oplog_seq: u64) -> Self {
        Self {
            timestamp_ms: self.timestamp_ms,
            node_id: self.node_id.clone(),
            tombstone: self.tombstone,
            oplog_seq,
        }
    }
}

/// Return whether the candidate conflict tuple strictly supersedes the existing tuple.
///
/// All durable writes and transient replication admission use this owner so
/// equal tuples are handled consistently.
pub fn tuple_is_strictly_newer(candidate: (u64, &str), existing: (u64, &str)) -> bool {
    candidate > existing
}

/// Durable per-object replication version state owned by one tenant generation.
pub struct VersionStore {
    connection: Connection,
}

impl VersionStore {
    pub fn database_path(tenant_generation_path: &Path) -> PathBuf {
        tenant_generation_path
            .join(VERSION_STORE_DIR)
            .join(VERSION_STORE_DATABASE)
    }

    pub fn open(tenant_generation_path: &Path) -> Result<Self> {
        let path = Self::database_path(tenant_generation_path);
        std::fs::create_dir_all(
            path.parent()
                .expect("version-store database path always has a parent"),
        )?;
        let connection = Connection::open(path)?;
        // `object_versions` remains the sole conflict owner. The task table is
        // transient crash evidence that prevents a B6 admission retry after its
        // oplog segment has already been reclaimed.
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS object_versions (
                object_id TEXT PRIMARY KEY NOT NULL,
                timestamp_ms BLOB NOT NULL,
                node_id TEXT NOT NULL,
                tombstone INTEGER NOT NULL,
                oplog_seq BLOB NOT NULL
            );
            CREATE TABLE IF NOT EXISTS finalized_write_tasks (
                task_id TEXT PRIMARY KEY NOT NULL
            );",
        )?;
        Ok(Self { connection })
    }

    /// Insert an unseen object or replace its row only for a strictly newer
    /// lexicographic `(timestamp_ms, node_id)` tuple.
    pub fn upsert(&self, object_id: &str, version: &VersionRecord) -> Result<bool> {
        self.upsert_with_equal_tuple_replacement(object_id, version, false)
    }

    /// Atomically apply the object-version evidence produced by one committed
    /// oplog receipt batch. Empty batches and config-only receipts are explicit
    /// no-ops because they contain no per-object conflict state.
    pub fn apply_receipts(&self, receipts: &[crate::index::oplog::OpLogReceipt]) -> Result<usize> {
        self.apply_receipts_with_hook(receipts, |_| Ok(()))
    }

    pub(crate) fn apply_receipts_with_hook(
        &self,
        receipts: &[crate::index::oplog::OpLogReceipt],
        after_receipt_statement: impl FnMut(usize) -> Result<()>,
    ) -> Result<usize> {
        self.apply_receipts_and_tasks_with_hook(receipts, &[], after_receipt_statement)
    }

    pub(crate) fn apply_receipts_and_tasks_with_hook(
        &self,
        receipts: &[crate::index::oplog::OpLogReceipt],
        finalized_task_ids: &[&str],
        mut after_receipt_statement: impl FnMut(usize) -> Result<()>,
    ) -> Result<usize> {
        if finalized_task_ids.is_empty()
            && !receipts.iter().any(|receipt| receipt.object_id.is_some())
        {
            return Ok(0);
        }

        let transaction = self.connection.unchecked_transaction()?;
        for task_id in finalized_task_ids {
            transaction.execute(
                "INSERT OR IGNORE INTO finalized_write_tasks (task_id) VALUES (?1)",
                [task_id],
            )?;
        }
        let mut changed_rows = 0;
        let mut receipt_statement_count = 0;
        for receipt in receipts {
            let Some(object_id) = receipt.object_id.as_deref() else {
                continue;
            };
            let version = VersionRecord::new(
                receipt.timestamp_ms,
                &receipt.node_id,
                receipt.is_tombstone,
                receipt.seq,
            );
            changed_rows += usize::from(execute_version_upsert(
                &transaction,
                object_id,
                &version,
                false,
            )?);
            receipt_statement_count += 1;
            after_receipt_statement(receipt_statement_count)?;
        }
        transaction.commit()?;
        Ok(changed_rows)
    }

    pub(crate) fn contains_finalized_task(&self, task_id: &str) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM finalized_write_tasks WHERE task_id = ?1
                )",
                [task_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub(crate) fn remove_finalized_tasks(&self, task_ids: &[&str]) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        for task_id in task_ids {
            transaction.execute(
                "DELETE FROM finalized_write_tasks WHERE task_id = ?1",
                [task_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn clear_finalized_tasks(&self) -> Result<()> {
        self.connection
            .execute("DELETE FROM finalized_write_tasks", [])?;
        Ok(())
    }

    fn upsert_with_equal_tuple_replacement(
        &self,
        object_id: &str,
        version: &VersionRecord,
        replace_equal_tuple: bool,
    ) -> Result<bool> {
        execute_version_upsert(&self.connection, object_id, version, replace_equal_tuple)
    }

    pub fn get(&self, object_id: &str) -> Result<Option<VersionRecord>> {
        self.connection
            .query_row(
                "SELECT timestamp_ms, node_id, tombstone, oplog_seq
                 FROM object_versions
                 WHERE object_id = ?1",
                [object_id],
                |row| row_to_version_record(row, 0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Merge destination-generation evidence into a staged generation.
    ///
    /// The newer conflict tuple wins. Equal tuples take the destination row
    /// because `oplog_seq` belongs to the destination oplog's local sequence
    /// domain, which replacement publication installs alongside this store. Any
    /// staged-winning row is restamped at the replacement watermark for the same
    /// destination-local reason.
    pub fn merge_destination_evidence(
        &self,
        destination: &Self,
        staged_winner_oplog_seq: u64,
    ) -> Result<()> {
        let destination_versions = destination.read_all()?;
        for (object_id, staged_record) in self.read_all()? {
            match destination_versions.get(&object_id) {
                Some(destination_record)
                    if !staged_record.tuple_is_newer_than(destination_record) => {}
                _ => {
                    self.replace_existing_metadata(
                        &object_id,
                        &staged_record.with_oplog_seq(staged_winner_oplog_seq),
                    )?;
                }
            }
        }
        for (object_id, record) in destination_versions {
            self.upsert_with_equal_tuple_replacement(&object_id, &record, true)?;
        }
        Ok(())
    }

    fn replace_existing_metadata(&self, object_id: &str, version: &VersionRecord) -> Result<()> {
        let timestamp_ms = encode_u64(version.timestamp_ms);
        let oplog_seq = encode_u64(version.oplog_seq);
        self.connection.execute(
            "UPDATE object_versions
             SET tombstone = ?2, oplog_seq = ?3
             WHERE object_id = ?1
                AND timestamp_ms = ?4
                AND node_id = ?5",
            params![
                object_id,
                version.tombstone,
                oplog_seq.as_slice(),
                timestamp_ms.as_slice(),
                version.node_id,
            ],
        )?;
        Ok(())
    }

    fn read_all(&self) -> Result<BTreeMap<String, VersionRecord>> {
        let mut rows = self.connection.prepare(
            "SELECT object_id, timestamp_ms, node_id, tombstone, oplog_seq
             FROM object_versions",
        )?;
        let versions = rows.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row_to_version_record(row, 1)?))
        })?;
        let mut records = BTreeMap::new();
        for version in versions {
            let (object_id, record) = version?;
            records.insert(object_id, record);
        }
        Ok(records)
    }
}

fn execute_version_upsert(
    connection: &Connection,
    object_id: &str,
    version: &VersionRecord,
    replace_equal_tuple: bool,
) -> Result<bool> {
    let timestamp_ms = encode_u64(version.timestamp_ms);
    let oplog_seq = encode_u64(version.oplog_seq);
    let changed = connection.execute(
        "INSERT INTO object_versions (
            object_id, timestamp_ms, node_id, tombstone, oplog_seq
        ) VALUES (?1, ?2, ?3, ?4, ?5)
        ON CONFLICT(object_id) DO UPDATE SET
            timestamp_ms = excluded.timestamp_ms,
            node_id = excluded.node_id,
            tombstone = excluded.tombstone,
            oplog_seq = excluded.oplog_seq
        WHERE excluded.timestamp_ms > object_versions.timestamp_ms
            OR (
                excluded.timestamp_ms = object_versions.timestamp_ms
                AND (
                    excluded.node_id > object_versions.node_id
                    OR (
                        ?6
                        AND excluded.node_id = object_versions.node_id
                    )
                )
            )",
        params![
            object_id,
            timestamp_ms.as_slice(),
            version.node_id,
            version.tombstone,
            oplog_seq.as_slice(),
            replace_equal_tuple
        ],
    )?;
    Ok(changed == 1)
}

fn encode_u64(value: u64) -> [u8; 8] {
    value.to_be_bytes()
}

fn row_to_version_record(
    row: &Row<'_>,
    first_field_index: usize,
) -> rusqlite::Result<VersionRecord> {
    Ok(VersionRecord {
        timestamp_ms: decode_u64(row, first_field_index)?,
        node_id: row.get(first_field_index + 1)?,
        tombstone: row.get(first_field_index + 2)?,
        oplog_seq: decode_u64(row, first_field_index + 3)?,
    })
}

fn decode_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    match row.get_ref(index)? {
        ValueRef::Blob(bytes) if bytes.len() == 8 => Ok(u64::from_be_bytes(
            bytes.try_into().expect("length checked"),
        )),
        ValueRef::Integer(value) if value >= 0 => Ok(value as u64),
        value => Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            value.data_type(),
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "version-store u64 value must be an 8-byte blob or non-negative integer",
            )),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{VersionRecord, VersionStore};
    use crate::index::oplog::OpLogReceipt;
    use tempfile::TempDir;

    fn assert_record(store: &VersionStore, expected: &VersionRecord) {
        assert_eq!(
            store.get("object-1").unwrap().as_ref(),
            Some(expected),
            "stored row must exactly match the winning tuple and its metadata"
        );
    }

    fn receipt(
        seq: u64,
        object_id: Option<&str>,
        timestamp_ms: u64,
        node_id: &str,
        is_tombstone: bool,
    ) -> OpLogReceipt {
        OpLogReceipt {
            seq,
            object_id: object_id.map(str::to_string),
            timestamp_ms,
            node_id: node_id.to_string(),
            is_tombstone,
        }
    }

    #[test]
    fn receipt_batch_is_atomic_and_rolls_back_all_rows_on_error() {
        let temp_dir = TempDir::new().unwrap();
        let store = VersionStore::open(temp_dir.path()).unwrap();
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_second_receipt
                 BEFORE INSERT ON object_versions
                 WHEN NEW.object_id = 'reject-me'
                 BEGIN
                     SELECT RAISE(ABORT, 'injected receipt failure');
                 END;",
            )
            .unwrap();

        let result = store.apply_receipts(&[
            receipt(1, Some("would-be-partial"), 1000, "node-a", false),
            receipt(2, Some("reject-me"), 2000, "node-b", true),
        ]);

        assert!(
            result.is_err(),
            "the injected second-row failure must surface"
        );
        assert_eq!(
            store.get("would-be-partial").unwrap(),
            None,
            "the first row must roll back with the failed receipt transaction"
        );
        assert_eq!(store.get("reject-me").unwrap(), None);
    }

    #[test]
    fn identical_receipt_batch_replay_is_idempotent() {
        let temp_dir = TempDir::new().unwrap();
        let store = VersionStore::open(temp_dir.path()).unwrap();
        let receipts = [
            receipt(7, Some("object-1"), 7000, "node-a", false),
            receipt(8, Some("object-2"), 8000, "node-b", true),
        ];

        assert_eq!(store.apply_receipts(&receipts).unwrap(), 2);
        assert_eq!(store.apply_receipts(&receipts).unwrap(), 0);
        assert_eq!(
            store.get("object-1").unwrap(),
            Some(VersionRecord::new(7000, "node-a", false, 7))
        );
        assert_eq!(
            store.get("object-2").unwrap(),
            Some(VersionRecord::new(8000, "node-b", true, 8))
        );
    }

    #[test]
    fn empty_and_config_only_receipt_batches_are_explicit_noops() {
        let temp_dir = TempDir::new().unwrap();
        let store = VersionStore::open(temp_dir.path()).unwrap();

        assert_eq!(store.apply_receipts(&[]).unwrap(), 0);
        assert_eq!(
            store
                .apply_receipts(&[receipt(1, None, 1000, "node-a", false)])
                .unwrap(),
            0
        );
        assert!(
            store.read_all().unwrap().is_empty(),
            "receipts without an object ID must not fabricate version rows"
        );
    }

    #[test]
    fn version_store_replaces_row_only_for_strictly_newer_tuple() {
        let temp_dir = TempDir::new().unwrap();
        let store = VersionStore::open(temp_dir.path()).unwrap();
        let initial = VersionRecord::new(100, "node-b", false, 7);
        assert!(store.upsert("object-1", &initial).unwrap());
        assert_record(&store, &initial);

        let newer_timestamp = VersionRecord::new(101, "node-a", true, 8);
        assert!(store.upsert("object-1", &newer_timestamp).unwrap());
        assert_record(&store, &newer_timestamp);

        let older_timestamp = VersionRecord::new(99, "node-z", false, 9);
        assert!(!store.upsert("object-1", &older_timestamp).unwrap());
        assert_record(&store, &newer_timestamp);

        let higher_node_id = VersionRecord::new(101, "node-z", false, 10);
        assert!(store.upsert("object-1", &higher_node_id).unwrap());
        assert_record(&store, &higher_node_id);

        let lower_node_id = VersionRecord::new(101, "node-y", true, 11);
        assert!(!store.upsert("object-1", &lower_node_id).unwrap());
        assert_record(&store, &higher_node_id);

        let identical_tuple_different_metadata = VersionRecord::new(101, "node-z", true, 12);
        assert!(!store
            .upsert("object-1", &identical_tuple_different_metadata)
            .unwrap());
        assert_record(&store, &higher_node_id);
    }

    #[test]
    fn version_store_rows_survive_reopen() {
        let temp_dir = TempDir::new().unwrap();
        let first = VersionRecord::new(42, "node-a", false, 3);
        let second = VersionRecord::new(84, "node-b", true, 9);

        {
            let store = VersionStore::open(temp_dir.path()).unwrap();
            assert!(store.upsert("object-1", &first).unwrap());
            assert!(store.upsert("object-2", &second).unwrap());
        }

        let reopened = VersionStore::open(temp_dir.path()).unwrap();
        assert_eq!(reopened.get("object-1").unwrap(), Some(first));
        assert_eq!(reopened.get("object-2").unwrap(), Some(second));
        assert_eq!(reopened.get("missing").unwrap(), None);
    }

    #[test]
    fn version_store_preserves_unsigned_timestamp_and_oplog_seq_boundaries() {
        let temp_dir = TempDir::new().unwrap();
        let store = VersionStore::open(temp_dir.path()).unwrap();
        let max_signed = i64::MAX as u64;
        let past_signed = max_signed + 1;
        let signed_boundary = VersionRecord::new(max_signed, "node-a", false, max_signed);
        let unsigned_boundary = VersionRecord::new(past_signed, "node-b", true, past_signed);

        assert!(store.upsert("signed-boundary", &signed_boundary).unwrap());
        assert!(store
            .upsert("unsigned-boundary", &unsigned_boundary)
            .unwrap());

        assert_eq!(store.get("signed-boundary").unwrap(), Some(signed_boundary));
        assert_eq!(
            store.get("unsigned-boundary").unwrap(),
            Some(unsigned_boundary)
        );
    }

    #[test]
    fn destination_alignment_uses_newest_tuple_and_destination_metadata_for_equal_tuple() {
        let staged_dir = TempDir::new().unwrap();
        let destination_dir = TempDir::new().unwrap();
        let staged = VersionStore::open(staged_dir.path()).unwrap();
        let destination = VersionStore::open(destination_dir.path()).unwrap();

        let staged_equal = VersionRecord::new(100, "node-a", false, 1);
        let destination_equal = VersionRecord::new(100, "node-a", true, 9);
        let staged_newer = VersionRecord::new(200, "node-z", false, 2);
        let destination_older = VersionRecord::new(199, "node-z", true, 10);
        let staged_older = VersionRecord::new(300, "node-a", false, 3);
        let destination_newer = VersionRecord::new(300, "node-b", true, 11);
        let staged_only = VersionRecord::new(400, "node-a", false, 4);
        let destination_only = VersionRecord::new(500, "node-a", true, 12);

        for (object_id, record) in [
            ("equal", &staged_equal),
            ("staged-newer", &staged_newer),
            ("destination-newer", &staged_older),
            ("staged-only", &staged_only),
        ] {
            assert!(staged.upsert(object_id, record).unwrap());
        }
        for (object_id, record) in [
            ("equal", &destination_equal),
            ("staged-newer", &destination_older),
            ("destination-newer", &destination_newer),
            ("destination-only", &destination_only),
        ] {
            assert!(destination.upsert(object_id, record).unwrap());
        }

        staged.merge_destination_evidence(&destination, 99).unwrap();

        assert_eq!(staged.get("equal").unwrap(), Some(destination_equal));
        assert_eq!(
            staged.get("staged-newer").unwrap(),
            Some(staged_newer.with_oplog_seq(99))
        );
        assert_eq!(
            staged.get("destination-newer").unwrap(),
            Some(destination_newer)
        );
        assert_eq!(
            staged.get("staged-only").unwrap(),
            Some(staged_only.with_oplog_seq(99))
        );
        assert_eq!(
            staged.get("destination-only").unwrap(),
            Some(destination_only)
        );
    }
}
