//! RED specimens for the bounded source identity partitioner.

use super::source_identity_partitions::{
    compare_receipt, IdentityComparisonError, SourceIdentityConfig, SourceIdentityError,
    SourceIdentityPartitions, SourceIdentityReceipt, SourceIdentityVersion,
};
use super::source_snapshot::update_source_item_hash_digest;
use serde_json::json;
use serial_test::serial;
use sha2::{Digest, Sha256};
use std::{ffi::OsString, fs::OpenOptions, io::Write, mem};
use tempfile::TempDir;

fn config_for_test(
    spool_root: &TempDir,
    budget_bytes: usize,
    certified_max_items: u64,
) -> SourceIdentityConfig {
    SourceIdentityConfig::for_test(spool_root.path(), budget_bytes, certified_max_items)
}

fn record_documents(
    partitions: &mut SourceIdentityPartitions,
    count: usize,
) -> Result<(), SourceIdentityError> {
    for index in 0..count {
        let object_id = format!("doc-{index:04}");
        let item_hash = format!("{index:064x}");
        partitions.record(&object_id, &item_hash, index / 64, index % 64)?;
    }
    Ok(())
}

struct EnvironmentVariableGuard {
    name: &'static str,
    original_value: Option<OsString>,
}

impl EnvironmentVariableGuard {
    fn remove(name: &'static str) -> Self {
        let original_value = std::env::var_os(name);
        std::env::remove_var(name);
        Self {
            name,
            original_value,
        }
    }
}

impl Drop for EnvironmentVariableGuard {
    fn drop(&mut self) {
        match &self.original_value {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[test]
fn oversized_tuple_is_rejected_before_allocating_owned_payloads() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let config = config_for_test(&spool_root, 1024, 8);
    let mut partitions =
        SourceIdentityPartitions::new(config).expect("test partitioner should initialize");

    let error = partitions
        .record(&"x".repeat(1025), "hash", 0, 0)
        .expect_err("oversized tuple must be rejected");

    assert!(matches!(
        error,
        SourceIdentityError::PartitionBudgetExceeded { .. }
    ));
    assert_eq!(
        partitions.tuple_allocations_for_test(),
        0,
        "rejected input must not be cloned into an owned tuple"
    );
}

#[test]
#[serial]
fn implicit_temp_spool_root_is_removed_with_validator() {
    let _spool_dir_guard =
        EnvironmentVariableGuard::remove("FLAPJACK_MIGRATION_IDENTITY_SPOOL_DIR");
    let _budget_guard =
        EnvironmentVariableGuard::remove("FLAPJACK_MIGRATION_IDENTITY_BUDGET_BYTES");
    let config = SourceIdentityConfig::from_env().expect("default config should be valid");
    let implicit_spool_root = config.spool_root.clone();
    let partitions =
        SourceIdentityPartitions::new(config).expect("default partitioner should initialize");

    assert!(implicit_spool_root.is_dir());
    drop(partitions);
    assert!(
        !implicit_spool_root.exists(),
        "validator must remove the implicit temp root it owns"
    );
}

#[test]
fn partition_buffer_metadata_must_fit_the_identity_budget() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let production_scale_config = config_for_test(&spool_root, 1, 64_000_000);
    let config_error = production_scale_config
        .validate()
        .expect_err("one-byte budget must be rejected without allocating partition buffers");

    assert_eq!(
        config_error,
        SourceIdentityError::InvalidConfig {
            name: "FLAPJACK_MIGRATION_IDENTITY_BUDGET_BYTES",
        }
    );
    assert!(config_error.is_infrastructure());

    let bounded_red_specimen = config_for_test(&spool_root, 1024, 1024);
    let new_error = SourceIdentityPartitions::new(bounded_red_specimen)
        .expect_err("partition metadata larger than the budget must be rejected");
    assert_eq!(new_error, config_error);
    assert!(
        spool_root
            .path()
            .read_dir()
            .expect("spool root should remain readable")
            .next()
            .is_none(),
        "invalid config must fail before creating an instance spool directory"
    );
}

#[test]
fn source_item_hash_encoding_updates_digest_without_a_preimage_buffer() {
    let mut actual = Sha256::new();
    update_source_item_hash_digest(&mut actual, "doc-1", "item-hash");

    let actual: [u8; 32] = actual.finalize().into();
    let expected: [u8; 32] = Sha256::digest(b"doc-1\0item-hash\n").into();
    assert_eq!(actual, expected);
}

#[test]
fn validator_budget_includes_buffer_metadata_and_owned_tuple_allocations() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let config = config_for_test(&spool_root, 4096, 1024);
    let mut partitions =
        SourceIdentityPartitions::new(config).expect("test partitioner should initialize");

