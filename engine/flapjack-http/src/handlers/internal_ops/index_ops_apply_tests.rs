use super::*;

fn index_op(
    seq: u64,
    timestamp_ms: u64,
    node_id: &str,
    tenant_id: &str,
    op_type: &str,
    payload: serde_json::Value,
) -> OpLogEntry {
    OpLogEntry {
        seq,
        timestamp_ms,
        node_id: node_id.to_string(),
        tenant_id: tenant_id.to_string(),
        op_type: op_type.to_string(),
        payload,
    }
}

fn move_op(seq: u64, timestamp_ms: u64, tenant_id: &str) -> OpLogEntry {
    index_op(
        seq,
        timestamp_ms,
        "node-a",
        tenant_id,
        "move_index",
        serde_json::json!({"source": format!("{tenant_id}-source"), "destination": tenant_id}),
    )
}

fn copy_op(
    seq: u64,
    timestamp_ms: u64,
    source: &str,
    destination: &str,
    scope: serde_json::Value,
    source_settings: serde_json::Value,
) -> OpLogEntry {
    index_op(
        seq,
        timestamp_ms,
        "node-a",
        source,
        "copy_index",
        serde_json::json!({
            "source": source,
            "destination": destination,
            "scope": scope,
            "source_settings": source_settings,
            "source_synonyms": null,
            "source_rules": null
        }),
    )
}

fn clear_op(seq: u64, timestamp_ms: u64, node_id: &str, tenant_id: &str) -> OpLogEntry {
    index_op(
        seq,
        timestamp_ms,
        node_id,
        tenant_id,
        "clear_index",
        serde_json::json!({"index_name": tenant_id}),
    )
}

async fn seed_document(manager: &IndexManager, tenant_id: &str, object_id: &str, name: &str) {
    if !manager.base_path.join(tenant_id).exists() {
        manager.create_tenant(tenant_id).unwrap();
    }
    manager
        .add_documents_sync(
            tenant_id,
            vec![
                Document::from_json(&serde_json::json!({"objectID": object_id, "name": name}))
                    .unwrap(),
            ],
        )
        .await
        .unwrap();
}

fn document_exists(manager: &IndexManager, tenant_id: &str, object_id: &str) -> bool {
    manager.base_path.join(tenant_id).exists()
        && manager
            .get_document(tenant_id, object_id)
            .unwrap()
            .is_some()
}

fn stored_positions(base_path: &Path, tenant_id: &str) -> TenantPositions {
    TenantPositions::parse(&std::fs::read(positions_path(base_path, tenant_id)).unwrap()).unwrap()
}

fn settings_with(attribute: &str) -> IndexSettings {
    IndexSettings {
        searchable_attributes: Some(vec![attribute.to_string()]),
        ..Default::default()
    }
}

#[tokio::test]
async fn apply_replicated_index_op_skips_equal_and_older_positions_without_effects() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "skip-older";
    let source = "skip-older-source";
    seed_document(&manager, source, "kept", "Stays in source").await;
    let mut positions = positions_with("node-a", Some((2_000, 5)), Some((1_500, 2)));
    load_tenant_positions(&manager, tenant).unwrap();
    persist_tenant_positions(&manager, tenant, &positions).unwrap();
    let stored_before = std::fs::read(positions_path(tmp.path(), tenant)).unwrap();

    for (seq, timestamp_ms) in [(5, 2_000), (9, 1_500), (4, 2_000), (1, 1)] {
        apply_replicated_index_op(
            &manager,
            tenant,
            &mut positions,
            &move_op(seq, timestamp_ms, tenant),
        )
        .await
        .unwrap_or_else(|error| {
            panic!("({timestamp_ms},{seq}) must be acknowledged as replay: {error}")
        });
        assert!(
            document_exists(&manager, source, "kept"),
            "({timestamp_ms},{seq}) must not consume the source"
        );
        assert!(
            !tmp.path().join(tenant).exists(),
            "({timestamp_ms},{seq}) must not create the destination"
        );
    }
    assert_eq!(
        positions,
        positions_with("node-a", Some((2_000, 5)), Some((1_500, 2)))
    );
    assert_eq!(
        std::fs::read(positions_path(tmp.path(), tenant)).unwrap(),
        stored_before
    );
}

