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
    #[error("ambiguous replication version proof: {0}")]
    AmbiguousProof(String),
}

pub type Result<T> = std::result::Result<T, VersionStoreError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionRecord {
    pub timestamp_ms: u64,
    pub node_id: String,
    pub tombstone: bool,
    pub oplog_seq: u64,
    pub origin_seq: Option<u64>,
    pub effect_digest: Option<[u8; 32]>,
}

/// Semantic ordering between one incoming replication effect and durable proof.
///
/// Base LWW tuples order first. Equal base tuples are comparable only when both
/// sides carry complete source sequence and effect proof; equal source sequence
/// is exact only for the same tombstone and digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionProofComparison {
    Newer,
    Older,
    Exact,
    Ambiguous,
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
            origin_seq: None,
            effect_digest: None,
        }
    }

    pub fn with_origin_proof(mut self, origin_seq: u64, effect_digest: [u8; 32]) -> Self {
        self.origin_seq = Some(origin_seq);
        self.effect_digest = Some(effect_digest);
        self
    }

    fn with_origin_evidence(
        mut self,
        origin_seq: Option<u64>,
        effect_digest: Option<[u8; 32]>,
    ) -> Self {
        self.origin_seq = origin_seq;
        self.effect_digest = effect_digest;
        self
    }

    pub fn compare_replication_proof(&self, other: &Self) -> VersionProofComparison {
        match (self.timestamp_ms, self.node_id.as_str())
            .cmp(&(other.timestamp_ms, other.node_id.as_str()))
        {
            std::cmp::Ordering::Greater => VersionProofComparison::Newer,
            std::cmp::Ordering::Less => VersionProofComparison::Older,
            std::cmp::Ordering::Equal => match (
                self.origin_seq,
                self.effect_digest,
                other.origin_seq,
                other.effect_digest,
            ) {
                (
                    Some(candidate_seq),
                    Some(candidate_digest),
                    Some(existing_seq),
                    Some(existing_digest),
                ) => match candidate_seq.cmp(&existing_seq) {
                    std::cmp::Ordering::Greater => VersionProofComparison::Newer,
                    std::cmp::Ordering::Less => VersionProofComparison::Older,
                    std::cmp::Ordering::Equal
                        if self.tombstone == other.tombstone
                            && candidate_digest == existing_digest =>
                    {
                        VersionProofComparison::Exact
                    }
                    std::cmp::Ordering::Equal => VersionProofComparison::Ambiguous,
                },
                _ => VersionProofComparison::Ambiguous,
            },
        }
    }

    fn with_oplog_seq(&self, oplog_seq: u64) -> Self {
        Self {
            timestamp_ms: self.timestamp_ms,
            node_id: self.node_id.clone(),
            tombstone: self.tombstone,
            oplog_seq,
            origin_seq: self.origin_seq,
            effect_digest: self.effect_digest,
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
                oplog_seq BLOB NOT NULL,
                origin_seq BLOB,
                effect_digest BLOB
            );
            CREATE TABLE IF NOT EXISTS finalized_write_tasks (
                task_id TEXT PRIMARY KEY NOT NULL
            );",
        )?;
        ensure_nullable_column(&connection, "origin_seq", "BLOB")?;
        ensure_nullable_column(&connection, "effect_digest", "BLOB")?;
        Ok(Self { connection })
    }

    /// Insert an unseen object or replace its row only for a strictly newer
    /// proven `(timestamp_ms, node_id, origin_seq)` tuple. Base tuples still
    /// order independently; equal legacy tuples do not silently replace.
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
            )
            .with_origin_evidence(receipt.origin_seq, receipt.effect_digest);
            let changed = match read_version_record(&transaction, object_id)? {
                Some(existing) => match version.compare_replication_proof(&existing) {
                    VersionProofComparison::Newer => {
                        execute_version_upsert(&transaction, object_id, &version, false)?
                    }
                    VersionProofComparison::Older | VersionProofComparison::Exact => false,
                    VersionProofComparison::Ambiguous => {
                        return Err(VersionStoreError::AmbiguousProof(format!(
                            "object {object_id} has conflicting equal origin evidence"
                        )));
                    }
                },
                None => execute_version_upsert(&transaction, object_id, &version, false)?,
            };
            changed_rows += usize::from(changed);
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
        read_version_record(&self.connection, object_id)
    }

    /// Merge destination-generation evidence into a staged generation.
    ///
    /// The newer proven conflict tuple wins. Exactly equal proof takes the destination row
    /// because `oplog_seq` belongs to the destination oplog's local sequence
    /// domain, which replacement publication installs alongside this store. Any
    /// staged-winning row is restamped at the replacement watermark for the same
    /// destination-local reason.
    pub fn merge_destination_evidence(
        &self,
        destination: &Self,
        staged_winner_oplog_seq: u64,
    ) -> Result<()> {
        let mut destination_versions = destination.read_all()?;
        let mut merged_versions = BTreeMap::new();

        // Resolve every semantic conflict before opening the write transaction.
        // An ambiguous row therefore cannot restamp any unrelated staged row.
        for (object_id, staged_record) in self.read_all()? {
            let winner = match destination_versions.remove(&object_id) {
                Some(destination_record) => {
                    match staged_record.compare_replication_proof(&destination_record) {
                        VersionProofComparison::Newer => {
                            staged_record.with_oplog_seq(staged_winner_oplog_seq)
                        }
                        VersionProofComparison::Older | VersionProofComparison::Exact => {
                            destination_record
                        }
                        VersionProofComparison::Ambiguous => {
                            return Err(VersionStoreError::AmbiguousProof(format!(
                                "object {object_id} differs at an equal origin tuple"
                            )));
                        }
                    }
                }
                None => staged_record.with_oplog_seq(staged_winner_oplog_seq),
            };
            merged_versions.insert(object_id, winner);
        }
        merged_versions.extend(destination_versions);

        let transaction = self.connection.unchecked_transaction()?;
        for (object_id, record) in merged_versions {
            execute_version_upsert(&transaction, &object_id, &record, true)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Resolve every legacy NULL proof from one strict retained-oplog inventory.
    pub(crate) fn resolve_legacy_proofs_from_retained_entries(
        &self,
        retained: &BTreeMap<u64, crate::index::oplog::OpLogEntry>,
    ) -> Result<usize> {
        let mut resolved = Vec::new();
        for (object_id, record) in self.read_all()? {
            match (record.origin_seq, record.effect_digest) {
                (Some(_), Some(_)) => continue,
                (Some(_), None) | (None, Some(_)) => {
                    return Err(VersionStoreError::AmbiguousProof(format!(
                        "object {object_id} has partial origin proof"
                    )));
                }
                (None, None) => {}
            }
            let entry = retained
                .get(&record.oplog_seq)
                .ok_or_else(|| missing_retained_proof(&object_id))?;
            let expected_op = if record.tombstone { "delete" } else { "upsert" };
            if entry.op_type != expected_op
                || entry.timestamp_ms != record.timestamp_ms
                || entry.node_id != record.node_id
                || crate::index::oplog::payload_object_id(&entry.payload)
                    != Some(object_id.as_str())
            {
                return Err(missing_retained_proof(&object_id));
            }
            let origin_seq = crate::index::oplog::replication_origin_seq(&entry.payload)
                .map_err(|error| retained_proof_error(&object_id, error))?
                .ok_or_else(|| missing_retained_proof(&object_id))?;
            let effect_digest =
                crate::index::oplog::operation_effect_digest(&entry.op_type, &entry.payload)
                    .ok_or_else(|| missing_retained_proof(&object_id))?;
            resolved.push((
                object_id,
                record.with_origin_proof(origin_seq, effect_digest),
            ));
        }
        if resolved.is_empty() {
            return Ok(0);
        }
        let transaction = self.connection.unchecked_transaction()?;
        for (object_id, record) in &resolved {
            let timestamp_ms = encode_u64(record.timestamp_ms);
            let oplog_seq = encode_u64(record.oplog_seq);
            let origin_seq = encode_u64(
                record
                    .origin_seq
                    .expect("resolved legacy proof always has an origin sequence"),
            );
            let effect_digest = record
                .effect_digest
                .expect("resolved legacy proof always has an effect digest");
            let changed = transaction.execute(
                "UPDATE object_versions SET origin_seq = ?1, effect_digest = ?2
                 WHERE object_id = ?3
                   AND timestamp_ms = ?4
                   AND node_id = ?5
                   AND tombstone = ?6
                   AND oplog_seq = ?7
                   AND origin_seq IS NULL
                   AND effect_digest IS NULL",
                params![
                    origin_seq.as_slice(),
                    effect_digest.as_slice(),
                    object_id,
                    timestamp_ms.as_slice(),
                    record.node_id,
                    record.tombstone,
                    oplog_seq.as_slice(),
                ],
            )?;
            if changed != 1 {
                return Err(VersionStoreError::AmbiguousProof(format!(
                    "object {object_id} changed during legacy proof resolution"
                )));
            }
        }
        transaction.commit()?;
        Ok(resolved.len())
    }

    fn read_all(&self) -> Result<BTreeMap<String, VersionRecord>> {
        let mut rows = self.connection.prepare(
            "SELECT object_id, timestamp_ms, node_id, tombstone, oplog_seq,
                    origin_seq, effect_digest
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

fn retained_proof_error(object_id: &str, error: impl std::fmt::Display) -> VersionStoreError {
    VersionStoreError::AmbiguousProof(format!(
        "object {object_id} retained oplog proof is unreadable: {error}"
    ))
}

fn missing_retained_proof(object_id: &str) -> VersionStoreError {
    VersionStoreError::AmbiguousProof(format!(
        "object {object_id} has legacy NULL proof without exact retained oplog evidence"
    ))
}

fn read_version_record(connection: &Connection, object_id: &str) -> Result<Option<VersionRecord>> {
    connection
        .query_row(
            "SELECT timestamp_ms, node_id, tombstone, oplog_seq,
                    origin_seq, effect_digest
             FROM object_versions
             WHERE object_id = ?1",
            [object_id],
            |row| row_to_version_record(row, 0),
        )
        .optional()
        .map_err(Into::into)
}

fn execute_version_upsert(
    connection: &Connection,
    object_id: &str,
    version: &VersionRecord,
    replace_equal_tuple: bool,
) -> Result<bool> {
    let timestamp_ms = encode_u64(version.timestamp_ms);
    let oplog_seq = encode_u64(version.oplog_seq);
    let origin_seq = version.origin_seq.map(encode_u64);
    let changed = connection.execute(
        "INSERT INTO object_versions (
            object_id, timestamp_ms, node_id, tombstone, oplog_seq,
            origin_seq, effect_digest
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(object_id) DO UPDATE SET
            timestamp_ms = excluded.timestamp_ms,
            node_id = excluded.node_id,
            tombstone = excluded.tombstone,
            oplog_seq = excluded.oplog_seq,
            origin_seq = excluded.origin_seq,
            effect_digest = excluded.effect_digest
        WHERE excluded.timestamp_ms > object_versions.timestamp_ms
            OR (
                excluded.timestamp_ms = object_versions.timestamp_ms
                AND (
                    excluded.node_id > object_versions.node_id
                    OR (
                        excluded.node_id = object_versions.node_id
                        AND (
                            (
                                excluded.origin_seq IS NOT NULL
                                AND object_versions.origin_seq IS NOT NULL
                                AND excluded.origin_seq > object_versions.origin_seq
                            )
                            OR (
                                ?8
                                AND (
                                    (
                                        excluded.origin_seq IS NULL
                                        AND object_versions.origin_seq IS NULL
                                    )
                                    OR excluded.origin_seq = object_versions.origin_seq
                                )
                            )
                        )
                    )
                )
            )",
        params![
            object_id,
            timestamp_ms.as_slice(),
            version.node_id,
            version.tombstone,
            oplog_seq.as_slice(),
            origin_seq.as_ref().map(|value| value.as_slice()),
            version.effect_digest.as_ref().map(|value| value.as_slice()),
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
        origin_seq: decode_optional_u64(row, first_field_index + 4)?,
        effect_digest: decode_optional_digest(row, first_field_index + 5)?,
    })
}

fn ensure_nullable_column(connection: &Connection, name: &str, sql_type: &str) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(object_versions)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == name {
            return Ok(());
        }
    }
    drop(statement);
    connection.execute(
        &format!("ALTER TABLE object_versions ADD COLUMN {name} {sql_type}"),
        [],
    )?;
    Ok(())
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

fn decode_optional_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    match row.get_ref(index)? {
        ValueRef::Null => Ok(None),
        ValueRef::Blob(bytes) if bytes.len() == 8 => Ok(Some(u64::from_be_bytes(
            bytes.try_into().expect("length checked"),
        ))),
        ValueRef::Integer(value) if value >= 0 => Ok(Some(value as u64)),
        value => Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            value.data_type(),
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "version-store optional u64 value must be NULL, an 8-byte blob, or a non-negative integer",
            )),
        )),
    }
}

