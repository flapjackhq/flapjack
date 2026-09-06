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
use std::sync::{Arc, Barrier, OnceLock};
use std::time::{Duration, Instant};
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

fn write_generation_journal(
    base: &std::path::Path,
    target_name: &str,
    transaction_name: &str,
    generation: &str,
    event: Option<PublicationEvent>,
) -> PublicationPaths {
    let target = PublicationTarget::new(target_name).unwrap();
    let transaction = PublicationTransactionId::new(transaction_name).unwrap();
    let paths = PublicationPaths::new(base, &target, &transaction);
    let mut journal = PublicationJournal::prepare(
        transaction,
        target,
        PublicationGenerationEvidence::new(generation).unwrap(),
        digest(),
        paths.clone(),
    );
    if let Some(event) = event {
        journal = journal.apply(event).unwrap();
    }
    std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    std::fs::write(paths.journal.clone(), journal.to_json_value().to_string()).unwrap();
    paths
}

#[test]
fn privacy_scrub_generation_evidence_requires_one_committed_current_journal() {
    let tmp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let expected = PublicationGenerationEvidence::new("generation_1").unwrap();

    let missing = verify_current_generation_evidence(tmp.path(), &target, &expected)
        .expect_err("missing journal evidence must fail closed")
        .to_string();
    assert!(missing.contains("missing current journal"), "{missing}");

    let prepared =
        write_generation_journal(tmp.path(), "products", "txn_prepared", "generation_1", None);
    let non_committed = verify_current_generation_evidence(tmp.path(), &target, &expected)
        .expect_err("prepared journal evidence must fail closed")
        .to_string();
    assert!(non_committed.contains("not committed"), "{non_committed}");
    std::fs::remove_dir_all(prepared.journal.parent().unwrap()).unwrap();

    write_generation_journal(
        tmp.path(),
        "products",
        "txn_committed",
        "generation_1",
        Some(PublicationEvent::Commit),
    );
    verify_current_generation_evidence(tmp.path(), &target, &expected)
        .expect("one committed matching journal should be accepted");

    let stale = PublicationGenerationEvidence::new("generation_2").unwrap();
    let stale_error = verify_current_generation_evidence(tmp.path(), &target, &stale)
        .expect_err("stale generation evidence must fail closed")
        .to_string();
    assert!(stale_error.contains("stale generation"), "{stale_error}");

    write_generation_journal(
        tmp.path(),
        "products",
        "txn_second",
        "generation_1",
        Some(PublicationEvent::Commit),
    );
    let ambiguous = verify_current_generation_evidence(tmp.path(), &target, &expected)
        .expect_err("ambiguous committed journals must fail closed")
        .to_string();
    assert!(
        ambiguous.contains("expected exactly one committed current journal"),
        "{ambiguous}"
    );
}

#[test]
fn privacy_scrub_generation_evidence_distinguishes_unjournaled_and_prepared_siblings() {
    let tmp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let expected = PublicationGenerationEvidence::new("generation_1").unwrap();

    write_generation_journal(
        tmp.path(),
        "products",
        "txn_committed",
        "generation_1",
        Some(PublicationEvent::Commit),
    );
    std::fs::create_dir_all(
        tmp.path()
            .join(".publication")
            .join("products")
            .join("txn_inflight")
            .join("staging"),
    )
    .unwrap();

    verify_current_generation_evidence(tmp.path(), &target, &expected)
        .expect("current generation proof should ignore an unjournaled staging namespace");

    write_generation_journal(tmp.path(), "products", "txn_prepared", "generation_2", None);
    let error = verify_current_generation_evidence(tmp.path(), &target, &expected)
        .expect_err("a prepared sibling makes the current generation indeterminate")
        .to_string();
    assert!(
        error.contains("not committed"),
        "prepared sibling must fail closed: {error}"
    );
}

#[cfg(unix)]
#[test]
fn privacy_scrub_generation_evidence_rejects_symlinked_publication_root() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let expected = PublicationGenerationEvidence::new("generation_1").unwrap();

    write_generation_journal(
        outside.path(),
        "products",
        "txn_committed",
        "generation_1",
        Some(PublicationEvent::Commit),
    );
    symlink(outside.path().join(".publication"), tmp.path().join(".publication")).unwrap();

    let error = verify_current_generation_evidence(tmp.path(), &target, &expected)
        .expect_err("symlinked publication roots must fail closed")
        .to_string();
    assert!(error.contains("symlink"), "{error}");
}

#[cfg(unix)]
#[test]
fn privacy_scrub_generation_evidence_rejects_symlinked_journal() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let expected = PublicationGenerationEvidence::new("generation_1").unwrap();

    let external = write_generation_journal(
        outside.path(),
        "products",
        "txn_external",
        "generation_1",
        Some(PublicationEvent::Commit),
    );
    let local = PublicationPaths::new(
        tmp.path(),
        &target,
        &PublicationTransactionId::new("txn_local").unwrap(),
    );
    std::fs::create_dir_all(local.journal.parent().unwrap()).unwrap();
    symlink(&external.journal, &local.journal).unwrap();

    let error = verify_current_generation_evidence(tmp.path(), &target, &expected)
        .expect_err("symlinked journals must fail closed")
        .to_string();
    assert!(error.contains("symlink"), "{error}");
}

/// TODO: Document reserved_namespace_classifier_recognizes_publication_evidence_paths.
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

/// TODO: Document reserved_namespace_classifier_rejects_unsafe_and_lookalike_paths.
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

/// TODO: Document transaction_namespace_paths_are_deterministic_and_isolated.
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

/// TODO: Document target_and_transaction_reject_caller_chosen_path_components.
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

/// TODO: Document journal_round_trip_preserves_exact_contract_values.
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

/// Invalid journal fields fail closed at the parser boundary.
#[test]
fn journal_rejects_invalid_schema_and_evidence() {
    let mut value = prepared_journal().to_json_value();
    value["target"] = serde_json::json!(".");
    let error = PublicationJournal::from_json(&value.to_string())
        .expect_err("current-directory journal targets must be rejected")
        .to_string();
    assert!(error.contains("reserved for server storage"), "{error}");

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
    assert_eq!(
        tombstone.outcome,
        Some(PublicationDisposition::Committed)
    );
    // Fence evidence must survive terminal compaction into the tombstone.
    assert_eq!(tombstone.fence_evidence, Some(fence));
}

#[test]
fn crawler_success_is_atomic_with_commit_and_survives_handoff_compaction() {
    let prepared = prepared_journal();
    let request_digest = ContentDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
    let completion = CrawlerPublicationCompletion {
        run_id: "018f3e2a-7b1c-7d45-8c90-1234567890ab".to_string(),
        request_digest,
        counters: CrawlerRunCountersEvidence {
            fetched: 2,
            discovered: 3,
            transformed: 2,
            published: 2,
        },
        duration_ms: 41,
        terminal_at_unix_ms: 1_725_000_000_000,
        task_id: 42,
    };

    let committed = prepared.apply_crawler_success(completion).unwrap();
    let terminal = committed
        .crawler_terminal
        .as_ref()
        .expect("committed crawler journal must contain terminal truth");
    assert_eq!(terminal.run_id, "018f3e2a-7b1c-7d45-8c90-1234567890ab");
    assert_eq!(terminal.outcome, CrawlerTerminalOutcome::Succeeded);
    let publication = terminal
        .publication
        .as_ref()
        .expect("success must contain publication evidence");
    assert_eq!(publication.transaction_id, committed.transaction_id);
    assert_eq!(publication.generation, committed.generation);
    assert_eq!(publication.digest, committed.digest);
    assert_eq!(publication.task_id, 42);

    let adopted = PublicationJobHandoff::adopt(&committed).unwrap();
    let tombstone = PublicationTombstone::from_adopted(&committed, &adopted).unwrap();
    assert_eq!(
        tombstone
            .crawler_run
            .as_ref()
            .and_then(|run| run.terminal.as_ref()),
        committed.crawler_terminal.as_ref()
    );
}

#[test]
fn crawler_journal_decode_fails_closed_on_terminal_publication_disagreement() {
    let committed = prepared_journal()
        .apply_crawler_success(CrawlerPublicationCompletion {
            run_id: "018f3e2a-7b1c-7d45-8c90-1234567890ab".to_string(),
            request_digest: digest(),
            counters: CrawlerRunCountersEvidence::default(),
            duration_ms: 1,
            terminal_at_unix_ms: 2,
            task_id: 3,
        })
        .unwrap();
    let mut value = committed.to_json_value();
    value["crawler_terminal"]["publication"]["digest"] =
        serde_json::json!(format!("sha256:{}", "c".repeat(64)));
    assert!(PublicationJournal::from_json(&value.to_string())
        .unwrap_err()
        .to_string()
        .contains("disagrees"));
}

#[test]
fn cancel_before_start_binds_digest_once_and_never_gains_publication() {
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let request_digest = ContentDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    let mut tombstone = PublicationTombstone::cancel_before_start(run_id, 100).unwrap();
    assert!(tombstone
        .crawler_run
        .as_ref()
        .expect("crawler tombstone")
        .request_digest
        .is_none());

    tombstone
        .bind_canceled_start(
            request_digest.clone(),
            CrawlerRunCountersEvidence::default(),
            3,
            103,
        )
        .unwrap();
    let run = tombstone.crawler_run.as_ref().unwrap();
    assert_eq!(run.request_digest.as_ref(), Some(&request_digest));
    let terminal = run.terminal.as_ref().expect("canceled terminal truth");
    assert_eq!(terminal.outcome, CrawlerTerminalOutcome::Canceled);
    assert!(terminal.publication.is_none());
    assert!(tombstone
        .bind_canceled_start(
            ContentDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
            CrawlerRunCountersEvidence::default(),
            4,
            104,
        )
        .is_err());
}

