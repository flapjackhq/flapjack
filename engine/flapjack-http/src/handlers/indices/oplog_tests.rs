use super::*;
use crate::test_helpers::TestStateBuilder;
use flapjack::index::manager::publication::{publication_admission_is_fenced, PublicationTarget};
use tempfile::TempDir;
use tokio::time::{timeout, Duration};

#[tokio::test]
async fn metadata_oplog_lifecycle_replacement_waits_for_append_readback() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let tenant_id = "metadata_lifecycle_receipt";
    state.manager.create_tenant(tenant_id).unwrap();
    assert!(state.replication_manager.is_none());
    let mut appended = state
        .manager
        .append_oplog(tenant_id, "clear_index", serde_json::json!({}))
        .await
        .unwrap();
    let manager = Arc::clone(&state.manager);
    let runtime = tokio::runtime::Handle::current();
    let mut replacement = tokio::task::spawn_blocking(move || {
        runtime.block_on(async move {
            manager.delete_tenant(&tenant_id.to_string()).await.unwrap();
            manager.create_tenant(tenant_id).unwrap();
        });
    });
    let target = PublicationTarget::new(tenant_id).unwrap();
    timeout(Duration::from_secs(5), async {
        while !publication_admission_is_fenced(tmp.path(), &target) && !replacement.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(publication_admission_is_fenced(tmp.path(), &target));
    assert!(
        timeout(Duration::from_millis(50), &mut replacement)
            .await
            .is_err(),
        "replacement must not remove the appended row before readback"
    );
    let row = appended.read_entry().unwrap();
    assert_eq!(row.seq, 1);
    assert_eq!(row.op_type, "clear_index");
    assert_eq!(row.payload, serde_json::json!({}));
    drop(appended);
    timeout(Duration::from_secs(5), replacement)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        state
            .manager
            .get_or_create_oplog(tenant_id)
            .unwrap()
            .current_seq(),
        0
    );
}

#[tokio::test]
async fn metadata_oplog_receipt_authenticates_the_appended_operation() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let tenant_id = "metadata_lifecycle_readback_corrupt";
    state.manager.create_tenant(tenant_id).unwrap();
    let mut appended = state
        .manager
        .append_oplog(
            tenant_id,
            "copy_index",
            serde_json::json!({"destination": "copy"}),
        )
        .await
        .unwrap();
    let row = appended.read_entry().unwrap();
    let segment = std::fs::read_dir(tmp.path().join(tenant_id).join("oplog"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .unwrap();
    let mut wrong_sequence = row.clone();
    wrong_sequence.seq += 1;
    let mut wrong_tenant = row.clone();
    wrong_tenant.tenant_id = "replacement_tenant".to_string();
    let mut wrong_operation = row.clone();
    wrong_operation.op_type = "move_index".to_string();
    let mut wrong_payload = row;
    wrong_payload.payload = serde_json::json!({"destination": "replacement"});
    for (case, bytes) in [
        ("missing row", Vec::new()),
        ("corrupt row", b"not json\n".to_vec()),
        (
            "wrong sequence",
            serde_json::to_vec(&wrong_sequence).unwrap(),
        ),
        ("wrong tenant", serde_json::to_vec(&wrong_tenant).unwrap()),
        (
            "wrong operation",
            serde_json::to_vec(&wrong_operation).unwrap(),
        ),
        ("wrong payload", serde_json::to_vec(&wrong_payload).unwrap()),
    ] {
        std::fs::write(&segment, bytes).unwrap();
        assert!(
            appended.read_entry().is_err(),
            "readback must reject {case} even without peer replication"
        );
    }
}

#[tokio::test]
async fn metadata_oplog_lifecycle_receipt_rejects_same_sequence_replacement() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let tenant_id = "metadata_lifecycle_same_sequence_replacement";
    state.manager.create_tenant(tenant_id).unwrap();
    let mut appended = state
        .manager
        .append_oplog(
            tenant_id,
            "copy_index",
            serde_json::json!({"destination": "original"}),
        )
        .await
        .unwrap();
    let mut replacement = appended.read_entry().unwrap();
    replacement.tenant_id = "replacement_tenant".to_string();
    replacement.op_type = "move_index".to_string();
    replacement.payload = serde_json::json!({"destination": "replacement"});
    let segment = std::fs::read_dir(tmp.path().join(tenant_id).join("oplog"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .unwrap();
    let mut bytes = serde_json::to_vec(&replacement).unwrap();
    bytes.push(b'\n');
    std::fs::write(segment, bytes).unwrap();

    assert!(
        appended.read_entry().is_err(),
        "a lifecycle replacement cannot substitute a different row at the receipt sequence"
    );
}

#[tokio::test]
async fn metadata_oplog_lifecycle_helper_publishes_without_peer_replication() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let tenant_id = "metadata_lifecycle_no_peers";
    state.manager.create_tenant(tenant_id).unwrap();
    for op_type in ["clear_index", "move_index", "copy_index"] {
        let payload = serde_json::json!({"index_name": tenant_id});
        replicate_oplog_entry(&state, tenant_id, op_type, payload.clone())
            .await
            .unwrap();
        let oplog = state.manager.get_oplog(tenant_id).unwrap();
        assert_eq!(oplog.committed_seq().unwrap(), Some(oplog.current_seq()));
        let row = oplog
            .read_since(oplog.current_seq() - 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(row.op_type, op_type);
        assert_eq!(row.payload, payload);
    }
}
