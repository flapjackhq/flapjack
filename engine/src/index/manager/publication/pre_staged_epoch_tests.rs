use super::*;
use crate::analytics::AnalyticsConfig;
use tempfile::TempDir;

#[test]
fn owned_first_staging_abort_preserves_proven_vacancy_without_epoch_advance() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("first_staging_abort").unwrap();
    let first =
        PreStagedPublication::prepare_for_vacant_target(temp.path(), target.clone()).unwrap();
    let second =
        PreStagedPublication::prepare_for_vacant_target(temp.path(), target.clone()).unwrap();
    let second_namespace = second.paths().staging.parent().unwrap().to_path_buf();
    first.abort().unwrap();
    assert!(
        second_namespace.exists(),
        "abort must not discard another staging owner"
    );
    second.abort().unwrap();
    let report = scan_and_repair_publication_target(
        temp.path(),
        &AnalyticsConfig::for_data_dir(temp.path()),
        target.clone(),
    )
    .unwrap();
    assert_eq!(report.disposition, PublicationTargetDisposition::Vacant);
    assert_eq!(
        read_publication_epoch(temp.path(), &target).unwrap(),
        PublicationEpoch(0)
    );
    let paths = publication_epoch_paths_for_target_path(&temp.path().join(target.as_str()));
    assert_eq!(std::fs::read_to_string(paths.epoch).unwrap(), "0");
    assert!(paths.lock.is_file(), "the lock inode must be retained");
}

#[test]
fn staging_abort_does_not_certify_unknown_lock_only_state() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("unknown_lock").unwrap();
    let paths = publication_epoch_paths_for_target_path(&temp.path().join(target.as_str()));
    std::fs::create_dir_all(paths.lock.parent().unwrap()).unwrap();
    std::fs::write(&paths.lock, b"").unwrap();
    assert!(PreStagedPublication::prepare_for_vacant_target(temp.path(), target.clone()).is_err());
    let publication = PreStagedPublication::prepare(temp.path(), target.clone()).unwrap();
    publication.abort().unwrap();
    let report = scan_and_repair_publication_target(
        temp.path(),
        &AnalyticsConfig::for_data_dir(temp.path()),
        target,
    )
    .unwrap();
    assert_eq!(
        report.disposition,
        PublicationTargetDisposition::Unavailable
    );
    assert!(!paths.epoch.exists());
    assert!(paths.lock.is_file());
}

#[test]
fn vacant_preparation_refuses_preexisting_unknown_or_corrupt_namespace() {
    for residue in [
        "empty",
        "epoch.tmp",
        "corrupt_epoch",
        "unknown_transaction",
        "lock_directory",
    ] {
        let temp = TempDir::new().unwrap();
        let target = PublicationTarget::new("unproven_initial_state").unwrap();
        let namespace = temp.path().join(".publication").join(target.as_str());
        std::fs::create_dir_all(&namespace).unwrap();
        match residue {
            "epoch.tmp" => std::fs::write(namespace.join("epoch.tmp"), "0").unwrap(),
            "corrupt_epoch" => std::fs::write(namespace.join("epoch"), "not-an-epoch").unwrap(),
            "unknown_transaction" => std::fs::create_dir(namespace.join("unowned")).unwrap(),
            "lock_directory" => std::fs::create_dir(namespace.join("epoch.lock")).unwrap(),
            _ => {}
        }
        let before = std::fs::read_dir(&namespace)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert!(
            PreStagedPublication::prepare_for_vacant_target(temp.path(), target).is_err(),
            "{residue}"
        );
        let after = std::fs::read_dir(&namespace)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(before, after, "must not mutate unowned {residue}");
        if residue == "corrupt_epoch" {
            assert_eq!(
                std::fs::read_to_string(namespace.join("epoch")).unwrap(),
                "not-an-epoch"
            );
        } else {
            assert!(
                !namespace.join("epoch").exists(),
                "must not mint evidence for {residue}"
            );
        }
    }
}

#[test]
fn vacant_preparation_preserves_durable_epoch_under_existing_quiesce_fence() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("previously_deleted").unwrap();
    let fence =
        compare_and_advance_publication_epoch(temp.path(), &target, PublicationEpoch(0)).unwrap();
    let publication =
        PreStagedPublication::prepare_for_vacant_target(temp.path(), target.clone()).unwrap();
    publication.abort().unwrap();
    assert_eq!(
        read_publication_epoch(temp.path(), &target).unwrap(),
        PublicationEpoch(1)
    );
    assert!(publication_admission_is_fenced(temp.path(), &target));
    drop(fence);
}

