use super::*;
use crate::handlers::index_resource_store::delete_resource_item_and_publish;
use flapjack::index::rules::RuleStore;

#[cfg(feature = "fault-injection")]
#[derive(Clone, Copy)]
enum ResourceClearKind {
    Rule,
    Synonym,
}

#[cfg(feature = "fault-injection")]
impl ResourceClearKind {
    fn route_segment(self) -> &'static str {
        match self {
            Self::Rule => "rules",
            Self::Synonym => "synonyms",
        }
    }

    fn clear_operation(self) -> &'static str {
        match self {
            Self::Rule => "clear_rules",
            Self::Synonym => "clear_synonyms",
        }
    }

    fn seed_body(self) -> &'static str {
        match self {
            Self::Rule => r#"[{"objectID":"clear-item","conditions":[],"consequence":{}}]"#,
            Self::Synonym => {
                r#"[{"objectID":"clear-item","type":"synonym","synonyms":["one","two"]}]"#
            }
        }
    }

    fn cached_item_exists(self, state: &AppState, tenant_id: &str) -> bool {
        match self {
            Self::Rule => state
                .manager
                .get_rules(tenant_id)
                .is_some_and(|store| store.get("clear-item").is_some()),
            Self::Synonym => state
                .manager
                .get_synonyms(tenant_id)
                .is_some_and(|store| store.get("clear-item").is_some()),
        }
    }
}

#[cfg(feature = "fault-injection")]
#[derive(Clone, Copy)]
enum MetadataOplogRoute {
    Settings,
    Rule,
    Synonym,
    ClearIndex,
}

#[cfg(feature = "fault-injection")]
#[derive(Clone, Copy)]
enum MetadataOplogFault {
    BeforeRowSync,
    BeforeWatermark,
}

#[cfg(feature = "fault-injection")]
async fn issue_metadata_oplog_request(
    app: &Router,
    tenant_id: &str,
    route: MetadataOplogRoute,
) -> axum::http::Response<Body> {
    match route {
        MetadataOplogRoute::Settings => {
            post_settings_for(
                app,
                tenant_id,
                r#"{"searchableAttributes":["metadata_title"]}"#,
            )
            .await
        }
        MetadataOplogRoute::Rule => {
            post_rules_batch(
                app,
                tenant_id,
                r#"[{"objectID":"metadata-rule","conditions":[],"consequence":{}}]"#,
                false,
            )
            .await
        }
        MetadataOplogRoute::Synonym => {
            post_synonyms_batch(
                app,
                tenant_id,
                r#"[{"objectID":"metadata-synonym","type":"synonym","synonyms":["one","two"]}]"#,
                false,
            )
            .await
        }
        MetadataOplogRoute::ClearIndex => clear_index_req(app, tenant_id).await,
    }
}

#[cfg(feature = "fault-injection")]
fn assert_metadata_resource_effect(
    state: &Arc<AppState>,
    data_root: &std::path::Path,
    tenant_id: &str,
    route: MetadataOplogRoute,
) {
    match route {
        MetadataOplogRoute::Settings => assert!(std::fs::read_to_string(
            data_root.join(tenant_id).join("settings.json")
        )
        .unwrap()
        .contains("metadata_title")),
        MetadataOplogRoute::Rule => assert!(std::fs::read_to_string(
            data_root.join(tenant_id).join("rules.json")
        )
        .unwrap()
        .contains("metadata-rule")),
        MetadataOplogRoute::Synonym => assert!(std::fs::read_to_string(
            data_root.join(tenant_id).join("synonyms.json")
        )
        .unwrap()
        .contains("metadata-synonym")),
        MetadataOplogRoute::ClearIndex => assert!(state
            .manager
            .get_document(tenant_id, "metadata-document")
            .unwrap()
            .is_none()),
    }
}

