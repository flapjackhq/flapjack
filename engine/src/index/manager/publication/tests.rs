// Stub summary for engine/src/index/manager/publication/tests.rs.
// Stub summary for engine/src/index/manager/publication/tests.rs.
// Stub summary for engine/src/index/manager/publication/tests.rs.
// Stub summary for engine/src/index/manager/publication/tests.rs.
// Stub summary for engine/src/index/manager/publication/tests.rs.
/// Stub summary for engine/src/index/manager/publication/tests.rs.
// Stub summary for engine/src/index/manager/publication/tests.rs.
// Stub summary for engine/src/index/manager/publication/tests.rs.
// Stub summary for engine/src/index/manager/publication/tests.rs.
use super::*;
use crate::analytics::AnalyticsConfig;
use crate::index::settings::IndexSettings;
use crate::{Document, IndexManager};
use crate::query_suggestions::QsConfigStore;
use crate::Index;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use tempfile::TempDir;

#[path = "tests/repair_cli_contract.rs"]
mod publication_repair_cli;
#[path = "tests/repair_cli_manifest.rs"]
mod repair_cli_manifest;

fn digest() -> ContentDigest {
    ContentDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap()
}

fn prepared_journal() -> PublicationJournal {
    let tmp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_001").unwrap();
    let paths = PublicationPaths::new(tmp.path(), &target, &transaction);
    PublicationJournal::prepare(
        transaction,
        target,
        PublicationGenerationEvidence::new("opaque_generation_7").unwrap(),
        digest(),
        paths,
    )
}

#[test]
fn reserved_namespace_classifier_recognizes_publication_evidence_paths() {
    for relative in [
        ".publication",
        ".publication/products/txn_001/staging",
        ".publication/products/txn_001/backup",
        ".publication/products/txn_001/journal.json",
        ".publication_quarantine",
        ".publication_quarantine/products/txn_001",
        ".publication_quarantine/products/txn_001/journal.json",
    ] {
        assert!(
            is_reserved_publication_namespace(Path::new(relative)),
            "{relative} must be reserved"
        );
    }
}

#[test]
fn reserved_namespace_classifier_rejects_unsafe_and_lookalike_paths() {
    for relative in [
        "",
        ".",
        "/tmp/.publication",
        "../.publication",
        ".publication/../products",
        ".publication_archive",
        ".publication_archive/products",
        "publication",
        "products/.publication",
        "products",
        "test.v2",
    ] {
        assert!(
            !is_reserved_publication_namespace(Path::new(relative)),
            "{relative} must not be reserved"
        );
    }
}

#[test]
fn transaction_namespace_paths_are_deterministic_and_isolated() {
    let tmp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_001").unwrap();
    let paths = PublicationPaths::new(tmp.path(), &target, &transaction);
    assert_eq!(paths.target, tmp.path().join("products"));
    assert_eq!(
        paths.staging,
        tmp.path().join(".publication/products/txn_001/staging")
    );
    assert_eq!(
        paths.backup,
        tmp.path().join(".publication/products/txn_001/backup")
    );
    assert_eq!(
        paths.journal,
        tmp.path().join(".publication/products/txn_001/journal.json")
    );
    assert_eq!(
        paths.quarantine,
        tmp.path().join(".publication_quarantine/products/txn_001")
    );
    assert_ne!(paths.staging, paths.target);
    assert_ne!(paths.backup, paths.target);
}

#[test]
fn target_and_transaction_reject_caller_chosen_path_components() {
    for invalid in [
        "",
        ".",
        "..",
        "../products",
        "products/blue",
        "products\\blue",
        "a\0b",
    ] {
        assert!(PublicationTarget::new(invalid).is_err(), "{invalid:?}");
    }
    for invalid in ["", ".", "..", "../txn", "txn/001", "txn\\001", "a\0b"] {
        assert!(PublicationTransactionId::new(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn public_surface_stays_node_local_and_rejects_cluster_atomicity() {
    for contract in public_surface_contracts() {
        assert!(contract.guarantee.contains("NODE-LOCAL"), "{}", contract.name);
        let lower = contract.guarantee.to_ascii_lowercase();
        assert!(!lower.contains("cluster-wide"));
        assert!(!lower.contains("replicated atomic"));
        assert!(!lower.contains("ha-atomic"));
    }
}

#[test]
fn journal_round_trip_preserves_exact_contract_values() {
    let journal = prepared_journal().apply(PublicationEvent::Commit).unwrap();
    let value = journal.to_json_value();
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["transaction_id"], "txn_001");
    assert_eq!(value["target"], "products");
    assert_eq!(value["generation"], "opaque_generation_7");
    assert_eq!(value["digest"], format!("sha256:{}", "a".repeat(64)));
    assert_eq!(value["transition_sequence"], 2);
    assert_eq!(value["phase"], "committed");
    assert_eq!(value["disposition"], "committed");
    // A non-fence activation must serialize the absence of fence evidence
    // explicitly as null, never as a placeholder zero watermark.
    assert!(value["fence_evidence"].is_null());

    let parsed = PublicationJournal::from_json(&value.to_string()).unwrap();
    assert_eq!(parsed.transaction_id.as_str(), "txn_001");
    assert_eq!(parsed.target.as_str(), "products");
    assert_eq!(parsed.phase, PublicationPhase::Committed);
    assert_eq!(parsed.disposition, Some(PublicationDisposition::Committed));
    assert_eq!(parsed.fence_evidence, None);
}

/// Round-trips exact `E_old`, `E_new`, staging-baseline, and `W` values through the
/// schema-version-2 journal for both serialization and parsing.
#[test]
fn journal_round_trip_preserves_exact_fence_evidence() {
    let fence = PublicationFenceEvidence::new(
        PublicationEpoch(6),
        PublicationEpoch(7),
        PublicationStagingBaseline(40),
        PublicationWatermark(42),
    )
    .unwrap();
    let mut journal = prepared_journal();
    journal.fence_evidence = Some(fence.clone());
    let journal = journal.apply(PublicationEvent::Commit).unwrap();

    let value = journal.to_json_value();
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["fence_evidence"]["epoch_old"], 6);
    assert_eq!(value["fence_evidence"]["epoch_new"], 7);
    assert_eq!(value["fence_evidence"]["staging_baseline"], 40);
    assert_eq!(value["fence_evidence"]["watermark"], 42);

    let parsed = PublicationJournal::from_json(&value.to_string()).unwrap();
    assert_eq!(parsed.fence_evidence, Some(fence));
    assert_eq!(parsed.phase, PublicationPhase::Committed);
}

/// A journal mutated to an unknown future `schema_version` must fail closed rather
/// than silently downgrade or read its future fence evidence.
#[test]
fn journal_parser_refuses_unknown_future_schema_version() {
    let mut value = prepared_journal().to_json_value();
    value["schema_version"] = serde_json::json!(3);
    value["fence_evidence"] = serde_json::json!({
        "epoch_old": 1,
        "epoch_new": 2,
        "staging_baseline": 0,
        "watermark": 5,
    });
    let error = PublicationJournal::from_json(&value.to_string())
        .expect_err("future schema versions must fail closed")
        .to_string();
    assert!(
        error.contains("unknown publication journal schema version"),
        "{error}"
    );
}

/// Compatibility policy: a legacy `schema_version: 1` journal predates fence
/// evidence and must be refused, never accepted as a fence-proven MIG-5 journal.
#[test]
fn journal_parser_refuses_legacy_v1_schema_without_fence_evidence() {
    let mut value = prepared_journal().to_json_value();
    value["schema_version"] = serde_json::json!(1);
    value["fence_evidence"] = serde_json::Value::Null;
    let error = PublicationJournal::from_json(&value.to_string())
        .expect_err("legacy v1 journals must not be read as fence-proven")
        .to_string();
    assert!(error.contains("predates fence evidence"), "{error}");
}

#[test]
fn journal_rejects_invalid_schema_and_evidence() {
    let mut value = prepared_journal().to_json_value();
    value["target"] = serde_json::json!(".");
    let error = PublicationJournal::from_json(&value.to_string())
        .expect_err("current-directory journal targets must be rejected")
        .to_string();
    assert!(error.contains("current-directory path component"), "{error}");

    let mut value = prepared_journal().to_json_value();
    value["schema_version"] = serde_json::json!(99);
    assert!(PublicationJournal::from_json(&value.to_string()).is_err());

    let mut value = prepared_journal().to_json_value();
    value["phase"] = serde_json::json!("replicated");
    assert!(PublicationJournal::from_json(&value.to_string()).is_err());

    let mut value = prepared_journal().to_json_value();
    value["digest"] = serde_json::json!("sha256:not_hex");
    assert!(PublicationJournal::from_json(&value.to_string()).is_err());

    let mut value = prepared_journal().to_json_value();
    value["generation"] = serde_json::json!("../writer");
    assert!(PublicationJournal::from_json(&value.to_string()).is_err());

    // A replacement epoch that is not exactly one past the old incarnation is
    // rejected on read, not silently accepted as fence-proven.
    let mut value = prepared_journal().to_json_value();
    value["fence_evidence"] = serde_json::json!({
        "epoch_old": 3,
        "epoch_new": 9,
        "staging_baseline": 0,
        "watermark": 5,
    });
    assert!(PublicationJournal::from_json(&value.to_string()).is_err());

    // A staging baseline past the drained watermark `W` is impossible evidence.
    let mut value = prepared_journal().to_json_value();
    value["fence_evidence"] = serde_json::json!({
        "epoch_old": 3,
        "epoch_new": 4,
        "staging_baseline": 9,
        "watermark": 5,
    });
    assert!(PublicationJournal::from_json(&value.to_string()).is_err());
}

#[test]
fn legal_transition_table_rejects_arbitrary_phase_changes() {
    let committed = prepared_journal().apply(PublicationEvent::Commit).unwrap();
    assert!(committed.apply(PublicationEvent::Rollback).is_err());
    let rolled_back = prepared_journal().apply(PublicationEvent::Rollback).unwrap();
    assert!(rolled_back.apply(PublicationEvent::Commit).is_err());
    let quarantined = prepared_journal().apply(PublicationEvent::Quarantine).unwrap();
    assert_eq!(quarantined.phase, PublicationPhase::Quarantined);
    assert_eq!(
        quarantined.disposition,
        Some(PublicationDisposition::Quarantined)
    );
}

#[test]
fn handoff_requires_publication_outcome_before_tombstone_retention() {
    let mut prepared = prepared_journal();
    let fence = PublicationFenceEvidence::new(
        PublicationEpoch(0),
        PublicationEpoch(1),
        PublicationStagingBaseline(3),
        PublicationWatermark(3),
    )
    .unwrap();
    prepared.fence_evidence = Some(fence.clone());
    let promoting = PublicationJobHandoff::promoting(prepared.transaction_id.clone());
    assert!(PublicationJobHandoff::adopt(&prepared).is_err());
    assert!(PublicationTombstone::from_adopted(&prepared, &promoting).is_err());

    let committed = prepared.apply(PublicationEvent::Commit).unwrap();
    let adopted = PublicationJobHandoff::adopt(&committed).unwrap();
    let tombstone = PublicationTombstone::from_adopted(&committed, &adopted).unwrap();
    assert!(tombstone.retention_eligible());
    assert_eq!(tombstone.outcome, PublicationDisposition::Committed);
    // Fence evidence must survive terminal compaction into the tombstone.
    assert_eq!(tombstone.fence_evidence, Some(fence));
}

#[test]
fn tenant_inventory_classifies_only_owner_known_artifacts() {
    let inventory = TantivyManagedInventory::new(vec![PathBuf::from("meta.json")]).unwrap();
    for relative in [
        "meta.json",
        "index_meta.json",
        "settings.json",
        "rules.json",
        "synonyms.json",
        "oplog/segment_0001.jsonl",
        "committed_seq",
        "vectors/id_map.json",
        ".dictionaries/stopwords.json",
        "recommend_rules/related-products/rules.json",
    ] {
        assert_eq!(
            classify_tenant_relative_path(Path::new(relative), &inventory).unwrap(),
            ArtifactDisposition::Preserve,
            "{relative}"
        );
    }
    let error = classify_tenant_relative_path(Path::new("segment_deadbeef.idx"), &inventory)
        .unwrap_err()
        .to_string();
    assert!(error.contains("segment_deadbeef.idx"));
}

#[test]
fn external_inventory_fails_closed_and_excludes_global_experiments() {
    let known_qs = vec![
        PathBuf::from("products.json"),
        PathBuf::from("products.status.json"),
        PathBuf::from("products.log.jsonl"),
    ];
    assert_eq!(
        classify_external_relative_path(
            ExternalArtifactRoot::QuerySuggestions,
            Path::new("products.json"),
            &known_qs
        )
        .unwrap(),
        Some(ArtifactDisposition::Journal)
    );
    let qs_error = classify_external_relative_path(
        ExternalArtifactRoot::QuerySuggestions,
        Path::new("products.extra"),
        &known_qs,
    )
    .unwrap_err()
    .to_string();
    assert!(qs_error.contains("products.extra"));

    let known_analytics = vec![
        PathBuf::from("products/searches"),
        PathBuf::from("products/events"),
        PathBuf::from("products/rollups"),
    ];
    assert_eq!(
        classify_external_relative_path(
            ExternalArtifactRoot::Analytics,
            Path::new("products/rollups/manifest.json"),
            &known_analytics
        )
        .unwrap(),
        Some(ArtifactDisposition::Journal)
    );
    let analytics_error = classify_external_relative_path(
        ExternalArtifactRoot::Analytics,
        Path::new("products/mystery.json"),
        &known_analytics,
    )
    .unwrap_err()
    .to_string();
    assert!(analytics_error.contains("products/mystery.json"));

    assert_eq!(
        classify_external_relative_path(
            ExternalArtifactRoot::Experiments,
            Path::new("experiment-1.json"),
            &[]
        )
        .unwrap(),
        None
    );
}

#[test]
fn policy_table_covers_required_dispositions() {
    let policies = artifact_policy_table();
    assert!(policies.iter().any(|p| p.key == "tenant_tantivy"));
    assert!(policies.iter().any(|p| p.key == "query_suggestions"));
    assert!(policies.iter().any(|p| p.key == "analytics"));
    assert!(policies
        .iter()
        .any(|p| p.disposition == ArtifactDisposition::PostcommitRebuild));
    assert!(policies.iter().all(|p| !p.owner.is_empty()));
    assert!(policies.iter().all(|p| !p.rollback.is_empty()));
    assert!(policies.iter().all(|p| !p.repair.is_empty()));
}

#[test]
fn canonical_tree_digest_is_stable_and_rejects_unknown_artifacts() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("tree");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::write(root.join("a.txt"), b"hi").unwrap();
    std::fs::write(root.join("nested").join("b.txt"), b"bye").unwrap();
    let inventory = TantivyManagedInventory::new([
        PathBuf::from("a.txt"),
        PathBuf::from("nested/b.txt"),
    ])
    .unwrap();

    let digest = canonical_tenant_tree_digest(&root, &inventory).unwrap();

    assert_eq!(
        digest.as_str(),
        "sha256:8085c38cfb011ab93e7485a17588e5d3a5daccf5f4bb3d72c2d7e5fd387cf1ab"
    );

    std::fs::remove_file(root.join("a.txt")).unwrap();
    std::fs::write(root.join("z.txt"), b"hi").unwrap();
    let reordered = canonical_tenant_tree_digest(&root, &inventory).unwrap_err();
    assert!(reordered.to_string().contains("z.txt"));
}

#[test]
fn canonical_tree_digest_changes_for_path_type_and_content() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("meta.json"), b"one").unwrap();
    let inventory = TantivyManagedInventory::new([PathBuf::from("meta.json")]).unwrap();
    let original = canonical_tenant_tree_digest(&root, &inventory).unwrap();

    std::fs::write(root.join("meta.json"), b"two").unwrap();
    let content_changed = canonical_tenant_tree_digest(&root, &inventory).unwrap();
    assert_ne!(original, content_changed);

    std::fs::remove_file(root.join("meta.json")).unwrap();
    std::fs::create_dir(root.join("meta.json")).unwrap();
    let type_changed = canonical_tenant_tree_digest(&root, &inventory).unwrap();
    assert_ne!(content_changed, type_changed);
}

