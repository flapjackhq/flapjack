use super::{
    migrate_from_algolia_with_test_source_factory, MigrateFromAlgoliaRequest,
    MigrateFromAlgoliaResponse, MIGRATION_HA_UNSUPPORTED_CODE, MIGRATION_HA_UNSUPPORTED_MESSAGE,
    MIGRATION_IMPORT_UNAVAILABLE_CODE, MIGRATION_IMPORT_UNAVAILABLE_MESSAGE,
};
use crate::handlers::indices::list_indices;
use crate::handlers::migration::algolia_client::AlgoliaIndexRecord;
use crate::handlers::migration::source_test_support::ScriptedSourceReader;
use crate::test_helpers::{body_json, TestStateBuilder};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use flapjack_replication::{
    config::{NodeConfig, PeerConfig},
    manager::ReplicationManager,
};
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tempfile::TempDir;

const SOURCE_APP_ID: &str = "LOCALMIGRATIONTEST";
const SOURCE_API_KEY: &str = "hermetic-source-key-not-used";
const SOURCE_INDEX: &str = "source_products";
const TARGET_INDEX: &str = "migrated_products";
// MIG-3 flips this when destination import is implemented; until then, MIG-1
// requires honest admission refusal with no target index or spool writes.
const EXPECT_IMPORT_IMPLEMENTED: bool = false;

fn valid_request() -> MigrateFromAlgoliaRequest {
    MigrateFromAlgoliaRequest {
        app_id: SOURCE_APP_ID.to_string(),
        api_key: SOURCE_API_KEY.to_string(),
        source_index: SOURCE_INDEX.to_string(),
        target_index: Some(TARGET_INDEX.to_string()),
        overwrite: false,
    }
}

fn hermetic_source_reader() -> ScriptedSourceReader {
    let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
    let source_record = AlgoliaIndexRecord {
        name: SOURCE_INDEX.to_string(),
        entries: 2,
        updated_at: "2026-07-16T00:00:00Z".to_string(),
        pending_task: false,
    };
    let settings = json!({"attributesForFaceting": ["category"]});
    let documents = vec![vec![
        json!({"objectID": "doc-1", "title": "One", "category": "alpha"}),
        json!({"objectID": "doc-2", "title": "Two", "category": "beta"}),
    ]];
    reader.push_quiescent(source_record.clone());
    reader.push_pass(settings.clone(), documents.clone(), vec![], vec![]);
    reader.push_pass(settings, documents, vec![], vec![]);
    reader.push_quiescent(source_record);
    reader
}

fn replication_manager_with_peers(peers: Vec<PeerConfig>) -> Arc<ReplicationManager> {
    ReplicationManager::new(
        NodeConfig {
            node_id: "local-test-node".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            peers,
        },
        None,
    )
}

fn peer_configured_replication_manager() -> Arc<ReplicationManager> {
    replication_manager_with_peers(vec![PeerConfig {
        node_id: "remote-test-node".to_string(),
        addr: "http://127.0.0.1:7701".to_string(),
    }])
}

#[tokio::test]
async fn migrate_reported_counts_imply_target_contains_documents() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    if EXPECT_IMPORT_IMPLEMENTED {
        assert!(
            source_factory_invoked.load(Ordering::SeqCst),
            "implemented migration must use the hermetic source fixture"
        );
        assert_import_reported_equals_target_contents(&state, response).await;
    } else {
        assert!(
            !source_factory_invoked.load(Ordering::SeqCst),
            "temporary refusal must happen before any source reader is constructed"
        );
        assert_refusal_and_no_target(&state, response).await;
    }
}

#[tokio::test]
async fn migrate_refuses_ha_cluster_before_import_unavailable() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(peer_configured_replication_manager())
        .build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    let error = response.expect_err("HA migration should be refused before import fallback");

    assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": MIGRATION_HA_UNSUPPORTED_MESSAGE,
            "status": 503,
            "code": MIGRATION_HA_UNSUPPORTED_CODE
        })
    );
    assert!(
        !source_factory_invoked.load(Ordering::SeqCst),
        "HA refusal must happen before any source reader is constructed"
    );
    assert_no_migration_artifacts(&state);
}

