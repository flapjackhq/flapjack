//! Spool checkpoint and resume contracts for durable source exports.
use super::*;
use chrono::{TimeZone, Utc};
use tempfile::TempDir;

fn export_resume_store(tmp: &TempDir) -> SpoolStore {
    let limits = SpoolLimits {
        max_compressed_page_bytes: 1_024,
        max_decompressed_page_bytes: 1_024,
        max_items_per_resource: 20,
        max_bytes_per_job: 10_240,
        max_global_bytes: 20_480,
        minimum_free_bytes: 32,
        max_staged_artifacts: 4,
        max_staged_artifact_bytes: 4_096,
        retention_seconds: 60,
    };
    SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        100_000,
    )
    .unwrap()
}

fn export_resume_denominators() -> ResourceDenominators {
    ResourceDenominators {
        settings: 1,
        documents: 3,
        rules: 2,
        synonyms: 2,
        config: 0,
    }
}

#[test]
fn export_resume_atomic_pages_publish_payload_and_exact_membership_together() {
    let tmp = TempDir::new().unwrap();
    let store = export_resume_store(&tmp);
    let view = store
        .create_export(&hex_digest(b"stable-source"), export_resume_denominators())
        .unwrap();

    store
        .commit_document_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"document-1"},{"objectID":"document-2"}]"#,
            &["document-1", "document-2"],
        )
        .unwrap();
    store
        .commit_rule_page_with_ids(view.job_uuid, br#"[{"objectID":"rule-1"}]"#, &["rule-1"])
        .unwrap();
    store
        .commit_synonym_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"synonym-1"}]"#,
            &["synonym-1"],
        )
        .unwrap();

    assert_eq!(
        store.completed_document_ids(view.job_uuid).unwrap(),
        vec!["document-1".to_string(), "document-2".to_string()]
    );
    assert_eq!(
        store.completed_rule_ids(view.job_uuid).unwrap(),
        vec!["rule-1".to_string()]
    );
    assert_eq!(
        store.completed_synonym_ids(view.job_uuid).unwrap(),
        vec!["synonym-1".to_string()]
    );
    assert_eq!(store.visible_artifacts(view.job_uuid).unwrap().len(), 3);
    assert_eq!(
        store
            .public_view(&view.public_handle)
            .unwrap()
            .progress
            .completed,
        4
    );
}

#[test]
fn export_resume_completed_pages_are_noops_after_reopen_and_page_shift() {
    let tmp = TempDir::new().unwrap();
    let store = export_resume_store(&tmp);
    let view = store
        .create_export(&hex_digest(b"stable-source"), export_resume_denominators())
        .unwrap();
    store
        .commit_document_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"document-1"}]"#,
            &["document-1"],
        )
        .unwrap();

    let reopened = export_resume_store(&tmp);
    reopened.recover().unwrap();
    reopened
        .commit_document_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"document-1"}]"#,
            &["document-1"],
        )
        .unwrap();
    reopened
        .commit_document_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"document-3"},{"objectID":"document-2"}]"#,
            &["document-3", "document-2"],
        )
        .unwrap();

    assert_eq!(reopened.visible_artifacts(view.job_uuid).unwrap().len(), 2);
    assert_eq!(
        reopened.completed_document_ids(view.job_uuid).unwrap(),
        vec![
            "document-1".to_string(),
            "document-3".to_string(),
            "document-2".to_string(),
        ]
    );
    assert_eq!(
        reopened
            .public_view(&view.public_handle)
            .unwrap()
            .progress
            .completed,
        3
    );
}