#[cfg(unix)]
#[test]
fn canonical_tree_digest_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("meta.json"), b"one").unwrap();
    symlink(root.join("meta.json"), root.join("index_meta.json")).unwrap();
    let inventory = TantivyManagedInventory::new([PathBuf::from("meta.json")]).unwrap();

    let error = canonical_tenant_tree_digest(&root, &inventory).unwrap_err();

    assert!(error.to_string().contains("symlink"));
}

#[test]
fn activation_rejects_unknown_artifacts_before_mutation() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    std::fs::create_dir_all(&fixture.paths.staging).unwrap();
    std::fs::write(fixture.paths.staging.join("mystery.bin"), b"new").unwrap();

    let error = activate_publication_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: PublicationArtifactManifest::default(),
            inventory: &fixture.inventory,
        },
        PublicationFaultPoint::NoFault,
    )
    .unwrap_err();

    assert!(error.to_string().contains("mystery.bin"));
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
    assert_eq!(fixture.read_target_file("settings.json"), b"old-settings");
    assert!(!fixture.paths.backup.exists());
}

#[test]
fn activation_rejects_invalid_manifest_without_deleting_outside_artifact_root() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();

    let sidecar_root = fixture.base().join("artifact_root");
    std::fs::create_dir_all(&sidecar_root).unwrap();
    let victim = fixture.base().join("victim_sidecar.txt");
    std::fs::write(&victim, b"keep").unwrap();
    let manifest = PublicationArtifactManifest {
        entries: vec![PublicationArtifactManifestEntry::journaled(
            "query_suggestions_primary",
            PublicationArtifactRoot::QuerySuggestions,
            PathBuf::from("products.json"),
            victim.clone(),
            sidecar_root,
        )],
    };

    let error = activate_publication(PublicationActivationInputs {
        paths: &fixture.paths,
        target: fixture.target.clone(),
        transaction_id: fixture.transaction.clone(),
        generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
        manifest,
        inventory: &fixture.inventory,
    })
    .unwrap_err();

    assert!(error.to_string().contains("must be relative"));
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
    assert!(!fixture.paths.journal.exists());
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
}

#[test]
fn replacement_activation_rolls_back_losslessly_after_promote_failure() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");

    let result = activate_publication_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: fixture.external_manifest(),
            inventory: &fixture.inventory,
        },
        PublicationFaultPoint::AfterStagingPromote,
    );

    assert!(result.is_err());
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
    assert_eq!(fixture.read_target_file("settings.json"), b"old-settings");
    assert_eq!(
        fixture.read_target_file("oplog/segment_0001.jsonl"),
        b"old-oplog"
    );
    assert_eq!(fixture.read_target_file("committed_seq"), b"7");
    assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"old-sidecar");

    let journal = fixture.read_journal();
    assert_eq!(journal.phase, PublicationPhase::RolledBack);
}

#[test]
fn failed_create_removes_target_staging_backup_and_records_rollback() {
    let fixture = ActivationFixture::new();
    fixture.write_new_staging();

    let result = activate_publication_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: PublicationArtifactManifest::default(),
            inventory: &fixture.inventory,
        },
        PublicationFaultPoint::AfterStagingPromote,
    );

    assert!(result.is_err());
    assert!(!fixture.paths.target.exists());
    assert!(!fixture.paths.staging.exists());
    assert!(!fixture.paths.backup.exists());
    assert_eq!(fixture.read_journal().phase, PublicationPhase::RolledBack);
}

#[test]
fn pre_staged_activation_reports_the_failed_filesystem_phase() {
    let temp = tempfile::TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    std::fs::create_dir_all(temp.path().join("products")).unwrap();
    std::fs::write(temp.path().join("products/old.txt"), "old").unwrap();

    let publication = PreStagedPublication::prepare(temp.path(), target.clone()).unwrap();
    std::fs::create_dir_all(&publication.paths().staging).unwrap();
    std::fs::write(publication.paths().staging.join("new.txt"), "new").unwrap();
    let error = publication
        .activate_with_fault_for_test(PublicationFaultPoint::AfterTargetBackup)
        .unwrap_err();
    assert_eq!(error.stage(), PreStagedActivationStage::BackupTarget);

    let publication = PreStagedPublication::prepare(temp.path(), target).unwrap();
    std::fs::create_dir_all(&publication.paths().staging).unwrap();
    std::fs::write(publication.paths().staging.join("new.txt"), "new").unwrap();
    let error = publication
        .activate_with_fault_for_test(PublicationFaultPoint::AfterStagingPromote)
        .unwrap_err();
    assert_eq!(error.stage(), PreStagedActivationStage::PromoteStaging);
}

#[test]
fn ambiguous_failed_create_quarantines_digest_bearing_journal() {
    let fixture = ActivationFixture::new();
    fixture.write_new_staging();
    let expected_digest = fixture.new_digest();

    let result = activate_publication_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: PublicationArtifactManifest::default(),
            inventory: &fixture.inventory,
        },
        PublicationFaultPoint::AfterRollbackJournal,
    );

    assert!(result.is_err());
    assert!(!fixture.paths.target.exists());
    assert!(fixture.paths.quarantine.join("journal.json").exists());
    let quarantined =
        PublicationJournal::from_json(&std::fs::read_to_string(fixture.paths.quarantine.join("journal.json")).unwrap())
            .unwrap();
    assert_eq!(quarantined.phase, PublicationPhase::Quarantined);
    assert_eq!(quarantined.digest.as_str(), expected_digest.as_str());
}

#[test]
fn successful_activation_promotes_journaled_sidecar_and_records_digests() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");

    let journal = activate_publication_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: fixture.promoting_external_manifest(),
            inventory: &fixture.inventory,
        },
        PublicationFaultPoint::NoFault,
    )
    .unwrap();

    assert_eq!(journal.phase, PublicationPhase::Committed);
    assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"new-sidecar");
    let entry = journal.artifact_manifest.entries.first().unwrap();
    assert_eq!(
        entry.original_digest.as_ref().unwrap().as_str(),
        "sha256:6ba99fa8de9635003f2460b593447339a725c78a96d02d17ff18fb27b7b76b1a"
    );
    assert_eq!(
        entry.promoted_digest.as_ref().unwrap().as_str(),
        "sha256:3c605939a467030daccf7d023930f7f984b0a525812df0ff9f407ebe9c5d2c09"
    );
    let persisted = fixture.read_journal();
    let persisted_entry = persisted.artifact_manifest.entries.first().unwrap();
    assert_eq!(persisted_entry.policy_key, entry.policy_key);
    assert_eq!(persisted_entry.root, entry.root);
    assert_eq!(
        persisted_entry.original_relative_path,
        entry.original_relative_path
    );
    assert_eq!(
        persisted_entry.promoted_relative_path,
        entry.promoted_relative_path
    );
    assert_eq!(persisted_entry.original_digest, entry.original_digest);
    assert_eq!(persisted_entry.promoted_digest, entry.promoted_digest);
}

#[cfg(unix)]
#[test]
fn activation_rejects_symlinked_manifest_components_before_mutation() {
    use std::os::unix::fs::symlink;

    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    std::fs::create_dir_all(&fixture.sidecar_root).unwrap();
    let outside = fixture.paths.target.parent().unwrap().join("outside_sidecars");
    std::fs::create_dir_all(&outside).unwrap();
    let manifest = PublicationArtifactManifest::new([PublicationArtifactManifestEntry::journaled(
        "query_suggestions",
        PublicationArtifactRoot::QuerySuggestions,
        PathBuf::from("escaped/products.json"),
        PathBuf::from("escaped/products.json"),
        fixture.sidecar_root.clone(),
    )])
    .unwrap();
    symlink(&outside, fixture.sidecar_root.join("escaped")).unwrap();
    std::fs::write(outside.join("products.json"), b"outside").unwrap();

    let error = activate_publication_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest,
            inventory: &fixture.inventory,
        },
        PublicationFaultPoint::NoFault,
    )
    .unwrap_err();

    assert!(error.to_string().contains("symlink"));
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
    assert!(!fixture.paths.backup.exists());
}

#[test]
fn manifest_builds_query_suggestions_and_analytics_entries_from_owner_resolvers() {
    let tmp = TempDir::new().unwrap();
    let qs_store = QsConfigStore::new(tmp.path());
    let qs_original = qs_store.target_artifact_paths("products").unwrap();
    let qs_promoted = qs_store.target_artifact_paths("products_tmp").unwrap();
    let analytics = AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 1,
        flush_size: 1,
        retention_days: 1,
    };
    let analytics_original = analytics.target_artifact_paths("products");
    let analytics_promoted = analytics.target_artifact_paths("products_tmp");

    let manifest = PublicationArtifactManifest::from_resolved_artifacts(
        Some((qs_original, qs_promoted)),
        Some((analytics_original, analytics_promoted)),
    )
    .unwrap();

    let paths: Vec<_> = manifest
        .entries
        .iter()
        .map(|entry| {
            (
                entry.policy_key.as_str(),
                entry.root,
                entry.original_relative_path.clone(),
                entry.promoted_relative_path.clone(),
            )
        })
        .collect();
    assert_eq!(
        paths,
        vec![
            (
                "query_suggestions",
                PublicationArtifactRoot::QuerySuggestions,
                PathBuf::from("products.json"),
                PathBuf::from("products_tmp.json"),
            ),
            (
                "query_suggestions",
                PublicationArtifactRoot::QuerySuggestions,
                PathBuf::from("products.log.jsonl"),
                PathBuf::from("products_tmp.log.jsonl"),
            ),
            (
                "query_suggestions",
                PublicationArtifactRoot::QuerySuggestions,
                PathBuf::from("products.status.json"),
                PathBuf::from("products_tmp.status.json"),
            ),
            (
                "analytics",
                PublicationArtifactRoot::Analytics,
                PathBuf::from("products"),
                PathBuf::from("products_tmp"),
            ),
        ]
    );
}

#[test]
fn manifest_rejects_overlapping_artifact_ownership() {
    let fixture = ActivationFixture::new();

    let error = PublicationArtifactManifest::new([
        PublicationArtifactManifestEntry::journaled(
            "analytics",
            PublicationArtifactRoot::Analytics,
            PathBuf::from("products"),
            PathBuf::from("products_tmp"),
            fixture.sidecar_root.clone(),
        ),
        PublicationArtifactManifestEntry::journaled(
            "analytics",
            PublicationArtifactRoot::Analytics,
            PathBuf::from("products/searches"),
            PathBuf::from("products_tmp/searches"),
            fixture.sidecar_root.clone(),
        ),
    ])
    .unwrap_err();

    assert!(error.to_string().contains("overlapping"));
}

#[test]
fn replacement_rollback_removes_sidecar_backup_residue() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");

    let result = activate_publication_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: fixture.promoting_external_manifest(),
            inventory: &fixture.inventory,
        },
        PublicationFaultPoint::AfterStagingPromote,
    );

    assert!(result.is_err());
    assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"old-sidecar");
    assert!(!fixture.sidecar_backup_dir().exists());
}

#[test]
fn activation_fault_hook_covers_durability_boundaries_without_success() {
    for fault in [
        PublicationFaultPoint::BeforeStagingDigest,
        PublicationFaultPoint::DuringStagingSync,
        PublicationFaultPoint::DuringPrepareJournalWrite,
        PublicationFaultPoint::AfterPrepareJournalRename,
        PublicationFaultPoint::AfterTargetBackup,
        PublicationFaultPoint::AfterStagingPromote,
        PublicationFaultPoint::BeforeCommitJournal,
        PublicationFaultPoint::DuringCommitJournalWrite,
        PublicationFaultPoint::AfterCommitJournalRename,
        PublicationFaultPoint::AfterRollbackJournal,
    ] {
        let fixture = ActivationFixture::new();
        fixture.write_old_target();
        fixture.write_new_staging();
        let result = activate_publication_for_test(
            PublicationActivationInputs {
                paths: &fixture.paths,
                target: fixture.target.clone(),
                transaction_id: fixture.transaction.clone(),
                generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
                manifest: PublicationArtifactManifest::default(),
                inventory: &fixture.inventory,
            },
            fault,
        );
        assert!(result.is_err(), "{fault:?} unexpectedly succeeded");
    }

    let fixture = ActivationFixture::new();
    fixture.write_new_staging();
    let result = activate_publication_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: PublicationArtifactManifest::default(),
            inventory: &fixture.inventory,
        },
        PublicationFaultPoint::DuringQuarantine,
    );
    assert!(result.is_err(), "DuringQuarantine unexpectedly succeeded");
}