#[tokio::test]
async fn migrate_without_replication_manager_keeps_import_unavailable_refusal() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    assert!(
        !source_factory_invoked.load(Ordering::SeqCst),
        "standalone refusal must happen before any source reader is constructed"
    );
    assert_refusal_and_no_target(&state, response).await;
}

#[tokio::test]
async fn migrate_with_zero_replication_peers_keeps_import_unavailable_refusal() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(replication_manager_with_peers(vec![]))
        .build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);

    assert_eq!(
        state
            .replication_manager
            .as_ref()
            .expect("test state should carry replication manager")
            .peer_count(),
        0
    );

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    assert!(
        !source_factory_invoked.load(Ordering::SeqCst),
        "zero-peer refusal must happen before any source reader is constructed"
    );
    assert_refusal_and_no_target(&state, response).await;
}

#[tokio::test]
async fn migrate_validates_request_before_ha_admission_guard() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(peer_configured_replication_manager())
        .build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(MigrateFromAlgoliaRequest {
            app_id: String::new(),
            api_key: SOURCE_API_KEY.to_string(),
            source_index: SOURCE_INDEX.to_string(),
            target_index: Some(TARGET_INDEX.to_string()),
            overwrite: false,
        }),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    let error = response.expect_err("invalid request should fail before HA admission");

    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": "appId, apiKey, and sourceIndex are required",
            "status": 400
        })
    );
    assert!(
        !source_factory_invoked.load(Ordering::SeqCst),
        "validation refusal must happen before any source reader is constructed"
    );
    assert_no_migration_artifacts(&state);
}

#[tokio::test]
async fn migrate_validates_target_index_before_ha_admission_guard() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(peer_configured_replication_manager())
        .build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(MigrateFromAlgoliaRequest {
            app_id: SOURCE_APP_ID.to_string(),
            api_key: SOURCE_API_KEY.to_string(),
            source_index: SOURCE_INDEX.to_string(),
            target_index: Some("../escape".to_string()),
            overwrite: false,
        }),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    let error = response.expect_err("invalid targetIndex should fail before HA admission");

    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": "Invalid query: Index name contains invalid characters (path traversal not allowed)",
            "status": 400
        })
    );
    assert!(
        !source_factory_invoked.load(Ordering::SeqCst),
        "targetIndex validation must happen before any source reader is constructed"
    );
    assert_no_migration_artifacts(&state);
}

async fn assert_refusal_and_no_target(
    state: &Arc<crate::handlers::AppState>,
    response: Result<Json<MigrateFromAlgoliaResponse>, super::MigrateError>,
) {
    let error = response.expect_err("migration import should be refused before export starts");

    assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": MIGRATION_IMPORT_UNAVAILABLE_MESSAGE,
            "status": 503,
            "code": MIGRATION_IMPORT_UNAVAILABLE_CODE
        })
    );
    assert_no_migration_artifacts(state);
}

fn assert_no_migration_artifacts(state: &Arc<crate::handlers::AppState>) {
    assert!(
        !state.manager.base_path.join(TARGET_INDEX).exists(),
        "refused migration must not create the target index"
    );
    assert!(
        !state
            .manager
            .base_path
            .join("migration_exports")
            .join("jobs")
            .exists(),
        "refused migration must not create spool jobs"
    );
}

async fn assert_import_reported_equals_target_contents(
    state: &Arc<crate::handlers::AppState>,
    response: Result<Json<MigrateFromAlgoliaResponse>, super::MigrateError>,
) {
    let Json(response) = response.expect("implemented migration should succeed");
    let Json(indices) = list_indices(State(Arc::clone(state)), Query(HashMap::new()))
        .await
        .expect("target index should be listable after import");
    let target = indices
        .items
        .iter()
        .find(|item| item.name == TARGET_INDEX)
        .expect("successful import should create the target index");

    assert_eq!(
        response.objects.imported as u64, target.entries,
        "reported imported object count must match target index entries"
    );
}
