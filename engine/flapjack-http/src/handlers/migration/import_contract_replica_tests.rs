use super::*;
use crate::handlers::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use flapjack::index::settings::IndexSettings;
use serde_json::json;
use std::sync::Arc;

#[test]
fn hermetic_fixture_uses_canonical_parser_for_malformed_replica_entries() {
    let reader = hermetic_source_reader_with_settings_and_pages(
        json!({
            "searchableAttributes": ["title"],
            "replicas": ["virtual(bad/name)"],
        }),
        vec![scripted_documents(EXPECTED_DOCUMENTS)],
    );

    assert!(
        reader.index_settings_reads.is_empty(),
        "invalid entries must follow the canonical parser result instead of queueing a derived name"
    );
}

#[test]
fn hermetic_fixture_queues_duplicate_replica_names_once_in_collector_order() {
    let reader = hermetic_source_reader_with_settings_and_pages(
        json!({
            "searchableAttributes": ["title"],
            "replicas": [
                "replica_price_asc",
                "virtual(replica_price_asc)",
                "replica_relevance",
                "replica_price_asc"
            ],
        }),
        vec![scripted_documents(EXPECTED_DOCUMENTS)],
    );

    let queued_names = reader
        .index_settings_reads
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(queued_names, ["replica_price_asc", "replica_relevance"]);
}

/// A source replica whose settings are fetched successfully migrates as a
/// virtual replica on the target primary, with fidelity loss reported as
/// warnings rather than a hard rejection.
#[tokio::test]
async fn migrate_replica_topology_activates_virtual_replica_with_warnings() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| {
            let settings = json!({
                "searchableAttributes": ["title"],
                "replicas": ["replica_idx"],
            });
            let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
            let source_record = AlgoliaIndexRecord {
                name: SOURCE_INDEX.to_string(),
                entries: EXPECTED_DOCUMENTS.len() as u64,
                updated_at: "2026-07-16T00:00:00Z".to_string(),
                pending_task: false,
            };
            reader.push_quiescent(source_record.clone());
            reader.push_pass(
                settings.clone(),
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader.push_pass(
                settings,
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader.push_index_settings(
                "replica_idx",
                Ok(json!({"ranking": ["desc(price)"], "relevancyStrictness": 80})),
            );
            reader.push_quiescent(source_record);
            Ok(reader)
        },
    )
    .await;

    let response = response.expect("replica topology should translate with warnings");
    assert_eq!(response.status, "complete");
    assert_warning_codes(
        &response.warnings,
        &[
            "ReplicaRelevancyStrictnessSemanticMismatch",
            "ReplicaExhaustiveSortApproximated",
        ],
    );
    assert_warning_message(
        &response.warnings,
        "ReplicaExhaustiveSortApproximated",
        "Algolia standard replica exhaustive sorting is approximated as a Flapjack virtual replica.",
    );
    let settings = state.manager.get_settings(TARGET_INDEX).unwrap();
    assert_eq!(
        settings.replicas,
        Some(vec!["virtual(replica_idx)".to_string()])
    );
    assert!(settings.relevancy_strictness.is_none());
    assert_activated_settings_json(&state);
    assert_no_retained_accepted_spool_document_artifacts(&state);
}

/// The activated primary's raw `settings.json` is the client-visible artifact:
/// assert the serialized document itself, not just the typed struct, so a future
/// serde default cannot reintroduce the dropped `relevancyStrictness` field.
fn assert_activated_settings_json(state: &Arc<AppState>) {
    let raw = std::fs::read_to_string(
        state
            .manager
            .base_path
            .join(TARGET_INDEX)
            .join("settings.json"),
    )
    .expect("activated primary should have an on-disk settings.json");
    let settings: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert!(
        settings.get("relevancyStrictness").is_none(),
        "activated settings.json must omit relevancyStrictness, got {raw}"
    );
    assert_eq!(settings["replicas"], json!(["virtual(replica_idx)"]));
}