#[tokio::test]
async fn apply_replicated_index_op_newer_timestamp_wins_despite_sequence_reset() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "sequence-reset";
    seed_document(&manager, tenant, "pre-clear", "Removed by clear").await;
    let mut positions = positions_with("node-a", Some((2_000, 50)), None);
    load_tenant_positions(&manager, tenant).unwrap();
    persist_tenant_positions(&manager, tenant, &positions).unwrap();

    apply_replicated_index_op(
        &manager,
        tenant,
        &mut positions,
        &clear_op(1, 2_500, "node-a", tenant),
    )
    .await
    .unwrap();

    assert_eq!(manager.tenant_doc_count(tenant), Some(0));
    let expected = positions_with("node-a", Some((2_500, 1)), Some((2_500, 1)));
    assert_eq!(positions, expected);
    assert_eq!(stored_positions(tmp.path(), tenant), expected);
}

#[tokio::test]
async fn apply_replicated_index_op_equal_timestamp_uses_sequence_as_tie_breaker() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "tie-breaker";
    seed_document(&manager, tenant, "pre-clear", "Removed by clear").await;
    let mut positions = positions_with("node-a", Some((3_000, 4)), None);
    load_tenant_positions(&manager, tenant).unwrap();
    persist_tenant_positions(&manager, tenant, &positions).unwrap();

    apply_replicated_index_op(
        &manager,
        tenant,
        &mut positions,
        &clear_op(5, 3_000, "node-a", tenant),
    )
    .await
    .unwrap();
    assert_eq!(manager.tenant_doc_count(tenant), Some(0));
    assert_eq!(
        positions,
        positions_with("node-a", Some((3_000, 5)), Some((3_000, 5)))
    );

    seed_document(&manager, tenant, "post-clear", "Written after clear").await;
    apply_replicated_index_op(
        &manager,
        tenant,
        &mut positions,
        &clear_op(4, 3_000, "node-a", tenant),
    )
    .await
    .unwrap();
    assert!(
        document_exists(&manager, tenant, "post-clear"),
        "an equal-timestamp lower sequence is a replay"
    );
    assert_eq!(
        positions,
        positions_with("node-a", Some((3_000, 5)), Some((3_000, 5)))
    );
    assert_eq!(stored_positions(tmp.path(), tenant), positions);
}

#[tokio::test]
async fn apply_replicated_index_op_keeps_origin_nodes_and_tenants_independent() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "independent";
    let other_tenant = "independent-other";
    seed_document(&manager, tenant, "pre-clear", "Removed by node-b clear").await;
    load_tenant_positions(&manager, other_tenant).unwrap();
    let other_before = std::fs::read(positions_path(tmp.path(), other_tenant)).unwrap();
    let mut positions = positions_with("node-a", Some((5_000, 9)), Some((4_000, 3)));
    load_tenant_positions(&manager, tenant).unwrap();
    persist_tenant_positions(&manager, tenant, &positions).unwrap();

    apply_replicated_index_op(
        &manager,
        tenant,
        &mut positions,
        &clear_op(1, 1_000, "node-b", tenant),
    )
    .await
    .unwrap();
    assert_eq!(
        manager.tenant_doc_count(tenant),
        Some(0),
        "node-b's older position must not be skipped by node-a's evidence"
    );
    let mut expected = positions_with("node-a", Some((5_000, 9)), Some((4_000, 3)));
    expected.nodes.insert(
        "node-b".to_string(),
        NodePositions {
            index_op: Some((1_000, 1)),
            clear: Some((1_000, 1)),
        },
    );
    assert_eq!(positions, expected);
    assert_eq!(stored_positions(tmp.path(), tenant), expected);

    seed_document(&manager, "independent-copy", "copied", "Copied").await;
    apply_replicated_index_op(
        &manager,
        tenant,
        &mut positions,
        &index_op(
            10,
            6_000,
            "node-a",
            tenant,
            "copy_index",
            serde_json::json!({"source": "independent-copy", "destination": tenant, "scope": null}),
        ),
    )
    .await
    .unwrap();
    assert!(document_exists(&manager, tenant, "copied"));
    expected.nodes.get_mut("node-a").unwrap().index_op = Some((6_000, 10));
    assert_eq!(
        positions, expected,
        "move/copy must advance index_op while preserving the prior clear position"
    );
    assert_eq!(stored_positions(tmp.path(), tenant), expected);
    assert_eq!(
        std::fs::read(positions_path(tmp.path(), other_tenant)).unwrap(),
        other_before
    );
}

