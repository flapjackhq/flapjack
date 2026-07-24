use super::admission::{
    WriteAdmissionEpochEvidence, WriteAdmissionRecord, WriteAdmissionStore, WriteAdmissionTicket,
    WRITE_ADMISSION_DIR,
};
use super::WriteAction;
use crate::index::manager::publication::PublicationEpoch;
use serde_json::json;
use sha2::{Digest, Sha256};
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
        "actions": [{"Delete": "legacy_doc"}]
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
