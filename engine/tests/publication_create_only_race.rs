//! Stub summary for engine/tests/publication_create_only_race.rs.
use flapjack::analytics::schema::SearchEvent;
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig, AnalyticsQueryEngine};
use flapjack::index::manager::publication::{
    canonical_tenant_tree_digest, scan_and_repair_publication_target, PreStagedActivationError,
    PreStagedPublication, PublicationGenerationEvidence, PublicationJournal, PublicationPaths,
    PublicationPhase, PublicationRepairStatus, PublicationScanAction, PublicationTarget,
    PublicationTargetDisposition, PublicationTransactionId, RepairDecision,
    TantivyManagedInventory,
};
use flapjack::{Document, FlapjackError, Index, IndexManager};
use serde_json::json;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::TempDir;

const TARGET_TENANT: &str = "race_target";
const ACKNOWLEDGED_DOC: &str = "acknowledged-doc";
const STAGED_DOC: &str = "staged-doc";
const FIRST_DOC: &str = "first-racer-doc";
const SECOND_DOC: &str = "second-racer-doc";
/// Independent races run by `exactly_one_create_only_activation_wins_a_concurrent_race`.
/// Sized so a check-then-act regression that survives one trial ~1 time in 3 survives
/// the whole test only ~1 time in 43 million.
const CREATE_ONLY_RACE_TRIALS: usize = 16;

// This nesting is load-bearing, not decoration. Stage 4's gate runs
// `cargo test -p flapjack -- index::manager::publication`, and libtest matches a
// test's MODULE PATH (not its file name). Without these three modules the race
// proof is silently filtered out and the gate passes green while proving nothing.
// The tests live out here (rather than in the publication module) so they cannot
// reach the pub(crate) fault seams and cannot collide with the repair track.
mod index {
    mod manager {
        mod publication {
            use super::super::super::*;

            /// TODO: Document create_only_refuses_target_created_before_existence_snapshot_without_mutation.
            #[tokio::test]
            async fn create_only_refuses_target_created_before_existence_snapshot_without_mutation()
            {
                let temp = TempDir::new().unwrap();
                let base = temp.path();

                {
                    let manager = IndexManager::new(base);
                    manager.create_tenant(TARGET_TENANT).unwrap();
                    manager
                        .add_documents_sync(
                            TARGET_TENANT,
                            vec![Document::from_json(&json!({
                                "objectID": ACKNOWLEDGED_DOC,
                                "title": "acknowledged target document"
                            }))
                            .unwrap()],
                        )
                        .await
                        .unwrap();

                    assert_eq!(
                        searchable_ids(&manager, TARGET_TENANT),
                        vec![ACKNOWLEDGED_DOC.to_string()]
                    );
                    manager.graceful_shutdown().await;
                }

                let publication = PreStagedPublication::prepare(
                    base,
                    PublicationTarget::new(TARGET_TENANT).unwrap(),
                )
                .unwrap();
                let staging_path = publication.paths().staging.clone();
                let staging_tenant = staging_path
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string();
                let staging_parent = staging_path.parent().unwrap().to_path_buf();

                stage_document(&staging_path, STAGED_DOC);

                {
                    let staging_manager = IndexManager::new(&staging_parent);
                    assert_eq!(
                        searchable_ids(&staging_manager, &staging_tenant),
                        vec![STAGED_DOC.to_string()]
                    );
                }

                let activation = publication.activate_create_only();
                let post_activation_manager = IndexManager::new(base);
                let post_activation_ids = searchable_ids(&post_activation_manager, TARGET_TENANT);
                let activation_source = activation.as_ref().err().and_then(activation_cause);

                assert!(
                    matches!(
                        activation_source,
                        Some(FlapjackError::IndexAlreadyExists(tenant))
                            if tenant == TARGET_TENANT
                    ) && post_activation_ids == [ACKNOWLEDGED_DOC],
                    "activation={activation:?} post_activation_ids={post_activation_ids:?}"
                );
            }

            /// Two fully staged create-only activations race for one absent target.
            ///
            /// Repeated over independent targets. A single race only exposes a
            /// check-then-act implementation when the two threads happen to interleave
            /// inside its check/act window — measured at roughly two runs in three — so one
            /// trial would let the defect through often enough to be useless as a gate.
            /// Independent trials drive detection to a near-certainty without weakening
            /// anything: the target-scoped publication fence yields exactly one winner under *every*
            /// interleaving, so no trial count can make a correct implementation fail here.
            /// The barrier only widens the window; it is not what makes the expectation
            /// hold. There are no sleeps and no retries — each trial asserts on its own.
            #[tokio::test]
            async fn exactly_one_create_only_activation_wins_a_concurrent_race() {
                let temp = TempDir::new().unwrap();
                let base = temp.path();

                for trial in 0..CREATE_ONLY_RACE_TRIALS {
                    assert_create_only_race_has_exactly_one_winner(
                        base,
                        &format!("{TARGET_TENANT}_{trial}"),
                    );
                }
            }

