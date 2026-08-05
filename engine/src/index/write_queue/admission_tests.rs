use super::admission::{
    reconcile_records, WriteAdmissionEpochEvidence, WriteAdmissionRecord, WriteAdmissionStore,
    WriteAdmissionTicket, WRITE_ADMISSION_DIR,
};
use super::{ReplicatedWriteOrigin, WriteAction};
use crate::index::manager::publication::PublicationEpoch;
use crate::index::version_store::{VersionRecord, VersionStore};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use tempfile::TempDir;

fn ticket(target: &str, epoch: u64) -> WriteAdmissionTicket {
    WriteAdmissionTicket::new(target.to_string(), PublicationEpoch(epoch))
}

fn delete_record(target: &str, epoch: u64) -> WriteAdmissionRecord {
    WriteAdmissionRecord::new(
        ticket(target, epoch),
        format!("task_{target}_epoch"),
        7,
        1,
        vec![WriteAction::Delete("stale_doc".to_string())],
    )
}

#[test]
fn new_admission_records_require_epoch_ticket_and_expose_observed_epoch() {
    let record = delete_record("products", 4);

    assert_eq!(
        record.epoch_evidence,
        WriteAdmissionEpochEvidence::Observed {
            target: "products".to_string(),
            epoch: PublicationEpoch(4)
        }
    );
}

#[test]
fn admission_record_round_trip_retains_epoch_ticket_and_checksums_it() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "products";
    std::fs::create_dir_all(tmp.path().join(tenant_id)).unwrap();
    let store = WriteAdmissionStore::open(tmp.path(), tenant_id).unwrap();
    store.append_record(delete_record(tenant_id, 9)).unwrap();

    let record_path = tmp
        .path()
        .join(tenant_id)
        .join(WRITE_ADMISSION_DIR)
        .join("00000000000000000001.json");
    let records = store.load_records().unwrap();
    assert_eq!(
        records[0].epoch_evidence,
        delete_record(tenant_id, 9).epoch_evidence
    );

    let mut envelope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    envelope["record"]["epoch_evidence"]["observed"]["epoch"] = json!(10);
    std::fs::write(&record_path, serde_json::to_vec(&envelope).unwrap()).unwrap();

    assert!(
        store.load_records().is_err(),
        "tampering with persisted epoch evidence must be caught by the existing checksum"
    );
}

#[test]
fn legacy_records_decode_as_unproven_epoch_evidence_without_fabrication() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "legacy";
    let admission_dir = tmp.path().join(tenant_id).join(WRITE_ADMISSION_DIR);
    std::fs::create_dir_all(&admission_dir).unwrap();
    let record = json!({
        "sequence": 1,
        "task_id": "task_legacy_1",
        "numeric_id": 1,
        "received_documents": 1,
        "created_at_ms": 0,
        "actions": [
            {
                "UpsertNoLwwUpdate": {
                    "id": "legacy_upsert",
                    "fields": {}
                }
            },
            {"DeleteNoLwwUpdate": "legacy_delete"}
        ]
    });
    let envelope = json!({
        "checksum": checksum(&record),
        "record": record
    });
    std::fs::write(
        admission_dir.join("00000000000000000001.json"),
        serde_json::to_vec(&envelope).unwrap(),
    )
    .unwrap();

    let store = WriteAdmissionStore::open(tmp.path(), tenant_id).unwrap();
    let records = store.load_records().unwrap();
    assert_eq!(
        records[0].epoch_evidence,
        WriteAdmissionEpochEvidence::LegacyUnproven
    );
    assert!(
        matches!(
            records[0].actions.as_slice(),
            [
                WriteAction::UpsertNoLwwUpdate(document),
                WriteAction::DeleteNoLwwUpdate(object_id)
            ] if document.id == "legacy_upsert" && object_id == "legacy_delete"
        ),
        "legacy no-origin variants must decode without inventing replicated provenance"
    );
}

/// Build a tenant directory with an admission store and an open version store.
fn replicated_reconcile_fixture(tenant_id: &str) -> (TempDir, WriteAdmissionStore, VersionStore) {
    let tmp = TempDir::new().unwrap();
    let tenant_path = tmp.path().join(tenant_id);
    std::fs::create_dir_all(&tenant_path).unwrap();
    let store = WriteAdmissionStore::open(tmp.path(), tenant_id).unwrap();
    let version_store = VersionStore::open(&tenant_path).unwrap();
    (tmp, store, version_store)
}

fn replicated_document(object_id: &str) -> crate::types::Document {
    crate::types::Document {
        id: object_id.to_string(),
        fields: std::collections::HashMap::from([(
            "title".to_string(),
            crate::types::FieldValue::Text(format!("body for {object_id}")),
        )]),
    }
}

