use super::*;
use crate::experiments::config::*;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use tempfile::TempDir;

/// Create a minimal draft `Experiment` fixture for testing with the given ID and index name.
///
/// Uses a 50/50 traffic split, CTR as the primary metric, and a variant arm that disables synonyms.
fn make_experiment(id: &str, index: &str) -> Experiment {
    Experiment {
        id: id.to_string(),
        name: "test".to_string(),
        index_name: index.to_string(),
        status: ExperimentStatus::Draft,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(QueryOverrides {
                enable_synonyms: Some(false),
                ..Default::default()
            }),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: 1700000000000,
        started_at: None,
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    }
}

#[test]
fn create_and_get_succeeds() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    let exp = make_experiment("abc-123", "products");
    store.create(exp.clone()).unwrap();
    let loaded = store.get("abc-123").unwrap();
    assert_eq!(loaded.name, "test");
    assert_eq!(loaded.index_name, "products");
}

#[test]
fn load_only_startup_preserves_experiment_bytes_and_defers_missing_directory_creation() {
    let populated = TempDir::new().unwrap();
    let store = ExperimentStore::new(populated.path()).unwrap();
    store
        .create(make_experiment("persisted-exp", "products"))
        .unwrap();
    let experiment_path = populated.path().join(".experiments/persisted-exp.json");
    let map_path = populated.path().join(".experiments/_id_map.json");
    let before = (
        std::fs::read(&experiment_path).unwrap(),
        std::fs::read(&map_path).unwrap(),
    );
    #[cfg(unix)]
    let before_inodes = (
        std::fs::metadata(&experiment_path).unwrap().ino(),
        std::fs::metadata(&map_path).unwrap().ino(),
    );

    let reopened = ExperimentStore::load_only(populated.path()).unwrap();
    assert_eq!(reopened.get("persisted-exp").unwrap().id, "persisted-exp");
    assert_eq!(
        (
            std::fs::read(&experiment_path).unwrap(),
            std::fs::read(&map_path).unwrap()
        ),
        before
    );
    #[cfg(unix)]
    assert_eq!(
        (
            std::fs::metadata(&experiment_path).unwrap().ino(),
            std::fs::metadata(&map_path).unwrap().ino()
        ),
        before_inodes,
        "load-only startup must not atomically republish experiment state"
    );

    let retried = ExperimentStore::load_only(populated.path()).unwrap();
    assert_eq!(retried.get("persisted-exp").unwrap().id, "persisted-exp");
    assert_eq!(
        (
            std::fs::read(&experiment_path).unwrap(),
            std::fs::read(&map_path).unwrap()
        ),
        before
    );
    #[cfg(unix)]
    assert_eq!(
        (
            std::fs::metadata(&experiment_path).unwrap().ino(),
            std::fs::metadata(&map_path).unwrap().ino()
        ),
        before_inodes,
        "a retried load-only startup must remain zero-write"
    );

    let absent = TempDir::new().unwrap();
    let deferred = ExperimentStore::load_only(absent.path()).unwrap();
    assert!(!absent.path().join(".experiments").exists());
    deferred
        .create(make_experiment("after-release", "products"))
        .expect("the first post-release mutation should initialize persistence");
    assert!(absent
        .path()
        .join(".experiments/after-release.json")
        .is_file());
}

#[test]
fn create_duplicate_id_fails() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    let exp = make_experiment("dup-id", "products");
    store.create(exp.clone()).unwrap();
    assert!(matches!(
        store.create(exp),
        Err(ExperimentError::AlreadyExists(_))
    ));
}

#[test]
fn experiment_atomic_write_removes_temp_after_failed_rename() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    let experiments_dir = tmp.path().join(".experiments");
    let target_path = experiments_dir.join("e1.json");
    std::fs::create_dir(&target_path).unwrap();

    let error = store
        .create(make_experiment("e1", "products"))
        .expect_err("renaming a completed experiment over a directory must fail");

    assert!(matches!(error, ExperimentError::Io(_)));
    assert!(
        target_path.is_dir(),
        "rename must leave the target directory intact"
    );
    assert!(
        !target_path.is_file(),
        "failed rename must not publish a file"
    );
    assert!(
        !experiments_dir.join("e1.json.tmp").exists(),
        "failed rename must remove the completed experiment temp file"
    );
}