#[test]
fn replacement_faults_after_promotion_restore_the_exact_old_publication() {
    for fault in [
        PublicationFaultPoint::BeforeCommitJournal,
        PublicationFaultPoint::DuringCommitJournalWrite,
        PublicationFaultPoint::AfterCommitJournalRename,
    ] {
        let fixture = ActivationFixture::new();
        fixture.write_old_target();
        fixture.write_new_staging();
        fixture.write_external_sidecar(b"old-sidecar");
        fixture.write_promoted_sidecar(b"new-sidecar");

        let result = activate_publication_for_test(
            PublicationActivationInputs {
                paths: &fixture.paths,
                target: fixture.target.clone(),
                transaction_id: fixture.transaction.clone(),
                generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
                manifest: fixture.promoting_external_manifest(),
                inventory: &fixture.inventory,
            },
            fault,
        );

        assert!(result.is_err(), "{fault:?} unexpectedly succeeded");
        assert_eq!(
            fixture.read_target_file("index_meta.json"),
            b"old-meta",
            "{fault:?} did not restore metadata"
        );
        assert_eq!(
            fixture.read_target_file("settings.json"),
            b"old-settings",
            "{fault:?} did not restore settings"
        );
        assert_eq!(
            fixture.read_target_file("oplog/segment_0001.jsonl"),
            b"old-oplog",
            "{fault:?} did not restore oplog"
        );
        assert_eq!(
            fixture.read_target_file("committed_seq"),
            b"7",
            "{fault:?} did not restore committed sequence"
        );
        assert_eq!(
            std::fs::read(fixture.sidecar_path()).unwrap(),
            b"old-sidecar",
            "{fault:?} did not restore the journaled sidecar"
        );
        assert_eq!(
            fixture.read_journal().phase,
            PublicationPhase::RolledBack,
            "{fault:?} did not durably record rollback"
        );
        assert!(!fixture.paths.backup.exists(), "{fault:?} left backup residue");
        assert!(!fixture.paths.staging.exists(), "{fault:?} left staging residue");
        assert!(
            !fixture.sidecar_backup_dir().exists(),
            "{fault:?} left sidecar backup residue"
        );
    }
}

#[test]
fn post_commit_faults_preserve_the_committed_publication() {
    for fault in [
        PublicationFaultPoint::AfterCommitJournal,
        PublicationFaultPoint::DuringCleanup,
    ] {
        let fixture = ActivationFixture::new();
        fixture.write_old_target();
        fixture.write_new_staging();
        fixture.write_external_sidecar(b"old-sidecar");
        fixture.write_promoted_sidecar(b"new-sidecar");

        let result = activate_publication_for_test(
            PublicationActivationInputs {
                paths: &fixture.paths,
                target: fixture.target.clone(),
                transaction_id: fixture.transaction.clone(),
                generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
                manifest: fixture.promoting_external_manifest(),
                inventory: &fixture.inventory,
            },
            fault,
        );

        assert!(result.is_ok(), "{fault:?} unexpectedly failed");
        assert_eq!(
            fixture.read_target_file("index_meta.json"),
            b"new-meta",
            "{fault:?} did not preserve committed metadata"
        );
        assert_eq!(
            std::fs::read(fixture.sidecar_path()).unwrap(),
            b"new-sidecar",
            "{fault:?} did not preserve committed sidecar"
        );
        assert_eq!(
            fixture.read_journal().phase,
            PublicationPhase::Committed,
            "{fault:?} did not preserve committed journal"
        );
    }
}

/// ADR 0008 fence evidence sized against the activation fixture: `E_new = E_old + 1`
/// and a staging baseline at or below the drained watermark `W` (the fixture's old
/// generation `committed_seq` is `7`).
fn fixture_fence_evidence() -> PublicationFenceEvidence {
    PublicationFenceEvidence::new(
        PublicationEpoch(2),
        PublicationEpoch(3),
        PublicationStagingBaseline(7),
        PublicationWatermark(7),
    )
    .unwrap()
}

#[test]
fn activation_rejects_invalid_caller_supplied_fence_before_persistence() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    let invalid_fence = PublicationFenceEvidence {
        epoch_old: PublicationEpoch(2),
        epoch_new: PublicationEpoch(9),
        staging_baseline: PublicationStagingBaseline(8),
        watermark: PublicationWatermark(7),
    };

    let error = activate_publication_with_fence_and_faults_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: PublicationArtifactManifest::default(),
            inventory: &fixture.inventory,
        },
        Some(invalid_fence),
        &PublicationFaultScript::recording(),
    )
    .expect_err("invalid caller-supplied fence evidence must not be persisted")
    .to_string();

    assert!(
        error.contains("publication fence replacement epoch must be exactly one past the old epoch"),
        "{error}"
    );
    assert!(!fixture.paths.journal.exists());
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
}

#[test]
fn activation_invalid_fence_cleanup_leaves_old_target_loadable() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");
    let invalid_fence = PublicationFenceEvidence {
        epoch_old: PublicationEpoch(2),
        epoch_new: PublicationEpoch(9),
        staging_baseline: PublicationStagingBaseline(8),
        watermark: PublicationWatermark(7),
    };

    let error = activate_publication_with_fence_and_faults_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: fixture.promoting_external_manifest(),
            inventory: &fixture.inventory,
        },
        Some(invalid_fence),
        &PublicationFaultScript::recording(),
    )
    .expect_err("invalid caller-supplied fence evidence must abort before journaling")
    .to_string();

    assert!(
        error.contains("publication fence replacement epoch must be exactly one past the old epoch"),
        "{error}"
    );
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
    assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"old-sidecar");
    assert!(!fixture.paths.journal.exists());
    assert!(!fixture.paths.staging.exists());
    assert!(!fixture.paths.backup.exists());
    assert!(!fixture.journal_temp_path().exists());
    assert!(!fixture.promoted_sidecar_path().exists());
    assert!(!executor::sidecar_residue_root(&fixture.paths).exists());
    assert!(!fixture.transaction_root().exists());

    let report = scan_and_repair_publication_target(
        fixture.base(),
        &AnalyticsConfig::for_data_dir(fixture.base()),
        fixture.target.clone(),
    )
    .unwrap();
    assert_eq!(report.status, PublicationRepairStatus::Clean);
    assert_eq!(report.action, PublicationScanAction::Clean);
    assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
}

#[test]
fn startup_repair_after_epoch_advance_before_journal_reopens_old_tree() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    let old_bytes = collect_fixture_tree_bytes(&fixture.paths.target);
    let old_digest = fixture.target_digest();
    let guard = compare_and_advance_publication_epoch(
        fixture.base(),
        &fixture.target,
        PublicationEpoch(0),
    )
    .unwrap();
    let fence = PublicationFenceEvidence::new(
        guard.previous(),
        guard.advanced(),
        PublicationStagingBaseline(7),
        PublicationWatermark(7),
    )
    .unwrap();

    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        activate_publication_with_fence_and_faults_for_test(
            PublicationActivationInputs {
                paths: &fixture.paths,
                target: fixture.target.clone(),
                transaction_id: fixture.transaction.clone(),
                generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
                manifest: PublicationArtifactManifest::default(),
                inventory: &fixture.inventory,
            },
            Some(fence),
            &PanicAtCheckpoint::new(PublicationFaultPoint::DuringPrepareJournalWrite),
        )
        .unwrap();
    }));
    assert!(crash.is_err(), "test crash hook must bypass activation rollback");
    drop(guard);
    assert_eq!(
        read_publication_epoch(fixture.base(), &fixture.target).unwrap(),
        PublicationEpoch(1)
    );
    assert!(!fixture.paths.journal.exists());
    assert!(fixture.journal_temp_path().exists());
    assert_eq!(collect_fixture_tree_bytes(&fixture.paths.target), old_bytes);
    assert_eq!(fixture.target_digest(), old_digest);

    let report = scan_and_repair_publication_target(
        fixture.base(),
        &AnalyticsConfig::for_data_dir(fixture.base()),
        fixture.target.clone(),
    )
    .unwrap();

    assert_eq!(report.status, PublicationRepairStatus::Repaired);
    assert_eq!(
        report.action,
        PublicationScanAction::Repaired(RepairDecision::Cleanup)
    );
    assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
    assert!(!report.live_target_mutated);
    assert_eq!(read_publication_epoch(fixture.base(), &fixture.target).unwrap(), PublicationEpoch(1));
    assert!(!fixture.paths.journal.exists());
    assert!(!fixture.paths.staging.exists());
    assert!(!fixture.paths.backup.exists());
    assert!(!fixture.journal_temp_path().exists());
    assert_eq!(collect_fixture_tree_bytes(&fixture.paths.target), old_bytes);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
}

#[test]
fn startup_repair_after_unfenced_prepare_crash_reopens_old_tree() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    let old_bytes = collect_fixture_tree_bytes(&fixture.paths.target);
    let old_digest = fixture.target_digest();

    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        activate_publication_with_faults_for_test(
            fixture.activation_inputs(PublicationArtifactManifest::default()),
            &PanicAtCheckpoint::new(PublicationFaultPoint::DuringPrepareJournalWrite),
        )
        .unwrap();
    }));
    assert!(crash.is_err(), "test crash hook must bypass activation rollback");
    assert!(!fixture.paths.journal.exists());
    assert!(fixture.journal_temp_path().exists());
    assert_eq!(collect_fixture_tree_bytes(&fixture.paths.target), old_bytes);
    assert_eq!(fixture.target_digest(), old_digest);

    let report = scan_and_repair_publication_target(
        fixture.base(),
        &AnalyticsConfig::for_data_dir(fixture.base()),
        fixture.target.clone(),
    )
    .unwrap();

    assert_eq!(report.status, PublicationRepairStatus::Repaired);
    assert_eq!(
        report.action,
        PublicationScanAction::Repaired(RepairDecision::Cleanup)
    );
    assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
    assert!(!report.live_target_mutated);
    assert!(!fixture.paths.journal.exists());
    assert!(!fixture.paths.staging.exists());
    assert!(!fixture.paths.backup.exists());
    assert!(!fixture.journal_temp_path().exists());
    assert_eq!(collect_fixture_tree_bytes(&fixture.paths.target), old_bytes);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
}

#[test]
fn startup_repair_epoch_only_window_negative_cases_fail_closed() {
    #[derive(Clone, Copy)]
    enum PreJournalNegativeCase {
        AbsentEpoch,
        CorruptEpoch,
        LiveTargetSymlink,
        MissingLiveTarget,
        BackupResidue,
    }

    let cases = [
        PreJournalNegativeCase::AbsentEpoch,
        PreJournalNegativeCase::CorruptEpoch,
        PreJournalNegativeCase::LiveTargetSymlink,
        PreJournalNegativeCase::MissingLiveTarget,
        PreJournalNegativeCase::BackupResidue,
    ];

    for case in cases {
        let fixture = ActivationFixture::new();
        fixture.write_old_target();
        fixture.write_new_staging();
        let guard = compare_and_advance_publication_epoch(
            fixture.base(),
            &fixture.target,
            PublicationEpoch(0),
        )
        .unwrap();
        let fence = PublicationFenceEvidence::new(
            guard.previous(),
            guard.advanced(),
            PublicationStagingBaseline(7),
            PublicationWatermark(7),
        )
        .unwrap();
        let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            activate_publication_with_fence_and_faults_for_test(
                PublicationActivationInputs {
                    paths: &fixture.paths,
                    target: fixture.target.clone(),
                    transaction_id: fixture.transaction.clone(),
                    generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
                    manifest: PublicationArtifactManifest::default(),
                    inventory: &fixture.inventory,
                },
                Some(fence),
                &PanicAtCheckpoint::new(PublicationFaultPoint::DuringPrepareJournalWrite),
            )
            .unwrap();
        }));
        assert!(crash.is_err());
        drop(guard);

        match case {
            PreJournalNegativeCase::AbsentEpoch => {
                std::fs::remove_file(fixture.paths.epoch_path()).unwrap();
            }
            PreJournalNegativeCase::CorruptEpoch => {
                std::fs::write(fixture.paths.epoch_path(), b"1\n").unwrap();
            }
            PreJournalNegativeCase::LiveTargetSymlink => {
                replace_path_with_symlink(&fixture.paths.target, &fixture.paths.staging);
            }
            PreJournalNegativeCase::MissingLiveTarget => {
                std::fs::remove_dir_all(&fixture.paths.target).unwrap();
            }
            PreJournalNegativeCase::BackupResidue => {
                fixture.write_old_backup();
            }
        }

        let target_before = fixture
            .paths
            .target
            .is_dir()
            .then(|| collect_fixture_tree_bytes(&fixture.paths.target));
        let result = scan_and_repair_publication_target(
            fixture.base(),
            &AnalyticsConfig::for_data_dir(fixture.base()),
            fixture.target.clone(),
        );

        if matches!(
            case,
            PreJournalNegativeCase::CorruptEpoch | PreJournalNegativeCase::LiveTargetSymlink
        ) {
            let error = result.expect_err("invalid evidence must fail closed before mutation");
            let message = error.to_string();
            let expected = match case {
                PreJournalNegativeCase::CorruptEpoch => "corrupt publication epoch state",
                PreJournalNegativeCase::LiveTargetSymlink => "publication repair managed",
                _ => unreachable!(),
            };
            assert!(message.contains(expected), "{message}");
            assert!(fixture.paths.staging.exists());
            assert!(fixture.journal_temp_path().exists());
            continue;
        }

        let report = result.unwrap();
        assert_eq!(report.status, PublicationRepairStatus::Quarantined);
        assert_eq!(report.action, PublicationScanAction::Quarantined);
        assert_eq!(report.disposition, PublicationTargetDisposition::Unavailable);
        assert!(!report.live_target_mutated);
        assert!(!fixture.paths.journal.exists());
        assert!(
            fixture.paths.quarantine.exists(),
            "quarantine evidence must remain for diagnosis"
        );
        if let Some(bytes) = target_before {
            assert_eq!(collect_fixture_tree_bytes(&fixture.paths.target), bytes);
        }
    }
}