fn upsert_with_origin(
    object_id: &str,
    origin_timestamp_ms: u64,
    origin_node_id: &str,
) -> WriteAction {
    WriteAction::UpsertWithOrigin {
        doc: replicated_document(object_id),
        origin: ReplicatedWriteOrigin::new(origin_timestamp_ms, origin_node_id.to_string()),
    }
}

fn delete_with_origin(
    object_id: &str,
    origin_timestamp_ms: u64,
    origin_node_id: &str,
) -> WriteAction {
    WriteAction::DeleteWithOrigin {
        object_id: object_id.to_string(),
        origin: ReplicatedWriteOrigin::new(origin_timestamp_ms, origin_node_id.to_string()),
    }
}

fn admission_record(
    target: &str,
    task_id: &str,
    actions: Vec<WriteAction>,
) -> WriteAdmissionRecord {
    WriteAdmissionRecord::new(
        ticket(target, 1),
        task_id.to_string(),
        11,
        actions.len(),
        actions,
    )
}

/// Seed a durable version row, asserting the write actually landed. A silently
/// skipped last-writer-wins upsert would leave a replay-direction test passing
/// because the row is missing rather than because its tuple differs.
fn publish_version(version_store: &VersionStore, object_id: &str, version: &VersionRecord) {
    assert!(
        version_store.upsert(object_id, version).unwrap(),
        "fixture version row for {object_id} must be durable before reconciliation"
    );
}

fn surviving_task_ids(store: &WriteAdmissionStore) -> Vec<String> {
    let mut ids: Vec<_> = store
        .load_records()
        .unwrap()
        .into_iter()
        .map(|record| record.task_id)
        .collect();
    ids.sort();
    ids
}