    partitions
        .record("doc-0000", &"0".repeat(64), 0, 0)
        .expect("fixture tuple should fit");
    let outcome = partitions.finish().expect("fixture should finish");

    // 128 partition-head pointers + one three-word tuple node + its exact spool encoding:
    // four u64 fields, the 8-byte object ID, and the 64-byte item hash.
    let partition_heads = 128 * mem::size_of::<usize>();
    let tuple_node = 3 * mem::size_of::<usize>();
    let encoded_tuple = 4 * mem::size_of::<u64>() + 8 + 64;
    assert_eq!(
        outcome.max_resident_bytes_observed(),
        partition_heads + tuple_node + encoded_tuple
    );
}

#[test]
fn phase_two_budget_includes_spool_bytes_and_tuple_offsets() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let config = config_for_test(&spool_root, 4096, 8);
    let mut partitions =
        SourceIdentityPartitions::new(config).expect("test partitioner should initialize");

    record_documents(&mut partitions, 27).expect("phase-one representation should fit");
    let error = partitions
        .finish()
        .expect_err("phase-two offsets must share the partition budget with spool bytes");

    // Each tuple occupies 104 spool bytes (four u64 fields + 8-byte ID + 64-byte hash)
    // and six usize fields in the sortable offset table.
    let expected_bytes = 27 * (104 + 6 * mem::size_of::<usize>());
    assert_eq!(
        error,
        SourceIdentityError::PartitionBudgetExceeded {
            partition: 0,
            bytes: expected_bytes,
            budget_bytes: 4096,
        }
    );
}

#[test]
fn complete_v2_digest_matches_known_answer() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let config = config_for_test(&spool_root, 4096, 8);
    let mut partitions =
        SourceIdentityPartitions::new(config).expect("test partitioner should initialize");

    partitions
        .record("beta", "hash-beta", 0, 0)
        .expect("beta should record");
    partitions
        .record("alpha", "hash-alpha", 0, 1)
        .expect("alpha should record");
    let outcome = partitions.finish().expect("fixture should finish");

    assert_eq!(outcome.partition_count, 1);
    assert_eq!(
        outcome.digest,
        "f4ba6fe05506737e23999d4fa49acd6a8f148c6b7109e9a4fd5f40f26c0bfa7a"
    );
}

#[test]
fn compare_receipt_covers_version_precedence_digest_mismatch_and_match() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let config = config_for_test(&spool_root, 4096, 8);
    let mut partitions =
        SourceIdentityPartitions::new(config).expect("test partitioner should initialize");
    partitions
        .record("alpha", "hash-alpha", 0, 0)
        .expect("fixture should record");
    let current = partitions.finish().expect("fixture should finish");

    let stale_version_and_digest = SourceIdentityReceipt {
        version: SourceIdentityVersion::V1,
        digest: "not-the-current-digest".to_string(),
    };
    assert_eq!(
        compare_receipt(&stale_version_and_digest, &current),
        Err(IdentityComparisonError::VersionMismatch {
            receipt: SourceIdentityVersion::V1,
            current: SourceIdentityVersion::V2,
        })
    );

    let stale_digest = SourceIdentityReceipt {
        version: SourceIdentityVersion::V2,
        digest: "not-the-current-digest".to_string(),
    };
    assert_eq!(
        compare_receipt(&stale_digest, &current),
        Err(IdentityComparisonError::DigestMismatch)
    );

    let matching_receipt = SourceIdentityReceipt {
        version: SourceIdentityVersion::V2,
        digest: current.digest.clone(),
    };
    assert_eq!(compare_receipt(&matching_receipt, &current), Ok(()));
}