#[tokio::test]
async fn apply_replicated_index_op_rejects_empty_node_id_before_effects() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "empty-node";
    let source = "empty-node-source";
    seed_document(&manager, source, "kept", "Stays in source").await;
    let mut positions = load_tenant_positions(&manager, tenant).unwrap();
    let mut op = move_op(1, 1_000, tenant);
    op.node_id.clear();

    assert!(
        apply_replicated_index_op(&manager, tenant, &mut positions, &op)
            .await
            .is_err()
    );

    assert!(document_exists(&manager, source, "kept"));
    assert!(!tmp.path().join(tenant).exists());
    assert_eq!(positions, TenantPositions::default());
    assert_eq!(
        stored_positions(tmp.path(), tenant),
        TenantPositions::default()
    );
}

#[tokio::test]
async fn apply_replicated_index_op_move_replay_after_restart_preserves_recreated_source() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "move-replay";
    let source = "move-replay-source";
    seed_document(&manager, source, "moved", "Original destination").await;
    let mut positions = load_tenant_positions(&manager, tenant).unwrap();
    let op = move_op(6, 4_000, tenant);
    apply_replicated_index_op(&manager, tenant, &mut positions, &op)
        .await
        .unwrap();
    assert!(document_exists(&manager, tenant, "moved"));
    assert!(!tmp.path().join(source).exists());
    assert_eq!(positions, positions_with("node-a", Some((4_000, 6)), None));
    seed_document(&manager, source, "recreated", "Recreated source").await;
    drop(manager);

    let manager = IndexManager::new(tmp.path());
    let mut positions = load_tenant_positions(&manager, tenant).unwrap();
    assert_eq!(positions, positions_with("node-a", Some((4_000, 6)), None));
    apply_replicated_index_op(&manager, tenant, &mut positions, &op)
        .await
        .expect("a durable position followed by a lost response must acknowledge the retry");

    assert!(
        document_exists(&manager, source, "recreated"),
        "the recreated source must survive the replay"
    );
    assert!(document_exists(&manager, tenant, "moved"));
    assert!(!document_exists(&manager, tenant, "recreated"));
    assert_eq!(positions, positions_with("node-a", Some((4_000, 6)), None));
}

#[tokio::test]
async fn apply_replicated_index_op_copy_replays_after_restart_keep_newer_destination_state() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let source = "copy-source";
    let destination = "copy-destination";
    seed_document(&manager, source, "source-doc", "Source snapshot").await;
    let mut positions = load_tenant_positions(&manager, source).unwrap();
    let full_copy = copy_op(
        20,
        2_000,
        source,
        destination,
        serde_json::Value::Null,
        serde_json::Value::Null,
    );
    apply_replicated_index_op(&manager, source, &mut positions, &full_copy)
        .await
        .unwrap();
    assert!(document_exists(&manager, destination, "source-doc"));
    seed_document(
        &manager,
        destination,
        "newer-destination",
        "New destination write",
    )
    .await;

    let settings_source = "settings-source";
    let settings_destination = "settings-destination";
    for tenant in [settings_source, settings_destination] {
        manager.create_tenant(tenant).unwrap();
    }
    let copied = settings_with("copied");
    copied
        .save(tmp.path().join(settings_source).join("settings.json"))
        .unwrap();
    let mut settings_positions = load_tenant_positions(&manager, settings_source).unwrap();
    let scoped_copy = copy_op(
        21,
        2_100,
        settings_source,
        settings_destination,
        serde_json::json!(["settings"]),
        serde_json::to_value(&copied).unwrap(),
    );
    apply_replicated_index_op(
        &manager,
        settings_source,
        &mut settings_positions,
        &scoped_copy,
    )
    .await
    .unwrap();
    let destination_settings = tmp.path().join(settings_destination).join("settings.json");
    assert_eq!(
        IndexSettings::load(&destination_settings)
            .unwrap()
            .searchable_attributes,
        copied.searchable_attributes
    );
    settings_with("newer").save(&destination_settings).unwrap();
    manager.invalidate_settings_cache(settings_destination);
    drop(manager);

    let manager = IndexManager::new(tmp.path());
    let mut positions = load_tenant_positions(&manager, source).unwrap();
    apply_replicated_index_op(&manager, source, &mut positions, &full_copy)
        .await
        .unwrap();
    assert!(document_exists(&manager, destination, "source-doc"));
    assert!(
        document_exists(&manager, destination, "newer-destination"),
        "a full-copy replay must keep the newer destination document"
    );
    assert_eq!(positions, positions_with("node-a", Some((2_000, 20)), None));

    let mut settings_positions = load_tenant_positions(&manager, settings_source).unwrap();
    apply_replicated_index_op(
        &manager,
        settings_source,
        &mut settings_positions,
        &scoped_copy,
    )
    .await
    .unwrap();
    assert_eq!(
        IndexSettings::load(&destination_settings)
            .unwrap()
            .searchable_attributes,
        Some(vec!["newer".to_string()]),
        "a scoped settings replay must keep the newer destination settings"
    );
    assert_eq!(
        settings_positions,
        positions_with("node-a", Some((2_100, 21)), None)
    );
}