#[test]
fn crawler_store_replays_matching_start_and_rejects_digest_mismatch() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let first = PublicationTombstone::started(run_id, digest(), 100).unwrap();
    store.create(&first).unwrap();
    store.create(&first).expect("matching replay is idempotent");
    assert!(matches!(
        store
            .start_classified_with_admission(run_id, digest(), 101, false)
            .unwrap(),
        CrawlerRunStartDisposition::Replay(_)
    ));
    assert!(matches!(
        store.start_classified_with_admission(
            "018f3e2a-7b1c-7d45-8c90-000000000000",
            digest(),
            101,
            false,
        ),
        Err(CrawlerRunStartError::AdmissionRejected)
    ));

    let mismatch = PublicationTombstone::started(
        run_id,
        ContentDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        100,
    )
    .unwrap();
    assert!(store
        .create(&mismatch)
        .unwrap_err()
        .to_string()
        .contains("digest disagrees"));
}

#[test]
fn crawler_store_expiry_uses_the_durable_deadline_and_requires_no_live_owner() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let canceled_run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    store
        .start_classified_with_deadline_admission(
            canceled_run_id,
            digest(),
            100,
            Some(150),
            true,
        )
        .unwrap();

    assert!(store
        .terminalize_expired_if_unowned(canceled_run_id, 149, 999)
        .unwrap()
        .is_none());
    let live_claim = match store.claim_execution(canceled_run_id).unwrap() {
        CrawlerRunExecutionClaimDisposition::Acquired(claim) => claim,
        _ => panic!("new run must have one execution owner"),
    };
    assert!(store
        .terminalize_expired_if_unowned(canceled_run_id, 150, 999)
        .unwrap()
        .is_none());
    drop(live_claim);

    store.request_cancel(canceled_run_id, 149).unwrap();
    let canceled = store
        .terminalize_expired_if_unowned(canceled_run_id, 150, 999)
        .unwrap();
    assert_eq!(
        canceled
            .unwrap()
            .crawler_run
            .as_ref()
            .unwrap()
            .terminal
            .as_ref()
            .unwrap()
            .outcome,
        CrawlerTerminalOutcome::Canceled
    );
    let replay = store
        .start_classified_with_deadline_admission(
            canceled_run_id,
            digest(),
            500,
            Some(550),
            false,
        )
        .unwrap();
    assert!(matches!(replay, CrawlerRunStartDisposition::Replay(_)));
    store.acknowledge(canceled_run_id, 151).unwrap();
    store.acknowledge(canceled_run_id, 999).unwrap();
    assert_eq!(
        store
            .load(canceled_run_id)
            .unwrap()
            .unwrap()
            .crawler_run
            .unwrap()
            .terminal
            .unwrap()
            .outcome,
        CrawlerTerminalOutcome::Canceled
    );

    let lost_run_id = "018f3e2a-7b1c-7d45-8c90-000000000001";
    store
        .start_classified_with_deadline_admission(lost_run_id, digest(), 200, Some(250), true)
        .unwrap();
    let lost = store
        .terminalize_expired_if_unowned(lost_run_id, 250, 999)
        .unwrap()
        .unwrap();
    assert_eq!(
        lost.crawler_run
            .unwrap()
            .terminal
            .unwrap()
            .outcome,
        CrawlerTerminalOutcome::Failed {
            error_code: CrawlerRunErrorCodeEvidence::WorkerLost,
        }
    );
    assert_eq!(
        store.request_cancel_with_disposition(lost_run_id, 251).unwrap(),
        CrawlerRunCancelDispositionEvidence::AlreadyTerminal
    );

    assert!(
        PublicationTombstone::started_with_deadline(canceled_run_id, digest(), 100, Some(99))
            .is_err()
    );
}

#[test]
fn crawler_store_reads_replays_and_expires_a_serialized_legacy_run_without_a_deadline_field() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let request_digest = format!("sha256:{}", "a".repeat(64));
    let legacy_json = format!(
        r#"{{"transaction_id":null,"target":null,"generation":null,"digest":null,"fence_evidence":null,"outcome":null,"adopted":false,"crawler_run":{{"run_id":"{run_id}","request_digest":"{request_digest}","started_at_unix_ms":100,"cancel_requested_at_unix_ms":null,"terminal":null}}}}"#
    );
    assert!(!legacy_json.contains("deadline_at_unix_ms"));
    let root = tmp.path().join(CRAWLER_TOMBSTONE_DIR);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(format!("{run_id}.json")), legacy_json).unwrap();

    let retained = store.load(run_id).unwrap().unwrap();
    assert_eq!(
        retained
            .crawler_run
            .as_ref()
            .unwrap()
            .deadline_at_unix_ms,
        None
    );
    assert!(matches!(
        store
            .start_classified_with_deadline_admission(run_id, digest(), 999, Some(1_049), false)
            .unwrap(),
        CrawlerRunStartDisposition::Replay(_)
    ));
    assert!(store
        .terminalize_expired_if_unowned(run_id, 149, 50)
        .unwrap()
        .is_none());
    let terminal = store
        .terminalize_expired_if_unowned(run_id, 150, 50)
        .unwrap()
        .expect("legacy run must expire at start plus the compatibility bound");
    assert_eq!(
        terminal
            .crawler_run
            .unwrap()
            .terminal
            .unwrap()
            .outcome,
        CrawlerTerminalOutcome::Failed {
            error_code: CrawlerRunErrorCodeEvidence::WorkerLost,
        }
    );
}

#[test]
fn crawler_store_runtime_terminalization_atomically_respects_cancellation_ordering() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let canceled_run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    store.start(canceled_run_id, digest(), 100).unwrap();
    store.request_cancel(canceled_run_id, 110).unwrap();
    let canceled = store
        .finish_runtime_without_publication(
            canceled_run_id,
            CrawlerTerminalOutcome::Failed {
                error_code: CrawlerRunErrorCodeEvidence::FetchTimeout,
            },
            CrawlerRunCountersEvidence::default(),
            20,
            120,
        )
        .unwrap();
    assert_eq!(
        canceled
            .crawler_run
            .unwrap()
            .terminal
            .unwrap()
            .outcome,
        CrawlerTerminalOutcome::Canceled
    );

    let failed_run_id = "018f3e2a-7b1c-7d45-8c90-000000000001";
    store.start(failed_run_id, digest(), 200).unwrap();
    let failed = store
        .finish_runtime_without_publication(
            failed_run_id,
            CrawlerTerminalOutcome::Failed {
                error_code: CrawlerRunErrorCodeEvidence::FetchTimeout,
            },
            CrawlerRunCountersEvidence::default(),
            20,
            220,
        )
        .unwrap();
    assert_eq!(
        failed
            .crawler_run
            .unwrap()
            .terminal
            .unwrap()
            .outcome,
        CrawlerTerminalOutcome::Failed {
            error_code: CrawlerRunErrorCodeEvidence::FetchTimeout,
        }
    );
    assert_eq!(
        store.request_cancel_with_disposition(failed_run_id, 221).unwrap(),
        CrawlerRunCancelDispositionEvidence::AlreadyTerminal
    );
}

#[test]
fn crawler_store_cancel_before_start_is_durable_and_prevents_running_state() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    store.request_cancel(run_id, 100).unwrap();

    let reserved = store.load(run_id).unwrap().expect("durable cancellation");
    let reservation_terminal = reserved
        .crawler_run
        .as_ref()
        .and_then(|run| run.terminal.as_ref())
        .expect("cancel-before-start is immediately terminal");
    assert_eq!(
        reservation_terminal.outcome,
        CrawlerTerminalOutcome::Canceled
    );
    store.acknowledge_classified(run_id, 101).unwrap();

    let canceled = store
        .bind_canceled_start(
            run_id,
            digest(),
            CrawlerRunCountersEvidence::default(),
            2,
            102,
        )
        .unwrap();
    let terminal = canceled
        .crawler_run
        .as_ref()
        .and_then(|run| run.terminal.as_ref())
        .unwrap();
    assert_eq!(terminal.outcome, CrawlerTerminalOutcome::Canceled);
    assert_eq!(terminal.acknowledged_at_unix_ms, Some(101));
    assert!(terminal.publication.is_none());
    assert_eq!(store.load(run_id).unwrap(), Some(canceled));
}

fn write_crawler_success_journal(
    base: &std::path::Path,
    run_id: &str,
) -> PublicationJournal {
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new(format!("txn_{}", &run_id[..8])).unwrap();
    let paths = PublicationPaths::new(base, &target, &transaction);
    let journal = PublicationJournal::prepare(
        transaction,
        target,
        PublicationGenerationEvidence::new("crawler_generation_1").unwrap(),
        digest(),
        paths.clone(),
    )
    .apply_crawler_success(CrawlerPublicationCompletion {
        run_id: run_id.to_string(),
        request_digest: ContentDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
        counters: CrawlerRunCountersEvidence {
            fetched: 2,
            discovered: 3,
            transformed: 2,
            published: 2,
        },
        duration_ms: 40,
        terminal_at_unix_ms: 1_000,
        task_id: 17,
    })
    .unwrap();
    std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    std::fs::write(
        &paths.journal,
        serde_json::to_vec_pretty(&journal.to_json_value()).unwrap(),
    )
    .unwrap();
    journal
}

#[test]
fn crawler_store_recovers_success_from_commit_before_projection_or_response() {
    let tmp = TempDir::new().unwrap();
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let journal = write_crawler_success_journal(tmp.path(), run_id);
    let store = CrawlerRunStore::new(tmp.path());

    let recovered = store.load(run_id).unwrap().expect("journal truth");
    assert_eq!(
        recovered
            .crawler_run
            .as_ref()
            .and_then(|run| run.terminal.as_ref()),
        journal.crawler_terminal.as_ref()
    );
}

