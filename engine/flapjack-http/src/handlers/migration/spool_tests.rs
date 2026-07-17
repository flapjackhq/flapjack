use super::*;
use chrono::{TimeZone, Utc};
use flapjack::index::manager::IndexManager;
use serde_json::Value;
use std::io::Write;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;

fn test_limits() -> SpoolLimits {
    SpoolLimits {
        max_compressed_page_bytes: 64,
        max_decompressed_page_bytes: 128,
        max_items_per_resource: 10,
        max_bytes_per_job: 512,
        max_global_bytes: 1024,
        minimum_free_bytes: 32,
        max_staged_artifacts: 2,
        max_staged_artifact_bytes: 128,
        retention_seconds: 60,
    }
}

fn fixed_store(tmp: &TempDir) -> SpoolStore {
    SpoolStore::new_for_tests(
        tmp.path(),
        test_limits(),
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .expect("test store should initialize")
}

fn denominators() -> ResourceDenominators {
    ResourceDenominators {
        settings: 1,
        documents: 3,
        rules: 2,
        synonyms: 1,
        config: 1,
    }
}

fn source_digest() -> String {
    hex_digest(b"source-identity")
}

#[tokio::test]
async fn creates_jobs_under_migration_export_root_with_private_mode() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("products").unwrap();

    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .expect("job should be created");

    let expected = tmp
        .path()
        .join("migration_exports")
        .join("jobs")
        .join(view.job_uuid.to_string());
    assert_eq!(store.job_dir(view.job_uuid), expected);
    assert!(expected.is_dir());
    assert!(!tmp
        .path()
        .join("products")
        .join(view.job_uuid.to_string())
        .exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(expected).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}

#[test]
fn manifest_scrubs_source_data_and_public_progress_is_derived() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();
    store
        .commit_settings(view.job_uuid, br#"{"not":"secret payload"}"#, 1)
        .unwrap();
    store
        .commit_document_page(view.job_uuid, br#"[{"objectID":"obj-1"}]"#, 2)
        .unwrap();

    let manifest = store.manifest_json(view.job_uuid).unwrap();
    for forbidden in [
        "ALGOLIA_API_KEY",
        "APPID123",
        "products_source",
        "obj-1",
        "secret payload",
        "ranking",
        "synonym-body",
        "canary-config",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "manifest leaked forbidden value {forbidden}: {manifest}"
        );
    }

    let decoded: Value = serde_json::from_str(&manifest).unwrap();
    assert!(decoded.get("progress").is_none());
    assert_ne!(view.public_handle, view.checkpoint_handle);
    assert_ne!(view.public_handle, view.job_uuid.to_string());
    assert_ne!(view.checkpoint_handle, view.job_uuid.to_string());

    let status = store.public_view(&view.public_handle).unwrap();
    assert_eq!(status.progress.completed, 3);
    assert_eq!(status.progress.total, 8);
    assert!((status.progress.ratio - 0.375).abs() < f64::EPSILON);
}

#[test]
fn limits_reject_writes_without_advancing_manifest_or_exposing_artifacts() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_compressed_page_bytes = 8;
    let store = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();

    let err = store
        .commit_document_page(view.job_uuid, b"this page is too large", 1)
        .expect_err("compressed byte limit should reject write");
    assert_eq!(err.kind(), SpoolErrorKind::CompressedPageBytesExceeded);
    assert!(store.visible_artifacts(view.job_uuid).unwrap().is_empty());
    assert_eq!(
        store
            .public_view(&view.public_handle)
            .unwrap()
            .progress
            .completed,
        0
    );
}

#[test]
fn staged_artifacts_recover_without_unregistered_visible_files() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();
    let staged = store
        .pre_register_artifact_for_test(view.job_uuid, ArtifactKind::DocumentPage, "leaked bytes")
        .unwrap();
    std::fs::write(
        store.job_dir(view.job_uuid).join(&staged.final_path),
        b"leaked bytes",
    )
    .unwrap();

    let reopened = fixed_store(&tmp);
    reopened.recover().unwrap();

    assert!(reopened
        .visible_artifacts(view.job_uuid)
        .unwrap()
        .is_empty());
    assert!(!reopened
        .job_dir(view.job_uuid)
        .join(&staged.final_path)
        .exists());
}

#[test]
fn completed_object_sidecar_uses_only_committed_prefix_after_reopen() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();
    store
        .mark_completed_object_ids(view.job_uuid, &["obj-1", "obj-2"])
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(store.completed_sidecar_path(view.job_uuid))
        .unwrap()
        .write_all(b"obj-3\n")
        .unwrap();

    let reopened = fixed_store(&tmp);
    reopened.recover().unwrap();

    assert_eq!(
        reopened.completed_object_ids(view.job_uuid).unwrap(),
        vec!["obj-1".to_string(), "obj-2".to_string()]
    );
    assert!(reopened
        .is_object_completed(view.job_uuid, "obj-1")
        .unwrap());
    assert!(!reopened
        .is_object_completed(view.job_uuid, "obj-3")
        .unwrap());
}