#[test]
fn id_map_persist_removes_temp_after_failed_rename() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    let experiments_dir = tmp.path().join(".experiments");
    let id_map_path = experiments_dir.join("_id_map.json");
    assert!(
        id_map_path.is_file(),
        "store construction must seed the ID map"
    );
    std::fs::remove_file(&id_map_path).unwrap();
    std::fs::create_dir(&id_map_path).unwrap();

    let error = store
        .create(make_experiment("e1", "products"))
        .expect_err("renaming a completed ID map over a directory must fail");

    assert!(matches!(error, ExperimentError::Io(_)));
    assert!(
        id_map_path.is_dir(),
        "rename must leave the target directory intact"
    );
    let persisted: Experiment = serde_json::from_slice(
        &std::fs::read(experiments_dir.join("e1.json"))
            .expect("experiment JSON is published before ID-map persistence"),
    )
    .unwrap();
    assert_eq!(persisted.id, "e1");
    assert!(
        !experiments_dir.join("_id_map.json.tmp").exists(),
        "failed rename must remove the completed ID-map temp file"
    );
}

#[test]
fn get_nonexistent_returns_not_found() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    assert!(matches!(
        store.get("ghost"),
        Err(ExperimentError::NotFound(_))
    ));
}

#[test]
fn list_returns_all_experiments() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.create(make_experiment("e2", "articles")).unwrap();
    let list = store.list(None);
    assert_eq!(list.len(), 2);
}

#[test]
fn list_filters_by_index() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.create(make_experiment("e2", "articles")).unwrap();
    let list = store.list(Some(ExperimentFilter {
        index_name: Some("products".to_string()),
        status: None,
    }));
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "e1");
}

#[test]
fn update_draft_succeeds() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    let mut updated = make_experiment("e1", "products");
    updated.name = "updated name".to_string();
    store.update(updated).unwrap();
    assert_eq!(store.get("e1").unwrap().name, "updated name");
}

#[test]
fn update_running_experiment_returns_invalid_status() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.start("e1").unwrap();
    let mut exp = store.get("e1").unwrap();
    exp.name = "new name".to_string();
    assert!(matches!(
        store.update(exp),
        Err(ExperimentError::InvalidStatus(_))
    ));
}

#[test]
fn start_transitions_draft_to_running() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    let started = store.start("e1").unwrap();
    assert_eq!(started.status, ExperimentStatus::Running);
    assert!(started.started_at.is_some());
}

#[test]
fn start_already_running_returns_invalid_status() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.start("e1").unwrap();
    assert!(matches!(
        store.start("e1"),
        Err(ExperimentError::InvalidStatus(_))
    ));
}

#[test]
fn stop_transitions_running_to_stopped() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.start("e1").unwrap();
    let stopped = store.stop("e1").unwrap();
    assert_eq!(stopped.status, ExperimentStatus::Stopped);
    assert!(stopped.stopped_at.is_some());
}

#[test]
fn stop_transitions_draft_to_stopped() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    let stopped = store.stop("e1").unwrap();
    assert_eq!(stopped.status, ExperimentStatus::Stopped);
    assert!(stopped.stopped_at.is_some());
}

/// Verify that concluding a running experiment transitions it to `Concluded`, sets `stopped_at`, and attaches the provided conclusion with winner and statistical details.
#[test]
fn conclude_running_experiment_sets_status_and_conclusion() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.start("e1").unwrap();

    let conclusion = ExperimentConclusion {
        winner: Some("variant".to_string()),
        reason: "Statistically significant result".to_string(),
        control_metric: 0.12,
        variant_metric: 0.14,
        confidence: 0.97,
        significant: true,
        promoted: false,
    };

    let concluded = store.conclude("e1", conclusion.clone()).unwrap();
    assert_eq!(concluded.status, ExperimentStatus::Concluded);
    assert!(concluded.stopped_at.is_some());
    assert_eq!(
        concluded.conclusion.as_ref().unwrap().winner,
        conclusion.winner
    );
    assert_eq!(
        concluded.conclusion.as_ref().unwrap().reason,
        conclusion.reason
    );
}

/// Verify that concluding a stopped experiment succeeds and preserves the original `stopped_at` timestamp rather than overwriting it.
#[test]
fn conclude_stopped_experiment_succeeds() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.start("e1").unwrap();
    let stopped = store.stop("e1").unwrap();
    let stopped_at = stopped.stopped_at;
    assert!(stopped_at.is_some());

    let conclusion = ExperimentConclusion {
        winner: None,
        reason: "Inconclusive — ending experiment".to_string(),
        control_metric: 0.10,
        variant_metric: 0.11,
        confidence: 0.60,
        significant: false,
        promoted: false,
    };

    let concluded = store.conclude("e1", conclusion).unwrap();
    assert_eq!(concluded.status, ExperimentStatus::Concluded);
    // stopped_at must be preserved from the stop transition, not overwritten
    assert_eq!(concluded.stopped_at, stopped_at);
    assert!(concluded.conclusion.is_some());
    assert!(concluded.conclusion.as_ref().unwrap().winner.is_none());
}