#[test]
fn export_resume_recovery_rolls_back_payload_and_membership_before_manifest_commit() {
    for (resource, kind, committed_id, uncommitted_id) in [
        (
            ObjectResource::Documents,
            ArtifactKind::DocumentPage,
            "document-1",
            "document-2",
        ),
        (
            ObjectResource::Rules,
            ArtifactKind::RulesPage,
            "rule-1",
            "rule-2",
        ),
        (
            ObjectResource::Synonyms,
            ArtifactKind::SynonymsPage,
            "synonym-1",
            "synonym-2",
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let store = export_resume_store(&tmp);
        let view = store
            .create_export(&hex_digest(b"stable-source"), export_resume_denominators())
            .unwrap();
        commit_one(&store, view.job_uuid, resource, committed_id);
        let staged = store
            .pre_register_artifact_for_test(view.job_uuid, kind, "uncommitted payload")
            .unwrap();
        std::fs::write(
            store.resource_sidecar_path(view.job_uuid, resource),
            format!("{committed_id}\n{uncommitted_id}\n"),
        )
        .unwrap();
        std::fs::write(
            store.job_dir(view.job_uuid).join(&staged.final_path),
            b"uncommitted payload",
        )
        .unwrap();

        let reopened = export_resume_store(&tmp);
        reopened.recover().unwrap();

        assert_eq!(
            completed_ids(&reopened, view.job_uuid, resource),
            [committed_id]
        );
        assert_eq!(reopened.visible_artifacts(view.job_uuid).unwrap().len(), 1);
        assert!(!reopened
            .job_dir(view.job_uuid)
            .join(staged.final_path)
            .exists());
    }
}

#[test]
fn export_resume_singleton_settings_is_atomic_and_idempotent_after_reopen() {
    let tmp = TempDir::new().unwrap();
    let store = export_resume_store(&tmp);
    let source_digest = hex_digest(b"stable-source");
    let settings = br#"{"attributesForFaceting":["category"]}"#;
    let settings_hash = hex_digest(settings);
    let view = store
        .create_export(&source_digest, export_resume_denominators())
        .unwrap();

    store
        .commit_settings_once(view.job_uuid, settings, &settings_hash)
        .unwrap();
    let reopened = export_resume_store(&tmp);
    reopened.recover().unwrap();
    reopened
        .commit_settings_once(view.job_uuid, settings, &settings_hash)
        .unwrap();
    assert_eq!(
        reopened
            .commit_settings(view.job_uuid, b"legacy-bypass", 1)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::ResourceComplete
    );

    assert_eq!(reopened.visible_artifacts(view.job_uuid).unwrap().len(), 1);
    assert_eq!(
        reopened
            .checkpoint(&view.checkpoint_handle, &source_digest)
            .unwrap()
            .resources
            .settings,
        ResourceCompletion {
            complete: true,
            count: 1,
            hash: settings_hash,
        }
    );
}

#[test]
fn export_resume_checkpoint_requires_exact_source_identity_without_advancing_state() {
    let tmp = TempDir::new().unwrap();
    let store = export_resume_store(&tmp);
    let source_digest = hex_digest(b"stable-source");
    let view = store
        .create_export(&source_digest, export_resume_denominators())
        .unwrap();
    let before = store.manifest_json(view.job_uuid).unwrap();

    let error = store
        .checkpoint(&view.checkpoint_handle, &hex_digest(b"changed-source"))
        .unwrap_err();

    assert_eq!(error.kind(), SpoolErrorKind::SourceIdentityMismatch);
    assert_eq!(store.manifest_json(view.job_uuid).unwrap(), before);
}

#[test]
fn export_resume_empty_resources_complete_and_terminal_states_fence_writes() {
    let tmp = TempDir::new().unwrap();
    let store = export_resume_store(&tmp);
    let source_digest = hex_digest(b"empty-source");
    let empty_hash = hex_digest(b"empty-resource");
    let denominators = ResourceDenominators {
        settings: 1,
        documents: 0,
        rules: 0,
        synonyms: 0,
        config: 0,
    };
    let view = store.create_export(&source_digest, denominators).unwrap();
    let settings = br#"{"searchableAttributes":[]}"#;
    store
        .commit_settings_once(view.job_uuid, settings, &hex_digest(settings))
        .unwrap();
    store
        .complete_documents(view.job_uuid, 0, &empty_hash)
        .unwrap();
    store.complete_rules(view.job_uuid, 0, &empty_hash).unwrap();
    store
        .complete_synonyms(view.job_uuid, 0, &empty_hash)
        .unwrap();
    store.accept_export(view.job_uuid).unwrap();

    let accepted = store
        .checkpoint(&view.checkpoint_handle, &source_digest)
        .unwrap();
    assert_eq!(accepted.state, "Accepted");
    assert_eq!(accepted.progress.completed, 1);
    assert_eq!(accepted.progress.total, 1);
    assert_eq!(accepted.resources.documents.count, 0);
    assert!(accepted.resources.documents.complete);
    assert_eq!(
        store
            .commit_document_page_with_ids(view.job_uuid, br#"[{"objectID":"late"}]"#, &["late"],)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::JobTerminal
    );

    let failed = store
        .create_export(&hex_digest(b"failed-source"), export_resume_denominators())
        .unwrap();
    store.fail_export(failed.job_uuid).unwrap();
    assert_eq!(
        store
            .commit_settings_once(failed.job_uuid, b"{}", &hex_digest(b"{}"))
            .unwrap_err()
            .kind(),
        SpoolErrorKind::JobTerminal
    );
    assert_eq!(
        store.public_view(&failed.public_handle).unwrap().state,
        "Failed"
    );
}

#[test]
fn export_resume_resource_completion_rejects_unverified_counts_and_incomplete_acceptance() {
    let tmp = TempDir::new().unwrap();
    let store = export_resume_store(&tmp);
    let view = store
        .create_export(&hex_digest(b"stable-source"), export_resume_denominators())
        .unwrap();

    let count_error = store
        .complete_documents(view.job_uuid, 3, &hex_digest(b"documents"))
        .unwrap_err();
    assert_eq!(
        count_error.kind(),
        SpoolErrorKind::ResourceVerificationFailed
    );
    let accept_error = store.accept_export(view.job_uuid).unwrap_err();
    assert_eq!(accept_error.kind(), SpoolErrorKind::ResourcesIncomplete);
}

#[test]
fn export_resume_nonempty_resource_completion_persists_verified_counts_and_hashes() {
    let tmp = TempDir::new().unwrap();
    let store = export_resume_store(&tmp);
    let source_digest = hex_digest(b"stable-source");
    let view = store
        .create_export(&source_digest, export_resume_denominators())
        .unwrap();
    let settings = b"{}";
    store
        .commit_settings_once(view.job_uuid, settings, &hex_digest(settings))
        .unwrap();
    for (resource, ids) in [
        (ObjectResource::Documents, &["doc-1", "doc-2", "doc-3"][..]),
        (ObjectResource::Rules, &["rule-1", "rule-2"][..]),
        (ObjectResource::Synonyms, &["syn-1", "syn-2"][..]),
    ] {
        for id in ids {
            commit_one(&store, view.job_uuid, resource, id);
        }
    }
    let documents_hash = hex_digest(b"documents");
    let rules_hash = hex_digest(b"rules");
    let synonyms_hash = hex_digest(b"synonyms");
    store
        .complete_documents(view.job_uuid, 3, &documents_hash)
        .unwrap();
    assert_eq!(
        store
            .commit_document_page_with_ids(view.job_uuid, br#"[{"objectID":"doc-4"}]"#, &["doc-4"],)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::ResourceComplete
    );
    assert_eq!(
        store
            .commit_document_page(view.job_uuid, b"legacy-bypass", 1)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::ResourceComplete
    );
    store.complete_rules(view.job_uuid, 2, &rules_hash).unwrap();
    store
        .complete_synonyms(view.job_uuid, 2, &synonyms_hash)
        .unwrap();

    let reopened = export_resume_store(&tmp);
    reopened.recover().unwrap();
    reopened.accept_export(view.job_uuid).unwrap();
    let checkpoint = reopened
        .checkpoint(&view.checkpoint_handle, &source_digest)
        .unwrap();

    assert_eq!(checkpoint.state, "Accepted");
    assert_eq!(checkpoint.resources.documents.count, 3);
    assert_eq!(checkpoint.resources.documents.hash, documents_hash);
    assert_eq!(checkpoint.resources.rules.hash, rules_hash);
    assert_eq!(checkpoint.resources.synonyms.hash, synonyms_hash);
    assert_eq!(checkpoint.progress.completed, 8);
    assert_eq!(checkpoint.progress.total, 8);
}

fn commit_one(store: &SpoolStore, job_uuid: Uuid, resource: ObjectResource, id: &str) {
    let payload = format!(r#"[{{"objectID":"{id}"}}]"#);
    match resource {
        ObjectResource::Documents => {
            store.commit_document_page_with_ids(job_uuid, payload.as_bytes(), &[id])
        }
        ObjectResource::Rules => {
            store.commit_rule_page_with_ids(job_uuid, payload.as_bytes(), &[id])
        }
        ObjectResource::Synonyms => {
            store.commit_synonym_page_with_ids(job_uuid, payload.as_bytes(), &[id])
        }
    }
    .unwrap();
}

fn completed_ids(store: &SpoolStore, job_uuid: Uuid, resource: ObjectResource) -> Vec<String> {
    match resource {
        ObjectResource::Documents => store.completed_document_ids(job_uuid),
        ObjectResource::Rules => store.completed_rule_ids(job_uuid),
        ObjectResource::Synonyms => store.completed_synonym_ids(job_uuid),
    }
    .unwrap()
}