#[test]
fn deletion_fence_is_digest_checked_and_blocks_future_commits() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();
    store
        .commit_document_page(view.job_uuid, b"page", 1)
        .unwrap();

    assert!(!store
        .delete_export_artifacts(view.job_uuid, "wrong-digest")
        .unwrap());
    assert_eq!(store.visible_artifacts(view.job_uuid).unwrap().len(), 1);

    assert!(store
        .delete_export_artifacts(view.job_uuid, &source_digest())
        .unwrap());
    assert!(store.visible_artifacts(view.job_uuid).unwrap().is_empty());
    assert_eq!(
        store
            .commit_document_page(view.job_uuid, b"late", 1)
            .expect_err("fenced job should reject writes")
            .kind(),
        SpoolErrorKind::JobDeleting
    );

    let second_handle = fixed_store(&tmp);
    assert_eq!(
        second_handle
            .mark_completed_object_ids(view.job_uuid, &["obj-9"])
            .expect_err("fence must be durable across handles")
            .kind(),
        SpoolErrorKind::JobDeleting
    );
}

#[test]
fn garbage_collection_keeps_tombstone_and_does_not_scan_unregistered_paths() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();
    let unrelated = tmp
        .path()
        .join("migration_exports")
        .join("unregistered-secret");
    std::fs::write(&unrelated, b"do not touch").unwrap();
    let outside = tmp.path().join("outside-secret");
    std::fs::write(&outside, b"do not touch").unwrap();

    store
        .delete_export_artifacts(view.job_uuid, &source_digest())
        .unwrap();
    let later = SpoolStore::new_for_tests(
        tmp.path(),
        test_limits(),
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 2, 0).unwrap(),
        10_000,
    )
    .unwrap();

    later.collect_garbage().unwrap();

    assert!(later
        .tombstone_json(view.job_uuid)
        .unwrap()
        .contains("deleted"));
    assert!(unrelated.exists());
    assert!(outside.exists());
}

#[test]
fn recovery_handles_each_artifact_transaction_boundary() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_staged_artifacts = 8;
    let store = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();

    let only_manifest = store
        .pre_register_artifact_for_test(view.job_uuid, ArtifactKind::RulesPage, "rules-secret")
        .unwrap();
    let with_temp = store
        .pre_register_artifact_for_test(
            view.job_uuid,
            ArtifactKind::SynonymsPage,
            "synonyms-secret",
        )
        .unwrap();
    std::fs::write(
        store.job_dir(view.job_uuid).join(&with_temp.temp_path),
        b"synonyms-secret",
    )
    .unwrap();
    let with_final = store
        .pre_register_artifact_for_test(view.job_uuid, ArtifactKind::Config, "config-secret")
        .unwrap();
    std::fs::write(
        store.job_dir(view.job_uuid).join(&with_final.final_path),
        b"config-secret",
    )
    .unwrap();
    store
        .commit_settings(view.job_uuid, b"registered-visible", 1)
        .unwrap();
    let registered_visible = store.visible_artifacts(view.job_uuid).unwrap()[0].clone();

    let reopened = fixed_store(&tmp);
    reopened.recover().unwrap();

    assert_eq!(
        reopened.visible_artifacts(view.job_uuid).unwrap(),
        vec![registered_visible]
    );
    for path in [
        only_manifest.temp_path,
        only_manifest.final_path,
        with_temp.temp_path,
        with_temp.final_path,
        with_final.temp_path,
        with_final.final_path,
    ] {
        assert!(
            !reopened.job_dir(view.job_uuid).join(path).exists(),
            "staged transaction residue should be removed"
        );
    }
}

#[test]
fn typed_artifact_methods_account_exact_limits_and_leave_no_partial_state() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_compressed_page_bytes = 6;
    limits.max_decompressed_page_bytes = 9;
    limits.max_items_per_resource = 2;
    limits.max_bytes_per_job = 12;
    limits.max_global_bytes = 18;
    limits.minimum_free_bytes = 4;
    let store = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10,
    )
    .unwrap();
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();

    store.commit_rules_page(view.job_uuid, b"rules", 1).unwrap();
    assert_eq!(
        store
            .commit_synonyms_page(view.job_uuid, b"synonyms", 1)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::CompressedPageBytesExceeded
    );
    assert_eq!(
        store
            .commit_config_file(view.job_uuid, b"123456", b"1234567890", 1)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::DecompressedPageBytesExceeded
    );
    assert_eq!(
        store
            .commit_rules_page(view.job_uuid, b"abc", 2)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::ResourceItemCountExceeded
    );
    assert_eq!(
        store
            .commit_settings(view.job_uuid, b"123456", 1)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::FreeSpaceFloor
    );

    assert_eq!(store.visible_artifacts(view.job_uuid).unwrap().len(), 1);
    let status = store.public_view(&view.public_handle).unwrap();
    assert_eq!(status.progress.completed, 1);
    assert_eq!(status.progress.total, 8);
}