#[test]
fn startup_repair_converges_across_fenced_activation_crash_boundaries() {
    let cases = [
        PublicationFaultPoint::AfterPrepareJournal,
        PublicationFaultPoint::AfterTargetBackup,
        PublicationFaultPoint::AfterStagingPromote,
    ];

    for fault in cases {
        let fixture = ActivationFixture::new();
        fixture.write_old_target();
        fixture.write_new_staging();
        let old_bytes = collect_fixture_tree_bytes(&fixture.paths.target);
        let new_bytes = collect_fixture_tree_bytes(&fixture.paths.staging);
        let guard = compare_and_advance_publication_epoch(
            fixture.base(),
            &fixture.target,
            PublicationEpoch(0),
        )
        .unwrap();
        let fence = PublicationFenceEvidence::new(
            guard.previous(),
            guard.advanced(),
            PublicationStagingBaseline(7),
            PublicationWatermark(7),
        )
        .unwrap();
        let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            activate_publication_with_fence_and_faults_for_test(
                fixture.activation_inputs(PublicationArtifactManifest::default()),
                Some(fence.clone()),
                &PanicAtCheckpoint::new(fault),
            )
            .unwrap();
        }));
        assert!(crash.is_err(), "{fault:?} must simulate a process crash");
        drop(guard);
        let prepared = fixture.read_journal();
        assert_eq!(prepared.phase, PublicationPhase::Prepared);
        assert_eq!(prepared.fence_evidence, Some(fence.clone()));
        assert_eq!(prepared.transition_sequence, 1);
        assert_eq!(read_publication_epoch(fixture.base(), &fixture.target).unwrap(), PublicationEpoch(1));

        let report = scan_and_repair_publication_target(
            fixture.base(),
            &AnalyticsConfig::for_data_dir(fixture.base()),
            fixture.target.clone(),
        )
        .unwrap();

        assert_eq!(report.status, PublicationRepairStatus::Repaired, "{fault:?}");
        assert_eq!(
            report.action,
            PublicationScanAction::Repaired(RepairDecision::Complete),
            "{fault:?}"
        );
        assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
        assert_eq!(collect_fixture_tree_bytes(&fixture.paths.target), new_bytes);
        assert_ne!(collect_fixture_tree_bytes(&fixture.paths.target), old_bytes);
        assert!(!fixture.paths.staging.exists());
        assert!(!fixture.paths.backup.exists());
        assert!(!fixture.journal_temp_path().exists());
        let committed = fixture.read_journal();
        assert_eq!(committed.phase, PublicationPhase::Committed);
        assert_eq!(committed.fence_evidence, Some(fence));
        assert_eq!(committed.transition_sequence, 2);

        let second = scan_and_repair_publication_target(
            fixture.base(),
            &AnalyticsConfig::for_data_dir(fixture.base()),
            fixture.target.clone(),
        )
        .unwrap();
        assert_eq!(second.status, PublicationRepairStatus::Clean);
        assert_eq!(second.action, PublicationScanAction::Clean);
        assert_eq!(second.disposition, PublicationTargetDisposition::Loadable);
        assert_eq!(collect_fixture_tree_bytes(&fixture.paths.target), new_bytes);
    }
}

#[test]
fn startup_repair_fenced_journals_without_matching_epoch_fail_closed() {
    let faults = [
        PublicationFaultPoint::AfterPrepareJournal,
        PublicationFaultPoint::AfterTargetBackup,
        PublicationFaultPoint::AfterStagingPromote,
    ];
    let mutations = [
        EpochMutation::Absent,
        EpochMutation::Corrupt,
        EpochMutation::Old,
        EpochMutation::Future,
    ];

    for fault in faults {
        for mutation in mutations {
            let fixture = ActivationFixture::new();
            fixture.write_old_target();
            fixture.write_new_staging();
            let guard = compare_and_advance_publication_epoch(
                fixture.base(),
                &fixture.target,
                PublicationEpoch(0),
            )
            .unwrap();
            let fence = PublicationFenceEvidence::new(
                guard.previous(),
                guard.advanced(),
                PublicationStagingBaseline(7),
                PublicationWatermark(7),
            )
            .unwrap();
            let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                activate_publication_with_fence_and_faults_for_test(
                    fixture.activation_inputs(PublicationArtifactManifest::default()),
                    Some(fence),
                    &PanicAtCheckpoint::new(fault),
                )
                .unwrap();
            }));
            assert!(crash.is_err());
            drop(guard);
            let before_scan = PublicationLayoutSnapshot::capture(&fixture);
            mutate_epoch_record(&fixture, mutation);

            let result = scan_and_repair_publication_target(
                fixture.base(),
                &AnalyticsConfig::for_data_dir(fixture.base()),
                fixture.target.clone(),
            );

            if matches!(mutation, EpochMutation::Corrupt) {
                let error = result.expect_err("corrupt epoch must surface as a typed scan error");
                assert!(
                    error
                        .to_string()
                        .contains("corrupt publication epoch state"),
                    "{error}"
                );
                assert_eq!(PublicationLayoutSnapshot::capture(&fixture), before_scan);
                continue;
            }

            let report = result.unwrap();
            assert_eq!(report.status, PublicationRepairStatus::Quarantined);
            assert_eq!(report.action, PublicationScanAction::Quarantined);
            assert_eq!(report.disposition, PublicationTargetDisposition::Unavailable);
            assert!(!report.live_target_mutated);
            assert_eq!(PublicationLayoutSnapshot::capture(&fixture), before_scan);
            assert!(fixture.paths.quarantine.join("journal.json").exists());
        }
    }
}

#[test]
fn activation_invalid_fence_cleanup_preserves_existing_durable_journal() {
    let fixture = ActivationFixture::new();
    fixture.write_old_backup();
    fixture.write_new_staging();
    std::fs::create_dir_all(fixture.sidecar_backup_dir()).unwrap();
    std::fs::write(
        fixture.sidecar_backup_dir().join("products.json"),
        b"old-sidecar",
    )
    .unwrap();
    let prepared = fixture.prepared_journal_for_staging();
    fixture.write_journal(prepared.clone());
    let invalid_fence = PublicationFenceEvidence {
        epoch_old: PublicationEpoch(2),
        epoch_new: PublicationEpoch(9),
        staging_baseline: PublicationStagingBaseline(8),
        watermark: PublicationWatermark(7),
    };

    let error = activate_publication_with_fence_and_faults_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: PublicationArtifactManifest::default(),
            inventory: &fixture.inventory,
        },
        Some(invalid_fence),
        &PublicationFaultScript::recording(),
    )
    .expect_err("invalid retry must not clean a journaled transaction namespace")
    .to_string();

    assert!(
        error.contains("cannot clean up a journaled publication transaction"),
        "{error}"
    );
    assert!(fixture.paths.journal.exists());
    assert!(fixture.paths.backup.exists());
    assert!(fixture.paths.staging.exists());
    assert!(!fixture.journal_temp_path().exists());
    assert!(executor::sidecar_residue_root(&fixture.paths).exists());
    assert!(fixture.transaction_root().exists());
    let preserved = fixture.read_journal();
    assert_eq!(preserved.phase, PublicationPhase::Prepared);
    assert_eq!(preserved.transaction_id, prepared.transaction_id);
    assert_eq!(preserved.digest, prepared.digest);
    assert_eq!(preserved.prior_digest, prepared.prior_digest);
    assert!(preserved.artifact_manifest.entries.is_empty());
    assert_eq!(
        fixture.read_target_file_from_root(&fixture.paths.backup, "index_meta.json"),
        b"old-meta"
    );

    let report = scan_and_repair_publication_target(
        fixture.base(),
        &AnalyticsConfig::for_data_dir(fixture.base()),
        fixture.target.clone(),
    )
    .unwrap();
    assert_eq!(report.status, PublicationRepairStatus::Repaired);
    assert_eq!(
        report.action,
        PublicationScanAction::Repaired(RepairDecision::Complete)
    );
    assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
}

#[test]
fn activation_invalid_fence_cleanup_removes_retry_staging_drift() {
    let fixture = ActivationFixture::new();
    fixture.write_old_backup();
    fixture.write_new_staging();
    let prepared = fixture.prepared_journal_for_staging();
    fixture.write_journal(prepared.clone());
    std::fs::write(
        fixture.paths.staging.join("index_meta.json"),
        b"retry-drifted-meta",
    )
    .unwrap();
    let invalid_fence = PublicationFenceEvidence {
        epoch_old: PublicationEpoch(2),
        epoch_new: PublicationEpoch(9),
        staging_baseline: PublicationStagingBaseline(8),
        watermark: PublicationWatermark(7),
    };

    let error = activate_publication_with_fence_and_faults_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: PublicationArtifactManifest::default(),
            inventory: &fixture.inventory,
        },
        Some(invalid_fence),
        &PublicationFaultScript::recording(),
    )
    .expect_err("invalid retry must not preserve drifted staging as recovery evidence")
    .to_string();

    assert!(
        error.contains("cannot clean up a journaled publication transaction"),
        "{error}"
    );
    assert!(fixture.paths.journal.exists());
    assert!(fixture.paths.backup.exists());
    assert!(!fixture.paths.staging.exists());
    let preserved = fixture.read_journal();
    assert_eq!(preserved.phase, PublicationPhase::Prepared);
    assert_eq!(preserved.digest, prepared.digest);
    assert_eq!(preserved.prior_digest, prepared.prior_digest);

    let report = scan_and_repair_publication_target(
        fixture.base(),
        &AnalyticsConfig::for_data_dir(fixture.base()),
        fixture.target.clone(),
    )
    .unwrap();
    assert_eq!(report.status, PublicationRepairStatus::Repaired);
    assert_eq!(
        report.action,
        PublicationScanAction::Repaired(RepairDecision::Rollback)
    );
    assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
}

#[test]
fn activation_invalid_fence_cleanup_removes_retry_backup_drift() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    let prepared = fixture.prepared_journal_for_staging();
    fixture.write_journal(prepared.clone());
    fixture.write_new_tree(&fixture.paths.backup);
    let invalid_fence = PublicationFenceEvidence {
        epoch_old: PublicationEpoch(2),
        epoch_new: PublicationEpoch(9),
        staging_baseline: PublicationStagingBaseline(8),
        watermark: PublicationWatermark(7),
    };

    let error = activate_publication_with_fence_and_faults_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: PublicationArtifactManifest::default(),
            inventory: &fixture.inventory,
        },
        Some(invalid_fence),
        &PublicationFaultScript::recording(),
    )
    .expect_err("invalid retry must not preserve drifted backup as recovery evidence")
    .to_string();

    assert!(
        error.contains("cannot clean up a journaled publication transaction"),
        "{error}"
    );
    assert!(fixture.paths.journal.exists());
    assert!(fixture.paths.staging.exists());
    assert!(!fixture.paths.backup.exists());
    let preserved = fixture.read_journal();
    assert_eq!(preserved.phase, PublicationPhase::Prepared);
    assert_eq!(preserved.digest, prepared.digest);
    assert_eq!(preserved.prior_digest, prepared.prior_digest);

    let report = scan_and_repair_publication_target(
        fixture.base(),
        &AnalyticsConfig::for_data_dir(fixture.base()),
        fixture.target.clone(),
    )
    .unwrap();
    assert_eq!(report.status, PublicationRepairStatus::Repaired);
    assert_eq!(
        report.action,
        PublicationScanAction::Repaired(RepairDecision::Complete)
    );
    assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"new-meta");
}

#[test]
fn activation_invalid_fence_cleanup_removes_retry_sidecar_drift() {
    let fixture = ActivationFixture::new();
    fixture.write_old_backup();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");
    let manifest = fixture.promoting_external_manifest();
    let mut prepared = fixture.prepared_journal_for_staging();
    prepared.artifact_manifest = fixture.with_current_sidecar_digests(manifest.clone());
    fixture.write_journal(prepared.clone());
    fixture.write_promoted_sidecar(b"retry-drifted-sidecar");
    let invalid_fence = PublicationFenceEvidence {
        epoch_old: PublicationEpoch(2),
        epoch_new: PublicationEpoch(9),
        staging_baseline: PublicationStagingBaseline(8),
        watermark: PublicationWatermark(7),
    };

    let error = activate_publication_with_fence_and_faults_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest,
            inventory: &fixture.inventory,
        },
        Some(invalid_fence),
        &PublicationFaultScript::recording(),
    )
    .expect_err("invalid retry must not preserve drifted sidecar as recovery evidence")
    .to_string();

    assert!(
        error.contains("cannot clean up a journaled publication transaction"),
        "{error}"
    );
    assert!(fixture.paths.journal.exists());
    assert!(fixture.paths.backup.exists());
    assert!(!fixture.paths.staging.exists());
    assert!(!fixture.promoted_sidecar_path().exists());
    assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"old-sidecar");
    let preserved = fixture.read_journal();
    assert_eq!(preserved.phase, PublicationPhase::Prepared);
    assert_eq!(preserved.digest, prepared.digest);
    assert_eq!(preserved.prior_digest, prepared.prior_digest);
    assert!(
        preserved
            .artifact_manifest
            .same_layout_as(&prepared.artifact_manifest)
    );
    assert_eq!(
        preserved
            .artifact_manifest
            .entries
            .first()
            .unwrap()
            .original_digest,
        prepared
            .artifact_manifest
            .entries
            .first()
            .unwrap()
            .original_digest
    );
    assert_eq!(
        preserved
            .artifact_manifest
            .entries
            .first()
            .unwrap()
            .promoted_digest,
        prepared
            .artifact_manifest
            .entries
            .first()
            .unwrap()
            .promoted_digest
    );

    let decision = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        fixture.promoting_external_manifest(),
        &fixture.inventory,
    )
    .unwrap();
    assert_eq!(decision, RepairDecision::Rollback);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
    assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"old-sidecar");
}