/// A replica settings fetch failure is a real source failure once translation is
/// active: it must surface as the typed, credential-scrubbed Algolia error and
/// leave no target behind, never as a generic malformed-payload rejection.
#[tokio::test]
async fn migrate_replica_settings_fetch_failure_fails_closed_with_source_error() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| {
            let settings = json!({
                "searchableAttributes": ["title"],
                "replicas": ["replica_idx"],
            });
            let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
            let source_record = AlgoliaIndexRecord {
                name: SOURCE_INDEX.to_string(),
                entries: EXPECTED_DOCUMENTS.len() as u64,
                updated_at: "2026-07-16T00:00:00Z".to_string(),
                pending_task: false,
            };
            reader.push_quiescent(source_record.clone());
            reader.push_pass(
                settings.clone(),
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader.push_pass(
                settings,
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader.push_index_settings(
                "replica_idx",
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Algolia replica settings request failed",
                )),
            );
            reader.push_quiescent(source_record);
            Ok(reader)
        },
    )
    .await;

    let error = response.expect_err("replica settings fetch failure must fail the import");
    assert_eq!(error.0, StatusCode::BAD_GATEWAY);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({"message": "Algolia replica settings request failed", "status": 502})
    );
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
}

/// After successful migration with a replica, the derived replica directory
/// exists as a settings-only virtual sidecar: settings.json with primary link
/// and translated rankings, but no meta.json or Tantivy data.
#[tokio::test]
async fn migrate_replica_materializes_virtual_sidecar_with_translated_rankings() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| {
            let settings = json!({
                "searchableAttributes": ["title"],
                "replicas": ["virtual(replica_price_asc)"],
            });
            let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
            let source_record = AlgoliaIndexRecord {
                name: SOURCE_INDEX.to_string(),
                entries: EXPECTED_DOCUMENTS.len() as u64,
                updated_at: "2026-07-16T00:00:00Z".to_string(),
                pending_task: false,
            };
            reader.push_quiescent(source_record.clone());
            reader.push_pass(
                settings.clone(),
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader.push_pass(
                settings,
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader.push_index_settings(
                "replica_price_asc",
                Ok(json!({
                    "ranking": ["desc(price)", "typo", "geo"],
                    "customRanking": ["asc(popularity)"],
                    "relevancyStrictness": 50
                })),
            );
            reader.push_quiescent(source_record);
            Ok(reader)
        },
    )
    .await;

    let response = response.expect("migration with replica should succeed");
    assert_eq!(response.status, "complete");

    // Sidecar directory must exist
    let replica_dir = state.manager.base_path.join("replica_price_asc");
    assert!(replica_dir.exists(), "replica sidecar directory must exist");

    // Must have settings.json with primary link and translated rankings
    let settings_path = replica_dir.join("settings.json");
    assert!(settings_path.exists(), "replica must have settings.json");
    let replica_settings = IndexSettings::load(&settings_path).unwrap();
    assert_eq!(
        replica_settings.primary.as_deref(),
        Some(TARGET_INDEX),
        "replica settings.primary must point to the migrated primary"
    );
    // Hand-derived from the source `ranking: ["desc(price)", "typo", "geo"]`:
    // the sort token lifts into custom_ranking, the two criterion tokens stay in
    // ranking, and the source `customRanking` is dropped because the source
    // ranking array does not contain `custom`.
    assert_eq!(
        replica_settings.ranking.as_deref(),
        Some(["typo".to_string(), "geo".to_string()].as_slice()),
        "replica ranking must keep only recognized criterion tokens, in source order"
    );
    assert_eq!(
        replica_settings.custom_ranking.as_deref(),
        Some(["desc(price)".to_string()].as_slice()),
        "replica custom_ranking must be the lifted sort token, with the \
         source customRanking dropped because ranking omits `custom`"
    );
    assert_eq!(
        replica_settings.relevancy_strictness,
        Some(50),
        "replica must carry its own relevancyStrictness"
    );

    // Must NOT have meta.json (settings-only sidecar)
    let meta_path = replica_dir.join("meta.json");
    assert!(
        !meta_path.exists(),
        "virtual replica sidecar must not have meta.json"
    );
}

/// When a derived replica directory already exists before migration starts,
/// the import must fail with 409 Conflict to avoid corrupting pre-existing data.
#[tokio::test]
async fn migrate_replica_collision_with_existing_directory_returns_409() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    // Pre-create the replica directory to simulate a collision
    std::fs::create_dir_all(state.manager.base_path.join("replica_price_asc")).unwrap();
    std::fs::write(
        state
            .manager
            .base_path
            .join("replica_price_asc")
            .join("settings.json"),
        "{}",
    )
    .unwrap();

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| {
            let settings = json!({
                "searchableAttributes": ["title"],
                "replicas": ["virtual(replica_price_asc)"],
            });
            let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
            let source_record = AlgoliaIndexRecord {
                name: SOURCE_INDEX.to_string(),
                entries: EXPECTED_DOCUMENTS.len() as u64,
                updated_at: "2026-07-16T00:00:00Z".to_string(),
                pending_task: false,
            };
            reader.push_quiescent(source_record.clone());
            reader.push_pass(
                settings.clone(),
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader.push_pass(
                settings,
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader
                .push_index_settings("replica_price_asc", Ok(json!({"ranking": ["desc(price)"]})));
            reader.push_quiescent(source_record);
            Ok(reader)
        },
    )
    .await;

    let error = response.expect_err("collision must fail the import");
    assert_eq!(error.0, StatusCode::CONFLICT);
}