#[test]
fn global_byte_limit_is_derived_across_jobs_under_root_lock() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_global_bytes = 7;
    let store = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    let first = store
        .create_export(&hex_digest(b"source-a"), denominators())
        .unwrap();
    let second = store
        .create_export(&hex_digest(b"source-b"), denominators())
        .unwrap();

    store
        .commit_document_page(first.job_uuid, b"1234", 1)
        .unwrap();
    assert_eq!(
        store
            .commit_document_page(second.job_uuid, b"1234", 1)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::GlobalBytesExceeded
    );
    store.commit_settings(second.job_uuid, b"123", 1).unwrap();

    assert_eq!(
        store
            .public_view(&first.public_handle)
            .unwrap()
            .progress
            .completed,
        1
    );
    assert_eq!(
        store
            .public_view(&second.public_handle)
            .unwrap()
            .progress
            .completed,
        1
    );
}

#[test]
fn completed_object_sidecar_commits_exact_membership_and_truncates_tail() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();

    store
        .mark_completed_object_ids(view.job_uuid, &["obj-1", "obj-2"])
        .unwrap();
    store
        .mark_completed_object_ids(view.job_uuid, &["obj-2", "obj-3"])
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(store.completed_sidecar_path(view.job_uuid))
        .unwrap()
        .write_all(b"obj-4\n")
        .unwrap();

    let reopened = fixed_store(&tmp);
    reopened.recover().unwrap();

    assert_eq!(
        reopened.completed_object_ids(view.job_uuid).unwrap(),
        vec![
            "obj-1".to_string(),
            "obj-2".to_string(),
            "obj-3".to_string()
        ]
    );
    assert!(reopened
        .is_object_completed(view.job_uuid, "obj-3")
        .unwrap());
    assert!(!reopened
        .is_object_completed(view.job_uuid, "obj-4")
        .unwrap());
}

#[test]
fn completed_object_sidecar_ignores_uncommitted_tail_before_next_commit() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();

    store
        .mark_completed_object_ids(view.job_uuid, &["obj-1", "obj-2"])
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(store.completed_sidecar_path(view.job_uuid))
        .unwrap()
        .write_all(b"obj-tail\n")
        .unwrap();
    store
        .mark_completed_object_ids(view.job_uuid, &["obj-3"])
        .unwrap();

    assert_eq!(
        store.completed_object_ids(view.job_uuid).unwrap(),
        vec![
            "obj-1".to_string(),
            "obj-2".to_string(),
            "obj-3".to_string()
        ]
    );
    assert!(!store
        .is_object_completed(view.job_uuid, "obj-tail")
        .unwrap());
}

#[test]
fn completed_object_sidecar_corruption_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();

    store
        .mark_completed_object_ids(view.job_uuid, &["obj-1"])
        .unwrap();
    std::fs::write(store.completed_sidecar_path(view.job_uuid), b"tampered\n").unwrap();

    assert_eq!(
        store
            .completed_object_ids(view.job_uuid)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::ManifestCorrupt
    );
}

#[test]
fn deletion_removes_completed_object_sidecar_and_resets_public_progress() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();

    store
        .commit_document_page(view.job_uuid, b"page", 2)
        .unwrap();
    store
        .mark_completed_object_ids(view.job_uuid, &["obj-secret"])
        .unwrap();
    assert!(store.completed_sidecar_path(view.job_uuid).exists());

    store
        .delete_export_artifacts(view.job_uuid, &source_digest())
        .unwrap();

    assert!(!store.completed_sidecar_path(view.job_uuid).exists());
    assert_eq!(
        store.completed_object_ids(view.job_uuid).unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        store
            .public_view(&view.public_handle)
            .unwrap()
            .progress
            .completed,
        0
    );
}

