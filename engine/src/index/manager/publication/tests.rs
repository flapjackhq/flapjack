/// Stub summary for engine/src/index/manager/publication/tests.rs.
// Stub summary for engine/src/index/manager/publication/tests.rs.
// Stub summary for engine/src/index/manager/publication/tests.rs.
// Stub summary for engine/src/index/manager/publication/tests.rs.
use super::*;
use crate::analytics::AnalyticsConfig;
use crate::query_suggestions::QsConfigStore;
use tempfile::TempDir;

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
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["transaction_id"], "txn_001");
    assert_eq!(value["target"], "products");
    assert_eq!(value["generation"], "opaque_generation_7");
    assert_eq!(value["digest"], format!("sha256:{}", "a".repeat(64)));
    assert_eq!(value["transition_sequence"], 2);
    assert_eq!(value["phase"], "committed");
    assert_eq!(value["disposition"], "committed");

    let parsed = PublicationJournal::from_json(&value.to_string()).unwrap();
    assert_eq!(parsed.transaction_id.as_str(), "txn_001");
    assert_eq!(parsed.target.as_str(), "products");
    assert_eq!(parsed.phase, PublicationPhase::Committed);
    assert_eq!(parsed.disposition, Some(PublicationDisposition::Committed));
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
    let prepared = prepared_journal();
    let promoting = PublicationJobHandoff::promoting(prepared.transaction_id.clone());
    assert!(PublicationJobHandoff::adopt(&prepared).is_err());
    assert!(PublicationTombstone::from_adopted(&prepared, &promoting).is_err());

    let committed = prepared.apply(PublicationEvent::Commit).unwrap();
    let adopted = PublicationJobHandoff::adopt(&committed).unwrap();
    let tombstone = PublicationTombstone::from_adopted(&committed, &adopted).unwrap();
    assert!(tombstone.retention_eligible());
    assert_eq!(tombstone.outcome, PublicationDisposition::Committed);
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
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
        PublicationFaultPoint::NoFault,
    )
    .unwrap_err();

    assert!(error.to_string().contains("mystery.bin"));
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
    assert_eq!(fixture.read_target_file("settings.json"), b"old-settings");
    assert!(!fixture.paths.backup.exists());
}