#[test]
fn crawler_store_recovers_success_when_data_dir_is_dot_relative() {
    let tmp = tempfile::tempdir_in(".").unwrap();
    let base = PathBuf::from(tmp.path().file_name().expect("temporary directory name")).join("data");
    std::fs::create_dir(&base).unwrap();
    assert!(!base.is_absolute(), "fixture must exercise a relative data dir");

    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let request_digest = ContentDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    let store = CrawlerRunStore::new(&base);
    store.start(run_id, request_digest, 900).unwrap();
    let journal = write_crawler_success_journal(&base, run_id);

    let expected = relative_path_evidence(&journal.target, &journal.transaction_id);
    let mut legacy = journal.to_json_value();
    legacy["paths"] = serde_json::json!({
        "target": PathBuf::from("./data").join(&expected.target),
        "staging": PathBuf::from("./data").join(&expected.staging),
        "backup": PathBuf::from("./data").join(&expected.backup),
        "journal": PathBuf::from("./data").join(&expected.journal),
        "quarantine": PathBuf::from("./data").join(&expected.quarantine),
    });
    std::fs::write(
        &journal.paths.journal,
        serde_json::to_vec_pretty(&legacy).unwrap(),
    )
    .unwrap();

    let recovered = store
        .load(run_id)
        .expect("legacy ./data journal paths must remain replayable")
        .expect("committed journal must supersede its running precursor");
    assert_eq!(
        recovered.crawler_run.unwrap().terminal,
        journal.crawler_terminal
    );
    assert_eq!(
        journal.to_json_value()["paths"],
        serde_json::json!({
            "target": expected.target,
            "staging": expected.staging,
            "backup": expected.backup,
            "journal": expected.journal,
            "quarantine": expected.quarantine,
        })
    );
}

#[test]
fn publication_journal_recovers_one_shared_safe_relative_path_prefix() {
    let journal = prepared_journal();
    let expected = relative_path_evidence(&journal.target, &journal.transaction_id);

    for prefix in ["data", "state/runtime", "./data"] {
        let mut raw = journal.to_json_value();
        raw["paths"] = serde_json::json!({
            "target": PathBuf::from(prefix).join(&expected.target),
            "staging": PathBuf::from(prefix).join(&expected.staging),
            "backup": PathBuf::from(prefix).join(&expected.backup),
            "journal": PathBuf::from(prefix).join(&expected.journal),
            "quarantine": PathBuf::from(prefix).join(&expected.quarantine),
        });
        assert_eq!(
            PublicationJournal::from_recovery_json(&raw.to_string())
                .unwrap_or_else(|error| panic!("safe shared prefix {prefix:?}: {error}"))
                .paths,
            expected,
        );
    }
}

#[test]
fn publication_journal_rejects_inconsistent_or_unsafe_relative_path_prefixes() {
    let journal = prepared_journal();
    let expected = relative_path_evidence(&journal.target, &journal.transaction_id);
    let prefixed = |prefix: &str| {
        serde_json::json!({
            "target": PathBuf::from(prefix).join(&expected.target),
            "staging": PathBuf::from(prefix).join(&expected.staging),
            "backup": PathBuf::from(prefix).join(&expected.backup),
            "journal": PathBuf::from(prefix).join(&expected.journal),
            "quarantine": PathBuf::from(prefix).join(&expected.quarantine),
        })
    };

    let mut inconsistent = journal.to_json_value();
    inconsistent["paths"] = prefixed("state");
    inconsistent["paths"]["journal"] =
        serde_json::json!(PathBuf::from("other").join(&expected.journal));

    let mut suffix_mismatch = journal.to_json_value();
    suffix_mismatch["paths"] = prefixed("state");
    suffix_mismatch["paths"]["target"] = serde_json::json!("state/other-target");

    let mut inconsistent_curdir = journal.to_json_value();
    inconsistent_curdir["paths"] = prefixed("state");
    inconsistent_curdir["paths"]["target"] =
        serde_json::json!(PathBuf::from("./state").join(&expected.target));

    for (label, paths) in [
        ("inconsistent base", inconsistent["paths"].clone()),
        ("suffix mismatch", suffix_mismatch["paths"].clone()),
        (
            "inconsistent leading curdir",
            inconsistent_curdir["paths"].clone(),
        ),
        ("absolute base", prefixed("/state")),
        ("parent traversal", prefixed("../state")),
    ] {
        let mut raw = journal.to_json_value();
        raw["paths"] = paths;
        assert!(
            PublicationJournal::from_recovery_json(&raw.to_string()).is_err(),
            "{label} must fail closed"
        );
    }
}

#[test]
fn crawler_success_recovery_supersedes_only_its_matching_running_precursor() {
    let tmp = TempDir::new().unwrap();
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let request_digest = ContentDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    store
        .create(&PublicationTombstone::started(
            run_id,
            request_digest.clone(),
            900,
        )
        .unwrap())
        .unwrap();
    let journal = write_crawler_success_journal(tmp.path(), run_id);

    let recovered = store.load(run_id).unwrap().unwrap();
    assert_eq!(
        recovered.crawler_run.unwrap().terminal,
        journal.crawler_terminal
    );

    let mut wrong = PublicationTombstone::started(
        run_id,
        ContentDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap(),
        900,
    )
    .unwrap();
    wrong.request_cancel(901).unwrap();
    store.persist(&wrong).unwrap();
    assert!(store.load(run_id).is_err());
}

#[test]
fn crawler_store_compacts_before_journal_removal_and_refuses_disagreement() {
    let tmp = TempDir::new().unwrap();
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let journal = write_crawler_success_journal(tmp.path(), run_id);
    let store = CrawlerRunStore::new(tmp.path());
    store.compact_success(&journal).unwrap();

    let mut conflicting = store.load(run_id).unwrap().unwrap();
    conflicting
        .crawler_run
        .as_mut()
        .unwrap()
        .terminal
        .as_mut()
        .unwrap()
        .counters
        .published = 99;
    store.persist(&conflicting).unwrap();
    assert!(store.load(run_id).is_err());

    store.persist(&PublicationTombstone::from_adopted(
        &journal,
        &PublicationJobHandoff::adopt(&journal).unwrap(),
    ).unwrap()).unwrap();
    std::fs::remove_dir_all(journal.paths.journal.parent().unwrap()).unwrap();
    assert_eq!(
        store
            .load(run_id)
            .unwrap()
            .unwrap()
            .crawler_run
            .unwrap()
            .terminal,
        journal.crawler_terminal
    );
}

#[test]
fn crawler_cleanup_path_persists_handoff_tombstone_before_deleting_journal() {
    let tmp = TempDir::new().unwrap();
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let journal = write_crawler_success_journal(tmp.path(), run_id);

    retire_committed_publication_journals(tmp.path(), &journal.target).unwrap();

    assert!(!journal.paths.journal.exists());
    let retained = CrawlerRunStore::new(tmp.path())
        .load(run_id)
        .unwrap()
        .expect("cleanup must compact crawler truth before journal deletion");
    assert_eq!(
        retained.crawler_run.unwrap().terminal,
        journal.crawler_terminal
    );
}

#[test]
fn crawler_success_ack_updates_journal_truth_and_compacted_projection() {
    let tmp = TempDir::new().unwrap();
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let journal = write_crawler_success_journal(tmp.path(), run_id);
    let store = CrawlerRunStore::new(tmp.path());

    store.acknowledge(run_id, 2_000).unwrap();

    let persisted = PublicationJournal::from_json(
        &std::fs::read_to_string(&journal.paths.journal).unwrap(),
    )
    .unwrap();
    assert_eq!(
        persisted
            .crawler_terminal
            .as_ref()
            .unwrap()
            .acknowledged_at_unix_ms,
        Some(2_000)
    );
    assert_eq!(
        store
            .load(run_id)
            .unwrap()
            .unwrap()
            .crawler_run
            .unwrap()
            .terminal
            .unwrap()
            .acknowledged_at_unix_ms,
        Some(2_000)
    );
}

#[test]
fn crawler_store_ack_and_retention_boundaries_never_prune_unacked_truth() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let terminal_at = 10_000;
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let mut failed = PublicationTombstone::started(run_id, digest(), 9_000).unwrap();
    failed
        .finish_without_publication(
            CrawlerTerminalOutcome::Failed {
                error_code: CrawlerRunErrorCodeEvidence::WorkerLost,
            },
            CrawlerRunCountersEvidence::default(),
            1_000,
            terminal_at,
        )
        .unwrap();
    store.create(&failed).unwrap();

    assert_eq!(
        store.prune(terminal_at + CrawlerRunStore::RETENTION_MS).unwrap(),
        0,
        "unacknowledged terminal truth is never pruned"
    );
    store.acknowledge(run_id, terminal_at + 1).unwrap();
    assert_eq!(
        store
            .prune(terminal_at + CrawlerRunStore::RETENTION_MS - 1)
            .unwrap(),
        0
    );
    assert_eq!(
        store.prune(terminal_at + CrawlerRunStore::RETENTION_MS).unwrap(),
        1
    );
    assert!(store.load(run_id).unwrap().is_none());
}

#[test]
fn crawler_store_prune_never_unlinks_a_live_execution_owner() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let terminal_at = 10_000;
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    store.start(run_id, digest(), 9_000).unwrap();
    let live_claim = match store.claim_execution(run_id).unwrap() {
        CrawlerRunExecutionClaimDisposition::Acquired(claim) => claim,
        _ => panic!("started run must own its execution lock"),
    };
    store
        .finish_without_publication(
            run_id,
            CrawlerTerminalOutcome::Failed {
                error_code: CrawlerRunErrorCodeEvidence::WorkerLost,
            },
            CrawlerRunCountersEvidence::default(),
            1_000,
            terminal_at,
        )
        .unwrap();
    store.acknowledge(run_id, terminal_at + 1).unwrap();

    assert_eq!(
        store
            .prune(terminal_at + CrawlerRunStore::RETENTION_MS)
            .unwrap(),
        0,
        "maintenance must not unlink a lock path while an execution owner is live"
    );
    assert!(store.load(run_id).unwrap().is_some());

    drop(live_claim);
    assert_eq!(
        store
            .prune(terminal_at + CrawlerRunStore::RETENTION_MS)
            .unwrap(),
        1
    );
    assert!(store.load(run_id).unwrap().is_none());
}