            /// The public deletion owner removes exact customer analytics even
            /// when analytics storage is configured outside the index data root.
            /// Late ingress stays fenced until an exact successful recreate.
            #[tokio::test]
            async fn index_delete_purges_external_analytics_and_blocks_late_resurrection() {
                let temp = TempDir::new().unwrap();
                let index_data_dir = temp.path().join("indexes");
                let analytics_data_dir = temp.path().join("external-analytics");
                let manager = IndexManager::new(&index_data_dir);
                let collector = AnalyticsCollector::new(AnalyticsConfig {
                    enabled: true,
                    data_dir: analytics_data_dir.clone(),
                    flush_interval_secs: 60,
                    flush_size: 100,
                    retention_days: 90,
                });
                manager.set_analytics_collector(Arc::clone(&collector));
                manager.create_tenant("delete-me").unwrap();
                manager.create_tenant("keep-me").unwrap();

                collector.record_search(search_event("delete-me", "deleted-query"));
                collector.record_search(search_event("keep-me", "retained-query"));
                collector.flush_searches();
                assert!(analytics_data_dir.join("delete-me").is_dir());
                assert!(analytics_data_dir.join("keep-me").is_dir());

                manager
                    .delete_tenant(&"delete-me".to_string())
                    .await
                    .unwrap();
                assert!(!analytics_data_dir.join("delete-me").exists());
                assert!(analytics_data_dir.join("keep-me").is_dir());
                assert!(collector.lookup_query_id("deleted-query").is_none());

                collector.record_search(search_event("delete-me", "late-query"));
                collector.flush_searches();
                assert!(!analytics_data_dir.join("delete-me").exists());
                assert!(collector.lookup_query_id("late-query").is_none());

                manager.create_tenant("delete-me").unwrap();
                collector.record_search(search_event("delete-me", "recreated-query"));
                collector.flush_searches();
                assert!(analytics_data_dir.join("delete-me").is_dir());
                assert!(collector.lookup_query_id("recreated-query").is_some());
            }

            #[tokio::test(flavor = "current_thread")]
            async fn raw_publication_scanner_refuses_loadable_while_analytics_delete_is_pending() {
                let temp = TempDir::new().unwrap();
                let base = temp.path();
                let manager = IndexManager::new(base);
                manager.create_tenant("pending-analytics-delete").unwrap();
                let marker =
                    base.join(".publication/pending-analytics-delete/analytics-purge-pending.json");
                fs::create_dir_all(marker.parent().unwrap()).unwrap();
                fs::write(
                    &marker,
                    br#"{"schemaVersion":1,"target":"pending-analytics-delete"}"#,
                )
                .unwrap();

                let report = scan_and_repair_publication_target(
                    base,
                    &AnalyticsConfig::for_data_dir(base),
                    PublicationTarget::new("pending-analytics-delete").unwrap(),
                )
                .unwrap();

                assert_eq!(report.status, PublicationRepairStatus::Unresolved);
                assert_eq!(report.action, PublicationScanAction::Unresolved);
                assert_eq!(
                    report.disposition,
                    PublicationTargetDisposition::Unavailable
                );
                assert!(marker.is_file());
            }

            #[test]
            fn raw_scanner_fences_an_initially_missing_namespace_before_reporting_loadable() {
                let temp = TempDir::new().unwrap();
                let base = temp.path();
                let target = "missing-namespace-live-target";
                fs::create_dir(base.join(target)).unwrap();
                assert!(!base.join(".publication").exists());

                let report = scan_and_repair_publication_target(
                    base,
                    &AnalyticsConfig::for_data_dir(base),
                    PublicationTarget::new(target).unwrap(),
                )
                .unwrap();

                assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
                assert!(
                    base.join(format!(".publication/{target}/epoch.lock"))
                        .is_file(),
                    "a targeted scan must leave the existing ABA-safe fence sidecar"
                );
            }

            #[test]
            fn analytics_index_listing_decodes_customer_names_and_skips_internal_quarantine() {
                let temp = TempDir::new().unwrap();
                let config = AnalyticsConfig::for_data_dir(temp.path());
                for index in ["keep-me", "_fj_customer"] {
                    fs::create_dir_all(config.target_artifact_paths(index).index_root).unwrap();
                }
                fs::create_dir_all(
                    config
                        .data_dir
                        .join("_fj_index_deletion_quarantine")
                        .join("deleted-customer"),
                )
                .unwrap();

                let mut indices = AnalyticsQueryEngine::new(config)
                    .list_analytics_indices()
                    .unwrap();
                indices.sort();

                assert_eq!(
                    indices,
                    vec!["_fj_customer".to_string(), "keep-me".to_string()]
                );
            }