/// Standard replicas from Algolia are also materialized as virtual sidecars
/// on Flapjack (both Standard and Virtual source entries become virtual on
/// the target), each with their own distinct translated ranking.
#[tokio::test]
async fn migrate_standard_replica_materializes_as_virtual_sidecar() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| {
            let settings = json!({
                "searchableAttributes": ["title"],
                "replicas": ["replica_date_desc"],
            });
            let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
            let source_record = AlgoliaIndexRecord {
                name: SOURCE_INDEX.to_string(),
                entries: EXPECTED_DOCUMENTS.len() as u64,
                updated_at: "2026-07-16T00:00:00Z".to_string(),
                pending_task: false,
            };
            reader.push_quiescent(source_record.clone());
            reader.push_pass(
                settings.clone(),
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader.push_pass(
                settings,
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader.push_index_settings(
                "replica_date_desc",
                Ok(json!({"ranking": ["desc(date)"], "customRanking": ["desc(views)"]})),
            );
            reader.push_quiescent(source_record);
            Ok(reader)
        },
    )
    .await;

    let response = response.expect("standard replica migration should succeed");
    assert_eq!(response.status, "complete");

    let replica_dir = state.manager.base_path.join("replica_date_desc");
    assert!(
        replica_dir.exists(),
        "standard replica sidecar directory must exist"
    );

    let settings_path = replica_dir.join("settings.json");
    let replica_settings = IndexSettings::load(&settings_path).unwrap();
    assert_eq!(replica_settings.primary.as_deref(), Some(TARGET_INDEX),);
    // Hand-derived from the source `ranking: ["desc(date)"]`: the lone token is
    // a sort token, not a criterion token, so ranking translates to an empty
    // list and the token lifts into custom_ranking. The source `customRanking`
    // is dropped because the source ranking array does not contain `custom`.
    assert_eq!(
        replica_settings.ranking.as_deref(),
        Some([].as_slice()),
        "a source ranking of only sort tokens must translate to an empty ranking"
    );
    assert_eq!(
        replica_settings.custom_ranking.as_deref(),
        Some(["desc(date)".to_string()].as_slice()),
        "the lifted sort token must be the sole custom_ranking entry"
    );

    // No meta.json — it's a virtual sidecar, not a physical index
    assert!(
        !replica_dir.join("meta.json").exists(),
        "standard replica must be materialized as virtual sidecar without meta.json"
    );
}

fn assert_warning_codes(
    warnings: &[crate::handlers::migration::MigrateWarning],
    expected_codes: &[&str],
) {
    let codes = warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect::<Vec<_>>();
    for expected_code in expected_codes {
        assert!(
            codes.contains(expected_code),
            "missing warning code {expected_code}; got {codes:?}"
        );
    }
}

fn assert_warning_message(
    warnings: &[crate::handlers::migration::MigrateWarning],
    code: &str,
    expected: &str,
) {
    let warning = warnings
        .iter()
        .find(|warning| warning.code == code)
        .unwrap_or_else(|| panic!("missing warning code {code}"));
    assert_eq!(warning.message, expected);
    assert_eq!(warning.resource, "Settings");
}

/// Reservation cleanup must survive an unwind, not just an early return: a panic
/// between reservation and activation has to release every attempt-owned claim
/// while leaving pre-existing directories untouched.
#[tokio::test]
async fn migrate_pre_activation_panic_releases_replica_claims_and_spares_existing_dirs() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    // An unrelated index that the reservation guard must never touch.
    let bystander_dir = state.manager.base_path.join("unrelated_idx");
    std::fs::create_dir_all(&bystander_dir).unwrap();
    let bystander_settings = bystander_dir.join("settings.json");
    std::fs::write(&bystander_settings, r#"{"primary":null}"#).unwrap();
    let bystander_bytes = std::fs::read(&bystander_settings).unwrap();

    let task_state = Arc::clone(&state);
    let hooks = ImportTestHooks::default()
        .with_before_activation(|| panic!("deterministic post-reservation unwind"));

    let task = tokio::spawn(async move {
        migrate_from_algolia_with_test_source_factory_and_hooks(
            State(task_state),
            Json(valid_request()),
            |_| {
                Ok(replica_source_reader(
                    "virtual(replica_price_asc)",
                    "replica_price_asc",
                ))
            },
            hooks,
        )
        .await
    });
    let join_error = task
        .await
        .expect_err("post-reservation hook should unwind the import task");
    assert!(join_error.is_panic());

    assert!(
        !state.manager.base_path.join("replica_price_asc").exists(),
        "attempt-owned replica claim must be released on unwind"
    );
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
    assert_eq!(
        std::fs::read(&bystander_settings).unwrap(),
        bystander_bytes,
        "unrelated pre-existing directories must be byte-for-byte unchanged"
    );
}