#[test]
fn crawler_store_prune_preserves_current_success_journal_replay_across_restart() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let request_digest = ContentDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    store.start(run_id, request_digest, 900).unwrap();
    let claim = match store.claim_execution(run_id).unwrap() {
        CrawlerRunExecutionClaimDisposition::Acquired(claim) => claim,
        _ => panic!("started run must create its execution lock"),
    };
    drop(claim);
    let journal = write_crawler_success_journal(tmp.path(), run_id);
    store.acknowledge(run_id, 2_000).unwrap();
    let acknowledged_terminal = store
        .load(run_id)
        .unwrap()
        .unwrap()
        .crawler_run
        .unwrap()
        .terminal;

    assert_eq!(
        store
            .prune(1_000 + CrawlerRunStore::RETENTION_MS)
            .unwrap(),
        1
    );
    assert!(journal.paths.journal.exists());
    let restarted = CrawlerRunStore::new(tmp.path());
    assert_eq!(
        restarted
            .load(run_id)
            .unwrap()
            .unwrap()
            .crawler_run
            .unwrap()
            .terminal,
        acknowledged_terminal
    );
}

#[test]
fn crawler_store_prune_makes_lock_removal_durable_before_tombstone_removal() {
    fn seed() -> (TempDir, CrawlerRunStore, PathBuf, PathBuf, u64) {
        let tmp = TempDir::new().unwrap();
        let store = CrawlerRunStore::new(tmp.path());
        let terminal_at = 10_000;
        let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
        store.start(run_id, digest(), 9_000).unwrap();
        let claim = match store.claim_execution(run_id).unwrap() {
            CrawlerRunExecutionClaimDisposition::Acquired(claim) => claim,
            _ => panic!("started run must own its execution lock"),
        };
        drop(claim);
        store
            .finish_without_publication(
                run_id,
                CrawlerTerminalOutcome::Failed {
                    error_code: CrawlerRunErrorCodeEvidence::WorkerLost,
                },
                CrawlerRunCountersEvidence::default(),
                1_000,
                terminal_at,
            )
            .unwrap();
        store.acknowledge(run_id, terminal_at + 1).unwrap();
        let root = tmp.path().join(CRAWLER_TOMBSTONE_DIR);
        let lock = root.join(format!("{run_id}.execution.lock"));
        let tombstone = root.join(format!("{run_id}.json"));
        (tmp, store, lock, tombstone, terminal_at)
    }

    let (_tmp, store, lock, tombstone, terminal_at) = seed();
    let recording = PublicationFaultScript::recording();
    assert_eq!(
        store
            .prune_with_faults_for_test(
                terminal_at + CrawlerRunStore::RETENTION_MS,
                &recording,
            )
            .unwrap(),
        1
    );
    let operations = recording.operations();
    assert_eq!(
        operations,
        vec![
            PublicationOperation::Remove(lock.clone()),
            PublicationOperation::SyncDirectory(lock.parent().unwrap().to_path_buf()),
            PublicationOperation::Remove(tombstone.clone()),
            PublicationOperation::SyncDirectory(tombstone.parent().unwrap().to_path_buf()),
        ]
    );

    let (_tmp, store, lock, tombstone, terminal_at) = seed();
    let crash_before_tombstone_remove = PublicationFaultScript::failing_at(2);
    store
        .prune_with_faults_for_test(
            terminal_at + CrawlerRunStore::RETENTION_MS,
            &crash_before_tombstone_remove,
        )
        .expect_err("simulated crash must stop before tombstone removal");
    assert!(!lock.exists());
    assert!(tombstone.exists());
    assert_eq!(
        crash_before_tombstone_remove.operations(),
        vec![
            PublicationOperation::Remove(lock.clone()),
            PublicationOperation::SyncDirectory(lock.parent().unwrap().to_path_buf()),
            PublicationOperation::Remove(tombstone.clone()),
        ]
    );
}

fn seed_unacknowledged_crawler_tombstones(
    base: &std::path::Path,
    run_id_prefix: &str,
    numbers: std::ops::Range<usize>,
) {
    let root = base.join(CRAWLER_TOMBSTONE_DIR);
    std::fs::create_dir_all(&root).unwrap();
    for number in numbers {
        let run_id = format!("{run_id_prefix}{number:012}");
        let tombstone = PublicationTombstone::cancel_before_start(&run_id, 1).unwrap();
        std::fs::write(
            root.join(format!("{run_id}.json")),
            serde_json::to_vec(&tombstone).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn crawler_store_enforces_finite_unacknowledged_cap() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let run_id_prefix = "018f3e2a-7b1c-7d45-8c90-";
    let first_run_id = format!("{run_id_prefix}{:012}", 0);
    store
        .create(&PublicationTombstone::cancel_before_start(&first_run_id, 1).unwrap())
        .expect("the production create path must establish the durable namespace");
    seed_unacknowledged_crawler_tombstones(
        tmp.path(),
        run_id_prefix,
        1..CrawlerRunStore::MAX_UNACKNOWLEDGED - 1,
    );
    assert_eq!(
        store.unacknowledged_count_unlocked().unwrap(),
        CrawlerRunStore::MAX_UNACKNOWLEDGED - 1
    );
    let boundary_run_id = format!(
        "{run_id_prefix}{:012}",
        CrawlerRunStore::MAX_UNACKNOWLEDGED - 1
    );
    store
        .create(&PublicationTombstone::cancel_before_start(&boundary_run_id, 1).unwrap())
        .expect("the final retained slot must remain usable");
    assert_eq!(
        store.unacknowledged_count_unlocked().unwrap(),
        CrawlerRunStore::MAX_UNACKNOWLEDGED
    );
    let overflow_run_id = "018f3e2a-7b1c-7d45-8c90-999999999999";
    let overflow = PublicationTombstone::cancel_before_start(overflow_run_id, 1).unwrap();
    assert!(store
        .create(&overflow)
        .unwrap_err()
        .to_string()
        .contains("retention cap"));
    assert!(!tmp
        .path()
        .join(CRAWLER_TOMBSTONE_DIR)
        .join(format!("{overflow_run_id}.json"))
        .exists());
}

#[test]
fn crawler_start_transition_has_exactly_one_concurrent_owner() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    let workers = 16;
    let barrier = Arc::new(Barrier::new(workers));
    let mut joins = Vec::new();

    for _ in 0..workers {
        let store = store.clone();
        let barrier = barrier.clone();
        joins.push(std::thread::spawn(move || {
            barrier.wait();
            store.start(run_id, digest(), 100).unwrap()
        }));
    }

    let dispositions = joins
        .into_iter()
        .map(|join| join.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        dispositions
            .iter()
            .filter(|disposition| matches!(disposition, CrawlerRunStartDisposition::Started(_)))
            .count(),
        1
    );
    assert_eq!(
        dispositions
            .iter()
            .filter(|disposition| matches!(disposition, CrawlerRunStartDisposition::Replay(_)))
            .count(),
        workers - 1
    );
}

#[test]
fn crawler_cancel_before_start_is_terminal_and_late_start_only_binds_digest() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    store.request_cancel(run_id, 90).unwrap();

    let disposition = store.start(run_id, digest(), 100).unwrap();
    let CrawlerRunStartDisposition::Canceled(canceled) = disposition else {
        panic!("cancel-before-start must never acquire fetch ownership");
    };
    let run = canceled.crawler_run.unwrap();
    assert_eq!(run.cancel_requested_at_unix_ms, Some(90));
    assert_eq!(run.request_digest, Some(digest()));
    assert_eq!(
        run.terminal.unwrap().outcome,
        CrawlerTerminalOutcome::Canceled
    );
    assert!(matches!(
        store.start(run_id, digest(), 101).unwrap(),
        CrawlerRunStartDisposition::Replay(_)
    ));
}

#[test]
fn crawler_cancel_ack_and_execution_claim_dispositions_are_atomic() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    store.start(run_id, digest(), 100).unwrap();

    let CrawlerRunExecutionClaimDisposition::Acquired(first_claim) =
        store.claim_execution(run_id).unwrap()
    else {
        panic!("first executor must acquire the durable run slot");
    };
    assert!(matches!(
        store.claim_execution(run_id).unwrap(),
        CrawlerRunExecutionClaimDisposition::AlreadyExecuting
    ));
    drop(first_claim);
    let CrawlerRunExecutionClaimDisposition::Acquired(restart_claim) =
        store.claim_execution(run_id).unwrap()
    else {
        panic!("dropped worker claim must be restartable");
    };
    drop(restart_claim);

    assert_eq!(
        store
            .request_cancel_with_disposition(run_id, 101)
            .unwrap(),
        CrawlerRunCancelDispositionEvidence::CancelRequested
    );
    assert_eq!(
        store
            .request_cancel_with_disposition(run_id, 102)
            .unwrap(),
        CrawlerRunCancelDispositionEvidence::AlreadyRequested
    );
    assert!(matches!(
        store.claim_execution(run_id).unwrap(),
        CrawlerRunExecutionClaimDisposition::NotRunnable
    ));
    assert!(matches!(
        store.acknowledge_classified(run_id, 103),
        Err(CrawlerRunAcknowledgeError::NotTerminal)
    ));

    store
        .finish_without_publication(
            run_id,
            CrawlerTerminalOutcome::Canceled,
            CrawlerRunCountersEvidence::default(),
            3,
            104,
        )
        .unwrap();
    assert_eq!(
        store
            .request_cancel_with_disposition(run_id, 105)
            .unwrap(),
        CrawlerRunCancelDispositionEvidence::AlreadyTerminal
    );
    store.acknowledge_classified(run_id, 106).unwrap();
    store.acknowledge_classified(run_id, 107).unwrap();
    assert_eq!(
        store
            .load(run_id)
            .unwrap()
            .unwrap()
            .crawler_run
            .unwrap()
            .terminal
            .unwrap()
            .acknowledged_at_unix_ms,
        Some(106)
    );
    assert!(matches!(
        store.acknowledge_classified("018f3e2a-7b1c-7d45-8c90-000000000000", 1),
        Err(CrawlerRunAcknowledgeError::NotFound)
    ));
}

fn crawler_completion(run_id: &str, request_digest: ContentDigest) -> CrawlerPublicationCompletion {
    CrawlerPublicationCompletion {
        run_id: run_id.to_string(),
        request_digest,
        counters: CrawlerRunCountersEvidence {
            fetched: 1,
            discovered: 1,
            transformed: 1,
            published: 1,
        },
        duration_ms: 10,
        terminal_at_unix_ms: 110,
        task_id: 42,
    }
}

