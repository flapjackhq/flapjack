/// Holds a pause-artifact atomic replacement at its pre-rename checkpoint so a test
/// can inspect the tenant directory while the temporary replacement is still live.
struct PauseArtifactPublicationGate {
    _hook: backpressure::PauseArtifactPublicationHookGuard,
    reached_checkpoint: std::sync::mpsc::Receiver<()>,
    release: Option<std::sync::mpsc::SyncSender<()>>,
}

impl PauseArtifactPublicationGate {
    fn install(base_path: &std::path::Path, tenant_id: &str) -> Self {
        let (reached_checkpoint_tx, reached_checkpoint) = std::sync::mpsc::sync_channel(1);
        let (release, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = std::sync::Mutex::new(release_rx);
        let hook = backpressure::set_pause_artifact_publication_hook_for_test(
            &backpressure::pause_artifact_path(base_path, tenant_id),
            std::sync::Arc::new(move |_| {
                reached_checkpoint_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(PUBLICATION_GATE_TIMEOUT)
                    .expect("artifact publication should be released by the test");
            }),
        );
        Self {
            _hook: hook,
            reached_checkpoint,
            release: Some(release),
        }
    }

    fn await_checkpoint(&self) {
        self.reached_checkpoint
            .recv_timeout(PUBLICATION_GATE_TIMEOUT)
            .expect("artifact rewrite should reach the publication checkpoint");
    }

    /// Lets the blocked replacement finish. Ignores a dropped receiver so a panic
    /// inside the observation thread surfaces from its `join`, not from here.
    fn release(mut self) {
        let _ = self.release.take().unwrap().send(());
    }
}

impl Drop for PauseArtifactPublicationGate {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

const PUBLICATION_GATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

fn setup_paused_artifact_tenant(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
) -> std::sync::Arc<crate::index::manager::IndexManager> {
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);
    for bytes in [10_000, 10_500, 11_000] {
        record_stage_6_observation(
            tmp,
            tenant_id,
            Ok(backpressure::segment_observation_for_test(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2,
                bytes,
                0,
            )),
        );
    }
    assert_stage_6_pause_artifact(tmp, tenant_id, "pause");
    assert_stage_6_backpressure_error(manager.add_documents(
        tenant_id,
        vec![text_document(
            "blocked_doc",
            "title",
            "blocked before artifact rewrite",
        )],
    ));
    manager
}

/// Records a settled observation off-thread so the caller can act while the
/// resulting artifact rewrite is parked on a `PauseArtifactPublicationGate`.
fn spawn_settled_observation(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
) -> std::thread::JoinHandle<crate::error::Result<()>> {
    let base_path = tmp.path().to_path_buf();
    let tenant_id = tenant_id.to_string();
    std::thread::spawn(move || {
        backpressure::record_observation_result_for_test(
            &base_path,
            &tenant_id,
            Ok(backpressure::segment_observation_for_test(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1,
                12_000,
                0,
            )),
        )
    })
}

#[tokio::test(flavor = "current_thread")]
async fn pause_artifact_rewrite_keeps_previous_json_readable_until_replacement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "pause_artifact_atomic_rewrite";
    let _manager = setup_paused_artifact_tenant(&tmp, tenant_id);

    let gate = PauseArtifactPublicationGate::install(tmp.path(), tenant_id);
    let observation = spawn_settled_observation(&tmp, tenant_id);

    gate.await_checkpoint();
    assert_stage_6_pause_artifact(&tmp, tenant_id, "pause");

    gate.release();
    observation.join().unwrap().unwrap();
    assert_stage_6_pause_artifact(&tmp, tenant_id, "admit");
}

#[tokio::test(flavor = "current_thread")]
async fn tenant_copy_excludes_in_flight_pause_artifact_replacement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source = "pause_artifact_publication_source";
    let destination = "pause_artifact_publication_destination";
    let manager = setup_paused_artifact_tenant(&tmp, source);

    let gate = PauseArtifactPublicationGate::install(tmp.path(), source);
    let observation = spawn_settled_observation(&tmp, source);

    gate.await_checkpoint();
    assert!(
        std::fs::read_dir(tmp.path().join(source))
            .unwrap()
            .any(|entry| crate::index::utils::is_temporary_entry(&entry.unwrap().path())),
        "the publication probe must overlap the atomic replacement"
    );

    manager.copy_index(source, destination, None).await.unwrap();

    assert_stage_6_pause_artifact(&tmp, destination, "pause");
    assert!(
        std::fs::read_dir(tmp.path().join(destination))
            .unwrap()
            .all(|entry| !crate::index::utils::is_temporary_entry(&entry.unwrap().path())),
        "tenant publication must exclude in-flight replacement files"
    );
    gate.release();
    observation.join().unwrap().unwrap();
    assert_stage_6_pause_artifact(&tmp, source, "admit");
}
