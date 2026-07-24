use super::epoch::{
    capture_publication_epoch, set_publication_epoch_admission_lock_checkpoint_hook_for_test,
    set_publication_epoch_admission_pre_lock_checkpoint_hook_for_test,
    set_publication_epoch_advance_checkpoint_hook_for_test,
    set_publication_epoch_open_lock_file_checkpoint_hook_for_test,
    try_validate_publication_epoch_admission, PublicationEpochAdmissionError,
};
use super::{
    compare_and_advance_publication_epoch, PublicationEpoch, PublicationEpochError,
    PublicationTarget,
};
use std::fs;
use std::sync::{mpsc, Mutex};
use std::time::Duration;
use tempfile::TempDir;

fn target(name: &str) -> PublicationTarget {
    PublicationTarget::new(name).unwrap()
}

#[test]
fn matching_observation_holds_admission_guard_until_dropped() {
    let tmp = TempDir::new().unwrap();
    let products = target("products");
    let observed = capture_publication_epoch(tmp.path(), &products).unwrap();
    let guard = try_validate_publication_epoch_admission(tmp.path(), &products, observed).unwrap();
    let (tx, rx) = mpsc::channel();
    let base = tmp.path().to_path_buf();

    let handle = std::thread::spawn(move || {
        tx.send(
            compare_and_advance_publication_epoch(&base, &target("products"), observed)
                .map(|fence| fence.advanced()),
        )
        .unwrap();
    });

    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(guard);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap(),
        PublicationEpoch(1)
    );
    handle.join().unwrap();
}

#[test]
fn stale_observation_is_rejected_without_refreshing_to_current_epoch() {
    let tmp = TempDir::new().unwrap();
    let target = target("products");
    let observed = capture_publication_epoch(tmp.path(), &target).unwrap();
    drop(compare_and_advance_publication_epoch(
        tmp.path(),
        &target,
        observed,
    ));

    match try_validate_publication_epoch_admission(tmp.path(), &target, observed) {
        Err(PublicationEpochAdmissionError::Stale { observed, current }) => {
            assert_eq!(observed, PublicationEpoch(0));
            assert_eq!(current, PublicationEpoch(1));
        }
        other => panic!("expected stale admission observation, got {other:?}"),
    }
}

#[test]
fn exclusive_fence_fails_same_target_admission_fast_but_not_other_targets() {
    let tmp = TempDir::new().unwrap();
    let products = target("products");
    let users = target("users");
    let observed_products = capture_publication_epoch(tmp.path(), &products).unwrap();
    let observed_users = capture_publication_epoch(tmp.path(), &users).unwrap();
    let fence =
        compare_and_advance_publication_epoch(tmp.path(), &products, observed_products).unwrap();

    assert!(matches!(
        try_validate_publication_epoch_admission(tmp.path(), &products, observed_products),
        Err(PublicationEpochAdmissionError::Busy)
    ));
    let users_guard =
        try_validate_publication_epoch_admission(tmp.path(), &users, observed_users).unwrap();
    drop(users_guard);
    drop(fence);
}

#[test]
#[serial_test::serial(publication_epoch_advance_checkpoint_hook)]
fn pending_exclusive_fence_request_fails_same_target_admission_fast() {
    let tmp = TempDir::new().unwrap();
    let products = target("pending_products");
    let users = target("pending_users");
    let observed_products = capture_publication_epoch(tmp.path(), &products).unwrap();
    let observed_users = capture_publication_epoch(tmp.path(), &users).unwrap();
    let active_admission =
        try_validate_publication_epoch_admission(tmp.path(), &products, observed_products).unwrap();
    let (pending_tx, pending_rx) = mpsc::channel();
    let _hook = set_publication_epoch_advance_checkpoint_hook_for_test({
        let products = products.clone();
        move |target, expected| {
            if target == &products && expected == PublicationEpoch(0) {
                pending_tx.send(()).unwrap();
            }
        }
    });
    let base = tmp.path().to_path_buf();
    let advance_target = products.clone();
    let advance = std::thread::spawn(move || {
        compare_and_advance_publication_epoch(&base, &advance_target, PublicationEpoch(0))
    });

    pending_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("exclusive epoch advance must publish its pending request before waiting");

    assert!(matches!(
        try_validate_publication_epoch_admission(tmp.path(), &products, observed_products),
        Err(PublicationEpochAdmissionError::Busy)
    ));
    let unrelated_guard =
        try_validate_publication_epoch_admission(tmp.path(), &users, observed_users).unwrap();
    drop(unrelated_guard);

    drop(active_admission);
    let fence = advance.join().unwrap().unwrap();
    assert_eq!(fence.advanced(), PublicationEpoch(1));
}