/// Verify that attempting to conclude an already-concluded experiment returns `InvalidStatus`, preventing conclusion overwrites.
#[test]
fn conclude_already_concluded_returns_invalid_status() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.start("e1").unwrap();

    let conclusion = ExperimentConclusion {
        winner: Some("variant".to_string()),
        reason: "First conclusion".to_string(),
        control_metric: 0.12,
        variant_metric: 0.14,
        confidence: 0.97,
        significant: true,
        promoted: false,
    };
    store.conclude("e1", conclusion).unwrap();

    let second = ExperimentConclusion {
        winner: Some("control".to_string()),
        reason: "Trying to override".to_string(),
        control_metric: 0.12,
        variant_metric: 0.14,
        confidence: 0.97,
        significant: true,
        promoted: false,
    };
    assert!(matches!(
        store.conclude("e1", second),
        Err(ExperimentError::InvalidStatus(_))
    ));
}

/// Verify that concluding a draft experiment (never started) returns `InvalidStatus`.
#[test]
fn conclude_draft_experiment_returns_invalid_status() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();

    let conclusion = ExperimentConclusion {
        winner: Some("variant".to_string()),
        reason: "Statistically significant result".to_string(),
        control_metric: 0.12,
        variant_metric: 0.14,
        confidence: 0.97,
        significant: true,
        promoted: false,
    };

    assert!(matches!(
        store.conclude("e1", conclusion),
        Err(ExperimentError::InvalidStatus(_))
    ));
}

#[test]
fn delete_draft_succeeds() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.delete("e1").unwrap();
    assert!(matches!(store.get("e1"), Err(ExperimentError::NotFound(_))));
}

#[test]
fn delete_running_experiment_returns_invalid_status() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.start("e1").unwrap();
    assert!(matches!(
        store.delete("e1"),
        Err(ExperimentError::InvalidStatus(_))
    ));
}

#[test]
fn get_active_for_index_returns_running_experiment() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.start("e1").unwrap();
    assert!(store.get_active_for_index("products").is_some());
    assert!(store.get_active_for_index("articles").is_none());
}

#[test]
fn get_active_for_index_returns_none_for_draft() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    assert!(store.get_active_for_index("products").is_none());
}

/// Verify that starting a second experiment on an index that already has a running experiment returns `InvalidStatus` mentioning the index name.
#[test]
fn start_second_experiment_on_same_index_fails() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.create(make_experiment("e2", "products")).unwrap();
    store.start("e1").unwrap();
    let result = store.start("e2");
    assert!(
        result.is_err(),
        "starting a second experiment on the same index should fail"
    );
    match result {
        Err(ExperimentError::InvalidStatus(msg)) => {
            assert!(
                msg.contains("products"),
                "error should mention the index name"
            );
        }
        other => panic!("expected InvalidStatus, got: {:?}", other),
    }
}

#[test]
fn start_experiment_on_different_index_succeeds() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.create(make_experiment("e2", "articles")).unwrap();
    store.start("e1").unwrap();
    assert!(
        store.start("e2").is_ok(),
        "starting experiment on different index should succeed"
    );
}

#[test]
fn get_active_for_index_returns_none_for_stopped() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.start("e1").unwrap();
    assert!(store.get_active_for_index("products").is_some());
    store.stop("e1").unwrap();
    assert!(
        store.get_active_for_index("products").is_none(),
        "stopped experiment must not be returned as active"
    );
}

/// Verify that `get_active_for_index` returns `None` after an experiment is concluded, ensuring concluded experiments are not treated as active.
#[test]
fn get_active_for_index_returns_none_for_concluded() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.start("e1").unwrap();
    assert!(store.get_active_for_index("products").is_some());
    let conclusion = ExperimentConclusion {
        winner: Some("variant".to_string()),
        reason: "test".to_string(),
        control_metric: 0.1,
        variant_metric: 0.2,
        confidence: 0.95,
        significant: true,
        promoted: false,
    };
    store.conclude("e1", conclusion).unwrap();
    assert!(
        store.get_active_for_index("products").is_none(),
        "concluded experiment must not be returned as active"
    );
}

#[test]
fn experiments_persist_across_store_restart() {
    let tmp = TempDir::new().unwrap();
    {
        let store = ExperimentStore::new(tmp.path()).unwrap();
        store.create(make_experiment("e1", "products")).unwrap();
    }
    let store2 = ExperimentStore::new(tmp.path()).unwrap();
    let loaded = store2.get("e1").unwrap();
    assert_eq!(loaded.id, "e1");
}