            /// Race two create-only activations for `tenant` and assert the invariant.
            fn assert_create_only_race_has_exactly_one_winner(base: &Path, tenant: &str) {
                let target = PublicationTarget::new(tenant).unwrap();
                let first = PreStagedPublication::prepare(base, target.clone()).unwrap();
                let second = PreStagedPublication::prepare(base, target).unwrap();
                let first_paths = first.paths().clone();
                let second_paths = second.paths().clone();
                stage_document(&first_paths.staging, FIRST_DOC);
                stage_document(&second_paths.staging, SECOND_DOC);

                let barrier = Arc::new(Barrier::new(2));
                let racers =
                    [(first, FIRST_DOC), (second, SECOND_DOC)].map(|(publication, staged_doc)| {
                        let barrier = Arc::clone(&barrier);
                        thread::spawn(move || {
                            barrier.wait();
                            (staged_doc, publication.activate_create_only())
                        })
                    });
                let outcomes = racers.map(|racer| racer.join().unwrap());

                let winners: Vec<&str> = outcomes
                    .iter()
                    .filter(|(_, activation)| activation.is_ok())
                    .map(|(staged_doc, _)| *staged_doc)
                    .collect();
                let losers: Vec<&PreStagedActivationError> = outcomes
                    .iter()
                    .filter_map(|(_, activation)| activation.as_ref().err())
                    .collect();
                assert_eq!(
                    winners.len(),
                    1,
                    "exactly one create-only activation may win {tenant}: {outcomes:?}"
                );
                assert_eq!(
                    losers.len(),
                    1,
                    "exactly one create-only activation must lose {tenant}: {outcomes:?}"
                );

                let loser_cause = activation_cause(losers[0]);
                assert!(
                    matches!(
                        loser_cause,
                        Some(FlapjackError::IndexAlreadyExists(conflict)) if conflict == tenant
                    ),
                    "loser must report the canonical typed conflict: {:?}",
                    losers[0]
                );
                assert_eq!(loser_cause.unwrap().status_code().as_u16(), 409);

                // The winner's tree is intact and the loser contributed nothing to it.
                let manager = IndexManager::new(base);
                assert_eq!(
                    searchable_ids(&manager, tenant),
                    vec![winners[0].to_string()]
                );
                for paths in [&first_paths, &second_paths] {
                    assert!(
                        !paths.backup.exists(),
                        "create-only activation must never back up a prior target: {}",
                        paths.backup.display()
                    );
                }
            }

            /// A create-only activation has no prior target, so it journals no prior digest
            /// and leaves behind neither a backup tree nor a visible placeholder.
            #[tokio::test]
            async fn successful_create_only_activation_records_no_prior_target_and_leaves_no_residue(
            ) {
                let temp = TempDir::new().unwrap();
                let base = temp.path();

                let publication = PreStagedPublication::prepare(
                    base,
                    PublicationTarget::new(TARGET_TENANT).unwrap(),
                )
                .unwrap();
                let paths = publication.paths().clone();
                stage_document(&paths.staging, STAGED_DOC);

                let journal = publication.activate_create_only().unwrap();

                assert_eq!(journal.prior_digest, None);
                assert_eq!(journal.phase, PublicationPhase::Committed);
                assert!(
                    !paths.backup.exists(),
                    "no prior target existed, so nothing may be backed up"
                );
                assert!(
                    !paths.staging.exists(),
                    "the promoted staging tree must not survive as residue"
                );
                assert!(
                    directory_entry_count(&paths.target) > 0,
                    "the published target must be the populated staged tree"
                );
                assert!(
                    paths.journal.exists(),
                    "the transaction namespace must hold committed evidence"
                );

                let manager = IndexManager::new(base);
                assert_eq!(
                    searchable_ids(&manager, TARGET_TENANT),
                    vec![STAGED_DOC.to_string()]
                );
            }

            /// An activation that fails while holding the external target fence must hand
            /// the target name back without creating a visible placeholder.
            ///
            /// Leaving `staging` unpopulated fails the digest step, which runs after the
            /// target fence is held — the narrowest public-API way to reach that window.
            #[tokio::test]
            async fn create_only_activation_failure_after_fencing_releases_the_target_name() {
                let temp = TempDir::new().unwrap();
                let base = temp.path();
                let target = PublicationTarget::new(TARGET_TENANT).unwrap();

                let unstaged = PreStagedPublication::prepare(base, target.clone()).unwrap();
                let unstaged_paths = unstaged.paths().clone();
                let failure = unstaged.activate_create_only();

                assert!(
                    failure.is_err(),
                    "activation without a staging tree must fail: {failure:?}"
                );
                assert!(
                    !unstaged_paths.target.exists(),
                    "a failed create-only activation must not expose the target name"
                );

                // The released name is reusable by a later create-only activation.
                let retry = PreStagedPublication::prepare(base, target).unwrap();
                stage_document(&retry.paths().staging, STAGED_DOC);
                retry.activate_create_only().unwrap();

                let manager = IndexManager::new(base);
                assert_eq!(
                    searchable_ids(&manager, TARGET_TENANT),
                    vec![STAGED_DOC.to_string()]
                );
            }