#[test]
fn duplicate_validator_is_exact_across_hash_partitions() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let config = config_for_test(&spool_root, 4096, 1024);
    let mut partitions =
        SourceIdentityPartitions::new(config).expect("test partitioner should initialize");

    for index in 0..320 {
        let object_id = format!("unique-{index:04}");
        let item_hash = format!("{:064x}", index + 17);
        partitions
            .record(&object_id, &item_hash, index / 40, index % 40)
            .expect("unique ids should be accepted");
    }
    partitions
        .record("tenant-secret-0042", "hash_a", 0, 3)
        .expect("first duplicate occurrence should be accepted");
    for index in 320..640 {
        let object_id = format!("unique-{index:04}");
        let item_hash = format!("{:064x}", index + 17);
        partitions
            .record(&object_id, &item_hash, index / 40, index % 40)
            .expect("unique ids should be accepted");
    }
    partitions
        .record("tenant-secret-0042", "hash_b", 7, 0)
        .expect("duplicate is reported at finish so both positions are retained");

    let error = partitions
        .finish()
        .expect_err("duplicate objectID must fail exactly");

    match &error {
        SourceIdentityError::Duplicate { first, second } => {
            assert_eq!(*first, (0, 3));
            assert_eq!(*second, (7, 0));
        }
        other => panic!("expected duplicate error, got {other:?}"),
    }
    assert!(!error.safe_message().contains("tenant-secret-0042"));
}

#[test]
fn validator_peak_memory_is_bounded_by_partition_not_corpus() {
    // budget_bytes = 4096, certified_max_items = 1024 -> 32 planning tuples
    // -> ceil(1024*4/32) = 128 partitions; fixture tuple = 8 + 64 + 16 = 88 bytes
    // -> floor(4096/88) = 46 resident tuples max.
    let run_a_root = TempDir::new().expect("temp spool root should be created");
    let run_b_root = TempDir::new().expect("temp spool root should be created");
    let config_a = config_for_test(&run_a_root, 4096, 1024);
    let config_b = config_for_test(&run_b_root, 4096, 1024);
    let mut run_a_partitions =
        SourceIdentityPartitions::new(config_a).expect("run A partitioner should initialize");
    let mut run_b_partitions =
        SourceIdentityPartitions::new(config_b).expect("run B partitioner should initialize");

    assert_eq!(run_b_partitions.partition_count(), 128);

    record_documents(&mut run_a_partitions, 256).expect("run A documents should record");
    record_documents(&mut run_b_partitions, 1024).expect("run B documents should record");

    let run_a = run_a_partitions
        .finish()
        .expect("run A should finish without duplicates");
    let run_b = run_b_partitions
        .finish()
        .expect("run B should finish without duplicates");

    assert!(run_a.max_resident_bytes_observed() <= 4096);
    assert!(run_b.max_resident_bytes_observed() <= 4096);
    assert!(run_b.max_resident_tuples_observed() <= 46);
    // Residency accessors live on the consumed SourceIdentityOutcome so phase-2 residency is observable.
    assert_eq!(run_b.count, 1024);
    assert_eq!(run_b.digest.len(), 64);
    assert!(run_b.max_resident_tuples_observed() >= 2);
}