#[test]
#[serial_test::serial(
    publication_epoch_advance_checkpoint_hook,
    publication_epoch_admission_lock_checkpoint_hook
)]
fn admission_that_acquires_shared_lock_before_pending_advance_proceeds() {
    let tmp = TempDir::new().unwrap();
    let products = target("postlock_products");
    let observed = capture_publication_epoch(tmp.path(), &products).unwrap();
    let expected_lock_path = tmp.path().join(".publication/postlock_products/epoch.lock");
    let (shared_locked_tx, shared_locked_rx) = mpsc::channel();
    let (release_validation_tx, release_validation_rx) = mpsc::channel();
    let release_validation_rx = std::sync::Mutex::new(release_validation_rx);
    let _admission_hook =
        set_publication_epoch_admission_lock_checkpoint_hook_for_test(move |lock_path| {
            if lock_path == expected_lock_path {
                shared_locked_tx.send(()).unwrap();
                release_validation_rx.lock().unwrap().recv().unwrap();
            }
        });
    let (advance_pending_tx, advance_pending_rx) = mpsc::channel();
    let _advance_hook = set_publication_epoch_advance_checkpoint_hook_for_test({
        let products = products.clone();
        move |target, expected| {
            if target == &products && expected == PublicationEpoch(0) {
                advance_pending_tx.send(()).unwrap();
            }
        }
    });
    let (admission_result_tx, admission_result_rx) = mpsc::channel();
    let (drop_guard_tx, drop_guard_rx) = mpsc::channel();
    let admission_base = tmp.path().to_path_buf();
    let admission_target = products.clone();
    let admission = std::thread::spawn(move || {
        let result =
            try_validate_publication_epoch_admission(&admission_base, &admission_target, observed);
        match result {
            Ok(guard) => {
                admission_result_tx.send(Ok(guard.observed())).unwrap();
                drop_guard_rx.recv().unwrap();
                drop(guard);
            }
            Err(error) => admission_result_tx.send(Err(format!("{error:?}"))).unwrap(),
        }
    });

    shared_locked_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("admission must acquire the shared epoch lock");
    let advance_base = tmp.path().to_path_buf();
    let advance_target = products.clone();
    let advance = std::thread::spawn(move || {
        compare_and_advance_publication_epoch(&advance_base, &advance_target, PublicationEpoch(0))
    });
    advance_pending_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("exclusive epoch advance must register its pending request");

    release_validation_tx.send(()).unwrap();
    let admission_result = admission_result_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("admission validation must finish");
    drop_guard_tx.send(()).ok();
    admission.join().unwrap();
    let fence = advance.join().unwrap().unwrap();

    assert_eq!(admission_result.unwrap(), PublicationEpoch(0));
    assert_eq!(fence.advanced(), PublicationEpoch(1));
}