#[test]
fn crawler_publication_admission_rechecks_digest_cancel_terminal_and_deadline() {
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";

    let mismatch_tmp = TempDir::new().unwrap();
    let mismatch_store = CrawlerRunStore::new(mismatch_tmp.path());
    mismatch_store.start(run_id, digest(), 100).unwrap();
    let mismatch = crawler_completion(
        run_id,
        ContentDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
    );
    assert!(mismatch_store
        .admit_publication(mismatch, Instant::now() + Duration::from_secs(60))
        .unwrap_err()
        .to_string()
        .contains("digest disagrees"));

    let canceled_tmp = TempDir::new().unwrap();
    let canceled_store = CrawlerRunStore::new(canceled_tmp.path());
    canceled_store.start(run_id, digest(), 100).unwrap();
    canceled_store.request_cancel(run_id, 101).unwrap();
    assert!(canceled_store
        .admit_publication(
            crawler_completion(run_id, digest()),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap_err()
        .to_string()
        .contains("canceled"));

    let terminal_tmp = TempDir::new().unwrap();
    let terminal_store = CrawlerRunStore::new(terminal_tmp.path());
    terminal_store.start(run_id, digest(), 100).unwrap();
    terminal_store
        .finish_without_publication(
            run_id,
            CrawlerTerminalOutcome::Failed {
                error_code: CrawlerRunErrorCodeEvidence::WorkerLost,
            },
            CrawlerRunCountersEvidence::default(),
            10,
            110,
        )
        .unwrap();
    assert!(terminal_store
        .admit_publication(
            crawler_completion(run_id, digest()),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap_err()
        .to_string()
        .contains("terminal"));

    let deadline_tmp = TempDir::new().unwrap();
    let deadline_store = CrawlerRunStore::new(deadline_tmp.path());
    deadline_store.start(run_id, digest(), 100).unwrap();
    assert!(deadline_store
        .admit_publication(
            crawler_completion(run_id, digest()),
            Instant::now() - Duration::from_secs(1),
        )
        .unwrap_err()
        .to_string()
        .contains("deadline"));

    let capacity_tmp = TempDir::new().unwrap();
    let capacity_store = CrawlerRunStore::new(capacity_tmp.path());
    capacity_store.start(run_id, digest(), 100).unwrap();
    let capacity_root = capacity_tmp.path().join(CRAWLER_TOMBSTONE_DIR);
    seed_unacknowledged_crawler_tombstones(
        capacity_tmp.path(),
        "018f3e2a-7b1c-7d45-8c91-",
        0..CrawlerRunStore::MAX_UNACKNOWLEDGED - 1,
    );
    let admission_at_cap = capacity_store
        .admit_publication(
            crawler_completion(run_id, digest()),
            Instant::now() + Duration::from_secs(60),
        )
        .expect("updating one of the capped retained runs adds no durable record");
    drop(admission_at_cap);
    let overflow_run_id = "018f3e2a-7b1c-7d45-8c91-999999999999";
    let overflow = PublicationTombstone::started(overflow_run_id, digest(), 100).unwrap();
    std::fs::write(
        capacity_root.join(format!("{overflow_run_id}.json")),
        serde_json::to_vec_pretty(&overflow).unwrap(),
    )
    .unwrap();
    assert!(capacity_store
        .admit_publication(
            crawler_completion(run_id, digest()),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap_err()
        .to_string()
        .contains("capacity"));
}

#[test]
fn crawler_publication_exclusion_blocks_competing_run_transition_until_release() {
    let tmp = TempDir::new().unwrap();
    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    store.start(run_id, digest(), 100).unwrap();
    let admission = store
        .admit_publication(
            crawler_completion(run_id, digest()),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();

    let contender = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(tmp.path().join(CRAWLER_TOMBSTONE_DIR).join(CRAWLER_TOMBSTONE_LOCK_FILE))
        .unwrap();
    assert!(matches!(
        contender.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    drop(admission);
    contender.try_lock().unwrap();
}

#[test]
fn canceled_crawler_binding_fails_before_any_target_or_journal_effect() {
    let tmp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    std::fs::create_dir_all(tmp.path().join("products")).unwrap();
    std::fs::write(tmp.path().join("products/old.txt"), "old").unwrap();
    let publication = PreStagedPublication::prepare(tmp.path(), target).unwrap();
    let transaction_namespace = publication.paths().staging.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&publication.paths().staging).unwrap();
    std::fs::write(publication.paths().staging.join("new.txt"), "new").unwrap();

    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    store.start(run_id, digest(), 100).unwrap();
    store.request_cancel(run_id, 101).unwrap();
    let error = publication
        .with_crawler_completion(
            &store,
            crawler_completion(run_id, digest()),
            Instant::now() + Duration::from_secs(60),
        )
        .err()
        .expect("canceled run must reject publication binding");

    assert!(error.to_string().contains("canceled"));
    assert_eq!(std::fs::read(tmp.path().join("products/old.txt")).unwrap(), b"old");
    assert!(!tmp.path().join("products/new.txt").exists());
    assert!(!transaction_namespace.exists());
    assert!(
        std::fs::read_dir(tmp.path().join(PUBLICATION_DIR).join("products"))
            .unwrap()
            .next()
            .is_none(),
        "rejected binding may leave only the empty target namespace"
    );
}

#[test]
fn admitted_crawler_success_commits_once_and_replays_from_durable_truth() {
    let tmp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    std::fs::create_dir_all(tmp.path().join("products")).unwrap();
    std::fs::write(tmp.path().join("products/old.txt"), "old").unwrap();
    let publication = PreStagedPublication::prepare(tmp.path(), target).unwrap();
    std::fs::create_dir_all(&publication.paths().staging).unwrap();
    std::fs::write(publication.paths().staging.join("new.txt"), "new").unwrap();

    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    store.start(run_id, digest(), 100).unwrap();
    let committed = publication
        .with_crawler_completion(
            &store,
            crawler_completion(run_id, digest()),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap()
        .activate()
        .unwrap();

    assert_eq!(std::fs::read(tmp.path().join("products/new.txt")).unwrap(), b"new");
    assert_eq!(
        store
            .load(run_id)
            .unwrap()
            .unwrap()
            .crawler_run
            .unwrap()
            .terminal,
        committed.crawler_terminal
    );
    assert!(matches!(
        store.start(run_id, digest(), 120).unwrap(),
        CrawlerRunStartDisposition::Replay(_)
    ));
}

#[test]
fn crawler_repair_after_promote_crash_preserves_atomic_success_truth() {
    let tmp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    write_authentic_fixture_tree(&tmp.path().join("products"), FixtureTreeKind::Old);
    let publication = PreStagedPublication::prepare(tmp.path(), target.clone()).unwrap();
    write_authentic_fixture_tree(&publication.paths().staging, FixtureTreeKind::New);

    let store = CrawlerRunStore::new(tmp.path());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
    store.start(run_id, digest(), 100).unwrap();
    let publication = publication
        .with_crawler_completion(
            &store,
            crawler_completion(run_id, digest()),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();

    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        publication
            .activate_with_faults_for_test(&PanicAtCheckpoint::new(
                PublicationFaultPoint::AfterStagingPromote,
            ))
            .unwrap();
    }));
    assert!(crash.is_err(), "the fault hook must model process loss");

    let report = scan_and_repair_publication_target(
        tmp.path(),
        &AnalyticsConfig::for_data_dir(tmp.path()),
        target,
    )
    .unwrap();
    assert_eq!(
        report.action,
        PublicationScanAction::Repaired(RepairDecision::Complete)
    );
    let recovered = store.load(run_id).unwrap().expect("durable run truth");
    assert_eq!(
        recovered
            .crawler_run
            .and_then(|run| run.terminal)
            .map(|terminal| terminal.outcome),
        Some(CrawlerTerminalOutcome::Succeeded),
        "repair must commit the crawler success fact with the promoted tree"
    );
}

#[derive(Clone, Copy)]
enum PreEffectCrawlerInterruption {
    Cancel,
    Terminal,
    DeadlineProofLost,
}

#[test]
fn crawler_pre_effect_crash_repair_never_starts_publication_without_live_admission() {
    let cases = [
        (
            PublicationFaultPoint::AfterPrepareJournal,
            PreEffectCrawlerInterruption::Cancel,
        ),
        (
            PublicationFaultPoint::AfterTargetBackup,
            PreEffectCrawlerInterruption::Terminal,
        ),
        (
            PublicationFaultPoint::AfterPrepareJournal,
            PreEffectCrawlerInterruption::DeadlineProofLost,
        ),
    ];

    for (fault, interruption) in cases {
        let tmp = TempDir::new().unwrap();
        let target = PublicationTarget::new("products").unwrap();
        write_authentic_fixture_tree(&tmp.path().join("products"), FixtureTreeKind::Old);
        let old_tree = collect_fixture_tree_bytes(&tmp.path().join("products"));
        let publication = PreStagedPublication::prepare(tmp.path(), target.clone()).unwrap();
        let paths = publication.paths().clone();
        write_authentic_fixture_tree(&paths.staging, FixtureTreeKind::New);

        let store = CrawlerRunStore::new(tmp.path());
        let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
        store.start(run_id, digest(), 100).unwrap();
        let publication = publication
            .with_crawler_completion(
                &store,
                crawler_completion(run_id, digest()),
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap();

        let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            publication
                .activate_with_faults_for_test(&PanicAtCheckpoint::new(fault))
                .unwrap();
        }));
        assert!(crash.is_err(), "{fault:?} must model process loss");

        match interruption {
            PreEffectCrawlerInterruption::Cancel => {
                store.request_cancel(run_id, 101).unwrap();
            }
            PreEffectCrawlerInterruption::Terminal => {
                store
                    .finish_without_publication(
                        run_id,
                        CrawlerTerminalOutcome::Failed {
                            error_code: CrawlerRunErrorCodeEvidence::WorkerLost,
                        },
                        CrawlerRunCountersEvidence::default(),
                        1,
                        101,
                    )
                    .unwrap();
            }
            PreEffectCrawlerInterruption::DeadlineProofLost => {
                // A monotonic Instant cannot survive process loss. With no live
                // admission guard, repair has no proof that the deadline remains open.
            }
        }

        let report = scan_and_repair_publication_target(
            tmp.path(),
            &AnalyticsConfig::for_data_dir(tmp.path()),
            target,
        )
        .unwrap();
        assert_eq!(
            report.action,
            PublicationScanAction::Repaired(RepairDecision::Rollback),
            "{fault:?} must not initiate crawler target effects after restart"
        );
        assert_eq!(collect_fixture_tree_bytes(&paths.target), old_tree);
        let rolled_back = PublicationJournal::from_recovery_json(
            &std::fs::read_to_string(&paths.journal).unwrap(),
        )
        .unwrap();
        assert_eq!(rolled_back.phase, PublicationPhase::RolledBack);
        assert!(rolled_back.crawler_completion.is_none());
        assert!(rolled_back.crawler_terminal.is_none());
    }
}

/// TODO: Document tenant_inventory_classifies_only_owner_known_artifacts.
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
fn version_store_sidecar_is_classified_by_publication_policy() {
    let inventory = TantivyManagedInventory::new(Vec::new()).unwrap();
    let database = crate::index::version_store::VersionStore::database_path(Path::new(""));
    let wal = database.with_file_name(format!(
        "{}-wal",
        database.file_name().unwrap().to_string_lossy()
    ));

    for relative in [database, wal] {
        assert_eq!(
            classify_tenant_relative_path(&relative, &inventory).unwrap(),
            ArtifactDisposition::Preserve,
            "{} must travel with its tenant generation",
            relative.display()
        );
    }
}

/// TODO: Document external_inventory_fails_closed_and_excludes_global_experiments.
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

/// TODO: Document canonical_tree_digest_is_stable_and_rejects_unknown_artifacts.
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

/// TODO: Document canonical_tree_digest_changes_for_path_type_and_content.
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

#[test]
fn canonical_tree_digest_ignores_atomic_write_temporary_files() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("index_meta.json"), b"published").unwrap();
    let inventory = TantivyManagedInventory::new([]).unwrap();
    let published_digest = canonical_tenant_tree_digest(&root, &inventory).unwrap();

    std::fs::write(root.join(".tmp.index_meta.json.123.tmp"), b"in flight").unwrap();
    std::fs::write(root.join(".index_meta.json.tmp"), b"legacy metadata temp").unwrap();
    std::fs::write(root.join(".committed_seq.42.99.tmp"), b"legacy watermark temp").unwrap();

    assert_eq!(
        canonical_tenant_tree_digest(&root, &inventory).unwrap(),
        published_digest,
        "publication digests must exclude both current and legacy atomic-write replacement files"
    );
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

/// TODO: Document activation_rejects_unknown_artifacts_before_mutation.
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

/// TODO: Document replacement_activation_rolls_back_losslessly_after_promote_failure.
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

/// TODO: Document failed_create_removes_target_staging_backup_and_records_rollback.
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

/// TODO: Document pre_staged_activation_reports_the_failed_filesystem_phase.
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

/// TODO: Document ambiguous_failed_create_quarantines_digest_bearing_journal.
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

/// TODO: Document successful_activation_promotes_journaled_sidecar_and_records_digests.
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

/// TODO: Document activation_rejects_symlinked_manifest_components_before_mutation.
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

/// TODO: Document manifest_builds_query_suggestions_and_analytics_entries_from_owner_resolvers.
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

/// TODO: Document manifest_rejects_overlapping_artifact_ownership.
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

/// TODO: Document replacement_rollback_removes_sidecar_backup_residue.
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

/// TODO: Document activation_fault_hook_covers_durability_boundaries_without_success.
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

/// TODO: Document replacement_faults_after_promotion_restore_the_exact_old_publication.
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

/// TODO: Document post_commit_faults_preserve_the_committed_publication.
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

const MUTATION_FENCE_WATERMARK: u64 = 7;

fn mutation_fence_strict_watermark() -> PublicationFenceEvidence {
    PublicationFenceEvidence::new(
        PublicationEpoch(2),
        PublicationEpoch(3),
        PublicationStagingBaseline(MUTATION_FENCE_WATERMARK),
        PublicationWatermark(MUTATION_FENCE_WATERMARK),
    )
    .unwrap()
}

fn staged_committed_seq_path(fixture: &ActivationFixture) -> PathBuf {
    fixture
        .paths
        .staging
        .join(crate::index::oplog::COMMITTED_SEQ_FILE)
}

fn write_staged_committed_seq(fixture: &ActivationFixture, bytes: &[u8]) {
    std::fs::write(staged_committed_seq_path(fixture), bytes).unwrap();
}

async fn expect_mutation_fence_strict_watermark_refusal(
    label: &str,
    configure_sidecar: impl FnOnce(&ActivationFixture),
) {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    write_fixture_control_index(fixture.base()).await;
    let old_target = PublicationLayoutSnapshot::capture(&fixture).target;
    configure_sidecar(&fixture);

    let error = activate_publication_with_fence(
        fixture.activation_inputs(PublicationArtifactManifest::default()),
        Some(mutation_fence_strict_watermark()),
    )
    .expect_err(label)
    .to_string();

    assert!(
        error.contains("strict committed_seq watermark proof"),
        "{label} returned the wrong refusal: {error}"
    );
    assert_eq!(
        fixture.read_target_file("index_meta.json"),
        b"old-meta",
        "{label} must abort before target mutation"
    );
    assert_eq!(PublicationLayoutSnapshot::capture(&fixture).target, old_target);
    assert!(!fixture.paths.journal.exists(), "{label}");
    assert!(!fixture.journal_temp_path().exists(), "{label}");
    assert!(!fixture.paths.backup.exists(), "{label}");

    let report = scan_and_repair_publication_target(
        fixture.base(),
        &AnalyticsConfig::for_data_dir(fixture.base()),
        fixture.target.clone(),
    )
    .unwrap();
    assert_eq!(report.status, PublicationRepairStatus::Clean, "{label}");
    assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
    assert_fixture_generation_reopenable(fixture.base(), fixture.target.as_str(), FixtureTreeKind::Old);
    assert_fixture_control_reopenable(fixture.base());
}

#[test]
fn mutation_fence_read_committed_seq_default_is_not_strict_watermark_proof() {
    let tmp = TempDir::new().unwrap();
    let zero = tmp.path().join("zero");
    let missing = tmp.path().join("missing");
    let malformed = tmp.path().join("malformed");
    let non_regular = tmp.path().join("non_regular");
    std::fs::create_dir_all(&zero).unwrap();
    std::fs::create_dir_all(&missing).unwrap();
    std::fs::create_dir_all(&malformed).unwrap();
    std::fs::create_dir_all(&non_regular).unwrap();
    std::fs::write(zero.join(crate::index::oplog::COMMITTED_SEQ_FILE), b"0").unwrap();
    std::fs::write(
        malformed.join(crate::index::oplog::COMMITTED_SEQ_FILE),
        b"not-a-sequence",
    )
    .unwrap();
    std::fs::create_dir(non_regular.join(crate::index::oplog::COMMITTED_SEQ_FILE)).unwrap();

    assert_eq!(crate::index::oplog::read_committed_seq(&zero), 0);
    assert_eq!(crate::index::oplog::read_committed_seq(&missing), 0);
    assert_eq!(crate::index::oplog::read_committed_seq(&malformed), 0);
    assert_eq!(crate::index::oplog::read_committed_seq(&non_regular), 0);
}

#[test]
fn mutation_fence_exact_staged_committed_seq_w_remains_eligible() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    write_staged_committed_seq(&fixture, MUTATION_FENCE_WATERMARK.to_string().as_bytes());

    let committed = activate_publication_with_fence(
        fixture.activation_inputs(PublicationArtifactManifest::default()),
        Some(mutation_fence_strict_watermark()),
    )
    .expect("exact staged committed_seq = W must remain eligible");

    assert_eq!(committed.phase, PublicationPhase::Committed);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"new-meta");
}