            /// Startup repair must discard a pre-promotion create-only transaction and must
            /// not promote the uncommitted staging tree that crash left behind.
            #[tokio::test]
            async fn startup_repair_discards_an_orphaned_create_only_transaction() {
                let temp = TempDir::new().unwrap();
                let base = temp.path();
                let orphan = orphaned_create_only_transaction(base);

                let manager = IndexManager::new(base);
                let report = manager.repair_publication_target(TARGET_TENANT).unwrap();

                assert_eq!(report.status, PublicationRepairStatus::Repaired);
                assert_eq!(
                    report.action,
                    PublicationScanAction::Repaired(RepairDecision::Rollback)
                );
                assert_eq!(
                    report.disposition,
                    PublicationTargetDisposition::Unavailable
                );
                assert!(
                    !orphan.target.exists(),
                    "repair must not materialize a destination for the orphaned transaction"
                );
                assert!(
                    !orphan.staging.exists(),
                    "repair must not keep an uncommitted staging tree"
                );
                assert!(
                    !searchable_ids(&manager, TARGET_TENANT).contains(&STAGED_DOC.to_string()),
                    "uncommitted staging must never become the live target"
                );
            }

            /// Build the exact on-disk state a crash between the prepare journal and the
            /// promote rename leaves behind for a create-only activation: no visible target,
            /// the staged tree uncommitted, and a prepared journal recording no prior digest.
            fn orphaned_create_only_transaction(base: &Path) -> PublicationPaths {
                let target = PublicationTarget::new(TARGET_TENANT).unwrap();
                let transaction = PublicationTransactionId::new("snapshot_orphan").unwrap();
                let generation = PublicationGenerationEvidence::new("snapshot_orphan").unwrap();
                let paths = PublicationPaths::new(base, &target, &transaction);

                fs::create_dir_all(paths.staging.parent().unwrap()).unwrap();
                stage_document(&paths.staging, STAGED_DOC);

                let inventory = TantivyManagedInventory::from_existing_trees([
                    paths.target.as_path(),
                    paths.staging.as_path(),
                    paths.backup.as_path(),
                ])
                .unwrap();
                let digest = canonical_tenant_tree_digest(&paths.staging, &inventory).unwrap();
                let journal = PublicationJournal::prepare(
                    transaction,
                    target,
                    generation,
                    digest,
                    paths.clone(),
                );
                assert_eq!(journal.prior_digest, None);
                fs::write(
                    &paths.journal,
                    serde_json::to_vec_pretty(&journal.to_json_value()).unwrap(),
                )
                .unwrap();
                paths
            }

            fn stage_document(staging_path: &Path, object_id: &str) {
                let staged_index = Index::create_in_dir(staging_path).unwrap();
                staged_index
                    .add_documents_simple(&[json!({
                        "objectID": object_id,
                        "title": "staged replacement document"
                    })])
                    .unwrap();
            }

            /// The `FlapjackError` an activation failed with, if it carries one.
            fn activation_cause(error: &PreStagedActivationError) -> Option<&FlapjackError> {
                error
                    .source()
                    .and_then(|source| source.downcast_ref::<FlapjackError>())
            }

            fn directory_entry_count(path: &Path) -> usize {
                fs::read_dir(path).map(Iterator::count).unwrap_or_default()
            }

            /// Document IDs visible at `tenant`, or none when it cannot be searched.
            fn searchable_ids(manager: &IndexManager, tenant: &str) -> Vec<String> {
                manager
                    .search(tenant, "", None, None, 10)
                    .map(|result| {
                        result
                            .documents
                            .into_iter()
                            .map(|hit| hit.document.id)
                            .collect()
                    })
                    .unwrap_or_default()
            }
        }
    }
}

fn search_event(index_name: &str, query_id: &str) -> SearchEvent {
    SearchEvent {
        timestamp_ms: 1_700_000_000_000,
        query: "query".to_string(),
        query_id: Some(query_id.to_string()),
        index_name: index_name.to_string(),
        nb_hits: 1,
        processing_time_ms: 1,
        user_token: Some("user-1".to_string()),
        user_ip: None,
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: None,
        region: None,
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    }
}