#[test]
fn replacement_activation_rolls_back_losslessly_after_promote_failure() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");

    let result = activate_publication_for_test(
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        fixture.external_manifest(),
        &fixture.inventory,
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
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
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
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
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
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        fixture.promoting_external_manifest(),
        &fixture.inventory,
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
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        manifest,
        &fixture.inventory,
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
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        fixture.promoting_external_manifest(),
        &fixture.inventory,
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
            &fixture.paths,
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationGenerationEvidence::new("generation_1").unwrap(),
            PublicationArtifactManifest::default(),
            &fixture.inventory,
            fault,
        );
        assert!(result.is_err(), "{fault:?} unexpectedly succeeded");
    }

    let fixture = ActivationFixture::new();
    fixture.write_new_staging();
    let result = activate_publication_for_test(
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
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
            &fixture.paths,
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationGenerationEvidence::new("generation_1").unwrap(),
            fixture.promoting_external_manifest(),
            &fixture.inventory,
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
            &fixture.paths,
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationGenerationEvidence::new("generation_1").unwrap(),
            fixture.promoting_external_manifest(),
            &fixture.inventory,
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

#[test]
fn activation_fault_hook_observes_every_durable_filesystem_boundary() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    fixture.write_external_sidecar(b"old-sidecar");
    fixture.write_promoted_sidecar(b"new-sidecar");
    let faults = PublicationFaultScript::recording();

    activate_publication_with_faults_for_test(
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        fixture.promoting_external_manifest(),
        &fixture.inventory,
        &faults,
    )
    .unwrap();

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
        from: journal_temp,
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
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        fixture.promoting_external_manifest(),
        &fixture.inventory,
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
            &fixture.paths,
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationGenerationEvidence::new("generation_1").unwrap(),
            fixture.promoting_external_manifest(),
            &fixture.inventory,
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
            &fixture.paths,
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationGenerationEvidence::new("generation_1").unwrap(),
            fixture.promoting_external_manifest(),
            &fixture.inventory,
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
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        PublicationArtifactManifest::default(),
        &fixture.inventory,
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
            &fixture.paths,
            fixture.target.clone(),
            fixture.transaction.clone(),
            PublicationGenerationEvidence::new("generation_1").unwrap(),
            PublicationArtifactManifest::default(),
            &fixture.inventory,
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

struct ActivationFixture {
    _tmp: TempDir,
    base: PathBuf,
    paths: PublicationPaths,
    target: PublicationTarget,
    transaction: PublicationTransactionId,
    inventory: TantivyManagedInventory,
    sidecar_root: PathBuf,
}

impl ActivationFixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let target = PublicationTarget::new("products").unwrap();
        let transaction = PublicationTransactionId::new("txn_001").unwrap();
        let paths = PublicationPaths::new(tmp.path(), &target, &transaction);
        let inventory = TantivyManagedInventory::new([
            PathBuf::from("index_meta.json"),
            PathBuf::from("settings.json"),
            PathBuf::from("oplog/segment_0001.jsonl"),
            PathBuf::from("committed_seq"),
        ])
        .unwrap();
        Self {
            sidecar_root: tmp.path().join(".query_suggestions"),
            base: tmp.path().to_path_buf(),
            _tmp: tmp,
            paths,
            target,
            transaction,
            inventory,
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
        std::fs::create_dir_all(root.join("oplog")).unwrap();
        std::fs::write(root.join("index_meta.json"), b"old-meta").unwrap();
        std::fs::write(root.join("settings.json"), b"old-settings").unwrap();
        std::fs::write(
            root.join("oplog").join("segment_0001.jsonl"),
            b"old-oplog",
        )
        .unwrap();
        std::fs::write(root.join("committed_seq"), b"7").unwrap();
    }

    fn write_new_staging(&self) {
        self.write_new_tree(&self.paths.staging);
    }

    fn write_new_target(&self) {
        self.write_new_tree(&self.paths.target);
    }

    fn write_new_tree(&self, root: &Path) {
        std::fs::create_dir_all(root.join("oplog")).unwrap();
        std::fs::write(root.join("index_meta.json"), b"new-meta").unwrap();
        std::fs::write(root.join("settings.json"), b"new-settings").unwrap();
        std::fs::write(
            root.join("oplog").join("segment_0001.jsonl"),
            b"new-oplog",
        )
        .unwrap();
        std::fs::write(root.join("committed_seq"), b"8").unwrap();
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
        std::fs::read(self.paths.target.join(relative)).unwrap()
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
    ];

    for phase in phases {
        for target in artifacts {
            for backup in artifacts {
                for staging in artifacts {
                    let evidence = RepairEvidence {
                        journal: RepairJournalEvidence::Valid,
                        phase,
                        target,
                        backup,
                        staging,
                        manifest_valid: true,
                        journal_temp_present: false,
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

fn repair_evidence_is_proven(evidence: RepairEvidence) -> bool {
    use RepairArtifactEvidence::{MatchesNew as New, MatchesOld as Old, Missing};
    matches!(
        (evidence.phase, evidence.target, evidence.backup, evidence.staging),
        (PublicationPhase::Prepared, Old, Missing, New)
            | (PublicationPhase::Prepared, Missing, Old, New)
            | (PublicationPhase::Prepared, New, Old, Missing)
            | (PublicationPhase::Prepared, Missing, Old, Missing)
            | (PublicationPhase::Prepared, Old, Missing, Missing)
            | (PublicationPhase::Committed, New, Missing, Missing)
            | (PublicationPhase::Committed, New, Old, Missing)
            | (PublicationPhase::RolledBack, Old, Missing, Missing)
            | (PublicationPhase::RolledBack, Missing, Missing, Missing)
            | (PublicationPhase::RolledBack, Old, Missing, New)
            | (PublicationPhase::RolledBack, Old, Missing, Old)
            | (PublicationPhase::RolledBack, Missing, Missing, New)
            | (PublicationPhase::RolledBack, Missing, Missing, Old)
    )
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
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        fixture.promoting_external_manifest(),
        &fixture.inventory,
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
