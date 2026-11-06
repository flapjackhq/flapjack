use super::*;
use crate::extractors::ValidatedIndexName;
use crate::handlers::AppState;
use flapjack_replication::types::{
    RELEASE_TRANSFER_CONTRACT_HEADER, RELEASE_TRANSFER_CONTRACT_V1,
    RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER, RELEASE_TRANSFER_STATUS_ACKNOWLEDGED,
    RELEASE_TRANSFER_STATUS_HEADER, RELEASE_TRANSFER_TENANT_HEADER,
    RELEASE_TRANSFER_THROUGH_SEQ_HEADER, RELEASE_TRANSFER_TRANSACTION_HEADER,
};

async fn imported_generation_roundtrips_native_snapshot(overwrite: bool) {
    let source_dir = TempDir::new().unwrap();
    let source = TestStateBuilder::new(&source_dir).build_shared();
    let tenant = "a123456789abcdef_products_import";
    if overwrite {
        source.manager.create_tenant(tenant).unwrap();
        source
            .manager
            .add_documents_sync(
                tenant,
                vec![Document::from_json(&json!({
                    "objectID": "old", "title": "discarded generation"
                }))
                .unwrap()],
            )
            .await
            .unwrap();
    }
    let mut request = valid_request();
    request.target_index = Some(tenant.into());
    request.overwrite = overwrite;
    let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
    let metadata = AlgoliaIndexRecord {
        name: SOURCE_INDEX.into(),
        entries: 2,
        updated_at: "2026-10-18T00:00:00Z".into(),
        pending_task: false,
    };
    reader.push_quiescent(metadata.clone());
    for _ in 0..2 {
        reader.push_pass(
            json!({"searchableAttributes": ["title"], "attributesForFaceting": ["category"]}),
            vec![scripted_documents(EXPECTED_DOCUMENTS)],
            vec![vec![json!({"objectID":"native-rule", "conditions":[{"pattern":"adapter","anchoring":"contains"}], "consequence":{}})]],
            vec![vec![json!({"objectID":"native-synonym", "type":"synonym", "synonyms":["adapter","connector"]})]],
        );
    }
    reader.push_quiescent(metadata);
    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&source)),
        Json(request),
        |_| Ok(reader),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(response.objects.imported, 2);
    assert_eq!(response.rules.imported, 1);
    assert_eq!(response.synonyms.imported, 1);
    let exported = crate::handlers::snapshot::export_snapshot(
        State(Arc::clone(&source)),
        ValidatedIndexName(tenant.into()),
    )
    .await;
    assert_eq!(exported.status(), StatusCode::OK);
    let snapshot = axum::body::to_bytes(exported.into_body(), usize::MAX)
        .await
        .unwrap();
    let watermark = source
        .manager
        .get_existing_oplog(tenant)
        .unwrap()
        .unwrap()
        .committed_seq()
        .unwrap()
        .unwrap();
    assert!(watermark > 0);
    let replica_dir = TempDir::new().unwrap();
    let replica = TestStateBuilder::new(&replica_dir).build_shared();
    let mut headers = HeaderMap::new();
    for (name, value) in [
        (
            RELEASE_TRANSFER_CONTRACT_HEADER,
            RELEASE_TRANSFER_CONTRACT_V1.to_string(),
        ),
        (RELEASE_TRANSFER_TENANT_HEADER, tenant.to_string()),
        (
            RELEASE_TRANSFER_TRANSACTION_HEADER,
            "native-import-roundtrip".to_string(),
        ),
        (RELEASE_TRANSFER_THROUGH_SEQ_HEADER, watermark.to_string()),
        (
            RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER,
            hex::encode(Sha256::digest(&snapshot)),
        ),
    ] {
        headers.insert(name, value.parse().unwrap());
    }
    let imported = crate::handlers::snapshot::import_snapshot(
        State(Arc::clone(&replica)),
        ValidatedIndexName(tenant.into()),
        headers,
        snapshot,
    )
    .await;
    let status = imported.status();
    let acknowledgement = imported
        .headers()
        .get(RELEASE_TRANSFER_STATUS_HEADER)
        .cloned();
    assert_eq!(
        status,
        StatusCode::OK,
        "native import must accept the published generation: {:?}",
        body_json(imported).await
    );
    assert_eq!(
        acknowledgement.unwrap(),
        RELEASE_TRANSFER_STATUS_ACKNOWLEDGED
    );
    assert_snapshot_generation_restored(&source_dir, &replica_dir, &replica, tenant, watermark)
        .await;
    assert_snapshot_tail_replicates(&source, &replica, tenant, watermark).await;
}

async fn assert_snapshot_generation_restored(
    source_dir: &TempDir,
    replica_dir: &TempDir,
    replica: &Arc<AppState>,
    tenant: &str,
    watermark: u64,
) {
    for (id, title, _) in EXPECTED_DOCUMENTS {
        assert_eq!(
            replica
                .manager
                .get_document(tenant, id)
                .unwrap()
                .unwrap()
                .fields
                .get("title")
                .and_then(|v| v.as_text()),
            Some(title)
        );
    }
    assert!(replica
        .manager
        .get_document(tenant, "old")
        .unwrap()
        .is_none());
    for file in ["settings.json", "rules.json", "synonyms.json"] {
        assert_eq!(
            std::fs::read(source_dir.path().join(tenant).join(file)).unwrap(),
            std::fs::read(replica_dir.path().join(tenant).join(file)).unwrap(),
            "{file}"
        );
    }
    assert_query_returns_document(
        replica,
        tenant,
        "connector",
        "doc-1",
        "Quartz adapter",
        "hardware",
    )
    .await;
    let restored = replica.manager.get_existing_oplog(tenant).unwrap().unwrap();
    assert_eq!(restored.committed_seq().unwrap(), Some(watermark));
    assert!(restored
        .read_since(0)
        .unwrap()
        .iter()
        .all(|entry| entry.tenant_id == tenant));
}

async fn assert_snapshot_tail_replicates(
    source: &AppState,
    replica: &AppState,
    tenant: &str,
    watermark: u64,
) {
    source
        .manager
        .add_documents_sync(
            tenant,
            vec![Document::from_json(&json!({
                "objectID": "tail", "title": "after snapshot"
            }))
            .unwrap()],
        )
        .await
        .unwrap();
    let tail = source
        .manager
        .get_existing_oplog(tenant)
        .unwrap()
        .unwrap()
        .read_since(watermark)
        .unwrap();
    assert!(!tail.is_empty());
    assert!(tail
        .iter()
        .all(|entry| entry.tenant_id == tenant && entry.seq > watermark));
    crate::handlers::internal::apply_ops_to_state(replica, tenant, &tail)
        .await
        .unwrap();
    assert_eq!(
        replica
            .manager
            .get_existing_oplog(tenant)
            .unwrap()
            .unwrap()
            .committed_seq()
            .unwrap(),
        Some(tail.last().unwrap().seq)
    );
    assert_eq!(
        replica
            .manager
            .get_document(tenant, "tail")
            .unwrap()
            .unwrap()
            .fields
            .get("title")
            .and_then(|v| v.as_text()),
        Some("after snapshot")
    );
}

#[tokio::test]
async fn imported_create_generation_roundtrips_native_snapshot() {
    imported_generation_roundtrips_native_snapshot(false).await;
}

#[tokio::test]
async fn imported_replace_generation_roundtrips_native_snapshot() {
    imported_generation_roundtrips_native_snapshot(true).await;
}