#[test]
fn recovery_recalculates_progress_when_visible_artifact_is_missing() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();

    store
        .commit_document_page(view.job_uuid, b"page", 2)
        .unwrap();
    let artifact = store.visible_artifacts(view.job_uuid).unwrap()[0].clone();
    std::fs::remove_file(store.job_dir(view.job_uuid).join(artifact)).unwrap();

    let reopened = fixed_store(&tmp);
    reopened.recover().unwrap();

    assert!(reopened
        .visible_artifacts(view.job_uuid)
        .unwrap()
        .is_empty());
    assert_eq!(
        reopened
            .public_view(&view.public_handle)
            .unwrap()
            .progress
            .completed,
        0
    );
}

#[test]
fn same_job_two_handles_preserve_artifact_and_sidecar_progress() {
    let tmp = TempDir::new().unwrap();
    let first = fixed_store(&tmp);
    let view = first
        .create_export(&source_digest(), denominators())
        .unwrap();
    let second = fixed_store(&tmp);
    let barrier = Arc::new(Barrier::new(2));

    let first_barrier = Arc::clone(&barrier);
    let first_job = view.job_uuid;
    let first_thread = std::thread::spawn(move || {
        first_barrier.wait();
        first.commit_document_page(first_job, b"doc-page", 1)
    });

    let second_barrier = Arc::clone(&barrier);
    let second_job = view.job_uuid;
    let second_thread = std::thread::spawn(move || {
        second_barrier.wait();
        second.mark_completed_object_ids(second_job, &["obj-1"])
    });

    first_thread.join().unwrap().unwrap();
    second_thread.join().unwrap().unwrap();

    let reopened = fixed_store(&tmp);
    assert_eq!(
        reopened
            .public_view(&view.public_handle)
            .unwrap()
            .progress
            .completed,
        1
    );
    assert_eq!(
        reopened.completed_object_ids(view.job_uuid).unwrap(),
        vec!["obj-1".to_string()]
    );
}

#[test]
fn cross_job_two_handles_cannot_exceed_global_cap_or_leave_orphans() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_global_bytes = 4;
    let first = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    let first_job = first
        .create_export(&hex_digest(b"source-a"), denominators())
        .unwrap();
    let first_job_uuid = first_job.job_uuid;
    let second = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    let second_job = second
        .create_export(&hex_digest(b"source-b"), denominators())
        .unwrap();
    let second_job_uuid = second_job.job_uuid;
    let barrier = Arc::new(Barrier::new(2));

    let first_barrier = Arc::clone(&barrier);
    let first_thread = std::thread::spawn(move || {
        first_barrier.wait();
        first.commit_document_page(first_job_uuid, b"1234", 1)
    });

    let second_barrier = Arc::clone(&barrier);
    let second_thread = std::thread::spawn(move || {
        second_barrier.wait();
        second.commit_document_page(second_job_uuid, b"5678", 1)
    });

    let results = [first_thread.join().unwrap(), second_thread.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(SpoolError::kind)
            .collect::<Vec<_>>(),
        vec![SpoolErrorKind::GlobalBytesExceeded]
    );

    let reopened = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    reopened.recover().unwrap();
    let visible_count = reopened
        .visible_artifacts(first_job.job_uuid)
        .unwrap()
        .len()
        + reopened
            .visible_artifacts(second_job.job_uuid)
            .unwrap()
            .len();
    assert_eq!(visible_count, 1);
}

#[test]
fn public_outputs_and_errors_are_scrubbed_and_source_identity_must_be_digest() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let forbidden = [
        "ALGOLIA_API_KEY",
        "APPID123",
        "products_source",
        "obj-secret",
        "record-secret",
        "settings-secret",
        "rules-secret",
        "synonyms-secret",
        "config-secret",
    ];

    let raw_err = store
        .create_export("APPID123-products_source", denominators())
        .unwrap_err();
    assert_eq!(raw_err.kind(), SpoolErrorKind::InvalidSourceIdentityDigest);

    let view = store
        .create_export(&source_digest(), denominators())
        .unwrap();
    store
        .commit_settings(view.job_uuid, b"settings-secret", 1)
        .unwrap();
    store
        .mark_completed_object_ids(view.job_uuid, &["obj-secret"])
        .unwrap();
    store
        .delete_export_artifacts(view.job_uuid, &source_digest())
        .unwrap();
    let later = SpoolStore::new_for_tests(
        tmp.path(),
        test_limits(),
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 2, 0).unwrap(),
        10_000,
    )
    .unwrap();
    later.collect_garbage().unwrap();

    let status = format!("{:?}", later.public_view(&view.public_handle).unwrap());
    let tombstone = later.tombstone_json(view.job_uuid).unwrap();
    let display = raw_err.to_string();
    let debug = format!("{raw_err:?}");
    for rendered in [status, tombstone, display, debug] {
        for secret in forbidden {
            assert!(
                !rendered.contains(secret),
                "scrubbed output leaked {secret}: {rendered}"
            );
        }
    }
}