#[test]
fn identity_infrastructure_error_is_not_reported_as_schema_violation() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let config = config_for_test(&spool_root, 4096, 8);
    let mut partitions =
        SourceIdentityPartitions::new(config).expect("test partitioner should initialize");

    record_documents(&mut partitions, 200).expect("documents should record before phase-2 load");

    let error = partitions
        .finish()
        .expect_err("oversized partition must be an infrastructure failure");

    match error {
        SourceIdentityError::PartitionBudgetExceeded { partition, .. } => {
            assert_eq!(partition, 0);
            assert!(error.is_infrastructure());
        }
        SourceIdentityError::Duplicate { .. }
        | SourceIdentityError::InvalidObjectId { .. }
        | SourceIdentityError::MalformedPayload { .. } => {
            panic!("expected infrastructure error, got schema error {error:?}");
        }
        other => panic!("expected partition budget error, got {other:?}"),
    }
}

#[test]
fn validator_rejects_missing_or_malformed_stable_id() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let config = config_for_test(&spool_root, 4096, 1024);
    let mut partitions =
        SourceIdentityPartitions::new(config).expect("test partitioner should initialize");

    let invalid_items = [
        (
            json!(7),
            SourceIdentityError::MalformedPayload {
                page_index: 0,
                item_index: 0,
            },
            0,
            0,
        ),
        (
            json!({"title": "missing"}),
            SourceIdentityError::InvalidObjectId {
                page_index: 1,
                item_index: 2,
            },
            1,
            2,
        ),
        (
            json!({"objectID": 7}),
            SourceIdentityError::InvalidObjectId {
                page_index: 3,
                item_index: 4,
            },
            3,
            4,
        ),
        (
            json!({"objectID": ""}),
            SourceIdentityError::InvalidObjectId {
                page_index: 5,
                item_index: 6,
            },
            5,
            6,
        ),
    ];

    for (value, expected_error, page_index, item_index) in invalid_items {
        let error = partitions
            .record_item(&value, page_index, item_index)
            .expect_err("invalid stable id must return an error");

        assert_eq!(error, expected_error);
    }

    let outcome = partitions
        .finish()
        .expect("invalid items should not be silently recorded");

    assert_eq!(outcome.count, 0);
}

#[test]
fn newline_in_object_id_survives_spool_round_trip() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let config = config_for_test(&spool_root, 4096, 1024);
    let mut partitions =
        SourceIdentityPartitions::new(config).expect("test partitioner should initialize");

    partitions
        .record_item(&json!({"objectID": "tenant\nsecret", "revision": 1}), 2, 3)
        .expect("first newline-bearing objectID should record");
    partitions
        .record_item(&json!({"objectID": "tenant\nsecret", "revision": 2}), 5, 8)
        .expect("duplicate newline-bearing objectID should record");

    let error = partitions
        .finish()
        .expect_err("duplicate newline-bearing objectID must be detected");

    assert_eq!(
        error,
        SourceIdentityError::Duplicate {
            first: (2, 3),
            second: (5, 8),
        }
    );
}

#[test]
fn phase_two_budget_is_enforced_before_reading_later_bytes() {
    let spool_root = TempDir::new().expect("temp spool root should be created");
    let config = config_for_test(&spool_root, 4096, 8);
    let mut partitions =
        SourceIdentityPartitions::new(config).expect("test partitioner should initialize");

    record_documents(&mut partitions, 47).expect("documents should record");
    let oversized_object_id = "x".repeat(4097);
    let flush_error = partitions
        .record(&oversized_object_id, "hash", 1, 0)
        .expect_err("oversized tuple should flush pending records and fail");
    assert!(matches!(
        flush_error,
        SourceIdentityError::PartitionBudgetExceeded { .. }
    ));

    let partition_path = partitions.partition_path_for_test(0);
    OpenOptions::new()
        .append(true)
        .open(partition_path)
        .and_then(|mut file| file.write_all(&[0xff]))
        .expect("corrupt test tail should be appended");

    let error = partitions
        .finish()
        .expect_err("resident tuple budget must fail before a later corrupt tail is read");

    assert!(matches!(
        error,
        SourceIdentityError::PartitionBudgetExceeded { partition: 0, .. }
    ));
}