#[test]
#[serial_test::serial(publication_epoch_open_lock_file_checkpoint_hook)]
fn fresh_namespace_rechecks_sidecars_before_persisting_zero() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("racing_epoch_owner").unwrap();
    let paths = publication_epoch_paths_for_target_path(&temp.path().join(target.as_str()));
    let hook_lock = paths.lock.clone();
    let hook_epoch = paths.epoch.clone();
    let _hook = set_publication_epoch_open_lock_file_checkpoint_hook_for_test(move |path| {
        if path == hook_lock {
            // Another process can establish evidence after our mkdir and before
            // our exclusive lock. Initializing zero must never overwrite it.
            std::fs::write(&hook_epoch, "9").unwrap();
        }
    });
    assert!(PreStagedPublication::prepare_for_vacant_target(temp.path(), target).is_err());
    assert_eq!(std::fs::read_to_string(&paths.epoch).unwrap(), "9");
    assert_eq!(
        std::fs::read_dir(paths.epoch.parent().unwrap())
            .unwrap()
            .count(),
        2
    );
}

#[test]
#[serial_test::serial(publication_epoch_open_lock_file_checkpoint_hook)]
fn interrupted_fresh_initialization_remains_unknown_on_retry() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("interrupted_initialization").unwrap();
    let paths = publication_epoch_paths_for_target_path(&temp.path().join(target.as_str()));
    let hook_lock = paths.lock.clone();
    let hook = set_publication_epoch_open_lock_file_checkpoint_hook_for_test(move |path| {
        if path == hook_lock {
            std::fs::create_dir(path).unwrap();
        }
    });
    assert!(PreStagedPublication::prepare_for_vacant_target(temp.path(), target.clone()).is_err());
    drop(hook);
    assert!(!paths.epoch.exists());
    assert!(PreStagedPublication::prepare_for_vacant_target(temp.path(), target).is_err());
    assert!(paths.lock.is_dir());
    assert!(!paths.epoch.exists());
}

#[test]
#[serial_test::serial(publication_epoch_open_lock_file_checkpoint_hook)]
fn concurrent_vacant_preparation_cannot_adopt_incomplete_initializer() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    };
    use std::time::Duration;
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("concurrent_initializers").unwrap();
    let paths = publication_epoch_paths_for_target_path(&temp.path().join(target.as_str()));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let paused = AtomicBool::new(false);
    let hook_lock = paths.lock.clone();
    let _hook = set_publication_epoch_open_lock_file_checkpoint_hook_for_test(move |path| {
        if path == hook_lock && !paused.swap(true, Ordering::SeqCst) {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
    });
    let base = temp.path().to_path_buf();
    let first_target = target.clone();
    let first = std::thread::spawn(move || {
        PreStagedPublication::prepare_for_vacant_target(&base, first_target)
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let fenced = publication_admission_is_fenced(temp.path(), &target);
    let second = PreStagedPublication::prepare_for_vacant_target(temp.path(), target.clone());
    release_tx.send(()).unwrap();
    let first = first.join().unwrap().unwrap();
    assert!(
        fenced,
        "pending initialization must stop in-process write admission"
    );
    assert!(
        second.is_err(),
        "an in-progress directory is not durable evidence"
    );
    let second =
        PreStagedPublication::prepare_for_vacant_target(temp.path(), target.clone()).unwrap();
    let second_namespace = second.paths().staging.parent().unwrap().to_owned();
    first.abort().unwrap();
    assert!(second_namespace.exists());
    second.abort().unwrap();
    assert_eq!(
        read_publication_epoch(temp.path(), &target).unwrap(),
        PublicationEpoch(0)
    );
}

#[cfg(unix)]
#[test]
fn vacant_preparation_refuses_symlinked_managed_paths() {
    use std::os::unix::fs::symlink;
    for component in ["namespace", "epoch", "epoch.tmp", "epoch.lock", "live"] {
        let temp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let target = PublicationTarget::new("symlink_initialization").unwrap();
        let namespace = temp.path().join(".publication").join(target.as_str());
        std::fs::create_dir_all(namespace.parent().unwrap()).unwrap();
        match component {
            "namespace" => symlink(outside.path(), &namespace).unwrap(),
            "live" => symlink(outside.path(), temp.path().join(target.as_str())).unwrap(),
            _ => {
                std::fs::create_dir(&namespace).unwrap();
                symlink(outside.path().join("untouched"), namespace.join(component)).unwrap();
            }
        }
        assert!(
            PreStagedPublication::prepare_for_vacant_target(temp.path(), target).is_err(),
            "{component}"
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            0,
            "{component}"
        );
    }
}