/// Verify that the `interleaving` flag on an experiment survives serialization and is correctly restored when a new store is constructed from the same data directory.
#[test]
fn interleaving_flag_persists_across_store_restart() {
    let tmp = TempDir::new().unwrap();
    {
        let store = ExperimentStore::new(tmp.path()).unwrap();
        let mut exp = make_experiment("e-il", "products_il");
        exp.interleaving = Some(true);
        exp.variant.query_overrides = None;
        exp.variant.index_name = Some("products_il_v2".to_string());
        store.create(exp).unwrap();
    }
    let store2 = ExperimentStore::new(tmp.path()).unwrap();
    let loaded = store2.get("e-il").unwrap();
    assert_eq!(
        loaded.interleaving,
        Some(true),
        "interleaving flag must survive persistence"
    );
}

/// Verify that constructing a store fails with `InvalidConfig` when a persisted experiment file contains an experiment that fails validation.
#[test]
fn new_store_rejects_invalid_experiment_from_disk() {
    let tmp = TempDir::new().unwrap();
    let experiments_dir = tmp.path().join(".experiments");
    std::fs::create_dir_all(&experiments_dir).unwrap();

    let mut invalid = make_experiment("bad1", "products");
    invalid.variant.index_name = Some("products_variant".to_string());
    let path = experiments_dir.join("bad1.json");
    std::fs::write(path, serde_json::to_string_pretty(&invalid).unwrap()).unwrap();

    let result = ExperimentStore::new(tmp.path());
    assert!(
        matches!(result, Err(ExperimentError::InvalidConfig(_))),
        "invalid persisted experiments must fail store startup with InvalidConfig"
    );
}

#[test]
fn create_assigns_sequential_numeric_ids() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    store.create(make_experiment("e2", "articles")).unwrap();
    let id1 = store.get_numeric_id("e1").unwrap();
    let id2 = store.get_numeric_id("e2").unwrap();
    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
}

#[test]
fn get_by_numeric_id_returns_experiment() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    let exp = store.get_by_numeric_id(1).unwrap();
    assert_eq!(exp.id, "e1");
}

#[test]
fn get_by_numeric_id_nonexistent_returns_not_found() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    assert!(store.get_by_numeric_id(999).is_err());
}

#[test]
fn numeric_ids_persist_across_store_restart() {
    let tmp = TempDir::new().unwrap();
    {
        let store = ExperimentStore::new(tmp.path()).unwrap();
        store.create(make_experiment("e1", "products")).unwrap();
        store.create(make_experiment("e2", "articles")).unwrap();
    }
    let store2 = ExperimentStore::new(tmp.path()).unwrap();
    assert_eq!(store2.get_numeric_id("e1"), Some(1));
    assert_eq!(store2.get_numeric_id("e2"), Some(2));
    // New experiments should continue from where we left off
    store2.create(make_experiment("e3", "blog")).unwrap();
    assert_eq!(store2.get_numeric_id("e3"), Some(3));
}

#[test]
fn delete_removes_numeric_id_mapping() {
    let tmp = TempDir::new().unwrap();
    let store = ExperimentStore::new(tmp.path()).unwrap();
    store.create(make_experiment("e1", "products")).unwrap();
    assert!(store.get_numeric_id("e1").is_some());
    store.delete("e1").unwrap();
    assert!(store.get_numeric_id("e1").is_none());
    assert!(store.get_by_numeric_id(1).is_err());
}

/// Verify that constructing a store fails with `InvalidConfig` when the persisted data contains two running experiments targeting the same index.
#[test]
fn new_store_rejects_multiple_running_experiments_for_same_index() {
    let tmp = TempDir::new().unwrap();
    let experiments_dir = tmp.path().join(".experiments");
    std::fs::create_dir_all(&experiments_dir).unwrap();

    let mut running_a = make_experiment("run-a", "products");
    running_a.status = ExperimentStatus::Running;
    running_a.started_at = Some(1700000000000);

    let mut running_b = make_experiment("run-b", "products");
    running_b.status = ExperimentStatus::Running;
    running_b.started_at = Some(1700000001000);

    std::fs::write(
        experiments_dir.join("run-a.json"),
        serde_json::to_string_pretty(&running_a).unwrap(),
    )
    .unwrap();
    std::fs::write(
        experiments_dir.join("run-b.json"),
        serde_json::to_string_pretty(&running_b).unwrap(),
    )
    .unwrap();

    let result = ExperimentStore::new(tmp.path());
    assert!(
        matches!(result, Err(ExperimentError::InvalidConfig(_))),
        "store startup must reject multiple running experiments for the same index"
    );
}