#[tokio::test]
async fn apply_replicated_index_op_clear_replay_after_restart_preserves_newer_document() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "clear-replay";
    seed_document(&manager, tenant, "before", "Removed by clear").await;
    let mut positions = load_tenant_positions(&manager, tenant).unwrap();
    let clear = clear_op(4, 2_000, "node-a", tenant);
    apply_replicated_index_op(&manager, tenant, &mut positions, &clear)
        .await
        .unwrap();
    assert_eq!(manager.tenant_doc_count(tenant), Some(0));
    seed_document(&manager, tenant, "after", "Written after clear").await;
    drop(manager);

    let manager = IndexManager::new(tmp.path());
    let mut positions = load_tenant_positions(&manager, tenant).unwrap();
    assert_eq!(
        positions,
        positions_with("node-a", Some((2_000, 4)), Some((2_000, 4)))
    );
    apply_replicated_index_op(&manager, tenant, &mut positions, &clear)
        .await
        .unwrap();

    assert!(
        document_exists(&manager, tenant, "after"),
        "a clear replay must not remove newer documents"
    );
    assert_eq!(
        positions,
        positions_with("node-a", Some((2_000, 4)), Some((2_000, 4)))
    );
}

#[tokio::test]
async fn apply_replicated_index_op_failed_effect_leaves_positions_unchanged() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "failed-effect";
    let mut positions = positions_with("node-a", Some((1_000, 1)), None);
    load_tenant_positions(&manager, tenant).unwrap();
    persist_tenant_positions(&manager, tenant, &positions).unwrap();
    let stored_before = std::fs::read(positions_path(tmp.path(), tenant)).unwrap();
    let mut broken = move_op(2, 2_000, tenant);
    broken.payload = serde_json::json!({"destination": tenant});

    assert!(
        apply_replicated_index_op(&manager, tenant, &mut positions, &broken)
            .await
            .is_err()
    );

    assert_eq!(positions, positions_with("node-a", Some((1_000, 1)), None));
    assert_eq!(
        std::fs::read(positions_path(tmp.path(), tenant)).unwrap(),
        stored_before
    );
    assert!(!tmp.path().join(tenant).exists());

    seed_document(&manager, "failed-effect-source", "moved", "Moved").await;
    apply_replicated_index_op(&manager, tenant, &mut positions, &move_op(2, 2_000, tenant))
        .await
        .expect("the same owner must still apply a valid operation");
    let expected = positions_with("node-a", Some((2_000, 2)), None);
    assert_eq!(
        positions, expected,
        "the control proves position advancement is live"
    );
    assert_eq!(stored_positions(tmp.path(), tenant), expected);
}