/// Builds a hermetic source whose primary declares exactly one replica entry,
/// with a fetchable settings payload for the derived replica name.
fn replica_source_reader(replica_entry: &str, derived_name: &str) -> ScriptedSourceReader {
    let settings = json!({
        "searchableAttributes": ["title"],
        "replicas": [replica_entry],
    });
    let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
    let source_record = AlgoliaIndexRecord {
        name: SOURCE_INDEX.to_string(),
        entries: EXPECTED_DOCUMENTS.len() as u64,
        updated_at: "2026-07-16T00:00:00Z".to_string(),
        pending_task: false,
    };
    reader.push_quiescent(source_record.clone());
    reader.push_pass(
        settings.clone(),
        vec![scripted_documents(EXPECTED_DOCUMENTS)],
        vec![],
        vec![],
    );
    reader.push_pass(
        settings,
        vec![scripted_documents(EXPECTED_DOCUMENTS)],
        vec![],
        vec![],
    );
    reader.push_index_settings(
        derived_name,
        Ok(json!({"ranking": ["desc(price)"], "customRanking": ["asc(popularity)"]})),
    );
    reader.push_quiescent(source_record);
    reader
}

/// A sidecar write that fails after the primary is committed must not roll the
/// primary back: the import still succeeds, unaffected replicas materialize,
/// and exactly one runtime warning names each replica left unmaterialized.
#[tokio::test]
async fn migrate_sidecar_failure_warns_without_rolling_back_committed_primary() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let hooks = ImportTestHooks::default().with_before_replica_materialization(|derived_name| {
        if derived_name == "replica_blocked" {
            return Err(flapjack::error::FlapjackError::Io(format!(
                "test obstruction for {derived_name}"
            )));
        }
        Ok(())
    });

    let response = migrate_from_algolia_with_test_source_factory_and_hooks(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| Ok(two_replica_source_reader()),
        hooks,
    )
    .await
    .expect("a failed sidecar must not fail the committed import");
    let response = response.0;

    assert_eq!(response.status, "complete");
    assert_eq!(response.objects.imported, EXPECTED_DOCUMENTS.len());

    // The primary stays committed and queryable with its full corpus.
    assert_eq!(
        state
            .manager
            .get_or_load(TARGET_INDEX)
            .unwrap()
            .reader()
            .searcher()
            .num_docs() as usize,
        EXPECTED_DOCUMENTS.len()
    );

    // The unaffected replica still materialized.
    let healthy_settings =
        IndexSettings::load(sidecar_settings_path(&state, "replica_ok").as_path()).unwrap();
    assert_eq!(healthy_settings.primary.as_deref(), Some(TARGET_INDEX));

    // Exactly one runtime warning, naming only the blocked replica.
    let sidecar_warnings = response
        .warnings
        .iter()
        .filter(|warning| warning.code == "ReplicaSidecarNotMaterialized")
        .collect::<Vec<_>>();
    assert_eq!(
        sidecar_warnings.len(),
        1,
        "one warning per unmaterialized replica; got {:?}",
        response.warnings
    );
    assert!(
        sidecar_warnings[0].message.contains("replica_blocked"),
        "warning must name the failed replica: {}",
        sidecar_warnings[0].message
    );
    assert!(
        !sidecar_warnings[0].message.contains("replica_ok"),
        "a successfully materialized replica must never appear in sidecar warnings"
    );
    assert!(
        sidecar_warnings[0]
            .message
            .contains(&format!("/1/indexes/{TARGET_INDEX}/settings")),
        "warning must name the safe recovery action: {}",
        sidecar_warnings[0].message
    );

    // Appending runtime warnings must not disturb the translation warnings:
    // they stay intact as a prefix, with the runtime warnings as a suffix.
    let codes = response
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect::<Vec<_>>();
    let runtime_start = codes
        .iter()
        .position(|code| *code == "ReplicaSidecarNotMaterialized")
        .expect("runtime warning must be present");
    assert!(
        codes[runtime_start..]
            .iter()
            .all(|code| *code == "ReplicaSidecarNotMaterialized"),
        "runtime warnings must form a contiguous suffix; got {codes:?}"
    );
    // The two standard source replicas each contribute their own translation
    // warnings, and the runtime append must leave that prefix exactly as
    // translation produced it.
    assert_eq!(
        codes[..runtime_start],
        [
            "ReplicaRelevancyStrictnessSemanticMismatch",
            "ReplicaMatchingCriticalFieldDiverges",
            "ReplicaRelevancyStrictnessSemanticMismatch",
            "ReplicaMatchingCriticalFieldDiverges",
            "ReplicaExhaustiveSortApproximated",
            "ReplicaExhaustiveSortApproximated",
        ],
        "translation warnings must be unchanged by the runtime append; got {codes:?}"
    );
}