#[test]
#[serial_test::serial(
    publication_epoch_advance_checkpoint_hook,
    publication_epoch_admission_pre_lock_checkpoint_hook
)]
fn admission_prevents_pending_advance_registration_before_shared_lock_acquisition() {
    let tmp = TempDir::new().unwrap();
    let products = target("prelock_products");
    let observed = capture_publication_epoch(tmp.path(), &products).unwrap();
    let expected_lock_path = tmp.path().join(".publication/prelock_products/epoch.lock");
    let (before_try_lock_tx, before_try_lock_rx) = mpsc::channel();
    let (release_admission_tx, release_admission_rx) = mpsc::channel();
    let release_admission_rx = std::sync::Mutex::new(release_admission_rx);
    let _admission_hook =
        set_publication_epoch_admission_pre_lock_checkpoint_hook_for_test(move |lock_path| {
            if lock_path == expected_lock_path {
                before_try_lock_tx.send(()).unwrap();
                release_admission_rx.lock().unwrap().recv().unwrap();
            }
        });
    let (advance_pending_tx, advance_pending_rx) = mpsc::channel();
    let _advance_hook = set_publication_epoch_advance_checkpoint_hook_for_test({
        let products = products.clone();
        move |target, expected| {
            if target == &products && expected == PublicationEpoch(0) {
                advance_pending_tx.send(()).unwrap();
            }
        }
    });
    let (admission_result_tx, admission_result_rx) = mpsc::channel();
    let admission_base = tmp.path().to_path_buf();
    let admission_target = products.clone();
    let admission = std::thread::spawn(move || {
        let result =
            try_validate_publication_epoch_admission(&admission_base, &admission_target, observed);
        admission_result_tx
            .send(result.map(|guard| guard.observed()))
            .unwrap();
    });

    before_try_lock_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("admission must pause after the pending check and before shared lock acquisition");
    let advance_base = tmp.path().to_path_buf();
    let advance_target = products.clone();
    let advance = std::thread::spawn(move || {
        compare_and_advance_publication_epoch(&advance_base, &advance_target, PublicationEpoch(0))
    });
    assert!(
        advance_pending_rx
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "exclusive epoch advance must not register between pending check and shared lock"
    );

    release_admission_tx.send(()).unwrap();
    assert_eq!(
        admission_result_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap(),
        PublicationEpoch(0)
    );
    admission.join().unwrap();
    advance_pending_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("exclusive epoch advance should register after admission takes the shared lock");
    let fence = advance.join().unwrap().unwrap();
    assert_eq!(fence.advanced(), PublicationEpoch(1));
}

#[test]
#[serial_test::serial(publication_epoch_open_lock_file_checkpoint_hook)]
fn paused_lock_file_open_for_one_target_does_not_delay_unrelated_target_admission_or_advance() {
    let tmp = TempDir::new().unwrap();
    let products = target("paused_products");
    let users = target("paused_users");
    let observed_products = capture_publication_epoch(tmp.path(), &products).unwrap();
    let observed_users = capture_publication_epoch(tmp.path(), &users).unwrap();
    let products_lock_path = tmp.path().join(".publication/paused_products/epoch.lock");
    let (products_open_tx, products_open_rx) = mpsc::channel();
    let (release_products_open_tx, release_products_open_rx) = mpsc::channel();
    let release_products_open_rx = Mutex::new(release_products_open_rx);
    let _hook = set_publication_epoch_open_lock_file_checkpoint_hook_for_test(move |lock_path| {
        if lock_path == products_lock_path {
            products_open_tx.send(()).unwrap();
            release_products_open_rx.lock().unwrap().recv().unwrap();
        }
    });
    let (products_result_tx, products_result_rx) = mpsc::channel();
    let products_base = tmp.path().to_path_buf();
    let products_target = products.clone();
    let products_admission = std::thread::spawn(move || {
        products_result_tx
            .send(
                try_validate_publication_epoch_admission(
                    &products_base,
                    &products_target,
                    observed_products,
                )
                .map(|guard| guard.observed()),
            )
            .unwrap();
    });

    products_open_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("target A admission must pause during lock-file open");

    let (users_admission_tx, users_admission_rx) = mpsc::channel();
    let users_admission_base = tmp.path().to_path_buf();
    let users_admission_target = users.clone();
    let users_admission = std::thread::spawn(move || {
        users_admission_tx
            .send(
                try_validate_publication_epoch_admission(
                    &users_admission_base,
                    &users_admission_target,
                    observed_users,
                )
                .map(|guard| guard.observed()),
            )
            .unwrap();
    });
    let users_admission_result = users_admission_rx.recv_timeout(Duration::from_secs(2));

    let (users_advance_tx, users_advance_rx) = mpsc::channel();
    let users_advance_base = tmp.path().to_path_buf();
    let users_advance_target = users.clone();
    let users_advance = std::thread::spawn(move || {
        users_advance_tx
            .send(
                compare_and_advance_publication_epoch(
                    &users_advance_base,
                    &users_advance_target,
                    PublicationEpoch(0),
                )
                .map(|fence| fence.advanced()),
            )
            .unwrap();
    });
    let users_advance_result = users_advance_rx.recv_timeout(Duration::from_secs(2));

    release_products_open_tx.send(()).unwrap();
    assert_eq!(
        products_result_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap(),
        PublicationEpoch(0)
    );
    products_admission.join().unwrap();
    users_admission.join().unwrap();
    users_advance.join().unwrap();

    assert_eq!(
        users_admission_result.unwrap().unwrap(),
        PublicationEpoch(0)
    );
    assert_eq!(users_advance_result.unwrap().unwrap(), PublicationEpoch(1));
}