#[cfg(unix)]
#[test]
fn activation_invalid_fence_cleanup_removes_tampered_journaled_staging() {
    use std::os::unix::fs::symlink;

    let fixture = ActivationFixture::new();
    fixture.write_old_backup();
    fixture.write_new_staging();
    let prepared = fixture.prepared_journal_for_staging();
    fixture.write_journal(prepared.clone());
    symlink(
        fixture.paths.staging.join("index_meta.json"),
        fixture.paths.staging.join("retry_symlink"),
    )
    .unwrap();
    let invalid_fence = PublicationFenceEvidence {
        epoch_old: PublicationEpoch(2),
        epoch_new: PublicationEpoch(9),
        staging_baseline: PublicationStagingBaseline(8),
        watermark: PublicationWatermark(7),
    };

    let error = activate_publication_with_fence_and_faults_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: PublicationArtifactManifest::default(),
            inventory: &fixture.inventory,
        },
        Some(invalid_fence),
        &PublicationFaultScript::recording(),
    )
    .expect_err("tampered retry staging must still return the bounded journaled refusal")
    .to_string();

    assert!(
        error.contains("cannot clean up a journaled publication transaction"),
        "{error}"
    );
    assert!(fixture.paths.journal.exists());
    assert!(fixture.paths.backup.exists());
    assert!(!fixture.paths.staging.exists());
    let preserved = fixture.read_journal();
    assert_eq!(preserved.phase, PublicationPhase::Prepared);
    assert_eq!(preserved.digest, prepared.digest);
    assert_eq!(preserved.prior_digest, prepared.prior_digest);

    let report = scan_and_repair_publication_target(
        fixture.base(),
        &AnalyticsConfig::for_data_dir(fixture.base()),
        fixture.target.clone(),
    )
    .unwrap();
    assert_eq!(report.status, PublicationRepairStatus::Repaired);
    assert_eq!(
        report.action,
        PublicationScanAction::Repaired(RepairDecision::Rollback)
    );
    assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
}

/// Fault hook that reads and parses the persisted journal at a chosen checkpoint,
/// letting a test prove exactly which evidence was durable at that boundary.
struct CaptureJournalAtCheckpoint {
    at: PublicationCheckpoint,
    journal_path: PathBuf,
    captured: std::cell::RefCell<Option<PublicationJournal>>,
}

impl CaptureJournalAtCheckpoint {
    fn new(at: PublicationCheckpoint, journal_path: PathBuf) -> Self {
        Self {
            at,
            journal_path,
            captured: std::cell::RefCell::new(None),
        }
    }

    fn captured(&self) -> Option<PublicationJournal> {
        self.captured.borrow().clone()
    }
}

impl PublicationFaultHook for CaptureJournalAtCheckpoint {
    fn before_operation(&self, operation: &PublicationOperation) -> Result<()> {
        if *operation == PublicationOperation::Checkpoint(self.at) {
            let journal =
                PublicationJournal::from_json(&std::fs::read_to_string(&self.journal_path).unwrap())
                    .unwrap();
            *self.captured.borrow_mut() = Some(journal);
        }
        Ok(())
    }
}

struct PanicAtCheckpoint {
    checkpoint: PublicationCheckpoint,
}

impl PanicAtCheckpoint {
    fn new(checkpoint: PublicationCheckpoint) -> Self {
        Self { checkpoint }
    }
}

impl PublicationFaultHook for PanicAtCheckpoint {
    fn before_operation(&self, operation: &PublicationOperation) -> Result<()> {
        if *operation == PublicationOperation::Checkpoint(self.checkpoint) {
            panic!("simulated crash before {operation:?}");
        }
        Ok(())
    }
}

#[test]
fn activation_fault_hook_observes_every_durable_filesystem_boundary() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");
    let fence = fixture_fence_evidence();
    let faults = PublicationFaultScript::recording();

    let committed = activate_publication_with_fence_and_faults_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: fixture.promoting_external_manifest(),
            inventory: &fixture.inventory,
        },
        Some(fence.clone()),
        &faults,
    )
    .unwrap();
    assert_eq!(committed.fence_evidence, Some(fence.clone()));

    let operations = faults.operations();
    let journal_temp = fixture.journal_temp_path();
    assert!(operations.contains(&PublicationOperation::Digest(
        fixture.paths.staging.clone()
    )));
    assert!(operations.contains(&PublicationOperation::SyncFile(
        fixture.paths.staging.join("index_meta.json")
    )));
    assert!(operations.contains(&PublicationOperation::SyncDirectory(
        fixture.paths.staging.join("oplog")
    )));
    assert!(operations.contains(&PublicationOperation::WriteFile(
        journal_temp.clone()
    )));
    assert!(operations.contains(&PublicationOperation::SyncFile(
        journal_temp.clone()
    )));
    assert!(operations.contains(&PublicationOperation::Rename {
        from: journal_temp.clone(),
        to: fixture.paths.journal.clone(),
    }));
    assert!(operations.contains(&PublicationOperation::SyncDirectory(
        fixture.paths.journal.parent().unwrap().to_path_buf()
    )));
    assert!(operations.contains(&PublicationOperation::Rename {
        from: fixture.paths.target.clone(),
        to: fixture.paths.backup.clone(),
    }));
    assert!(operations.contains(&PublicationOperation::Rename {
        from: fixture.paths.staging.clone(),
        to: fixture.paths.target.clone(),
    }));
    assert!(operations.contains(&PublicationOperation::Rename {
        from: fixture.promoted_sidecar_path(),
        to: fixture.sidecar_path(),
    }));
    assert!(operations.contains(&PublicationOperation::Remove(
        fixture.paths.backup.clone()
    )));

    // The prepared journal must be fully durable (write, sync, rename, dir sync)
    // before the target backup and staging promotion touch the live target.
    let position = |operation: &PublicationOperation| {
        operations
            .iter()
            .position(|recorded| recorded == operation)
            .unwrap_or_else(|| panic!("missing durable operation {operation:?}"))
    };
    let prepare_write = position(&PublicationOperation::WriteFile(journal_temp.clone()));
    let prepare_sync = position(&PublicationOperation::SyncFile(journal_temp.clone()));
    let prepare_rename = position(&PublicationOperation::Rename {
        from: journal_temp.clone(),
        to: fixture.paths.journal.clone(),
    });
    let prepare_dir_sync = position(&PublicationOperation::SyncDirectory(
        fixture.paths.journal.parent().unwrap().to_path_buf(),
    ));
    let target_backup = position(&PublicationOperation::Rename {
        from: fixture.paths.target.clone(),
        to: fixture.paths.backup.clone(),
    });
    let staging_promote = position(&PublicationOperation::Rename {
        from: fixture.paths.staging.clone(),
        to: fixture.paths.target.clone(),
    });
    assert!(prepare_write < prepare_sync, "prepared journal synced before write");
    assert!(prepare_sync < prepare_rename, "prepared journal renamed before sync");
    assert!(
        prepare_rename < prepare_dir_sync,
        "prepared journal directory synced before rename"
    );
    assert!(
        prepare_dir_sync < target_backup,
        "target backed up before the prepared journal was durable"
    );
    assert!(
        target_backup < staging_promote,
        "staging promoted before the target was backed up"
    );

    // The persisted prepared journal itself must already carry the fence evidence
    // at the AfterPrepareJournal boundary, before any later activation checkpoint.
    let replay = ActivationFixture::new();
    replay.write_old_target();
    replay.write_new_staging();
    replay.write_external_sidecar(b"old-sidecar");
    replay.write_promoted_sidecar(b"new-sidecar");
    let replay_capture = CaptureJournalAtCheckpoint::new(
        PublicationCheckpoint::AfterPrepareJournal,
        replay.paths.journal.clone(),
    );
    activate_publication_with_fence_and_faults_for_test(
        PublicationActivationInputs {
            paths: &replay.paths,
            target: replay.target.clone(),
            transaction_id: replay.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: replay.promoting_external_manifest(),
            inventory: &replay.inventory,
        },
        Some(fence.clone()),
        &replay_capture,
    )
    .unwrap();
    let prepared = replay_capture
        .captured()
        .expect("prepared journal must be persisted by the AfterPrepareJournal checkpoint");
    assert_eq!(prepared.phase, PublicationPhase::Prepared);
    assert_eq!(prepared.fence_evidence, Some(fence));
}

#[test]
fn every_activation_filesystem_fault_restores_the_old_publication() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");
    let recording = PublicationFaultScript::recording();
    activate_publication_with_faults_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: fixture.promoting_external_manifest(),
            inventory: &fixture.inventory,
        },
        &recording,
    )
    .unwrap();

    let operations = recording.operations();
    let commit_index = operations
        .iter()
        .position(|op| {
            *op == PublicationOperation::Checkpoint(PublicationCheckpoint::CommitDurable)
        })
        .unwrap();

    for operation_index in 0..commit_index {
        let fixture = ActivationFixture::new();
        fixture.write_old_target();
        fixture.write_new_staging();
        fixture.write_external_sidecar(b"old-sidecar");
        fixture.write_promoted_sidecar(b"new-sidecar");
        let faults = PublicationFaultScript::failing_at(operation_index);

        let result = activate_publication_with_faults_for_test(
            PublicationActivationInputs {
                paths: &fixture.paths,
                target: fixture.target.clone(),
                transaction_id: fixture.transaction.clone(),
                generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
                manifest: fixture.promoting_external_manifest(),
                inventory: &fixture.inventory,
            },
            &faults,
        );

        assert!(
            result.is_err(),
            "operation {operation_index} unexpectedly succeeded: {:?}",
            faults.operations()
        );
        assert_eq!(
            fixture.read_target_file("index_meta.json"),
            b"old-meta",
            "operation {operation_index}: {:?}",
            faults.operations()
        );
        assert_eq!(
            std::fs::read(fixture.sidecar_path()).unwrap(),
            b"old-sidecar",
            "operation {operation_index}: {:?}",
            faults.operations()
        );
    }

    for operation_index in commit_index..operations.len() {
        let fixture = ActivationFixture::new();
        fixture.write_old_target();
        fixture.write_new_staging();
        fixture.write_external_sidecar(b"old-sidecar");
        fixture.write_promoted_sidecar(b"new-sidecar");
        let faults = PublicationFaultScript::failing_at(operation_index);

        let result = activate_publication_with_faults_for_test(
            PublicationActivationInputs {
                paths: &fixture.paths,
                target: fixture.target.clone(),
                transaction_id: fixture.transaction.clone(),
                generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
                manifest: fixture.promoting_external_manifest(),
                inventory: &fixture.inventory,
            },
            &faults,
        );

        assert!(
            result.is_ok(),
            "post-commit operation {operation_index} unexpectedly failed: {:?}",
            faults.operations()
        );
        assert_eq!(
            fixture.read_target_file("index_meta.json"),
            b"new-meta",
            "post-commit operation {operation_index}: {:?}",
            faults.operations()
        );
        assert_eq!(
            std::fs::read(fixture.sidecar_path()).unwrap(),
            b"new-sidecar",
            "post-commit operation {operation_index}: {:?}",
            faults.operations()
        );
    }
}

#[test]
fn every_precommit_create_fault_resolves_without_publication_residue() {
    let fixture = ActivationFixture::new();
    fixture.write_new_staging();
    let recording = PublicationFaultScript::recording();
    activate_publication_with_faults_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: PublicationArtifactManifest::default(),
            inventory: &fixture.inventory,
        },
        &recording,
    )
    .unwrap();
    let operations = recording.operations();
    let commit_index = operations
        .iter()
        .position(|operation| {
            *operation
                == PublicationOperation::Checkpoint(PublicationCheckpoint::CommitDurable)
        })
        .unwrap();

    for operation_index in 0..commit_index {
        let fixture = ActivationFixture::new();
        fixture.write_new_staging();
        let faults = PublicationFaultScript::failing_at(operation_index);
        let result = activate_publication_with_faults_for_test(
            PublicationActivationInputs {
                paths: &fixture.paths,
                target: fixture.target.clone(),
                transaction_id: fixture.transaction.clone(),
                generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
                manifest: PublicationArtifactManifest::default(),
                inventory: &fixture.inventory,
            },
            &faults,
        );

        assert!(result.is_err(), "operation {operation_index} unexpectedly succeeded");
        assert!(!fixture.paths.target.exists(), "operation {operation_index}");
        assert!(!fixture.paths.staging.exists(), "operation {operation_index}");
        assert!(!fixture.paths.backup.exists(), "operation {operation_index}");
        assert!(!fixture.journal_temp_path().exists(), "operation {operation_index}");
        assert!(!fixture.sidecar_backup_dir().exists(), "operation {operation_index}");
    }
}

#[tokio::test]
async fn activation_fixture_old_new_and_control_indexes_reopen_through_index_manager() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    write_fixture_control_index(fixture.base()).await;

    assert_reopenable_fixture_tree(
        fixture.base(),
        fixture.target.as_str(),
        "old-widget",
        serde_json::json!({
            "_id": "old-widget",
            "objectID": "old-widget",
            "title": "legacy waffle iron",
            "body": "old repair guide",
            "generation": "old"
        }),
        "legacy",
        &["old-widget"],
    );
    assert_reopenable_fixture_tree(
        fixture.paths.staging.parent().unwrap(),
        "staging",
        "new-widget",
        serde_json::json!({
            "_id": "new-widget",
            "objectID": "new-widget",
            "title": "modern waffle iron",
            "body": "new repair guide",
            "generation": "new"
        }),
        "modern",
        &["new-widget"],
    );
    assert_reopenable_fixture_tree(
        fixture.base(),
        "control_products",
        "control-widget",
        serde_json::json!({
            "_id": "control-widget",
            "objectID": "control-widget",
            "title": "control waffle iron",
            "body": "unchanged control guide",
            "generation": "control"
        }),
        "control",
        &["control-widget"],
    );
}

#[test]
fn activation_fixture_fresh_generations_have_identical_managed_inventories_and_digests() {
    let first = ActivationFixture::new();
    first.write_old_target();
    first.write_new_staging();
    let second = ActivationFixture::new();
    second.write_old_target();
    second.write_new_staging();

    assert_eq!(
        TantivyManagedInventory::from_existing_trees([first.paths.target.as_path()]).unwrap(),
        TantivyManagedInventory::from_existing_trees([second.paths.target.as_path()]).unwrap()
    );
    assert_eq!(
        TantivyManagedInventory::from_existing_trees([first.paths.staging.as_path()]).unwrap(),
        TantivyManagedInventory::from_existing_trees([second.paths.staging.as_path()]).unwrap()
    );
    assert_eq!(first.target_digest(), second.target_digest());
    assert_eq!(first.new_digest(), second.new_digest());
}

struct ActivationFixture {
    _tmp: Option<TempDir>,
    _source_tmp: TempDir,
    base: PathBuf,
    paths: PublicationPaths,
    target: PublicationTarget,
    transaction: PublicationTransactionId,
    inventory: TantivyManagedInventory,
    old_source: PathBuf,
    new_source: PathBuf,
    sidecar_root: PathBuf,
}