#[cfg(unix)]
#[tokio::test]
async fn apply_replicated_index_op_initialization_failure_precedes_any_effect() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "init-failure";
    let source = "init-failure-source";
    seed_document(&manager, source, "must-stay", "Unchanged").await;
    let root = positions_root(tmp.path());
    flapjack::index::ensure_private_directory(&root).unwrap();
    set_unix_mode(&root, 0o500);
    assert_directory_write_denied(&root);

    let load_result = load_tenant_positions(&manager, tenant);
    set_unix_mode(&root, 0o700);
    assert!(
        load_result.is_err(),
        "initialization must fail before dispatch"
    );
    assert!(document_exists(&manager, source, "must-stay"));
    assert!(!tmp.path().join(tenant).exists());
    assert!(!positions_path(tmp.path(), tenant).exists());

    let mut positions = load_tenant_positions(&manager, tenant).unwrap();
    apply_replicated_index_op(&manager, tenant, &mut positions, &move_op(2, 2_000, tenant))
        .await
        .unwrap();
    assert!(document_exists(&manager, tenant, "must-stay"));
    assert_eq!(
        stored_positions(tmp.path(), tenant),
        positions_with("node-a", Some((2_000, 2)), None)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn apply_replicated_index_op_effect_precedes_persistence_failure_and_retries_after_restart() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "persist-failure";
    let source = "persist-failure-source";
    seed_document(&manager, source, "moved", "Move once").await;
    let prior = positions_with("node-a", Some((1_000, 1)), None);
    load_tenant_positions(&manager, tenant).unwrap();
    persist_tenant_positions(&manager, tenant, &prior).unwrap();
    let path = positions_path(tmp.path(), tenant);
    let prior_bytes = std::fs::read(&path).unwrap();
    let root = positions_root(tmp.path());
    let mut positions = load_tenant_positions(&manager, tenant).unwrap();
    set_unix_mode(&root, 0o500);
    assert_directory_write_denied(&root);
    let op = move_op(4, 2_000, tenant);

    let result = apply_replicated_index_op(&manager, tenant, &mut positions, &op).await;
    let bytes_after = std::fs::read(&path).unwrap();
    set_unix_mode(&root, 0o700);

    assert!(
        result.is_err(),
        "a persistence failure must not report success"
    );
    assert!(
        !tmp.path().join(source).exists(),
        "the move effect must precede the persistence failure"
    );
    assert!(document_exists(&manager, tenant, "moved"));
    assert_eq!(bytes_after, prior_bytes);
    assert_eq!(
        positions, prior,
        "the in-memory owner must stay aligned with disk"
    );
    drop(manager);

    let manager = IndexManager::new(tmp.path());
    let mut positions = load_tenant_positions(&manager, tenant).unwrap();
    assert_eq!(positions, prior);
    apply_replicated_index_op(&manager, tenant, &mut positions, &op)
        .await
        .expect("the retry must succeed through the absent-source move no-op");
    assert!(document_exists(&manager, tenant, "moved"));
    let expected = positions_with("node-a", Some((2_000, 4)), None);
    assert_eq!(positions, expected);
    assert_eq!(stored_positions(tmp.path(), tenant), expected);
}

#[cfg(unix)]
#[tokio::test]
async fn apply_replicated_index_op_retries_with_the_same_owner_after_persistence_failure() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant = "same-owner-retry";
    seed_document(&manager, tenant, "before", "Removed by clear").await;
    let mut positions = load_tenant_positions(&manager, tenant).unwrap();
    let root = positions_root(tmp.path());
    set_unix_mode(&root, 0o500);
    assert_directory_write_denied(&root);
    let clear = clear_op(3, 2_000, "node-a", tenant);

    let result = apply_replicated_index_op(&manager, tenant, &mut positions, &clear).await;
    set_unix_mode(&root, 0o700);
    assert!(result.is_err());
    assert_eq!(
        manager.tenant_doc_count(tenant),
        Some(0),
        "the clear effect precedes persistence"
    );
    assert_eq!(positions, TenantPositions::default());
    assert_eq!(
        stored_positions(tmp.path(), tenant),
        TenantPositions::default()
    );

    apply_replicated_index_op(&manager, tenant, &mut positions, &clear)
        .await
        .expect("the same owner must retry once persistence is possible");
    let expected = positions_with("node-a", Some((2_000, 3)), Some((2_000, 3)));
    assert_eq!(positions, expected);
    assert_eq!(stored_positions(tmp.path(), tenant), expected);
}