#[tokio::test]
async fn mutation_fence_missing_committed_seq_must_not_fail_open_as_w() {
    expect_mutation_fence_strict_watermark_refusal(
        "missing committed_seq must not prove strict W",
        |fixture| {
            std::fs::remove_file(staged_committed_seq_path(fixture)).unwrap();
            assert_eq!(
                crate::index::oplog::read_committed_seq(&fixture.paths.staging),
                0,
                "read_committed_seq fail-open default is not strict W proof"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn mutation_fence_non_regular_committed_seq_must_not_fail_open_as_w() {
    expect_mutation_fence_strict_watermark_refusal(
        "non-regular committed_seq must not prove strict W",
        |fixture| {
            std::fs::remove_file(staged_committed_seq_path(fixture)).unwrap();
            std::fs::create_dir(staged_committed_seq_path(fixture)).unwrap();
            assert_eq!(
                crate::index::oplog::read_committed_seq(&fixture.paths.staging),
                0,
                "read_committed_seq unreadable/non-regular default is not strict W proof"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn mutation_fence_malformed_committed_seq_must_not_fail_open_as_w() {
    expect_mutation_fence_strict_watermark_refusal(
        "malformed committed_seq must not prove strict W",
        |fixture| {
            write_staged_committed_seq(fixture, b"not-a-sequence");
            assert_eq!(
                crate::index::oplog::read_committed_seq(&fixture.paths.staging),
                0,
                "read_committed_seq malformed default is not strict W proof"
            );
        },
    )
    .await;
}

#[tokio::test]
async fn mutation_fence_numeric_committed_seq_mismatch_must_not_count_as_w() {
    expect_mutation_fence_strict_watermark_refusal(
        "numeric committed_seq mismatch must not prove strict W",
        |fixture| {
            write_staged_committed_seq(fixture, b"6");
            assert_eq!(
                crate::index::oplog::read_committed_seq(&fixture.paths.staging),
                6
            );
        },
    )
    .await;
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

#[tokio::test]
async fn startup_repair_after_epoch_advance_before_journal_reopens_old_tree() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    write_fixture_control_index(fixture.base()).await;
    // A valid fence carries the drained watermark; the staged tree must therefore
    // present a strict `committed_seq = W` before activation proceeds past its proof.
    write_staged_committed_seq(&fixture, b"7");
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
    assert_eq!(read_strict_committed_seq(&fixture.paths.target).unwrap(), 0);
    assert_fixture_generation_reopenable(fixture.base(), fixture.target.as_str(), FixtureTreeKind::Old);
    assert_fixture_control_reopenable(fixture.base());
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
        // A valid fence requires strict staged `committed_seq = W` before activation.
        write_staged_committed_seq(&fixture, b"7");
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

fn crash_fenced_activation_at_checkpoint(
    fixture: &ActivationFixture,
    fault: PublicationFaultPoint,
    watermark: u64,
) -> PublicationFenceEvidence {
    let guard = compare_and_advance_publication_epoch(
        fixture.base(),
        &fixture.target,
        PublicationEpoch(0),
    )
    .unwrap();
    let fence = PublicationFenceEvidence::new(
        guard.previous(),
        guard.advanced(),
        PublicationStagingBaseline(watermark),
        PublicationWatermark(watermark),
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
    fence
}

#[tokio::test]
async fn startup_repair_converges_across_fenced_activation_crash_boundaries() {
    let cases = [
        PublicationFaultPoint::AfterPrepareJournal,
        PublicationFaultPoint::AfterTargetBackup,
        PublicationFaultPoint::AfterStagingPromote,
    ];

    for fault in cases {
        let fixture = ActivationFixture::new();
        fixture.write_old_target();
        fixture.write_new_staging();
        write_fixture_control_index(fixture.base()).await;
        // A valid fence requires strict staged `committed_seq = W`; stage it before
        // capturing the expected promoted bytes so target and staging stay identical.
        write_staged_committed_seq(&fixture, b"0");
        let old_bytes = collect_fixture_tree_bytes(&fixture.paths.target);
        let new_bytes = collect_fixture_tree_bytes(&fixture.paths.staging);
        let old_digest = fixture.target_digest();
        let new_digest = fixture.new_digest();
        let fence = crash_fenced_activation_at_checkpoint(&fixture, fault, 0);
        let prepared = fixture.read_journal();
        assert_eq!(prepared.phase, PublicationPhase::Prepared);
        assert_eq!(prepared.fence_evidence, Some(fence.clone()));
        assert_eq!(prepared.prior_digest, Some(old_digest));
        assert_eq!(prepared.digest, new_digest);
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
        assert_eq!(read_strict_committed_seq(&fixture.paths.target).unwrap(), 0);

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
        assert_fixture_generation_reopenable(
            fixture.base(),
            fixture.target.as_str(),
            FixtureTreeKind::New,
        );
        assert_fixture_control_reopenable(fixture.base());
    }
}

#[tokio::test]
async fn startup_repair_rejects_digest_valid_fenced_watermark_mismatch() {
    for fault in [
        PublicationFaultPoint::AfterPrepareJournal,
        PublicationFaultPoint::AfterTargetBackup,
        PublicationFaultPoint::AfterStagingPromote,
    ] {
        let fixture = ActivationFixture::new();
        fixture.write_old_target();
        fixture.write_new_staging();
        write_fixture_control_index(fixture.base()).await;
        write_staged_committed_seq(&fixture, b"6");
        crash_fenced_activation_at_checkpoint(&fixture, fault, 6);

        let mut journal = fixture.read_journal();
        journal.fence_evidence = Some(
            PublicationFenceEvidence::new(
                PublicationEpoch(0),
                PublicationEpoch(1),
                PublicationStagingBaseline(6),
                PublicationWatermark(7),
            )
            .unwrap(),
        );
        fixture.write_journal(journal);
        let persisted = fixture.read_journal();
        let digest_matching_new_tree = match fault {
            PublicationFaultPoint::AfterStagingPromote => &fixture.paths.target,
            _ => &fixture.paths.staging,
        };
        assert_eq!(
            canonical_tenant_tree_digest(digest_matching_new_tree, &fixture.inventory).unwrap(),
            persisted.digest,
            "{fault:?} must isolate strict-watermark evidence from digest evidence"
        );
        assert_eq!(
            read_strict_committed_seq(digest_matching_new_tree).unwrap(),
            6
        );
        assert_eq!(
            persisted.fence_evidence.as_ref().unwrap().watermark(),
            PublicationWatermark(7)
        );
        let before_scan = PublicationLayoutSnapshot::capture(&fixture);

        let report = scan_and_repair_publication_target(
            fixture.base(),
            &AnalyticsConfig::for_data_dir(fixture.base()),
            fixture.target.clone(),
        )
        .unwrap();

        assert_eq!(report.status, PublicationRepairStatus::Quarantined, "{fault:?}");
        assert_eq!(report.action, PublicationScanAction::Quarantined, "{fault:?}");
        assert_eq!(report.disposition, PublicationTargetDisposition::Unavailable);
        assert!(!report.live_target_mutated);
        assert_eq!(PublicationLayoutSnapshot::capture(&fixture), before_scan);
        assert_fixture_control_reopenable(fixture.base());
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
            // A valid fence requires strict staged `committed_seq = W`.
            write_staged_committed_seq(&fixture, b"7");
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

#[derive(Clone)]
struct PublicationCheckpointGate {
    state: std::sync::Arc<(std::sync::Mutex<(bool, bool)>, std::sync::Condvar)>,
}

impl PublicationCheckpointGate {
    fn new() -> Self {
        Self {
            state: std::sync::Arc::new((
                std::sync::Mutex::new((false, false)),
                std::sync::Condvar::new(),
            )),
        }
    }

    fn wait_until_reached(&self) {
        let (state, changed) = &*self.state;
        let mut state = state.lock().unwrap();
        while !state.0 {
            state = changed.wait(state).unwrap();
        }
    }

    fn release(&self) {
        let (state, changed) = &*self.state;
        let mut state = state.lock().unwrap();
        state.1 = true;
        changed.notify_all();
    }
}

impl PublicationFaultHook for PublicationCheckpointGate {
    fn before_operation(&self, operation: &PublicationOperation) -> Result<()> {
        if *operation != PublicationOperation::Checkpoint(PublicationCheckpoint::AfterPrepareJournal)
        {
            return Ok(());
        }
        let (state, changed) = &*self.state;
        let mut state = state.lock().unwrap();
        state.0 = true;
        changed.notify_all();
        while !state.1 {
            state = changed.wait(state).unwrap();
        }
        Ok(())
    }
}

#[test]
fn create_only_prepared_publication_is_invisible_to_concurrent_observers() {
    let fixture = ActivationFixture::new();
    let publication = PreStagedPublication::prepare(fixture.base(), fixture.target.clone()).unwrap();
    fixture.write_new_tree(&publication.paths().staging);
    let target = publication.paths().target.clone();
    let gate = PublicationCheckpointGate::new();
    let activation_gate = gate.clone();

    let activation = std::thread::spawn(move || {
        publication.activate_create_only_with_faults_for_test(&activation_gate)
    });

    gate.wait_until_reached();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _runtime_guard = runtime.enter();
    let competing_manager = IndexManager::new(fixture.base());
    let competing_create = competing_manager
        .create_tenant(fixture.target.as_str())
        .expect_err("the create-only publication fence must exclude ordinary tenant creation");
    assert!(
        matches!(
            competing_create,
            crate::error::FlapjackError::IndexPaused(ref tenant)
                if tenant == fixture.target.as_str()
        ),
        "the competing creator must receive the canonical retriable fence error: {competing_create:?}"
    );
    let target_was_hidden = !target.exists();
    gate.release();
    assert!(activation.join().unwrap().is_ok());
    assert!(
        target_was_hidden,
        "a durable prepared journal must not expose an empty destination"
    );
    assert!(target.exists(), "the populated staging tree must publish atomically");
}

#[test]
fn initial_tenant_creation_honors_an_external_publication_fence() {
    let fixture = ActivationFixture::new();
    let lock_path = publication_epoch_paths_for_target_path(&fixture.paths.target).lock;
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let external_fence = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    external_fence.lock().unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _runtime_guard = runtime.enter();
    let manager = IndexManager::new(fixture.base());
    let blocked = manager
        .create_tenant(fixture.target.as_str())
        .expect_err("an external publication fence must exclude initial tenant creation");
    assert!(
        matches!(
            blocked,
            crate::error::FlapjackError::IndexPaused(ref tenant)
                if tenant == fixture.target.as_str()
        ),
        "external exclusion must use the canonical retriable error: {blocked:?}"
    );
    assert!(
        !fixture.paths.target.exists(),
        "a cross-process fence must prevent any visible tenant tree"
    );

    external_fence.unlock().unwrap();
    manager.create_tenant(fixture.target.as_str()).unwrap();
    assert!(fixture.paths.target.exists());
}

#[test]
fn create_only_process_loss_after_prepare_leaves_no_visible_destination() {
    let fixture = ActivationFixture::new();
    let publication = PreStagedPublication::prepare(fixture.base(), fixture.target.clone()).unwrap();
    fixture.write_new_tree(&publication.paths().staging);
    let paths = publication.paths().clone();

    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = publication.activate_create_only_with_faults_for_test(&PanicAtCheckpoint::new(
            PublicationCheckpoint::AfterPrepareJournal,
        ));
    }));

    assert!(crash.is_err(), "the injected process-loss checkpoint must fire");
    assert!(paths.journal.exists(), "prepared evidence must be durable");
    assert!(paths.staging.exists(), "recovery must retain the staged tree evidence");
    assert!(
        !paths.target.exists(),
        "process loss before promotion must not leave a visible empty destination"
    );
}

#[test]
#[serial_test::serial(publication_epoch_open_lock_file_checkpoint_hook)]
fn startup_repair_fences_before_inspecting_a_create_only_target() {
    let fixture = ActivationFixture::new();
    let publication = PreStagedPublication::prepare(fixture.base(), fixture.target.clone()).unwrap();
    fixture.write_new_tree(&publication.paths().staging);
    let paths = publication.paths().clone();
    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = publication.activate_create_only_with_faults_for_test(&PanicAtCheckpoint::new(
            PublicationCheckpoint::AfterPrepareJournal,
        ));
    }));
    assert!(crash.is_err());

    let expected_lock = publication_epoch_paths_for_target_path(&paths.target).lock;
    let (repair_opened_tx, repair_opened_rx) = std::sync::mpsc::channel();
    let (release_repair_tx, release_repair_rx) = std::sync::mpsc::channel();
    let release_repair_rx = std::sync::Mutex::new(release_repair_rx);
    let opened_once = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _hook = set_publication_epoch_open_lock_file_checkpoint_hook_for_test({
        let opened_once = std::sync::Arc::clone(&opened_once);
        move |lock_path| {
            if lock_path == expected_lock
                && !opened_once.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                repair_opened_tx.send(()).unwrap();
                release_repair_rx.lock().unwrap().recv().unwrap();
            }
        }
    });

    let base = fixture.base().to_path_buf();
    let target = fixture.target.clone();
    let repair = std::thread::spawn(move || {
        scan_and_repair_publication_target(
            &base,
            &AnalyticsConfig::for_data_dir(&base),
            target,
        )
    });
    repair_opened_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("repair must acquire target exclusion before inspecting evidence");

    // Model a creator in another process that completed just before repair took
    // the cross-process lock. Repair must inspect this tree after acquisition and
    // quarantine the ambiguous transaction without deleting the creator's data.
    fixture.write_old_tree(&paths.target);
    release_repair_tx.send(()).unwrap();
    let report = repair.join().unwrap().unwrap();

    assert_eq!(report.action, PublicationScanAction::Quarantined);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"old-meta");
}

#[test]
fn create_only_process_loss_after_promote_repairs_committed_generation() {
    let fixture = ActivationFixture::new();
    let publication = PreStagedPublication::prepare(fixture.base(), fixture.target.clone()).unwrap();
    fixture.write_new_tree(&publication.paths().staging);
    let store = CrawlerRunStore::new(fixture.base());
    let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ac";
    store.start(run_id, digest(), 100).unwrap();
    let publication = publication
        .with_crawler_completion(
            &store,
            crawler_completion(run_id, digest()),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
    let paths = publication.paths().clone();

    let crash = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = publication.activate_create_only_with_faults_for_test(&PanicAtCheckpoint::new(
            PublicationCheckpoint::AfterStagingPromote,
        ));
    }));

    assert!(crash.is_err(), "the injected process-loss checkpoint must fire");
    assert!(paths.target.exists(), "the atomic staging rename must be visible");
    assert!(!paths.staging.exists(), "the staged tree must have been promoted");
    let read_journal = || {
        PublicationJournal::from_json(&std::fs::read_to_string(&paths.journal).unwrap()).unwrap()
    };
    assert_eq!(read_journal().phase, PublicationPhase::Prepared);

    let report = scan_and_repair_publication_target(
        fixture.base(),
        &AnalyticsConfig::for_data_dir(fixture.base()),
        fixture.target.clone(),
    )
    .unwrap();
    assert_eq!(
        report.action,
        PublicationScanAction::Repaired(RepairDecision::Complete)
    );
    assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
    assert_eq!(read_journal().phase, PublicationPhase::Committed);
    assert_eq!(fixture.read_target_file("index_meta.json"), b"new-meta");
    assert!(!paths.backup.exists(), "create-only never owns a prior backup");
    assert_eq!(
        store
            .load(run_id)
            .unwrap()
            .and_then(|state| state.crawler_run)
            .and_then(|run| run.terminal)
            .map(|terminal| terminal.outcome),
        Some(CrawlerTerminalOutcome::Succeeded),
        "repair must commit the crawler terminal fact with the published generation"
    );
}

/// TODO: Document activation_fault_hook_observes_every_durable_filesystem_boundary.
#[test]
fn activation_fault_hook_observes_every_durable_filesystem_boundary() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    // A valid fence requires strict staged `committed_seq = W` before activation.
    write_staged_committed_seq(&fixture, b"7");
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
    // A valid fence requires strict staged `committed_seq = W` before activation.
    write_staged_committed_seq(&replay, b"7");
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

/// TODO: Document every_activation_filesystem_fault_restores_the_old_publication.
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
    let prepare_durable_index = operations
        .iter()
        .position(|op| {
            *op == PublicationOperation::Checkpoint(PublicationCheckpoint::AfterPrepareJournal)
        })
        .expect("fault script must observe the prepare-durable boundary");
    assert!(prepare_durable_index < commit_index);

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
        assert_precommit_fault_reached_clean_disposition(
            &fixture,
            operation_index,
            operation_index >= prepare_durable_index,
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

/// TODO: Document every_precommit_create_fault_resolves_without_publication_residue.
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
    let prepare_durable_index = operations
        .iter()
        .position(|operation| {
            *operation
                == PublicationOperation::Checkpoint(PublicationCheckpoint::AfterPrepareJournal)
        })
        .expect("fault script must observe the prepare-durable boundary");
    assert!(prepare_durable_index < commit_index);

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
        assert_precommit_fault_reached_clean_disposition(
            &fixture,
            operation_index,
            operation_index >= prepare_durable_index,
        );
    }
}

/// Disposition half of invariant 6: a pre-commit publication fault must leave
/// the journal in a clean terminal disposition. A fault before the
/// `AfterPrepareJournal` checkpoint may leave no journal because prepare was not
/// yet durable. At or after that boundary, exactly one journal must survive and
/// its phase must be `RolledBack` or `Quarantined`. A missing durable journal,
/// one left `Prepared`, a false `Committed`, or two conflicting journals are all
/// invalid dispositions.
fn assert_precommit_fault_reached_clean_disposition(
    fixture: &ActivationFixture,
    operation_index: usize,
    prepare_was_durable: bool,
) {
    let durable = fixture
        .paths
        .journal
        .exists()
        .then(|| fixture.read_journal().phase);
    let quarantine_journal = fixture.paths.quarantine.join("journal.json");
    let quarantined = quarantine_journal.exists().then(|| {
        PublicationJournal::from_json(&std::fs::read_to_string(&quarantine_journal).unwrap())
            .unwrap()
            .phase
    });
    match (durable, quarantined) {
        (Some(phase), None) | (None, Some(phase)) => assert!(
            matches!(
                phase,
                PublicationPhase::RolledBack | PublicationPhase::Quarantined
            ),
            "operation {operation_index}: surviving publication journal must be RolledBack or \
             Quarantined, found {phase:?}"
        ),
        (None, None) if !prepare_was_durable => {}
        (None, None) => panic!(
            "operation {operation_index}: fault after prepare durability lost the publication journal"
        ),
        (Some(durable_phase), Some(quarantined_phase)) => panic!(
            "operation {operation_index}: pre-commit fault left two conflicting journals \
             (durable={durable_phase:?}, quarantined={quarantined_phase:?})"
        ),
    }
}

/// TODO: Document activation_fixture_old_new_and_control_indexes_reopen_through_index_manager.
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

/// TODO: Document activation_fixture_fresh_generations_have_identical_managed_inventories_and_digests.
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
    /// TODO: Document ActivationFixture.new.
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().to_path_buf();
        Self::from_base(base, Some(tmp))
    }

    fn new_at(base: PathBuf) -> Self {
        Self::from_base(base, None)
    }

    /// TODO: Document ActivationFixture.from_base.
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

    /// TODO: Document ActivationFixture.read_target_file.
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

