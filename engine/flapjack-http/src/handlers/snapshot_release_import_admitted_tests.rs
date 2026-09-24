async fn assert_successful_release_import(
    snapshot_bytes: &[u8],
    through_sequence: &str,
    destination_kind: ReleaseImportDestination,
    tenant_id: &str,
) {
    let snapshot_digest = format!("{:x}", Sha256::digest(snapshot_bytes));
    let (_destination_tmp, destination, app) =
        release_import_destination_for(destination_kind, tenant_id).await;
    let writer_closes_before = retained_channel_closed_count(tenant_id);
    let response = app
        .oneshot(release_import_request_for_tenant(
            snapshot_bytes.to_vec(),
            &snapshot_digest,
            Some(through_sequence),
            tenant_id,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let response_headers = response.headers().clone();
    assert_eq!(
        body_json(response).await,
        serde_json::json!({"status": "imported"})
    );
    assert_release_import_content_for(&destination, tenant_id);
    assert_release_import_proof_for_tenant(
        &response_headers,
        &snapshot_digest,
        through_sequence,
        tenant_id,
    );
    assert!(has_lifecycle_phase(tenant_id, "snapshot_restore_entry"));
    assert!(has_lifecycle_phase(
        tenant_id,
        "snapshot_restore_publication"
    ));
    if matches!(destination_kind, ReleaseImportDestination::Existing) {
        assert_retained_channel_closed_delta(
            tenant_id,
            writer_closes_before,
            "successful release import must drain the existing destination writer",
        );
    }
}

#[tokio::test]
async fn release_import_accepts_the_full_unsigned_sequence_domain() {
    let tenant_id = "release-import-sequence-products";
    let (_source_tmp, snapshot_bytes) = release_import_snapshot_bytes_for(tenant_id).await;
    for through_sequence in [
        "0",
        "9223372036854775807",
        "9223372036854775808",
        "18446744073709551615",
    ] {
        for destination_kind in [
            ReleaseImportDestination::Absent,
            ReleaseImportDestination::Existing,
        ] {
            assert_successful_release_import(
                &snapshot_bytes,
                through_sequence,
                destination_kind,
                tenant_id,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn release_import_headerless_request_preserves_legacy_response_for_both_destinations() {
    let tenant_id = "release-import-legacy-products";
    let (_source_tmp, snapshot_bytes) = release_import_snapshot_bytes_for(tenant_id).await;
    for destination_kind in [
        ReleaseImportDestination::Absent,
        ReleaseImportDestination::Existing,
    ] {
        let (_destination_tmp, destination, app) =
            release_import_destination_for(destination_kind, tenant_id).await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/1/indexes/{tenant_id}/import"))
                    .body(Body::from(snapshot_bytes.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let response_headers = response.headers().clone();
        assert_eq!(
            body_json(response).await,
            serde_json::json!({"status": "imported"})
        );
        assert_release_import_content_for(&destination, tenant_id);
        assert_no_release_import_proof(&response_headers);
    }
}

#[tokio::test]
async fn release_import_invalid_archive_preserves_installation_error_contract() {
    let tenant_id = "release-import-invalid-products";
    let invalid_snapshot = b"not-a-valid-snapshot".to_vec();
    let snapshot_digest = format!("{:x}", Sha256::digest(&invalid_snapshot));
    for destination_kind in [
        ReleaseImportDestination::Absent,
        ReleaseImportDestination::Existing,
    ] {
        let (destination_tmp, destination, app) =
            release_import_destination_for(destination_kind, tenant_id).await;
        let response = app
            .clone()
            .oneshot(release_import_request_for_tenant(
                invalid_snapshot.clone(),
                &snapshot_digest,
                Some("7"),
                tenant_id,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_no_release_import_proof(response.headers());
        assert_eq!(
            body_json(response).await,
            serde_json::json!({
                "message": "Internal server error",
                "status": 500,
                "sub_step": "import_extract"
            })
        );
        assert!(has_lifecycle_phase(tenant_id, "snapshot_restore_entry"));

        destination.manager.graceful_shutdown().await;
        drop(app);
        drop(destination);
        let restarted = TestStateBuilder::new(&destination_tmp).build_shared();
        let tenant_path = restarted.manager.base_path.join(tenant_id);
        if matches!(destination_kind, ReleaseImportDestination::Existing) {
            assert_original_destination_persisted_for("invalid archive", &restarted, tenant_id);
        } else {
            assert!(!tenant_path.exists());
        }
    }
}