impl ActivationFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().to_path_buf();
        Self::from_base(base, Some(tmp))
    }

    fn new_at(base: PathBuf) -> Self {
        Self::from_base(base, None)
    }

    fn from_base(base: PathBuf, tmp: Option<TempDir>) -> Self {
        let target = PublicationTarget::new("products").unwrap();
        let transaction = PublicationTransactionId::new("txn_001").unwrap();
        let paths = PublicationPaths::new(&base, &target, &transaction);
        let source_tmp = TempDir::new().unwrap();
        let old_source = source_tmp.path().join("old");
        let new_source = source_tmp.path().join("new");
        let source_bytes = fixture_source_bytes();
        write_fixture_tree_bytes(&old_source, &source_bytes.old);
        write_fixture_tree_bytes(&new_source, &source_bytes.new);
        let inventory = TantivyManagedInventory::from_existing_trees([
            old_source.as_path(),
            new_source.as_path(),
        ])
        .unwrap();
        Self {
            sidecar_root: base.join(".query_suggestions"),
            base,
            _tmp: tmp,
            _source_tmp: source_tmp,
            paths,
            target,
            transaction,
            inventory,
            old_source,
            new_source,
        }
    }

    fn base(&self) -> &Path {
        &self.base
    }

    fn write_old_target(&self) {
        self.write_old_tree(&self.paths.target);
    }

    fn write_old_backup(&self) {
        self.write_old_tree(&self.paths.backup);
    }

    fn write_old_tree(&self, root: &Path) {
        copy_fixture_tree(&self.old_source, root);
    }

    fn write_new_staging(&self) {
        self.write_new_tree(&self.paths.staging);
    }

    fn write_new_target(&self) {
        self.write_new_tree(&self.paths.target);
    }

    fn write_new_tree(&self, root: &Path) {
        copy_fixture_tree(&self.new_source, root);
    }

    fn write_external_sidecar(&self, bytes: &[u8]) {
        std::fs::create_dir_all(&self.sidecar_root).unwrap();
        std::fs::write(self.sidecar_path(), bytes).unwrap();
    }

    fn write_promoted_sidecar(&self, bytes: &[u8]) {
        std::fs::create_dir_all(&self.sidecar_root).unwrap();
        std::fs::write(self.promoted_sidecar_path(), bytes).unwrap();
    }

    fn sidecar_path(&self) -> PathBuf {
        self.sidecar_root.join("products.json")
    }

    fn promoted_sidecar_path(&self) -> PathBuf {
        self.sidecar_root.join("products_next.json")
    }

    fn external_manifest(&self) -> PublicationArtifactManifest {
        PublicationArtifactManifest::new([PublicationArtifactManifestEntry::journaled(
            "query_suggestions",
            PublicationArtifactRoot::QuerySuggestions,
            PathBuf::from("products.json"),
            PathBuf::from("products.json"),
            self.sidecar_root.clone(),
        )])
        .unwrap()
    }

    fn promoting_external_manifest(&self) -> PublicationArtifactManifest {
        PublicationArtifactManifest::new([PublicationArtifactManifestEntry::journaled(
            "query_suggestions",
            PublicationArtifactRoot::QuerySuggestions,
            PathBuf::from("products.json"),
            PathBuf::from("products_next.json"),
            self.sidecar_root.clone(),
        )])
        .unwrap()
    }

    fn with_current_sidecar_digests(
        &self,
        mut manifest: PublicationArtifactManifest,
    ) -> PublicationArtifactManifest {
        let entry = manifest.entries.first_mut().unwrap();
        entry.original_digest = self
            .sidecar_path()
            .exists()
            .then(|| executor::artifact_digest(&self.sidecar_path()).unwrap());
        entry.promoted_digest = self
            .promoted_sidecar_path()
            .exists()
            .then(|| executor::artifact_digest(&self.promoted_sidecar_path()).unwrap());
        manifest
    }

    fn sidecar_backup_dir(&self) -> PathBuf {
        self.paths
            .journal
            .parent()
            .unwrap()
            .join("sidecars")
            .join("query_suggestions")
    }

    fn read_target_file(&self, relative: &str) -> Vec<u8> {
        self.read_target_file_from_root(&self.paths.target, relative)
    }

    fn read_target_file_from_root(&self, root: &Path, relative: &str) -> Vec<u8> {
        if relative == "settings.json" {
            return match self.read_target_generation_from_root(root).as_deref() {
                Some("old") => b"old-settings".to_vec(),
                Some("new") => b"new-settings".to_vec(),
                _ => std::fs::read(root.join(relative)).unwrap(),
            };
        }
        if relative == "oplog/segment_0001.jsonl" {
            return match self.read_target_generation_from_root(root).as_deref() {
                Some("old") => b"old-oplog".to_vec(),
                Some("new") => b"new-oplog".to_vec(),
                _ => std::fs::read(root.join(relative)).unwrap(),
            };
        }
        if relative == "committed_seq" {
            return match self.read_target_generation_from_root(root).as_deref() {
                Some("old") => b"7".to_vec(),
                Some("new") => b"8".to_vec(),
                _ => std::fs::read(root.join(relative)).unwrap(),
            };
        }
        std::fs::read(root.join(relative)).unwrap()
    }

    fn read_target_generation_from_root(&self, root: &Path) -> Option<String> {
        match std::fs::read(root.join("index_meta.json")).ok()?.as_slice() {
            b"old-meta" => Some("old".to_string()),
            b"new-meta" => Some("new".to_string()),
            _ => None,
        }
    }

    fn read_journal(&self) -> PublicationJournal {
        PublicationJournal::from_json(&std::fs::read_to_string(&self.paths.journal).unwrap())
            .unwrap()
    }

    fn write_journal(&self, journal: PublicationJournal) {
        std::fs::create_dir_all(self.paths.journal.parent().unwrap()).unwrap();
        std::fs::write(
            &self.paths.journal,
            serde_json::to_vec_pretty(&journal.to_json_value()).unwrap(),
        )
        .unwrap();
    }

    fn new_digest(&self) -> ContentDigest {
        canonical_tenant_tree_digest(&self.paths.staging, &self.inventory).unwrap()
    }

    fn old_backup_digest(&self) -> ContentDigest {
        canonical_tenant_tree_digest(&self.paths.backup, &self.inventory).unwrap()
    }

    fn target_digest(&self) -> ContentDigest {
        canonical_tenant_tree_digest(&self.paths.target, &self.inventory).unwrap()
    }

    fn prepared_journal_for_staging(&self) -> PublicationJournal {
        let mut journal = PublicationJournal::prepare(
            self.transaction.clone(),
            self.target.clone(),
            PublicationGenerationEvidence::new("generation_1").unwrap(),
            self.new_digest(),
            self.paths.clone(),
        );
        if self.paths.target.exists() {
            journal.prior_digest = Some(self.target_digest());
        } else if self.paths.backup.exists() {
            journal.prior_digest = Some(self.old_backup_digest());
        }
        journal
    }

    fn committed_journal_for_target(&self) -> PublicationJournal {
        let mut journal = PublicationJournal::prepare(
            self.transaction.clone(),
            self.target.clone(),
            PublicationGenerationEvidence::new("generation_1").unwrap(),
            self.target_digest(),
            self.paths.clone(),
        );
        if self.paths.backup.exists() {
            journal.prior_digest = Some(self.old_backup_digest());
        }
        journal.apply(PublicationEvent::Commit).unwrap()
    }

    fn journal_temp_path(&self) -> PathBuf {
        self.paths.journal.with_extension("json.tmp")
    }

    fn transaction_root(&self) -> PathBuf {
        self.paths.journal.parent().unwrap().to_path_buf()
    }

    fn activation_inputs(
        &self,
        manifest: PublicationArtifactManifest,
    ) -> PublicationActivationInputs<'_> {
        PublicationActivationInputs {
            paths: &self.paths,
            target: self.target.clone(),
            transaction_id: self.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest,
            inventory: &self.inventory,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PublicationLayoutSnapshot {
    target: Option<BTreeMap<PathBuf, Vec<u8>>>,
    staging: Option<BTreeMap<PathBuf, Vec<u8>>>,
    backup: Option<BTreeMap<PathBuf, Vec<u8>>>,
}

impl PublicationLayoutSnapshot {
    fn capture(fixture: &ActivationFixture) -> Self {
        Self {
            target: capture_tree_if_dir(&fixture.paths.target),
            staging: capture_tree_if_dir(&fixture.paths.staging),
            backup: capture_tree_if_dir(&fixture.paths.backup),
        }
    }
}

#[derive(Clone, Copy)]
enum EpochMutation {
    Absent,
    Corrupt,
    Old,
    Future,
}

fn capture_tree_if_dir(path: &Path) -> Option<BTreeMap<PathBuf, Vec<u8>>> {
    path.is_dir().then(|| collect_fixture_tree_bytes(path))
}

fn mutate_epoch_record(fixture: &ActivationFixture, mutation: EpochMutation) {
    match mutation {
        EpochMutation::Absent => {
            std::fs::remove_file(fixture.paths.epoch_path()).unwrap();
        }
        EpochMutation::Corrupt => {
            std::fs::write(fixture.paths.epoch_path(), b"1\n").unwrap();
        }
        EpochMutation::Old => {
            std::fs::write(fixture.paths.epoch_path(), b"0").unwrap();
        }
        EpochMutation::Future => {
            std::fs::write(fixture.paths.epoch_path(), b"2").unwrap();
        }
    }
}

#[derive(Clone, Copy)]
enum FixtureTreeKind {
    Old,
    New,
}

impl FixtureTreeKind {
    fn generation(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }

    fn object_id(self) -> &'static str {
        match self {
            Self::Old => "old-widget",
            Self::New => "new-widget",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Old => "legacy waffle iron",
            Self::New => "modern waffle iron",
        }
    }

    fn body(self) -> &'static str {
        match self {
            Self::Old => "old repair guide",
            Self::New => "new repair guide",
        }
    }

    fn metadata_marker(self) -> &'static [u8] {
        match self {
            Self::Old => b"old-meta",
            Self::New => b"new-meta",
        }
    }
}

fn write_authentic_fixture_tree(root: &Path, kind: FixtureTreeKind) {
    if root.exists() {
        std::fs::remove_dir_all(root).unwrap();
    }
    let _index = Index::create_in_dir(root).unwrap();
    IndexSettings::default().save(root.join("settings.json")).unwrap();
    std::fs::write(root.join("index_meta.json"), kind.metadata_marker()).unwrap();
    std::fs::create_dir_all(root.join("oplog")).unwrap();
    std::fs::write(
        root.join("oplog").join("segment_0001.jsonl"),
        deterministic_fixture_oplog(kind),
    )
    .unwrap();
    std::fs::write(root.join("committed_seq"), b"0").unwrap();
}

fn deterministic_fixture_oplog(kind: FixtureTreeKind) -> Vec<u8> {
    let entry = serde_json::json!({
        "seq": 1,
        "timestamp_ms": 1,
        "node_id": "fixture-node",
        "tenant_id": "products",
        "op_type": "upsert",
        "payload": {
            "objectID": kind.object_id(),
            "body": {
                "objectID": kind.object_id(),
                "title": kind.title(),
                "body": kind.body(),
                "generation": kind.generation()
            }
        }
    });
    let mut line = serde_json::to_vec(&entry).unwrap();
    line.push(b'\n');
    line
}

fn copy_fixture_tree(source: &Path, destination: &Path) {
    if destination.exists() {
        std::fs::remove_dir_all(destination).unwrap();
    }
    copy_fixture_tree_inner(source, destination);
}

fn copy_fixture_tree_inner(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    let mut entries = std::fs::read_dir(source)
        .unwrap()
        .map(|entry| entry.unwrap())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_fixture_tree_inner(&source_path, &destination_path);
        } else {
            std::fs::copy(&source_path, &destination_path).unwrap();
        }
    }
}

struct FixtureSourceBytes {
    old: BTreeMap<PathBuf, Vec<u8>>,
    new: BTreeMap<PathBuf, Vec<u8>>,
}

static FIXTURE_SOURCE_BYTES: OnceLock<FixtureSourceBytes> = OnceLock::new();

fn fixture_source_bytes() -> &'static FixtureSourceBytes {
    FIXTURE_SOURCE_BYTES.get_or_init(|| {
        let tmp = TempDir::new().unwrap();
        let old = tmp.path().join("old");
        let new = tmp.path().join("new");
        write_authentic_fixture_tree(&old, FixtureTreeKind::Old);
        write_authentic_fixture_tree(&new, FixtureTreeKind::New);
        FixtureSourceBytes {
            old: collect_fixture_tree_bytes(&old),
            new: collect_fixture_tree_bytes(&new),
        }
    })
}

fn collect_fixture_tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    collect_fixture_tree_bytes_inner(root, root, &mut files);
    files
}

fn collect_fixture_tree_bytes_inner(
    root: &Path,
    current: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) {
    let mut entries = std::fs::read_dir(current)
        .unwrap()
        .map(|entry| entry.unwrap())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_fixture_tree_bytes_inner(root, &path, files);
        } else {
            files.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                std::fs::read(path).unwrap(),
            );
        }
    }
}

