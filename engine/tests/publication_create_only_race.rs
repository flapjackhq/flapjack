use flapjack::index::manager::publication::{
    canonical_tenant_tree_digest, PreStagedActivationError, PreStagedPublication,
    PublicationGenerationEvidence, PublicationJournal, PublicationPaths, PublicationPhase,
    PublicationRepairStatus, PublicationScanAction, PublicationTarget,
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
            /// anything: an atomic reservation yields exactly one winner under *every*
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
            /// and leaves behind neither a backup tree nor its empty reservation.
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
                    "the reservation must be replaced by the staged tree, not left empty"
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

            /// An activation that fails after reserving must hand the target name back.
            ///
            /// Leaving `staging` unpopulated fails the digest step, which runs after the
            /// reservation is held — the narrowest public-API way to reach that window.
            #[tokio::test]
            async fn create_only_activation_failure_after_reservation_releases_the_target_name() {
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
                    "a failed create-only activation must release the reserved target name"
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

            /// Startup repair must release a reservation orphaned by a crash and must not
            /// promote the uncommitted staging tree that crash left behind.
            #[tokio::test]
            async fn startup_repair_releases_an_orphaned_create_only_reservation() {
                let temp = TempDir::new().unwrap();
                let base = temp.path();
                let orphan = orphaned_create_only_reservation(base);

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
                    "repair must release the orphaned reservation"
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
            /// promote rename leaves behind for a create-only activation: the reservation
            /// still held at the target, the staged tree uncommitted, and a prepared
            /// journal recording no prior digest because no prior target ever existed.
            fn orphaned_create_only_reservation(base: &Path) -> PublicationPaths {
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
                fs::create_dir(&paths.target).unwrap();
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