fn decode_optional_digest(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<[u8; 32]>> {
    match row.get_ref(index)? {
        ValueRef::Null => Ok(None),
        ValueRef::Blob(bytes) if bytes.len() == 32 => {
            Ok(Some(bytes.try_into().expect("length checked")))
        }
        value => Err(rusqlite::Error::FromSqlConversionFailure(
            index,
            value.data_type(),
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "version-store effect digest must be NULL or a 32-byte blob",
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
            origin_seq: object_id.map(|_| seq),
            effect_digest: object_id.map(|_| [seq as u8; 32]),
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
            Some(VersionRecord::new(7000, "node-a", false, 7).with_origin_proof(7, [7; 32]))
        );
        assert_eq!(
            store.get("object-2").unwrap(),
            Some(VersionRecord::new(8000, "node-b", true, 8).with_origin_proof(8, [8; 32]))
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
    fn replication_origin_proof_old_schema_rows_migrate_with_null_proof() {
        let temp_dir = TempDir::new().unwrap();
        let database_path = VersionStore::database_path(temp_dir.path());
        std::fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let legacy_connection = rusqlite::Connection::open(&database_path).unwrap();
        legacy_connection
            .execute_batch(
                "CREATE TABLE object_versions (
                    object_id TEXT PRIMARY KEY NOT NULL,
                    timestamp_ms BLOB NOT NULL,
                    node_id TEXT NOT NULL,
                    tombstone INTEGER NOT NULL,
                    oplog_seq BLOB NOT NULL
                );",
            )
            .unwrap();
        legacy_connection
            .execute(
                "INSERT INTO object_versions (
                    object_id, timestamp_ms, node_id, tombstone, oplog_seq
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    "legacy-object",
                    42_u64.to_be_bytes().as_slice(),
                    "legacy-node",
                    false,
                    7_u64.to_be_bytes().as_slice()
                ],
            )
            .unwrap();
        drop(legacy_connection);

        let store = VersionStore::open(temp_dir.path()).unwrap();
        let columns = store
            .connection
            .prepare("PRAGMA table_info(object_versions)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "origin_seq"));
        assert!(columns.iter().any(|column| column == "effect_digest"));

        let proof = store
            .connection
            .query_row(
                "SELECT origin_seq, effect_digest
                 FROM object_versions
                 WHERE object_id = 'legacy-object'",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<Vec<u8>>>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(proof, (None, None));
        let legacy = store.get("legacy-object").unwrap().unwrap();
        assert_eq!(legacy.origin_seq, None);
        assert_eq!(legacy.effect_digest, None);
    }

    #[test]
    fn replication_origin_proof_higher_source_seq_wins_equal_base_tuple() {
        let temp_dir = TempDir::new().unwrap();
        let store = VersionStore::open(temp_dir.path()).unwrap();
        let lower = VersionRecord::new(100, "node-a", false, 1).with_origin_proof(40, [0x40; 32]);
        let higher = VersionRecord::new(100, "node-a", true, 2).with_origin_proof(41, [0x41; 32]);

        assert!(store.upsert("object-1", &lower).unwrap());
        assert!(store.upsert("object-1", &higher).unwrap());
        assert!(!store.upsert("object-1", &lower).unwrap());
        assert_eq!(store.get("object-1").unwrap(), Some(higher));
    }

    #[test]
    fn replication_origin_proof_destination_restamp_preserves_source_proof() {
        let staged_dir = TempDir::new().unwrap();
        let destination_dir = TempDir::new().unwrap();
        let staged = VersionStore::open(staged_dir.path()).unwrap();
        let destination = VersionStore::open(destination_dir.path()).unwrap();
        let staged_newer =
            VersionRecord::new(100, "node-a", false, 3).with_origin_proof(9, [0x55; 32]);
        let destination_older =
            VersionRecord::new(100, "node-a", true, 7).with_origin_proof(8, [0x44; 32]);
        assert!(staged.upsert("object-1", &staged_newer).unwrap());
        assert!(destination.upsert("object-1", &destination_older).unwrap());

        staged.merge_destination_evidence(&destination, 99).unwrap();

        assert_eq!(
            staged.get("object-1").unwrap(),
            Some(staged_newer.with_oplog_seq(99))
        );
    }

    #[test]
    fn replication_origin_proof_generation_merge_rejects_ambiguous_equal_proof_atomically() {
        let cases = [
            (
                VersionRecord::new(100, "node-a", false, 1).with_origin_proof(17, [0x11; 32]),
                VersionRecord::new(100, "node-a", true, 9).with_origin_proof(17, [0x22; 32]),
                "tombstone and digest mismatch",
            ),
            (
                VersionRecord::new(100, "node-a", false, 1),
                VersionRecord::new(100, "node-a", false, 9).with_origin_proof(17, [0x11; 32]),
                "legacy staged proof against proven destination",
            ),
            (
                VersionRecord::new(100, "node-a", false, 1).with_origin_proof(17, [0x11; 32]),
                VersionRecord::new(100, "node-a", false, 9),
                "proven staged proof against legacy destination",
            ),
        ];

        for (staged_conflict, destination_conflict, case) in cases {
            let staged_dir = TempDir::new().unwrap();
            let destination_dir = TempDir::new().unwrap();
            let staged = VersionStore::open(staged_dir.path()).unwrap();
            let destination = VersionStore::open(destination_dir.path()).unwrap();
            let staged_would_restamp =
                VersionRecord::new(300, "node-z", false, 2).with_origin_proof(30, [0x30; 32]);
            let destination_older =
                VersionRecord::new(299, "node-z", true, 8).with_origin_proof(29, [0x29; 32]);

            assert!(staged.upsert("conflict", &staged_conflict).unwrap());
            assert!(staged
                .upsert("would-restamp", &staged_would_restamp)
                .unwrap());
            assert!(destination
                .upsert("conflict", &destination_conflict)
                .unwrap());
            assert!(destination
                .upsert("would-restamp", &destination_older)
                .unwrap());

            let error = staged
                .merge_destination_evidence(&destination, 99)
                .expect_err("ambiguous equal proof must refuse generation publication");

            assert!(
                error.to_string().contains("ambiguous"),
                "{case} must produce an explicit ambiguity error: {error}"
            );
            assert_eq!(
                staged.get("conflict").unwrap(),
                Some(staged_conflict),
                "{case} must leave the conflicting staged row unchanged"
            );
            assert_eq!(
                staged.get("would-restamp").unwrap(),
                Some(staged_would_restamp),
                "{case} must fail before restamping any other staged row"
            );
        }
    }

    #[test]
    fn replication_origin_proof_generation_merge_accepts_exact_proof_and_restamps() {
        let staged_dir = TempDir::new().unwrap();
        let destination_dir = TempDir::new().unwrap();
        let staged = VersionStore::open(staged_dir.path()).unwrap();
        let destination = VersionStore::open(destination_dir.path()).unwrap();
        let exact_staged =
            VersionRecord::new(100, "node-a", false, 1).with_origin_proof(17, [0x11; 32]);
        let exact_destination =
            VersionRecord::new(100, "node-a", false, 9).with_origin_proof(17, [0x11; 32]);
        let staged_newer =
            VersionRecord::new(300, "node-z", false, 2).with_origin_proof(30, [0x30; 32]);
        let destination_older =
            VersionRecord::new(299, "node-z", true, 8).with_origin_proof(29, [0x29; 32]);

        assert!(staged.upsert("exact", &exact_staged).unwrap());
        assert!(staged.upsert("staged-newer", &staged_newer).unwrap());
        assert!(destination.upsert("exact", &exact_destination).unwrap());
        assert!(destination
            .upsert("staged-newer", &destination_older)
            .unwrap());

        staged.merge_destination_evidence(&destination, 99).unwrap();

        assert_eq!(staged.get("exact").unwrap(), Some(exact_destination));
        assert_eq!(
            staged.get("staged-newer").unwrap(),
            Some(staged_newer.with_oplog_seq(99))
        );
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

        let staged_equal =
            VersionRecord::new(100, "node-a", false, 1).with_origin_proof(10, [0x10; 32]);
        let destination_equal =
            VersionRecord::new(100, "node-a", false, 9).with_origin_proof(10, [0x10; 32]);
        let staged_newer =
            VersionRecord::new(200, "node-z", false, 2).with_origin_proof(20, [0x20; 32]);
        let destination_older =
            VersionRecord::new(199, "node-z", true, 10).with_origin_proof(19, [0x19; 32]);
        let staged_older =
            VersionRecord::new(300, "node-a", false, 3).with_origin_proof(30, [0x30; 32]);
        let destination_newer =
            VersionRecord::new(300, "node-b", true, 11).with_origin_proof(31, [0x31; 32]);
        let staged_only =
            VersionRecord::new(400, "node-a", false, 4).with_origin_proof(40, [0x40; 32]);
        let destination_only =
            VersionRecord::new(500, "node-a", true, 12).with_origin_proof(50, [0x50; 32]);

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