fn write_fixture_tree_bytes(root: &Path, files: &BTreeMap<PathBuf, Vec<u8>>) {
    if root.exists() {
        std::fs::remove_dir_all(root).unwrap();
    }
    for (relative, bytes) in files {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

#[cfg(unix)]
fn replace_path_with_symlink(path: &Path, target: &Path) {
    if path.exists() {
        std::fs::remove_dir_all(path).unwrap();
    }
    std::os::unix::fs::symlink(target, path).unwrap();
}

async fn write_fixture_control_index(base: &Path) {
    let manager = IndexManager::new(base);
    manager.create_tenant("control_products").unwrap();
    manager
        .add_documents_sync(
            "control_products",
            vec![Document::from_json(&serde_json::json!({
                "objectID": "control-widget",
                "title": "control waffle iron",
                "body": "unchanged control guide",
                "generation": "control"
            }))
            .unwrap()],
        )
        .await
        .unwrap();
    manager.graceful_shutdown().await;
}

fn assert_reopenable_fixture_tree(
    base: &Path,
    tenant: &str,
    object_id: &str,
    expected_object: serde_json::Value,
    query: &str,
    expected_hits: &[&str],
) {
    let manager = IndexManager::new(base);
    let document = manager
        .get_document(tenant, object_id)
        .unwrap_or_else(|error| {
            panic!(
                "{tenant}/{object_id} should reopen through IndexManager at {}: {error}",
                base.display()
            )
        })
        .unwrap_or_else(|| panic!("{tenant}/{object_id} should exist"));
    assert_eq!(document.to_json(), expected_object);

    let hits = manager
        .search(tenant, query, None, None, 10)
        .unwrap_or_else(|error| panic!("{tenant} query {query:?} should search: {error}"))
        .documents
        .into_iter()
        .map(|hit| hit.document.id)
        .collect::<Vec<_>>();
    assert_eq!(hits, expected_hits);
}

#[test]
fn repair_decision_table_is_total_for_every_phase_and_artifact_evidence() {
    let phases = [
        PublicationPhase::Prepared,
        PublicationPhase::Committed,
        PublicationPhase::RolledBack,
        PublicationPhase::Quarantined,
    ];
    let artifacts = [
        RepairArtifactEvidence::Missing,
        RepairArtifactEvidence::MatchesOld,
        RepairArtifactEvidence::MatchesNew,
        RepairArtifactEvidence::Mismatch,
        RepairArtifactEvidence::Unreadable,
        RepairArtifactEvidence::Reservation,
        RepairArtifactEvidence::StructurallyProvenOld,
    ];
    let epochs = [
        RepairEpochEvidence::UnfencedOrLegacy,
        RepairEpochEvidence::FencedMatch,
        RepairEpochEvidence::FencedMissing,
        RepairEpochEvidence::FencedMismatch,
    ];

    for phase in phases {
        for target in artifacts {
            for backup in artifacts {
                for staging in artifacts {
                    for epoch in epochs {
                        let evidence = RepairEvidence {
                            journal: RepairJournalEvidence::Valid,
                            phase,
                            target,
                            backup,
                            staging,
                            manifest_valid: true,
                            journal_temp_present: false,
                            epoch,
                        };
                        let decision = decide_publication_repair(evidence);
                        assert!(
                            matches!(
                                decision,
                                RepairDecision::None
                                    | RepairDecision::Complete
                                    | RepairDecision::Rollback
                                    | RepairDecision::Cleanup
                                    | RepairDecision::Quarantine
                            ),
                            "unclassified evidence: {evidence:?}"
                        );
                        if matches!(
                            epoch,
                            RepairEpochEvidence::FencedMissing | RepairEpochEvidence::FencedMismatch
                        ) {
                            assert_eq!(
                                decision,
                                RepairDecision::Quarantine,
                                "fenced non-match must fail closed: {evidence:?}"
                            );
                        }
                        if decision != RepairDecision::Quarantine {
                            assert!(
                                repair_evidence_is_proven(evidence),
                                "mutation selected for unproven evidence: {evidence:?} => {decision:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

fn repair_evidence_is_proven(evidence: RepairEvidence) -> bool {
    use RepairArtifactEvidence::{MatchesNew as New, MatchesOld as Old, Missing};
    matches!(
        (evidence.phase, evidence.target, evidence.backup, evidence.staging),
        (PublicationPhase::Prepared, Old, Missing, New)
            | (PublicationPhase::Prepared, Missing, Old, New)
            | (PublicationPhase::Prepared, New, Old, Missing)
            | (PublicationPhase::Prepared, Missing, Old, Missing)
            | (PublicationPhase::Prepared, Old, Missing, Missing)
            | (
                PublicationPhase::Prepared,
                RepairArtifactEvidence::Reservation,
                Missing,
                New,
            )
            | (
                PublicationPhase::Prepared,
                RepairArtifactEvidence::Reservation,
                Missing,
                Missing,
            )
            | (PublicationPhase::Committed, New, Missing, Missing)
            | (PublicationPhase::Committed, RepairArtifactEvidence::Mismatch, Missing, Missing)
            | (PublicationPhase::Committed, New, Old, Missing)
            | (PublicationPhase::RolledBack, Old, Missing, Missing)
            | (PublicationPhase::RolledBack, RepairArtifactEvidence::Mismatch, Missing, Missing)
            | (PublicationPhase::RolledBack, Missing, Missing, Missing)
            | (PublicationPhase::RolledBack, Old, Missing, New)
            | (PublicationPhase::RolledBack, Old, Missing, Old)
            | (PublicationPhase::RolledBack, Old, Missing, RepairArtifactEvidence::Mismatch)
            | (PublicationPhase::RolledBack, Missing, Missing, New)
            | (PublicationPhase::RolledBack, Missing, Missing, Old)
            | (PublicationPhase::RolledBack, Missing, Missing, RepairArtifactEvidence::Mismatch)
            // An empty staging tree recorded against no prior digest. These reach the
            // same residue Cleanup as their Mismatch counterparts above; only the
            // classification of the empty tree is sharper.
            | (
                PublicationPhase::RolledBack,
                Old,
                Missing,
                RepairArtifactEvidence::Reservation,
            )
            | (
                PublicationPhase::RolledBack,
                Missing,
                Missing,
                RepairArtifactEvidence::Reservation,
            )
    )
}

#[test]
fn repair_decision_table_gates_fenced_journals_on_epoch_match() {
    let mut evidence = RepairEvidence::valid(
        PublicationPhase::Prepared,
        RepairArtifactEvidence::MatchesOld,
        RepairArtifactEvidence::Missing,
        RepairArtifactEvidence::MatchesNew,
    );

    evidence.epoch = RepairEpochEvidence::UnfencedOrLegacy;
    assert_eq!(decide_publication_repair(evidence), RepairDecision::Complete);
    evidence.epoch = RepairEpochEvidence::FencedMatch;
    assert_eq!(decide_publication_repair(evidence), RepairDecision::Complete);
    for epoch in [
        RepairEpochEvidence::FencedMissing,
        RepairEpochEvidence::FencedMismatch,
    ] {
        evidence.epoch = epoch;
        assert_eq!(
            decide_publication_repair(evidence),
            RepairDecision::Quarantine
        );
    }
}

#[test]
fn repair_decision_table_limits_pre_journal_epoch_cleanup() {
    let mut evidence = RepairEvidence {
        journal: RepairJournalEvidence::Missing,
        phase: PublicationPhase::Prepared,
        target: RepairArtifactEvidence::StructurallyProvenOld,
        backup: RepairArtifactEvidence::Missing,
        staging: RepairArtifactEvidence::Mismatch,
        manifest_valid: true,
        journal_temp_present: true,
        epoch: RepairEpochEvidence::PreJournalAdvanced,
    };
    assert_eq!(decide_publication_repair(evidence), RepairDecision::Cleanup);
    evidence.epoch = RepairEpochEvidence::UnfencedOrLegacy;
    assert_eq!(decide_publication_repair(evidence), RepairDecision::Cleanup);
    evidence.journal_temp_present = false;
    assert_eq!(
        decide_publication_repair(evidence),
        RepairDecision::Quarantine
    );
    evidence.journal_temp_present = true;

    evidence.backup = RepairArtifactEvidence::MatchesOld;
    assert_eq!(
        decide_publication_repair(evidence),
        RepairDecision::Quarantine
    );
    evidence.backup = RepairArtifactEvidence::Missing;
    evidence.target = RepairArtifactEvidence::Missing;
    assert_eq!(
        decide_publication_repair(evidence),
        RepairDecision::Quarantine
    );
    evidence.target = RepairArtifactEvidence::StructurallyProvenOld;
    evidence.epoch = RepairEpochEvidence::FencedMissing;
    assert_eq!(
        decide_publication_repair(evidence),
        RepairDecision::Quarantine
    );
}

#[test]
fn repair_decision_table_only_allows_digest_proven_mutations() {
    let cases = [
        (
            RepairEvidence::valid(
                PublicationPhase::Prepared,
                RepairArtifactEvidence::MatchesOld,
                RepairArtifactEvidence::Missing,
                RepairArtifactEvidence::MatchesNew,
            ),
            RepairDecision::Complete,
        ),
        (
            RepairEvidence::valid(
                PublicationPhase::Prepared,
                RepairArtifactEvidence::Missing,
                RepairArtifactEvidence::MatchesOld,
                RepairArtifactEvidence::Missing,
            ),
            RepairDecision::Rollback,
        ),
        (
            RepairEvidence::valid(
                PublicationPhase::Committed,
                RepairArtifactEvidence::MatchesNew,
                RepairArtifactEvidence::MatchesOld,
                RepairArtifactEvidence::Missing,
            ),
            RepairDecision::Cleanup,
        ),
        (
            RepairEvidence::valid(
                PublicationPhase::Committed,
                RepairArtifactEvidence::MatchesNew,
                RepairArtifactEvidence::Missing,
                RepairArtifactEvidence::Missing,
            ),
            RepairDecision::None,
        ),
        (
            RepairEvidence::valid(
                PublicationPhase::Committed,
                RepairArtifactEvidence::Mismatch,
                RepairArtifactEvidence::Missing,
                RepairArtifactEvidence::Missing,
            ),
            RepairDecision::None,
        ),
    ];

    for (evidence, expected) in cases {
        assert_eq!(decide_publication_repair(evidence), expected);
    }

    for unsafe_artifact in [
        RepairArtifactEvidence::Mismatch,
        RepairArtifactEvidence::Unreadable,
    ] {
        let evidence = RepairEvidence::valid(
            PublicationPhase::Committed,
            RepairArtifactEvidence::MatchesNew,
            unsafe_artifact,
            RepairArtifactEvidence::Missing,
        );
        assert_eq!(
            decide_publication_repair(evidence),
            RepairDecision::Quarantine
        );
    }
}

#[test]
fn rolled_back_runtime_mutated_target_without_residue_is_converged() {
    let evidence = RepairEvidence::valid(
        PublicationPhase::RolledBack,
        RepairArtifactEvidence::Mismatch,
        RepairArtifactEvidence::Missing,
        RepairArtifactEvidence::Missing,
    );

    assert_eq!(decide_publication_repair(evidence), RepairDecision::None);
}

#[test]
fn repair_decision_table_never_promotes_temp_journal_to_authority() {
    let mut valid = RepairEvidence::valid(
        PublicationPhase::Committed,
        RepairArtifactEvidence::MatchesNew,
        RepairArtifactEvidence::Missing,
        RepairArtifactEvidence::Missing,
    );
    valid.journal_temp_present = true;
    assert_eq!(decide_publication_repair(valid), RepairDecision::Cleanup);

    for journal in [
        RepairJournalEvidence::Missing,
        RepairJournalEvidence::Corrupt,
    ] {
        let evidence = RepairEvidence {
            journal,
            journal_temp_present: true,
            ..valid
        };
        assert_eq!(
            decide_publication_repair(evidence),
            RepairDecision::Quarantine
        );
    }
}

#[test]
fn repair_decision_table_quarantines_invalid_manifest_and_ambiguous_states() {
    let mut invalid_manifest = RepairEvidence::valid(
        PublicationPhase::Prepared,
        RepairArtifactEvidence::MatchesOld,
        RepairArtifactEvidence::Missing,
        RepairArtifactEvidence::MatchesNew,
    );
    invalid_manifest.manifest_valid = false;
    assert_eq!(
        decide_publication_repair(invalid_manifest),
        RepairDecision::Quarantine
    );

    let ambiguous = RepairEvidence::valid(
        PublicationPhase::Prepared,
        RepairArtifactEvidence::MatchesNew,
        RepairArtifactEvidence::Missing,
        RepairArtifactEvidence::MatchesNew,
    );
    assert_eq!(
        decide_publication_repair(ambiguous),
        RepairDecision::Quarantine
    );
}

#[test]
fn repair_publication_completes_prepared_swap_with_digest_proof() {
    let fixture = ActivationFixture::new();
    fixture.write_old_backup();
    fixture.write_new_staging();
    fixture.write_journal(fixture.prepared_journal_for_staging());

    let decision = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
    )
    .unwrap();

    assert_eq!(decision, RepairDecision::Complete);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"new-meta");
    assert_eq!(fixture.read_target_file("settings.json"), b"new-settings");
    assert_eq!(fixture.read_journal().phase, PublicationPhase::Committed);
    assert!(!fixture.paths.backup.exists());
    assert!(!fixture.paths.staging.exists());
}

#[test]
fn repair_publication_completes_journaled_sidecar_promotion() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");
    let manifest = fixture.promoting_external_manifest();
    let mut journal = fixture.prepared_journal_for_staging();
    journal.artifact_manifest = fixture.with_current_sidecar_digests(manifest.clone());
    fixture.write_journal(journal);

    let decision = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        manifest,
        &fixture.inventory,
    )
    .unwrap();

    assert_eq!(decision, RepairDecision::Complete);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"new-meta");
    assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"new-sidecar");
    assert!(!fixture.promoted_sidecar_path().exists());
}

#[test]
fn repair_publication_quarantines_sidecar_digest_mismatch_before_mutation() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");
    let manifest = fixture.promoting_external_manifest();
    let mut journal = fixture.prepared_journal_for_staging();
    journal.artifact_manifest = manifest.clone();
    let entry = journal.artifact_manifest.entries.first_mut().unwrap();
    entry.original_digest = Some(
        ContentDigest::new(
            "sha256:6ba99fa8de9635003f2460b593447339a725c78a96d02d17ff18fb27b7b76b1a",
        )
        .unwrap(),
    );
    entry.promoted_digest = Some(
        ContentDigest::new(
            "sha256:3c605939a467030daccf7d023930f7f984b0a525812df0ff9f407ebe9c5d2c09",
        )
        .unwrap(),
    );
    fixture.write_journal(journal);
    fixture.write_promoted_sidecar(b"tampered-sidecar");

    let decision = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        manifest,
        &fixture.inventory,
    )
    .unwrap();

    assert_eq!(decision, RepairDecision::Quarantine);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
    assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"old-sidecar");
    assert!(fixture.paths.quarantine.join("journal.json").exists());
}

#[test]
fn repair_publication_rolls_back_journaled_sidecar_from_backup() {
    let fixture = ActivationFixture::new();
    fixture.write_new_staging();
    fixture.write_old_backup();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");
    let manifest = fixture.promoting_external_manifest();
    let mut journal = fixture.prepared_journal_for_staging();
    journal.artifact_manifest = fixture.with_current_sidecar_digests(manifest.clone());
    fixture.write_journal(journal);
    std::fs::remove_dir_all(&fixture.paths.staging).unwrap();
    std::fs::create_dir_all(fixture.sidecar_backup_dir()).unwrap();
    std::fs::write(
        fixture.sidecar_backup_dir().join("products.json"),
        b"old-sidecar",
    )
    .unwrap();
    std::fs::remove_file(fixture.sidecar_path()).unwrap();
    std::fs::rename(fixture.promoted_sidecar_path(), fixture.sidecar_path()).unwrap();

    let decision = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        manifest,
        &fixture.inventory,
    )
    .unwrap();

    assert_eq!(decision, RepairDecision::Rollback);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
    assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"old-sidecar");
    assert!(!fixture.sidecar_backup_dir().exists());
}