#[cfg(feature = "fault-injection")]
async fn assert_metadata_oplog_fault_response(
    tenant_id: &str,
    route: MetadataOplogRoute,
    fault: MetadataOplogFault,
) {
    let temp_dir = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&temp_dir).build_shared();
    let app = replica_sync_router_with_synonyms_rules(Arc::clone(&state));
    state.manager.create_tenant(tenant_id).unwrap();
    if matches!(route, MetadataOplogRoute::ClearIndex) {
        state
            .manager
            .add_documents_sync(
                tenant_id,
                vec![make_doc("metadata-document", "must be cleared")],
            )
            .await
            .unwrap();
    }
    let oplog = state.manager.get_or_create_oplog(tenant_id).unwrap();
    let prior_seq = oplog.current_seq();
    let watermark_path = temp_dir.path().join(tenant_id).join("committed_seq");
    let prior_watermark = std::fs::read(&watermark_path).unwrap();
    let publication_baseline_seq = if matches!(route, MetadataOplogRoute::ClearIndex) {
        0
    } else {
        prior_seq
    };
    let publication_baseline_watermark = if matches!(route, MetadataOplogRoute::ClearIndex) {
        b"0".to_vec()
    } else {
        prior_watermark
    };

    let response = match fault {
        MetadataOplogFault::BeforeRowSync => {
            let _fault = state
                .manager
                .fail_next_taskless_oplog_row_sync_for_test(tenant_id);
            issue_metadata_oplog_request(&app, tenant_id, route).await
        }
        MetadataOplogFault::BeforeWatermark => {
            let _fault = state
                .manager
                .fail_next_committed_seq_publication_for_test(tenant_id);
            issue_metadata_oplog_request(&app, tenant_id, route).await
        }
    };

    assert!(!response.status().is_success());
    assert_metadata_resource_effect(&state, temp_dir.path(), tenant_id, route);
    assert_eq!(
        std::fs::read(&watermark_path).unwrap(),
        publication_baseline_watermark
    );
    let reopened_manager = flapjack::IndexManager::new(temp_dir.path());
    let reopened_oplog = reopened_manager.get_or_create_oplog(tenant_id).unwrap();
    match fault {
        MetadataOplogFault::BeforeRowSync => {
            assert_eq!(reopened_oplog.current_seq(), publication_baseline_seq);
            assert!(reopened_oplog
                .read_since(publication_baseline_seq)
                .unwrap()
                .is_empty());
        }
        MetadataOplogFault::BeforeWatermark => {
            assert_eq!(reopened_oplog.current_seq(), publication_baseline_seq + 1);
            assert_eq!(
                reopened_oplog
                    .read_since(publication_baseline_seq)
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                reopened_oplog.committed_seq().unwrap(),
                Some(publication_baseline_seq)
            );
        }
    }
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_settings_fails_before_row_sync() {
    assert_metadata_oplog_fault_response(
        "metadata_http_settings_row_sync",
        MetadataOplogRoute::Settings,
        MetadataOplogFault::BeforeRowSync,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_settings_fails_before_watermark() {
    assert_metadata_oplog_fault_response(
        "metadata_http_settings_watermark",
        MetadataOplogRoute::Settings,
        MetadataOplogFault::BeforeWatermark,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_rule_fails_before_row_sync() {
    assert_metadata_oplog_fault_response(
        "metadata_http_rule_row_sync",
        MetadataOplogRoute::Rule,
        MetadataOplogFault::BeforeRowSync,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_rule_fails_before_watermark() {
    assert_metadata_oplog_fault_response(
        "metadata_http_rule_watermark",
        MetadataOplogRoute::Rule,
        MetadataOplogFault::BeforeWatermark,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_synonym_fails_before_row_sync() {
    assert_metadata_oplog_fault_response(
        "metadata_http_synonym_row_sync",
        MetadataOplogRoute::Synonym,
        MetadataOplogFault::BeforeRowSync,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_synonym_fails_before_watermark() {
    assert_metadata_oplog_fault_response(
        "metadata_http_synonym_watermark",
        MetadataOplogRoute::Synonym,
        MetadataOplogFault::BeforeWatermark,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_clear_index_fails_before_row_sync() {
    assert_metadata_oplog_fault_response(
        "metadata_http_clear_row_sync",
        MetadataOplogRoute::ClearIndex,
        MetadataOplogFault::BeforeRowSync,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_clear_index_fails_before_watermark() {
    assert_metadata_oplog_fault_response(
        "metadata_http_clear_watermark",
        MetadataOplogRoute::ClearIndex,
        MetadataOplogFault::BeforeWatermark,
    )
    .await;
}

#[tokio::test]
async fn metadata_oplog_successful_settings_response_is_fully_published() {
    let temp_dir = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&temp_dir).build_shared();
    let app = settings_router(Arc::clone(&state));

    let response = post_settings(
        &app,
        r#"{"searchableAttributes":["metadata_success_title"]}"#,
    )
    .await;

    assert!(response.status().is_success());
    assert!(
        std::fs::read_to_string(temp_dir.path().join("test_idx/settings.json"))
            .unwrap()
            .contains("metadata_success_title")
    );
    let oplog = state.manager.get_or_create_oplog("test_idx").unwrap();
    assert_eq!(oplog.committed_seq().unwrap(), Some(oplog.current_seq()));
}

#[cfg(feature = "fault-injection")]
async fn assert_primary_resource_clear_durability(kind: ResourceClearKind, tenant_id: &str) {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = replica_sync_router_with_synonyms_rules(Arc::clone(&state));
    let resource = kind.route_segment();
    let batch_uri = format!("/1/indexes/{tenant_id}/{resource}/batch");
    let clear_uri = format!("/1/indexes/{tenant_id}/{resource}/clear");
    assert!(post_json(&app, &batch_uri, kind.seed_body())
        .await
        .status()
        .is_success());
    assert!(kind.cached_item_exists(&state, tenant_id));

    let oplog = state.manager.get_oplog(tenant_id).unwrap();
    let prior_seq = oplog.current_seq();
    let watermark_path = tmp.path().join(tenant_id).join("committed_seq");
    let prior_watermark = std::fs::read(&watermark_path).unwrap();
    let tenant_path = tmp.path().join(tenant_id);

    let unlink_fault = flapjack::index::fail_next_directory_sync_for_test(&tenant_path);
    let response = post_json(&app, &clear_uri, "").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(unlink_fault.was_triggered());
    assert!(!tenant_path.join(format!("{resource}.json")).exists());
    assert!(kind.cached_item_exists(&state, tenant_id));
    assert_eq!(oplog.current_seq(), prior_seq);
    assert_eq!(std::fs::read(&watermark_path).unwrap(), prior_watermark);
    assert!(oplog.read_since(prior_seq).unwrap().is_empty());

    let missing_file_fault = flapjack::index::fail_next_directory_sync_for_test(&tenant_path);
    let response = post_json(&app, &clear_uri, "").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(missing_file_fault.was_triggered());
    assert!(kind.cached_item_exists(&state, tenant_id));
    assert_eq!(oplog.current_seq(), prior_seq);
    assert_eq!(std::fs::read(&watermark_path).unwrap(), prior_watermark);

    let response = post_json(&app, &clear_uri, "").await;
    assert!(response.status().is_success());
    let response_json = crate::test_helpers::body_json(response).await;
    assert!(response_json["taskID"].is_number());
    assert!(!kind.cached_item_exists(&state, tenant_id));
    assert_eq!(oplog.current_seq(), prior_seq + 1);
    assert_eq!(oplog.committed_seq().unwrap(), Some(prior_seq + 1));
    let rows = oplog.read_since(prior_seq).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].op_type, kind.clear_operation());

    drop(rows);
    drop(oplog);
    drop(app);
    drop(state);
    let reopened = flapjack::IndexManager::new(tmp.path());
    let reopened_oplog = reopened.get_or_create_oplog(tenant_id).unwrap();
    assert_eq!(reopened_oplog.committed_seq().unwrap(), Some(prior_seq + 1));
    let rows = reopened_oplog.read_since(prior_seq).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].op_type, kind.clear_operation());
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_resource_clear_requires_durable_parent_sync() {
    assert_primary_resource_clear_durability(
        ResourceClearKind::Rule,
        "metadata_resource_clear_rules",
    )
    .await;
    assert_primary_resource_clear_durability(
        ResourceClearKind::Synonym,
        "metadata_resource_clear_synonyms",
    )
    .await;
}

#[cfg(feature = "fault-injection")]
async fn delete_metadata_resource(
    state: Arc<AppState>,
    tenant_id: &str,
    resource: &str,
) -> axum::http::Response<Body> {
    let app = Router::new()
        .route(
            "/1/indexes/:indexName/rules/:objectID",
            axum::routing::delete(crate::handlers::rules::delete_rule),
        )
        .route(
            "/1/indexes/:indexName/synonyms/:objectID",
            axum::routing::delete(crate::handlers::synonyms::delete_synonym),
        )
        .with_state(state);
    app.oneshot(
        Request::builder()
            .method("DELETE")
            .uri(format!("/1/indexes/{tenant_id}/{resource}/retry-item"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

#[cfg(feature = "fault-injection")]
async fn seed_delete_retry_resource(state: &Arc<AppState>, tenant_id: &str, resource: &str) {
    let app = replica_sync_router_with_synonyms_rules(Arc::clone(state));
    let body = if resource == "rules" {
        r#"[{"objectID":"retry-item","conditions":[],"consequence":{}}]"#
    } else {
        r#"[{"objectID":"retry-item","type":"synonym","synonyms":["one","two"]}]"#
    };
    assert!(post_json(
        &app,
        &format!("/1/indexes/{tenant_id}/{resource}/batch"),
        body
    )
    .await
    .status()
    .is_success());
}

#[cfg(feature = "fault-injection")]
async fn assert_delete_not_found_response(retry: axum::http::Response<Body>, resource: &str) {
    let json = crate::test_helpers::body_json(retry).await;
    let kind = if resource == "rules" {
        "Rule"
    } else {
        "Synonym"
    };
    assert_eq!(
        json,
        serde_json::json!({"message": format!("{kind} retry-item not found"), "status": 404})
    );
}

#[cfg(feature = "fault-injection")]
async fn assert_delete_retry_publication(
    tenant_id: &str,
    resource: &str,
    fault: MetadataOplogFault,
) {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    seed_delete_retry_resource(&state, tenant_id, resource).await;
    let prior_seq = state.manager.get_oplog(tenant_id).unwrap().current_seq();
    let watermark_path = tmp.path().join(tenant_id).join("committed_seq");
    let prior_watermark = std::fs::read(&watermark_path).unwrap();
    let response = match fault {
        MetadataOplogFault::BeforeRowSync => {
            let _fault = state
                .manager
                .fail_next_taskless_oplog_row_sync_for_test(tenant_id);
            delete_metadata_resource(Arc::clone(&state), tenant_id, resource).await
        }
        MetadataOplogFault::BeforeWatermark => {
            let _fault = state
                .manager
                .fail_next_committed_seq_publication_for_test(tenant_id);
            delete_metadata_resource(Arc::clone(&state), tenant_id, resource).await
        }
    };
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let resource_path = tmp.path().join(tenant_id).join(format!("{resource}.json"));
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(resource_path).unwrap()).unwrap();
    assert_eq!(persisted, serde_json::json!([]));
    assert_eq!(std::fs::read(&watermark_path).unwrap(), prior_watermark);
    drop(
        state
            .manager
            .quiesce_tenant(&tenant_id.to_string())
            .await
            .unwrap(),
    );
    drop(state);
    let reopened = TestStateBuilder::new(&tmp).build_shared();
    let oplog = reopened.manager.get_or_create_oplog(tenant_id).unwrap();
    let retained_rows = u64::from(matches!(fault, MetadataOplogFault::BeforeWatermark));
    assert_eq!(oplog.current_seq(), prior_seq + retained_rows);
    assert_eq!(oplog.committed_seq().unwrap(), Some(prior_seq));
    assert_eq!(
        oplog.read_since(prior_seq).unwrap().len(),
        retained_rows as usize
    );
    let retry = delete_metadata_resource(Arc::clone(&reopened), tenant_id, resource).await;
    match fault {
        MetadataOplogFault::BeforeRowSync => {
            assert_eq!(retry.status(), StatusCode::NOT_FOUND);
            assert_delete_not_found_response(retry, resource).await;
            assert_eq!(oplog.committed_seq().unwrap(), Some(prior_seq + 1));
        }
        MetadataOplogFault::BeforeWatermark => {
            assert_eq!(retry.status(), StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!(std::fs::read(&watermark_path).unwrap(), prior_watermark);
        }
    }
    let rows = oplog.read_since(prior_seq).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].seq, prior_seq + 1);
    assert_eq!(
        rows[0].op_type,
        if resource == "rules" {
            "delete_rule"
        } else {
            "delete_synonym"
        }
    );
    assert_eq!(
        rows[0].payload,
        serde_json::json!({"objectID": "retry-item"})
    );
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_rule_delete_retry_after_row_sync_rollback() {
    assert_delete_retry_publication(
        "metadata_rule_delete_retry_sync",
        "rules",
        MetadataOplogFault::BeforeRowSync,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_synonym_delete_retry_after_row_sync_rollback() {
    assert_delete_retry_publication(
        "metadata_synonym_delete_retry_sync",
        "synonyms",
        MetadataOplogFault::BeforeRowSync,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_rule_delete_retry_stays_blocked_before_watermark() {
    assert_delete_retry_publication(
        "metadata_rule_delete_retry_watermark",
        "rules",
        MetadataOplogFault::BeforeWatermark,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_synonym_delete_retry_stays_blocked_before_watermark() {
    assert_delete_retry_publication(
        "metadata_synonym_delete_retry_watermark",
        "synonyms",
        MetadataOplogFault::BeforeWatermark,
    )
    .await;
}

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn metadata_oplog_delete_from_missing_index_preserves_later_creation() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    for resource in ["rules", "synonyms"] {
        let tenant_id = format!("metadata_missing_index_delete_{resource}");
        let response = delete_metadata_resource(Arc::clone(&state), &tenant_id, resource).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            !tmp.path().join(&tenant_id).exists(),
            "a not-found delete must not leave a partial tenant"
        );
        state.manager.create_tenant(&tenant_id).unwrap();
    }
}

#[tokio::test]
async fn metadata_oplog_shared_delete_publication_applies_physical_index_policy() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    let absent_tenant = "metadata_shared_delete_absent";
    let removed =
        delete_resource_item_and_publish::<RuleStore>(&state, absent_tenant, "missing-rule")
            .await
            .unwrap();
    assert!(!removed);
    assert!(!tmp.path().join(absent_tenant).exists());

    let physical_tenant = "metadata_shared_delete_physical";
    state.manager.create_tenant(physical_tenant).unwrap();
    let removed =
        delete_resource_item_and_publish::<RuleStore>(&state, physical_tenant, "missing-rule")
            .await
            .unwrap();
    assert!(!removed);

    let oplog = state.manager.get_oplog(physical_tenant).unwrap();
    assert_eq!(oplog.current_seq(), 1);
    assert_eq!(oplog.committed_seq().unwrap(), Some(1));
    let rows = oplog.read_since(0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].op_type, "delete_rule");
    assert_eq!(
        rows[0].payload,
        serde_json::json!({"objectID": "missing-rule"})
    );
}