#[test]
fn corrupt_epoch_evidence_fails_capture_and_locked_validation_closed() {
    let tmp = TempDir::new().unwrap();
    let products = target("products");
    let namespace = tmp.path().join(".publication/products");
    fs::create_dir_all(&namespace).unwrap();
    fs::write(namespace.join("epoch"), b"01").unwrap();

    assert!(matches!(
        capture_publication_epoch(tmp.path(), &products),
        Err(PublicationEpochAdmissionError::Epoch(
            PublicationEpochError::CorruptState { .. }
        ))
    ));
    assert!(matches!(
        try_validate_publication_epoch_admission(tmp.path(), &products, PublicationEpoch(0)),
        Err(PublicationEpochAdmissionError::Epoch(
            PublicationEpochError::CorruptState { .. }
        ))
    ));
}

mod existing_epoch_tests {
    use super::super::epoch::*;
    use super::super::{PublicationPaths, PublicationTarget, PublicationTransactionId};
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::Duration;
    use tempfile::TempDir;

    fn target(name: &str) -> PublicationTarget {
        PublicationTarget::new(name).unwrap()
    }

    fn transaction(id: &str) -> PublicationTransactionId {
        PublicationTransactionId::new(id).unwrap()
    }

    #[test]
    fn epoch_paths_are_per_target_and_transaction_independent() {
        let tmp = TempDir::new().unwrap();
        let products = target("products");
        let users = target("users");
        let first = PublicationPaths::new(tmp.path(), &products, &transaction("txn_001"));
        let second = PublicationPaths::new(tmp.path(), &products, &transaction("txn_002"));
        let other = PublicationPaths::new(tmp.path(), &users, &transaction("txn_001"));

        assert_eq!(
            first.epoch_path(),
            tmp.path().join(".publication/products/epoch")
        );
        assert_eq!(
            first.epoch_temp_path(),
            tmp.path().join(".publication/products/epoch.tmp")
        );
        assert_eq!(
            first.epoch_lock_path(),
            tmp.path().join(".publication/products/epoch.lock")
        );
        assert_eq!(first.epoch_path(), second.epoch_path());
        assert_eq!(first.epoch_temp_path(), second.epoch_temp_path());
        assert_eq!(first.epoch_lock_path(), second.epoch_lock_path());
        assert_ne!(first.epoch_path(), other.epoch_path());
        assert_ne!(first.epoch_lock_path(), other.epoch_lock_path());
    }

    #[test]
    fn missing_epoch_reads_as_initial_zero() {
        let tmp = TempDir::new().unwrap();

        assert_eq!(
            read_publication_epoch(tmp.path(), &target("products")).unwrap(),
            PublicationEpoch(0)
        );
    }