/// TODO: Document deterministic_fixture_oplog.
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

/// TODO: Document copy_fixture_tree_inner.
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

/// TODO: Document collect_fixture_tree_bytes_inner.
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

/// TODO: Document write_fixture_control_index.
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

/// TODO: Document assert_reopenable_fixture_tree.
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

fn assert_fixture_generation_reopenable(
    base: &Path,
    tenant: &str,
    kind: FixtureTreeKind,
) {
    assert_reopenable_fixture_tree(
        base,
        tenant,
        kind.object_id(),
        serde_json::json!({
            "_id": kind.object_id(),
            "objectID": kind.object_id(),
            "title": kind.title(),
            "body": kind.body(),
            "generation": kind.generation()
        }),
        kind.generation(),
        &[kind.object_id()],
    );
}

fn assert_fixture_control_reopenable(base: &Path) {
    assert_reopenable_fixture_tree(
        base,
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

/// TODO: Document repair_decision_table_is_total_for_every_phase_and_artifact_evidence.
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
        RepairArtifactEvidence::StrictWatermarkMismatch,
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

/// TODO: Document repair_evidence_is_proven.
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

/// TODO: Document repair_decision_table_only_allows_digest_proven_mutations.
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
        RepairArtifactEvidence::StrictWatermarkMismatch,
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

/// TODO: Document repair_decision_table_never_promotes_temp_journal_to_authority.
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

/// TODO: Document repair_decision_table_quarantines_invalid_manifest_and_ambiguous_states.
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

/// TODO: Document repair_publication_completes_prepared_swap_with_digest_proof.
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

/// TODO: Document repair_publication_completes_journaled_sidecar_promotion.
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

/// TODO: Document repair_publication_quarantines_sidecar_digest_mismatch_before_mutation.
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

/// TODO: Document repair_publication_rolls_back_journaled_sidecar_from_backup.
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

/// TODO: Document repair_publication_rejects_symlinked_managed_parent_before_mutation.
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

/// TODO: Document failed_create_removes_newly_promoted_sidecar.
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

/// TODO: Document repair_publication_cleans_committed_backup_and_stale_journal_temp.
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

/// TODO: Document repair_publication_faults_converge_without_second_recovery_path.
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

/// TODO: Document every_repair_filesystem_fault_converges_from_live_old_target_and_new_staging.
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

/// TODO: Document every_sidecar_completion_repair_fault_converges.
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

/// TODO: Document prepared_sidecar_rollback_fixture.
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

/// TODO: Document every_sidecar_rollback_repair_fault_converges.
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

/// TODO: Document rolled_back_repair_is_idempotent_on_second_scan.
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

/// TODO: Document repair_publication_quarantines_unproven_evidence_without_touching_live_target.
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

/// TODO: Document repair_publication_rejects_serialized_path_mismatch.
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