/// The advertised recovery action must actually work: re-POSTing the complete
/// replicas list to the primary recreates the missing name as a settings-only
/// virtual sidecar, never as a physical tenant.
#[tokio::test]
async fn reposting_primary_replicas_recreates_missing_sidecar_as_virtual_only() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let hooks = ImportTestHooks::default().with_before_replica_materialization(|derived_name| {
        if derived_name == "replica_blocked" {
            return Err(flapjack::error::FlapjackError::Io(format!(
                "test obstruction for {derived_name}"
            )));
        }
        Ok(())
    });
    let _ = migrate_from_algolia_with_test_source_factory_and_hooks(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| Ok(two_replica_source_reader()),
        hooks,
    )
    .await
    .expect("import should complete despite the obstructed sidecar");

    assert!(
        !sidecar_settings_path(&state, "replica_blocked").exists(),
        "precondition: the obstructed replica must start without a sidecar"
    );

    // The obstruction is gone; replay the documented recovery action.
    let recovery = crate::handlers::settings::set_settings(
        State(Arc::clone(&state)),
        crate::extractors::ValidatedIndexName(TARGET_INDEX.to_string()),
        axum::extract::Query(std::collections::HashMap::new()),
        Json(
            serde_json::from_value(json!({
                "replicas": ["virtual(replica_ok)", "virtual(replica_blocked)"],
            }))
            .unwrap(),
        ),
    )
    .await;
    assert!(
        recovery.is_ok(),
        "re-posting the primary replicas list must succeed"
    );

    let recovered =
        IndexSettings::load(sidecar_settings_path(&state, "replica_blocked").as_path()).unwrap();
    assert_eq!(
        recovered.primary.as_deref(),
        Some(TARGET_INDEX),
        "recovery must restore the virtual sidecar's primary link"
    );
    assert!(
        !crate::handlers::replicas::has_physical_index_data(&state, "replica_blocked"),
        "recovery must not create a physical tenant for a virtual replica"
    );
    assert!(
        !state
            .manager
            .base_path
            .join("replica_blocked")
            .join("meta.json")
            .exists(),
        "recreated sidecar must remain settings-only"
    );
}

fn sidecar_settings_path(state: &Arc<AppState>, replica_name: &str) -> std::path::PathBuf {
    state
        .manager
        .base_path
        .join(replica_name)
        .join("settings.json")
}

/// A primary declaring two standard replicas: standard entries also emit an
/// `ReplicaExhaustiveSortApproximated` translation warning, which lets the
/// warning-ordering assertions run against a real translation warning.
fn two_replica_source_reader() -> ScriptedSourceReader {
    let settings = json!({
        "searchableAttributes": ["title"],
        "replicas": ["replica_ok", "replica_blocked"],
    });
    let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
    let source_record = AlgoliaIndexRecord {
        name: SOURCE_INDEX.to_string(),
        entries: EXPECTED_DOCUMENTS.len() as u64,
        updated_at: "2026-07-16T00:00:00Z".to_string(),
        pending_task: false,
    };
    reader.push_quiescent(source_record.clone());
    reader.push_pass(
        settings.clone(),
        vec![scripted_documents(EXPECTED_DOCUMENTS)],
        vec![],
        vec![],
    );
    reader.push_pass(
        settings,
        vec![scripted_documents(EXPECTED_DOCUMENTS)],
        vec![],
        vec![],
    );
    reader.push_index_settings("replica_ok", Ok(json!({"ranking": ["desc(price)"]})));
    reader.push_index_settings("replica_blocked", Ok(json!({"ranking": ["desc(date)"]})));
    reader.push_quiescent(source_record);
    reader
}