#[cfg(unix)]
#[test]
fn repair_publication_rejects_symlinked_managed_parent_before_mutation() {
    use std::os::unix::fs::symlink;

    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_journal(fixture.prepared_journal_for_staging());
    let managed_target_parent = fixture.paths.journal.parent().unwrap().parent().unwrap();
    let relocated_parent = fixture.base().join("relocated_publication_target");
    std::fs::rename(managed_target_parent, &relocated_parent).unwrap();
    symlink(&relocated_parent, managed_target_parent).unwrap();

    let result = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
    );

    assert!(result.is_err());
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
    assert!(!fixture.paths.backup.exists());
}

#[test]
fn failed_create_removes_newly_promoted_sidecar() {
    let fixture = ActivationFixture::new();
    fixture.write_new_staging();
    fixture.write_promoted_sidecar(b"new-sidecar");

    let result = activate_publication_for_test(
        PublicationActivationInputs {
            paths: &fixture.paths,
            target: fixture.target.clone(),
            transaction_id: fixture.transaction.clone(),
            generation: PublicationGenerationEvidence::new("generation_1").unwrap(),
            manifest: fixture.promoting_external_manifest(),
            inventory: &fixture.inventory,
        },
        PublicationFaultPoint::AfterStagingPromote,
    );

    assert!(result.is_err());
    assert!(!fixture.sidecar_path().exists());
    assert!(!fixture.promoted_sidecar_path().exists());
}

#[test]
fn repair_publication_cleans_committed_backup_and_stale_journal_temp() {
    let fixture = ActivationFixture::new();
    fixture.write_new_target();
    fixture.write_old_backup();
    fixture.write_journal(fixture.committed_journal_for_target());
    std::fs::write(fixture.journal_temp_path(), b"stale partial journal").unwrap();

    let decision = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
    )
    .unwrap();

    assert_eq!(decision, RepairDecision::Cleanup);
    assert_eq!(fixture.read_target_file("committed_seq"), b"8");
    assert!(!fixture.paths.backup.exists());
    assert!(!fixture.journal_temp_path().exists());
    assert_eq!(fixture.read_journal().phase, PublicationPhase::Committed);
}

#[test]
fn repair_publication_faults_converge_without_second_recovery_path() {
    for fault in [
        PublicationFaultPoint::AfterRepairTargetRename,
        PublicationFaultPoint::DuringCommitJournalWrite,
        PublicationFaultPoint::AfterCommitJournalRename,
        PublicationFaultPoint::DuringCleanup,
    ] {
        let fixture = ActivationFixture::new();
        fixture.write_old_backup();
        fixture.write_new_staging();
        fixture.write_journal(fixture.prepared_journal_for_staging());

        let first = repair_publication_for_test(
            fixture.base(),
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationArtifactManifest::default(),
            &fixture.inventory,
            fault,
        );
        assert!(first.is_err(), "{fault:?} unexpectedly succeeded");

        let decision = repair_publication(
            fixture.base(),
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationArtifactManifest::default(),
            &fixture.inventory,
        )
        .unwrap();

        assert!(
            matches!(decision, RepairDecision::Complete | RepairDecision::Cleanup),
            "{fault:?} converged with unexpected decision {decision:?}"
        );
        assert_eq!(fixture.read_target_file("index_meta.json"), b"new-meta");
        assert!(!fixture.paths.backup.exists());
        assert!(!fixture.paths.staging.exists());
        assert_eq!(fixture.read_journal().phase, PublicationPhase::Committed);
    }

    for fault in [
        PublicationFaultPoint::AfterRepairTargetRename,
        PublicationFaultPoint::DuringRollbackJournalWrite,
        PublicationFaultPoint::AfterRollbackJournalRename,
    ] {
        let fixture = ActivationFixture::new();
        fixture.write_new_staging();
        fixture.write_old_backup();
        fixture.write_journal(fixture.prepared_journal_for_staging());
        std::fs::remove_dir_all(&fixture.paths.staging).unwrap();

        let first = repair_publication_for_test(
            fixture.base(),
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationArtifactManifest::default(),
            &fixture.inventory,
            fault,
        );
        assert!(first.is_err(), "{fault:?} unexpectedly succeeded");

        let decision = repair_publication(
            fixture.base(),
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationArtifactManifest::default(),
            &fixture.inventory,
        )
        .unwrap();

        assert!(
            matches!(decision, RepairDecision::Rollback | RepairDecision::None),
            "{fault:?} converged with unexpected decision {decision:?}"
        );
        assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
        assert!(!fixture.paths.backup.exists());
        assert!(!fixture.paths.staging.exists());
        assert_eq!(fixture.read_journal().phase, PublicationPhase::RolledBack);
    }
}

#[test]
fn every_repair_filesystem_fault_converges_from_live_old_target_and_new_staging() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_journal(fixture.prepared_journal_for_staging());
    let recording = PublicationFaultScript::recording();
    repair_publication_with_faults_for_test(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
        &recording,
    )
    .unwrap();

    for operation_index in 0..recording.operations().len() {
        let fixture = ActivationFixture::new();
        fixture.write_old_target();
        fixture.write_new_staging();
        fixture.write_journal(fixture.prepared_journal_for_staging());
        let faults = PublicationFaultScript::failing_at(operation_index);
        let first = repair_publication_with_faults_for_test(
            fixture.base(),
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationArtifactManifest::default(),
            &fixture.inventory,
            &faults,
        );
        assert!(
            matches!(first, Err(_) | Ok(RepairDecision::Quarantine)),
            "operation {operation_index} unexpectedly completed: {:?}",
            faults.operations()
        );

        let decision = repair_publication(
            fixture.base(),
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationArtifactManifest::default(),
            &fixture.inventory,
        )
        .unwrap();
        assert!(
            matches!(
                decision,
                RepairDecision::Complete | RepairDecision::Cleanup | RepairDecision::None
            ),
            "operation {operation_index} converged to {decision:?}: {:?}",
            faults.operations()
        );
        assert_eq!(
            fixture.read_target_file("index_meta.json"),
            b"new-meta",
            "operation {operation_index}: {:?}",
            faults.operations()
        );
        assert_eq!(fixture.read_journal().phase, PublicationPhase::Committed);
        assert!(!fixture.paths.backup.exists());
        assert!(!fixture.paths.staging.exists());
    }
}

fn prepared_sidecar_completion_fixture() -> (ActivationFixture, PublicationArtifactManifest) {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");
    let manifest = fixture.promoting_external_manifest();
    let mut journal = fixture.prepared_journal_for_staging();
    journal.artifact_manifest = fixture.with_current_sidecar_digests(manifest.clone());
    fixture.write_journal(journal);
    (fixture, manifest)
}

#[test]
fn every_sidecar_completion_repair_fault_converges() {
    let (fixture, manifest) = prepared_sidecar_completion_fixture();
    let recording = PublicationFaultScript::recording();
    repair_publication_with_faults_for_test(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        manifest,
        &fixture.inventory,
        &recording,
    )
    .unwrap();
    let operations = recording.operations();
    let sidecar_backup = fixture.sidecar_backup_dir().join("products.json");
    assert!(operations.contains(&PublicationOperation::CopyFile {
        from: fixture.sidecar_path(),
        to: sidecar_backup.clone(),
    }));
    assert!(operations.contains(&PublicationOperation::SyncFile(sidecar_backup)));
    assert!(operations.contains(&PublicationOperation::Remove(fixture.sidecar_path())));
    assert!(operations.contains(&PublicationOperation::Rename {
        from: fixture.promoted_sidecar_path(),
        to: fixture.sidecar_path(),
    }));

    for operation_index in 0..operations.len() {
        let (fixture, manifest) = prepared_sidecar_completion_fixture();
        let faults = PublicationFaultScript::failing_at(operation_index);
        let first = repair_publication_with_faults_for_test(
            fixture.base(),
            fixture.target.clone(),
            fixture.transaction.clone(),
            manifest.clone(),
            &fixture.inventory,
            &faults,
        );
        assert!(
            matches!(first, Err(_) | Ok(RepairDecision::Quarantine)),
            "operation {operation_index} unexpectedly completed: {:?}",
            faults.operations()
        );

        let decision = repair_publication(
            fixture.base(),
            fixture.target.clone(),
            fixture.transaction.clone(),
            manifest,
            &fixture.inventory,
        )
        .unwrap_or_else(|error| {
            panic!(
                "operation {operation_index} did not rerun: {error}; {:?}",
                faults.operations()
            )
        });
        assert!(
            matches!(
                decision,
                RepairDecision::Complete | RepairDecision::Cleanup | RepairDecision::None
            ),
            "operation {operation_index} converged to {decision:?}: {:?}",
            faults.operations()
        );
        assert_eq!(fixture.read_target_file("index_meta.json"), b"new-meta");
        assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"new-sidecar");
        assert!(!fixture.promoted_sidecar_path().exists());
        assert!(!fixture.sidecar_backup_dir().exists());
        assert_eq!(fixture.read_journal().phase, PublicationPhase::Committed);
    }
}

fn prepared_sidecar_rollback_fixture() -> (ActivationFixture, PublicationArtifactManifest) {
    let fixture = ActivationFixture::new();
    fixture.write_new_staging();
    fixture.write_old_backup();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");
    let manifest = fixture.promoting_external_manifest();
    let mut journal = fixture.prepared_journal_for_staging();
    journal.artifact_manifest = fixture.with_current_sidecar_digests(manifest.clone());
    fixture.write_journal(journal);
    std::fs::remove_dir_all(&fixture.paths.staging).unwrap();
    std::fs::create_dir_all(fixture.sidecar_backup_dir()).unwrap();
    std::fs::write(
        fixture.sidecar_backup_dir().join("products.json"),
        b"old-sidecar",
    )
    .unwrap();
    std::fs::remove_file(fixture.sidecar_path()).unwrap();
    std::fs::rename(fixture.promoted_sidecar_path(), fixture.sidecar_path()).unwrap();
    (fixture, manifest)
}

#[test]
fn every_sidecar_rollback_repair_fault_converges() {
    let (fixture, manifest) = prepared_sidecar_rollback_fixture();
    let recording = PublicationFaultScript::recording();
    repair_publication_with_faults_for_test(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        manifest,
        &fixture.inventory,
        &recording,
    )
    .unwrap();
    let operations = recording.operations();
    let sidecar_backup = fixture.sidecar_backup_dir().join("products.json");
    assert!(operations.contains(&PublicationOperation::Remove(fixture.sidecar_path())));
    assert!(operations.contains(&PublicationOperation::CopyFile {
        from: sidecar_backup,
        to: fixture.sidecar_path(),
    }));
    assert!(operations.contains(&PublicationOperation::SyncFile(fixture.sidecar_path())));

    for operation_index in 0..operations.len() {
        let (fixture, manifest) = prepared_sidecar_rollback_fixture();
        let faults = PublicationFaultScript::failing_at(operation_index);
        let first = repair_publication_with_faults_for_test(
            fixture.base(),
            fixture.target.clone(),
            fixture.transaction.clone(),
            manifest.clone(),
            &fixture.inventory,
            &faults,
        );
        assert!(
            matches!(first, Err(_) | Ok(RepairDecision::Quarantine)),
            "operation {operation_index} unexpectedly completed: {:?}",
            faults.operations()
        );

        let decision = repair_publication(
            fixture.base(),
            fixture.target.clone(),
            fixture.transaction.clone(),
            manifest,
            &fixture.inventory,
        )
        .unwrap();
        assert!(
            matches!(decision, RepairDecision::Rollback | RepairDecision::None),
            "operation {operation_index} converged to {decision:?}: {:?}",
            faults.operations()
        );
        assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
        assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"old-sidecar");
        assert!(!fixture.promoted_sidecar_path().exists());
        assert!(!fixture.sidecar_backup_dir().exists());
        assert_eq!(fixture.read_journal().phase, PublicationPhase::RolledBack);
    }
}

#[test]
fn rolled_back_repair_is_idempotent_on_second_scan() {
    let (fixture, manifest) = prepared_sidecar_rollback_fixture();

    let first = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        manifest.clone(),
        &fixture.inventory,
    )
    .unwrap();
    assert_eq!(first, RepairDecision::Rollback);
    assert_eq!(fixture.read_journal().phase, PublicationPhase::RolledBack);

    let second = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        manifest,
        &fixture.inventory,
    )
    .unwrap();
    assert_eq!(second, RepairDecision::None);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
    assert_eq!(std::fs::read(fixture.sidecar_path()).unwrap(), b"old-sidecar");
    assert!(!fixture.promoted_sidecar_path().exists());
    assert!(!fixture.sidecar_backup_dir().exists());
    assert_eq!(fixture.read_journal().phase, PublicationPhase::RolledBack);
}

#[test]
fn repair_publication_quarantines_unproven_evidence_without_touching_live_target() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_journal(fixture.prepared_journal_for_staging());
    std::fs::write(fixture.paths.target.join("index_meta.json"), b"surprise-live-meta").unwrap();

    let decision = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
    )
    .unwrap();

    assert_eq!(decision, RepairDecision::Quarantine);
    assert_eq!(
        fixture.read_target_file("index_meta.json"),
        b"surprise-live-meta"
    );
    assert!(fixture.paths.quarantine.join("journal.json").exists());
    assert!(fixture.paths.quarantine.join("staging").exists());
}

#[test]
fn repair_publication_rejects_serialized_path_mismatch() {
    let fixture = ActivationFixture::new();
    fixture.write_new_target();
    let journal = fixture.committed_journal_for_target();
    let mut value = journal.to_json_value();
    value["paths"]["target"] = serde_json::json!("other_target");
    std::fs::create_dir_all(fixture.paths.journal.parent().unwrap()).unwrap();
    std::fs::write(&fixture.paths.journal, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    let decision = repair_publication(
        fixture.base(),
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
    )
    .unwrap();

    assert_eq!(decision, RepairDecision::Quarantine);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"new-meta");
    assert!(fixture.paths.quarantine.join("journal.json").exists());
}