/// A surviving admission record whose replicated version tuple is already in the
/// version store must be reclaimed, not re-driven, even when the committed-prefix
/// replay set is empty and the transient finalized-task marker is already gone.
#[test]
fn reconcile_reclaims_records_already_published_by_the_version_store() {
    let (_tmp, store, version_store) = replicated_reconcile_fixture("replicated_reclaim");
    store
        .append_record(admission_record(
            "replicated",
            "task_replicated_reclaim",
            vec![upsert_with_origin("replicated_object", 4_200, "peer-node")],
        ))
        .unwrap();
    publish_version(
        &version_store,
        "replicated_object",
        &VersionRecord::new(4_200, "peer-node", false, 9),
    );
    assert!(
        !version_store
            .contains_finalized_task("task_replicated_reclaim")
            .unwrap(),
        "fixture must exercise the version-store arm, not the finalized-task marker"
    );

    let pending = reconcile_records(&store, &BTreeSet::new()).unwrap();

    assert!(
        pending.is_empty(),
        "already-published replicated write must not be re-driven, got {:?}",
        pending
            .iter()
            .map(|record| record.task_id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(surviving_task_ids(&store), Vec::<String>::new());
}

/// The same reclamation must not fire when the durable version tuple differs from
/// the record's origin: that write was never published and still owes a replay.
#[test]
fn reconcile_replays_records_whose_version_tuple_does_not_match() {
    let (_tmp, store, version_store) = replicated_reconcile_fixture("replicated_replay");
    store
        .append_record(admission_record(
            "replicated",
            "task_replicated_replay",
            vec![upsert_with_origin("replicated_object", 4_200, "peer-node")],
        ))
        .unwrap();
    publish_version(
        &version_store,
        "replicated_object",
        &VersionRecord::new(4_199, "peer-node", false, 9),
    );

    let pending = reconcile_records(&store, &BTreeSet::new()).unwrap();

    assert_eq!(
        pending
            .iter()
            .map(|record| record.task_id.as_str())
            .collect::<Vec<_>>(),
        vec!["task_replicated_replay"],
        "an unpublished replicated write must still be replayed"
    );
    assert_eq!(
        surviving_task_ids(&store),
        vec!["task_replicated_replay".to_string()]
    );
}

/// A record is only reclaimed when every one of its actions is published. A single
/// unmatched action keeps the whole record pending.
#[test]
fn reconcile_replays_records_with_one_unpublished_action() {
    let (_tmp, store, version_store) = replicated_reconcile_fixture("replicated_partial");
    store
        .append_record(admission_record(
            "replicated",
            "task_replicated_partial",
            vec![
                upsert_with_origin("published_object", 7_000, "peer-node"),
                delete_with_origin("unpublished_object", 7_001, "peer-node"),
            ],
        ))
        .unwrap();
    publish_version(
        &version_store,
        "published_object",
        &VersionRecord::new(7_000, "peer-node", false, 3),
    );

    let pending = reconcile_records(&store, &BTreeSet::new()).unwrap();

    assert_eq!(
        pending
            .iter()
            .map(|record| record.task_id.as_str())
            .collect::<Vec<_>>(),
        vec!["task_replicated_partial"],
        "a record with an unpublished action must be replayed as a whole"
    );
    assert_eq!(
        surviving_task_ids(&store),
        vec!["task_replicated_partial".to_string()]
    );
}

/// A replicated delete publishes a tombstone version row, so its record is
/// reclaimed only when the durable row is itself a tombstone.
#[test]
fn reconcile_reclaims_replicated_deletes_published_as_tombstones() {
    let (_tmp, store, version_store) = replicated_reconcile_fixture("replicated_tombstone");
    store
        .append_record(admission_record(
            "replicated",
            "task_replicated_tombstone",
            vec![delete_with_origin("deleted_object", 8_100, "peer-node")],
        ))
        .unwrap();
    publish_version(
        &version_store,
        "deleted_object",
        &VersionRecord::new(8_100, "peer-node", true, 6),
    );

    let pending = reconcile_records(&store, &BTreeSet::new()).unwrap();

    assert!(
        pending.is_empty(),
        "a published replicated delete must not be re-driven, got {:?}",
        pending
            .iter()
            .map(|record| record.task_id.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(surviving_task_ids(&store), Vec::<String>::new());
}

/// The tombstone flag is part of the published-version identity: a live row that
/// happens to share the delete's timestamp and node has not applied that delete.
#[test]
fn reconcile_replays_replicated_deletes_whose_durable_row_is_not_a_tombstone() {
    let (_tmp, store, version_store) = replicated_reconcile_fixture("replicated_live_row");
    store
        .append_record(admission_record(
            "replicated",
            "task_replicated_live_row",
            vec![delete_with_origin("contested_object", 8_100, "peer-node")],
        ))
        .unwrap();
    publish_version(
        &version_store,
        "contested_object",
        &VersionRecord::new(8_100, "peer-node", false, 6),
    );

    let pending = reconcile_records(&store, &BTreeSet::new()).unwrap();

    assert_eq!(
        pending
            .iter()
            .map(|record| record.task_id.as_str())
            .collect::<Vec<_>>(),
        vec!["task_replicated_live_row"],
        "a delete whose durable row is still live must be replayed"
    );
    assert_eq!(
        surviving_task_ids(&store),
        vec!["task_replicated_live_row".to_string()]
    );
}

/// Reclamation is keyed on the replicated origin tuple. A local write carries no
/// origin, so a coincidentally matching version row must not retire its record.
#[test]
fn reconcile_keeps_local_records_without_replicated_origin() {
    let (_tmp, store, version_store) = replicated_reconcile_fixture("local_no_origin");
    store
        .append_record(admission_record(
            "local",
            "task_local_no_origin",
            vec![WriteAction::Upsert(replicated_document("local_object"))],
        ))
        .unwrap();
    publish_version(
        &version_store,
        "local_object",
        &VersionRecord::new(9_000, "local-node", false, 4),
    );

    let pending = reconcile_records(&store, &BTreeSet::new()).unwrap();

    assert_eq!(
        pending
            .iter()
            .map(|record| record.task_id.as_str())
            .collect::<Vec<_>>(),
        vec!["task_local_no_origin"],
        "a write with no recoverable origin tuple cannot be proven published"
    );
    assert_eq!(
        surviving_task_ids(&store),
        vec!["task_local_no_origin".to_string()]
    );
}

/// The committed-prefix replay set stays the first reclamation arm: a task already
/// applied from the oplog is retired without consulting the version store.
#[test]
fn reconcile_reclaims_records_already_applied_from_the_committed_prefix() {
    let (_tmp, store, _version_store) = replicated_reconcile_fixture("applied_prefix");
    store
        .append_record(admission_record(
            "replicated",
            "task_applied_prefix",
            vec![upsert_with_origin("prefix_object", 1_000, "peer-node")],
        ))
        .unwrap();

    let applied = BTreeSet::from(["task_applied_prefix".to_string()]);
    let pending = reconcile_records(&store, &applied).unwrap();

    assert!(pending.is_empty(), "replayed task must not be re-driven");
    assert_eq!(surviving_task_ids(&store), Vec::<String>::new());
}

fn checksum(record: &serde_json::Value) -> String {
    let canonical = canonical_json(record);
    let bytes = serde_json::to_vec(&canonical).unwrap();
    format!("{:x}", Sha256::digest(bytes))
}

fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_unstable_by_key(|(key, _)| *key);
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonical_json(value)))
                    .collect(),
            )
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonical_json).collect())
        }
        _ => value.clone(),
    }
}