    #[test]
    fn missing_epoch_with_lock_residue_still_reads_zero_but_observes_sidecar_state() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");
        let paths = publication_epoch_paths_for_target_path(&tmp.path().join("products"));
        fs::create_dir_all(paths.lock.parent().unwrap()).unwrap();
        fs::write(&paths.lock, b"").unwrap();

        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(0)
        );
        assert_eq!(
            observe_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpochObservation::AbsentWithSidecars
        );
    }

    #[test]
    fn epoch_reader_rejects_noncanonical_or_overflowing_state_with_typed_error() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");
        let paths = publication_epoch_paths_for_target_path(&tmp.path().join("products"));
        fs::create_dir_all(paths.epoch.parent().unwrap()).unwrap();

        for bytes in [
            b"".as_slice(),
            b"-1".as_slice(),
            b" 1".as_slice(),
            b"1 ".as_slice(),
            b"1\n".as_slice(),
            b"01".as_slice(),
            b"1x".as_slice(),
            &[0xff, b'1'],
            b"18446744073709551616".as_slice(),
        ] {
            fs::write(&paths.epoch, bytes).unwrap();
            match read_publication_epoch(tmp.path(), &target) {
                Err(PublicationEpochError::CorruptState { path }) => {
                    assert_eq!(path, paths.epoch);
                }
                other => panic!("expected corrupt epoch state for {bytes:?}, got {other:?}"),
            }
        }

        fs::write(&paths.epoch, b"18446744073709551615").unwrap();
        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(u64::MAX)
        );
    }

    #[cfg(unix)]
    #[test]
    fn epoch_reader_rejects_oversized_state_before_loading_contents() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let target = target("products");
        let paths = publication_epoch_paths_for_target_path(&tmp.path().join("products"));
        fs::create_dir_all(paths.epoch.parent().unwrap()).unwrap();
        fs::write(&paths.epoch, b"184467440737095516150").unwrap();
        fs::set_permissions(&paths.epoch, fs::Permissions::from_mode(0o000)).unwrap();

        let result = read_publication_epoch(tmp.path(), &target);

        fs::set_permissions(&paths.epoch, fs::Permissions::from_mode(0o600)).unwrap();
        match result {
            Err(PublicationEpochError::CorruptState { path }) => {
                assert_eq!(path, paths.epoch);
            }
            other => {
                panic!("expected oversized epoch state to fail closed as corrupt, got {other:?}")
            }
        }
    }

    #[test]
    fn epoch_io_rejects_symlinked_managed_components_without_external_mutation() {
        let tmp = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".publication")).unwrap();
        symlink_dir(external.path(), tmp.path().join(".publication/products")).unwrap();

        match read_publication_epoch(tmp.path(), &target("products")) {
            Err(PublicationEpochError::Io { path, source }) => {
                assert_eq!(path, tmp.path().join(".publication/products/epoch"));
                assert_eq!(source.kind(), io::ErrorKind::InvalidInput);
            }
            other => panic!("expected symlink rejection for epoch read, got {other:?}"),
        }

        match compare_and_advance_publication_epoch(
            tmp.path(),
            &target("products"),
            PublicationEpoch(0),
        ) {
            Err(PublicationEpochError::Io { path, source }) => {
                assert_eq!(path, tmp.path().join(".publication/products/epoch.lock"));
                assert_eq!(source.kind(), io::ErrorKind::InvalidInput);
            }
            other => panic!("expected symlink rejection for lock open, got {other:?}"),
        }

        assert!(!external.path().join("epoch").exists());
        assert!(!external.path().join("epoch.tmp").exists());
        assert!(!external.path().join("epoch.lock").exists());
    }

    #[test]
    fn epoch_advance_persists_monotonic_value_and_survives_reopen() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");

        let guard = compare_and_advance_publication_epoch(tmp.path(), &target, PublicationEpoch(0))
            .unwrap();
        assert_eq!(guard.previous(), PublicationEpoch(0));
        assert_eq!(guard.advanced(), PublicationEpoch(1));
        drop(guard);

        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(1)
        );
        let guard = compare_and_advance_publication_epoch(tmp.path(), &target, PublicationEpoch(1))
            .unwrap();
        assert_eq!(guard.previous(), PublicationEpoch(1));
        assert_eq!(guard.advanced(), PublicationEpoch(2));
    }

    #[test]
    fn epoch_advance_rejects_stale_expected_value_without_mutation() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");
        drop(compare_and_advance_publication_epoch(
            tmp.path(),
            &target,
            PublicationEpoch(0),
        ));

        match compare_and_advance_publication_epoch(tmp.path(), &target, PublicationEpoch(0)) {
            Err(PublicationEpochError::ExpectedMismatch { expected, actual }) => {
                assert_eq!(expected, PublicationEpoch(0));
                assert_eq!(actual, PublicationEpoch(1));
            }
            other => panic!("expected stale epoch mismatch, got {other:?}"),
        }
        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(1)
        );
    }

    #[test]
    fn concurrent_epoch_advances_have_exactly_one_winner() {
        let tmp = TempDir::new().unwrap();
        let base = Arc::new(tmp.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(2));
        let (tx, rx) = mpsc::channel();

        for _ in 0..2 {
            let base = Arc::clone(&base);
            let barrier = Arc::clone(&barrier);
            let tx = tx.clone();
            std::thread::spawn(move || {
                let target = target("products");
                barrier.wait();
                tx.send(compare_and_advance_publication_epoch(
                    &base,
                    &target,
                    PublicationEpoch(0),
                ))
                .unwrap();
            });
        }
        drop(tx);

        let mut winners = 0;
        let mut stale = 0;
        for _ in 0..2 {
            let result = rx.recv().unwrap();
            match result {
                Ok(guard) => {
                    assert_eq!(guard.previous(), PublicationEpoch(0));
                    assert_eq!(guard.advanced(), PublicationEpoch(1));
                    winners += 1;
                    drop(guard);
                }
                Err(PublicationEpochError::ExpectedMismatch { expected, actual }) => {
                    assert_eq!(expected, PublicationEpoch(0));
                    assert_eq!(actual, PublicationEpoch(1));
                    stale += 1;
                }
                other => panic!("unexpected concurrent advance result: {other:?}"),
            }
        }
        assert_eq!(winners, 1);
        assert_eq!(stale, 1);
        assert_eq!(
            read_publication_epoch(&base, &target("products")).unwrap(),
            PublicationEpoch(1)
        );
    }

    #[test]
    fn epoch_fence_stays_exclusive_until_returned_guard_is_dropped() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().to_path_buf();
        let products = target("products");
        let guard =
            compare_and_advance_publication_epoch(&base, &products, PublicationEpoch(0)).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let base_for_thread = base.clone();

        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = compare_and_advance_publication_epoch(
                &base_for_thread,
                &target("products"),
                PublicationEpoch(1),
            );
            done_tx.send(result.map(|guard| guard.advanced())).unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(guard);

        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            PublicationEpoch(2)
        );
        handle.join().unwrap();
        assert_eq!(
            read_publication_epoch(&base, &target("products")).unwrap(),
            PublicationEpoch(2)
        );
    }

    #[test]
    fn epoch_advance_rejects_u64_overflow_without_mutation() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");
        let paths = publication_epoch_paths_for_target_path(&tmp.path().join("products"));
        fs::create_dir_all(paths.epoch.parent().unwrap()).unwrap();
        fs::write(&paths.epoch, u64::MAX.to_string()).unwrap();

        match compare_and_advance_publication_epoch(tmp.path(), &target, PublicationEpoch(u64::MAX))
        {
            Err(PublicationEpochError::Overflow { current }) => {
                assert_eq!(current, PublicationEpoch(u64::MAX));
            }
            other => panic!("expected overflow rejection, got {other:?}"),
        }
        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(u64::MAX)
        );
    }

    #[test]
    fn epoch_advance_io_failure_returns_typed_error_without_success() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");
        let paths = publication_epoch_paths_for_target_path(&tmp.path().join("products"));
        fs::create_dir_all(&paths.temp).unwrap();

        match compare_and_advance_publication_epoch(tmp.path(), &target, PublicationEpoch(0)) {
            Err(PublicationEpochError::Io { path, source }) => {
                assert_eq!(path, paths.temp);
                assert!(matches!(
                    source.kind(),
                    io::ErrorKind::IsADirectory | io::ErrorKind::PermissionDenied
                ));
            }
            other => panic!("expected temp write I/O failure, got {other:?}"),
        }
        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(0)
        );
    }

    #[cfg(unix)]
    fn symlink_dir(target: &Path, link: PathBuf) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_dir(target: &Path, link: PathBuf) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }
}
