use super::*;
use crate::{Document, IndexManager};

#[test]
fn selected_boundary_operation_remains_pending_before_pause_report() {
    let tmp = tempfile::TempDir::new().unwrap();
    let from = tmp.path().join("journal.json.tmp");
    let to = tmp.path().join("journal.json");
    fs::write(&from, b"prepared").unwrap();

    let hook = PausingFaultHook::new(
        "case".to_string(),
        ActivationKind::Create,
        tmp.path().to_path_buf(),
        "create|rename:journal.json.tmp->journal.json|2".to_string(),
        tmp.path().join("pause.json"),
    );
    hook.before_operation(&PublicationOperation::Rename {
        from: from.clone(),
        to: to.clone(),
    })
    .unwrap();

    assert!(
        from.exists(),
        "the selected rename must remain pending when the pre-operation hook returns"
    );
    assert!(
        !to.exists(),
        "the pre-operation hook must not materialize the selected rename itself"
    );
}

#[test]
fn generated_layout_index_preserves_manifest_oracle_data() {
    let scenario = manifest_with_base("base_001_create")
        .scenarios
        .into_iter()
        .next()
        .unwrap();
    let layout = GeneratedLayout::from_scenario(&scenario, vec![
        "create|sync_dir:.publication|1".to_string(),
    ]);

    assert_eq!(layout.scenario_id, scenario.id);
    assert_eq!(layout.tenant.as_deref(), Some("products"));
    assert_eq!(layout.transaction.as_deref(), Some("txn_001"));
    assert_eq!(layout.journal_phase.as_deref(), Some("prepared"));
    assert_eq!(layout.disposition, "commit");
    assert_eq!(layout.cli.status, "clean");
    assert_eq!(layout.visible.object, "new-meta");
    assert_eq!(layout.residue.journal, "present");
    assert_eq!(
        layout.digests.get("target").map(String::as_str),
        Some("sha256:35820c78a8b1cb061ab3b7356634b956cb18ca51479d1c0a1fe96ea6c6c6acf7")
    );
}

#[test]
fn generated_base_layout_parses_materialized_journal_identity() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manifest = load_scenario_manifest();
    let scenario = manifest
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "base_001_create")
        .unwrap();

    generate_base_scenario(tmp.path(), scenario);

    assert_materialized_journal_matches_manifest_identity(
        &case_root_for(tmp.path(), &scenario.id),
        scenario,
    )
    .unwrap();
}

#[test]
fn generated_base_layout_cli_oracle_matches_scanner_report() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manifest = load_scenario_manifest();
    let scenario = manifest
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "base_001_create")
        .unwrap();

    generate_base_scenario(tmp.path(), scenario);

    let case_root = case_root_for(tmp.path(), &scenario.id);
    let report = scan_and_repair_publication_target(
        &case_root,
        &AnalyticsConfig::for_data_dir(&case_root),
        PublicationTarget::new(scenario.tenant.as_deref().unwrap_or("products")).unwrap(),
    )
    .unwrap();

    assert_eq!(report.status.as_str(), scenario.cli.status);
    assert_eq!(report.action.as_str(), scenario.cli.action);
}

#[test]
fn replacement_commit_oracle_distinguishes_fixture_and_clean_report_phase() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manifest = load_scenario_manifest();
    let scenario = manifest
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "base_019_replacement")
        .unwrap();

    assert_eq!(scenario.journal_phase.as_deref(), Some("prepared"));
    assert_eq!(scenario.clean_report_phase.as_deref(), Some("committed"));

    generate_base_scenario(tmp.path(), scenario);

    let case_root = case_root_for(tmp.path(), &scenario.id);
    let first_report = scan_and_repair_publication_target(
        &case_root,
        &AnalyticsConfig::for_data_dir(&case_root),
        PublicationTarget::new(scenario.tenant.as_deref().unwrap_or("products")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        first_report.phase.map(|phase| phase.as_str()),
        scenario.journal_phase.as_deref()
    );

    let second_report = scan_and_repair_publication_target(
        &case_root,
        &AnalyticsConfig::for_data_dir(&case_root),
        PublicationTarget::new(scenario.tenant.as_deref().unwrap_or("products")).unwrap(),
    )
    .unwrap();

    assert_eq!(
        second_report.phase.map(|phase| phase.as_str()),
        scenario.clean_report_phase.as_deref()
    );
}

#[test]
fn generated_base_layout_checks_manifest_owner_oracles() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manifest = load_scenario_manifest();
    let scenario = manifest
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "base_020_replacement")
        .unwrap();

    let layout = generate_base_scenario(tmp.path(), scenario);

    assert_generated_case_matches_manifest(
        &case_root_for(tmp.path(), &scenario.id),
        scenario,
        &layout,
    )
    .unwrap();
}

#[test]
fn manifest_source_oracle_guard_rejects_recomputed_digest_values() {
    let mut manifest = manifest_with_base("base_001_create");
    let scenario = manifest.scenarios.first_mut().unwrap();
    scenario
        .digests
        .insert("new".to_string(), "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff".to_string());

    let error = assert_manifest_source_oracles(scenario)
        .expect_err("source digest oracles must be owner-computed manifest values");

    assert!(error.contains("new digest"), "{error}");
}

#[test]
fn generated_layout_keeps_manifest_oracles_separate_from_filesystem_state() {
    let mut manifest = manifest_with_base("base_001_create");
    let scenario = manifest.scenarios.first_mut().unwrap();
    scenario.cli.status = "sentinel_status".to_string();
    scenario.residue.staging = "sentinel_residue".to_string();
    scenario
        .digests
        .insert("target".to_string(), "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string());

    let layout = GeneratedLayout::from_scenario(
        scenario,
        vec!["create|sync_dir:.publication|1".to_string()],
    );

    assert_eq!(layout.cli.status, "sentinel_status");
    assert_eq!(layout.residue.staging, "sentinel_residue");
    assert_eq!(
        layout.digests.get("target").map(String::as_str),
        Some("sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")
    );
}

#[test]
fn generated_layout_preserves_visible_and_exit_code_oracles() {
    let manifest = manifest_with_base("base_001_create");
    let scenario = manifest.scenarios.first().unwrap();
    let mut layout = GeneratedLayout::from_scenario(
        scenario,
        vec!["create|sync_dir:.publication|1".to_string()],
    );
    layout.visible.object = "rewritten".to_string();
    layout.cli.exit_code += 1;

    let error = assert_layout_preserves_manifest_oracles(scenario, &layout)
        .expect_err("generated layout must not rewrite manifest-owned visibility or exit code");

    assert!(error.contains("oracle was rewritten"), "{error}");
}

#[tokio::test]
async fn generated_base_layout_materializes_control_index_for_http_projection() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manifest = load_scenario_manifest();
    let scenario = manifest
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "base_020_replacement")
        .unwrap();

    generate_base_scenario(tmp.path(), scenario);

    let fixture = &manifest.live_http_fixture;
    assert_generator_fixture_tree(
        &case_root_for(tmp.path(), &scenario.id),
        &fixture.control_index,
        &fixture.control_object.object_id,
        index_manager_document_body(
            &fixture.control_object.body,
            &fixture.control_object.object_id,
        ),
        &fixture.control_query.text,
        &fixture
            .control_query
            .ordered_hit_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
}

#[tokio::test]
async fn generated_loadable_layout_rejects_unqueryable_target() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manifest = load_scenario_manifest();
    let scenario = manifest
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "base_020_replacement")
        .unwrap();
    let layout = generate_base_scenario(tmp.path(), scenario);
    let case_root = case_root_for(tmp.path(), &scenario.id);
    let staging = case_root.join(".publication/products/txn_001/staging");
    std::fs::remove_dir_all(&staging).unwrap();
    std::fs::create_dir(&staging).unwrap();

    let error = assert_generated_case_matches_manifest(&case_root, scenario, &layout)
        .expect_err("loadable manifest cases must fail when the target cannot query");

    assert!(error.contains("query"), "{error}");
}

#[test]
fn worker_generation_uses_owner_activation_seam_and_test_binary() {
    let tmp = tempfile::TempDir::new().unwrap();
    let report_path = tmp.path().join("pause.json");
    let command = worker_command(
        tmp.path(),
        "case",
        "create|sync_dir:.publication|1",
        &report_path,
    );

    assert_eq!(
        command.get_program(),
        std::env::current_exe().unwrap().as_os_str()
    );
    assert!(
        command
            .get_args()
            .any(|arg| arg == "publication_repair_cli_generates_owner_authentic_layouts")
    );
    assert!(
        command
            .get_args()
            .all(|arg| !arg.to_string_lossy().contains("repair-publication"))
    );

    let fixture = ActivationFixture::new_at(tmp.path().join("worker_case"));
    let hook = PublicationFaultScript::recording();
    materialize_worker_layout(&fixture, ActivationKind::Create, &hook).unwrap();
    assert!(
        !hook.operations().is_empty(),
        "worker generation must invoke activate_publication_with_faults_for_test"
    );
}

#[tokio::test]
async fn generator_activation_fixture_reopens_old_new_and_control_indexes() {
    let fixture = ActivationFixture::new();
    fixture.write_old_target();
    fixture.write_new_staging();
    write_generator_control_index(fixture.base()).await;

    assert_generator_fixture_tree(
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
    assert_generator_fixture_tree(
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
    assert_generator_fixture_tree(
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

#[test]
fn generator_activation_fixture_fresh_generations_are_inventory_and_digest_stable() {
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

async fn write_generator_control_index(base: &Path) {
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

fn assert_generator_fixture_tree(
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

#[test]
fn ambiguous_target_and_staging_mutation_materializes_both_trees() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manifest = load_scenario_manifest();
    let base = manifest
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "base_020_replacement")
        .unwrap();
    let mutation = manifest
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "mutation_ambiguous_target_and_staging")
        .unwrap();

    generate_base_scenario(tmp.path(), base);
    generate_mutation_scenario(tmp.path(), mutation);

    let mutation_root = case_root_for(tmp.path(), &mutation.id);
    let fixture = ActivationFixture::new_at(mutation_root);
    assert!(
        fixture.paths.target.exists(),
        "ambiguous mutation must materialize the live target side"
    );
    assert!(
        fixture.paths.staging.exists(),
        "ambiguous mutation must preserve the staged target side"
    );
}

#[test]
fn ambiguous_target_and_staging_manifest_expects_sidecar_backups() {
    let manifest = load_scenario_manifest();
    let mutation = manifest
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "mutation_ambiguous_target_and_staging")
        .unwrap();

    for (name, values) in &mutation.sidecars {
        assert_eq!(
            values.get("backup"),
            values.get("old"),
            "{name} backup must preserve the original sidecar bytes during quarantine"
        );
    }
}

#[test]
fn corrupt_journal_manifest_expects_absent_sidecar_targets() {
    let manifest = load_scenario_manifest();
    let mutation = manifest
        .scenarios
        .iter()
        .find(|scenario| scenario.id == "mutation_corrupt_journal")
        .unwrap();

    assert_eq!(mutation.visible.target, "absent");
    for (name, values) in &mutation.sidecars {
        assert_eq!(
            values.get("target").map(String::as_str),
            Some("absent"),
            "{name} target sidecar must be absent when corrupt journal repair leaves no visible target"
        );
    }
}

#[test]
fn pause_report_validation_rejects_pid_and_identity_mismatches() {
    let valid = PauseReport {
        pid: 42,
        case_id: "case".to_string(),
        boundary: "boundary".to_string(),
    };
    assert!(validate_pause_report(&valid, 42, "case", "boundary").is_ok());

    let mut missing_pid = valid.clone();
    missing_pid.pid = 0;
    let error = validate_pause_report(&missing_pid, 42, "case", "boundary").unwrap_err();
    assert!(error.contains("missing worker PID"), "{error}");

    let stale_pid = PauseReport {
        pid: 99,
        ..valid.clone()
    };
    let error = validate_pause_report(&stale_pid, 42, "case", "boundary").unwrap_err();
    assert!(error.contains("worker PID mismatch"), "{error}");

    let error = validate_pause_report(&valid, 24, "case", "boundary").unwrap_err();
    assert!(error.contains("worker PID mismatch"), "{error}");

    let error = validate_pause_report(&valid, 42, "other", "boundary").unwrap_err();
    assert!(error.contains("worker case mismatch"), "{error}");

    let error = validate_pause_report(&valid, 42, "case", "other").unwrap_err();
    assert!(error.contains("worker boundary mismatch"), "{error}");
}

#[test]
fn pause_report_parser_rejects_duplicate_payloads() {
    let raw = r#"{"pid":1,"case_id":"case","boundary":"one"}
{"pid":1,"case_id":"case","boundary":"two"}"#;

    let error = parse_pause_report(raw).unwrap_err();

    assert!(error.contains("failed to parse pause report"), "{error}");
}

#[test]
fn wait_for_pause_report_uses_bounded_timeout() {
    let tmp = tempfile::TempDir::new().unwrap();
    let report_path = tmp.path().join("pause.json");
    let mut child = Command::new("/bin/sleep").arg("2").spawn().unwrap();

    let error =
        wait_for_pause_report_with_timeout(&mut child, &report_path, Duration::from_millis(25))
            .unwrap_err();

    child.kill().ok();
    child.wait().ok();
    assert!(error.contains("timed out waiting for pause report"), "{error}");
}

#[test]
fn wait_for_pause_report_tolerates_partial_report_writes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let report_path = tmp.path().join("pause.json");
    std::fs::write(&report_path, b"").unwrap();
    let mut child = Command::new("/bin/sleep").arg("2").spawn().unwrap();
    let child_pid = child.id();
    let writer_path = report_path.clone();
    let writer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        std::fs::write(
            writer_path,
            serde_json::to_vec(&PauseReport {
                pid: child_pid,
                case_id: "case".to_string(),
                boundary: "boundary".to_string(),
            })
            .unwrap(),
        )
        .unwrap();
    });

    let report =
        wait_for_pause_report_with_timeout(&mut child, &report_path, Duration::from_secs(2))
            .expect("partial pause report writes should be retried until valid JSON appears");

    child.kill().ok();
    child.wait().ok();
    writer.join().unwrap();
    assert_eq!(report.pid, child_pid);
    assert_eq!(report.case_id, "case");
    assert_eq!(report.boundary, "boundary");
}

#[test]
fn generated_layout_index_rejects_duplicate_ids() {
    let generated = vec![
        GeneratedLayout::base("base_001_create", "create|sync_dir:.publication|1"),
        GeneratedLayout::base("base_001_create", "create|sync_dir:.publication|1"),
    ];

    let error = validate_generated_layout_index(&manifest_with_base("base_001_create"), &generated)
        .expect_err("duplicate generated IDs must fail");

    assert!(error.contains("duplicate generated scenario id"), "{error}");
}

#[test]
fn generated_layout_index_rejects_missing_manifest_case() {
    let generated = Vec::new();

    let error = validate_generated_layout_index(&manifest_with_base("base_001_create"), &generated)
        .expect_err("missing generated scenario must fail");

    assert!(error.contains("missing generated layout"), "{error}");
}

#[test]
fn generated_layout_index_rejects_extra_generated_case() {
    let generated = vec![
        GeneratedLayout::base("base_001_create", "create|sync_dir:.publication|1"),
        GeneratedLayout::mutation("extra_case"),
    ];

    let error = validate_generated_layout_index(&manifest_with_base("base_001_create"), &generated)
        .expect_err("extra generated scenario must fail");

    assert!(error.contains("not present in manifest"), "{error}");
}

#[test]
fn generated_layout_index_rejects_missing_or_duplicate_base_boundaries() {
    let missing = vec![GeneratedLayout::mutation("base_001_create")];
    let error = validate_generated_layout_index(&manifest_with_base("base_001_create"), &missing)
        .expect_err("missing base boundary must fail");
    assert!(error.contains("missing generated boundary"), "{error}");

    let duplicate = vec![
        GeneratedLayout::base("base_001_create", "create|sync_dir:.publication|1")
            .with_boundary("create|sync_dir:.publication|1"),
    ];
    let error = validate_generated_layout_index(&manifest_with_base("base_001_create"), &duplicate)
        .expect_err("duplicate base boundary must fail");
    assert!(error.contains("observed 2 times"), "{error}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_missing_fixture() {
    let error = serde_json::from_value::<ScenarioManifest>(serde_json::json!({
        "schema_version": 1,
        "layout_count": 0,
        "scenarios": []
    }))
    .expect_err("live_http_fixture must be required");

    assert!(error.to_string().contains("live_http_fixture"), "{error}");
}

/// Known-answer proof that the manifest highlight oracle is the production float
/// rendering. Rust's `f64` Display keeps the sign of negative zero, never falls back
/// to exponent notation, and prints the shortest round-tripping decimal — so the
/// Python-native renderings (`0`, `1e-07`, the exact binary expansion of `1e300`)
/// that a hand-written mirror produces are all wrong.
#[test]
fn live_http_fixture_expected_highlight_renders_floats_exactly_like_production() {
    let body = serde_json::json!({
        "objectID": "float-widget",
        "integral": 7.0,
        "negative_zero": -0.0,
        "tiny": 1e-7,
        "huge": 1e300
    });

    let rendered = repair_cli_manifest::production_highlight_value_strings(&body)
        .expect("float body must build a document");

    let expected = std::collections::BTreeMap::from([
        ("integral".to_string(), "7".to_string()),
        ("negative_zero".to_string(), "-0".to_string()),
        ("tiny".to_string(), "0.0000001".to_string()),
        ("huge".to_string(), format!("1{}", "0".repeat(300))),
    ]);
    assert_eq!(rendered, expected);
}

/// The oracle must reach every leaf the production highlighter emits, including
/// array items and nested object fields, because the live helper asserts each one.
#[test]
fn live_http_fixture_expected_highlight_covers_nested_and_array_leaves() {
    let body = serde_json::json!({
        "objectID": "nested-widget",
        "tags": ["alpha", 2.5],
        "meta": {"score": -0.0, "label": "beta"}
    });

    let rendered = repair_cli_manifest::production_highlight_value_strings(&body)
        .expect("nested body must build a document");

    let expected = std::collections::BTreeMap::from([
        ("tags[0]".to_string(), "alpha".to_string()),
        ("tags[1]".to_string(), "2.5".to_string()),
        ("meta.label".to_string(), "beta".to_string()),
        ("meta.score".to_string(), "-0".to_string()),
    ]);
    assert_eq!(rendered, expected);
}

#[test]
fn live_http_fixture_expected_highlight_preserves_booleans_and_omits_empty_values() {
    let body = serde_json::json!({
        "objectID": "sparse-widget",
        "flag": true,
        "nothing": null,
        "empty_list": [],
        "empty_object": {},
        "emptied_object": {"flag": false},
        "kept": "value"
    });

    let rendered = repair_cli_manifest::production_highlight_value_strings(&body)
        .expect("sparse body must build a document");

    let expected = std::collections::BTreeMap::from([
        ("emptied_object.flag".to_string(), "false".to_string()),
        ("flag".to_string(), "true".to_string()),
        ("kept".to_string(), "value".to_string()),
    ]);
    assert_eq!(rendered, expected);
}

#[test]
fn live_http_fixture_manifest_contract_rejects_python_native_float_highlight_string() {
    let mut manifest = manifest_with_base("base_001_create");
    manifest.live_http_fixture.target_object.body["tiny"] = serde_json::json!(1e-7);
    manifest
        .live_http_fixture
        .target_object
        .expected_highlight
        .insert("tiny".to_string(), "1e-07".to_string());

    let panic = std::panic::catch_unwind(|| manifest.validate_shape())
        .expect_err("a Python-native float rendering must fail closed");
    let message = panic_message(panic);
    assert!(message.contains("expected_highlight must match"), "{message}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_missing_expected_highlight_entry() {
    let mut manifest = manifest_with_base("base_001_create");
    manifest
        .live_http_fixture
        .control_object
        .expected_highlight
        .remove("generation");

    let panic = std::panic::catch_unwind(|| manifest.validate_shape())
        .expect_err("a missing highlight leaf must fail closed");
    let message = panic_message(panic);
    assert!(message.contains("expected_highlight must match"), "{message}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_extra_expected_highlight_entry() {
    let mut manifest = manifest_with_base("base_001_create");
    manifest
        .live_http_fixture
        .old_target_object
        .expected_highlight
        .insert("phantom".to_string(), "ghost".to_string());

    let panic = std::panic::catch_unwind(|| manifest.validate_shape())
        .expect_err("an unknown highlight leaf must fail closed");
    let message = panic_message(panic);
    assert!(message.contains("expected_highlight must match"), "{message}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_unknown_expectation_key() {
    let mut manifest = manifest_with_base("base_001_create");
    manifest.live_http_fixture.expectations.insert(
        "target_maybe".to_string(),
        repair_cli_manifest::LiveHttpVisibility {
            target: "present".to_string(),
            object: "present".to_string(),
            search: "present".to_string(),
        },
    );

    let panic = std::panic::catch_unwind(|| manifest.validate_shape())
        .expect_err("unknown live HTTP expectation keys must fail closed");
    let message = panic_message(panic);
    assert!(message.contains("expectations must be closed"), "{message}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_unknown_visibility_value() {
    let mut manifest = manifest_with_base("base_001_create");
    manifest
        .live_http_fixture
        .expectations
        .get_mut("target_present")
        .unwrap()
        .object = "maybe".to_string();

    let panic = std::panic::catch_unwind(|| manifest.validate_shape())
        .expect_err("unknown live HTTP visibility values must fail closed");
    let message = panic_message(panic);
    assert!(message.contains("unknown value maybe"), "{message}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_missing_surface_status() {
    let mut manifest = manifest_with_base("base_001_create");
    manifest
        .live_http_fixture
        .surface_statuses
        .remove("object_unavailable");

    let panic = std::panic::catch_unwind(|| manifest.validate_shape())
        .expect_err("missing live HTTP surface status keys must fail closed");
    let message = panic_message(panic);
    assert!(message.contains("surface_statuses must be closed"), "{message}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_inconsistent_surface_status_body() {
    let mut manifest = manifest_with_base("base_001_create");
    manifest
        .live_http_fixture
        .surface_statuses
        .get_mut("search_unavailable")
        .unwrap()
        .body["status"] = serde_json::json!(404);

    let panic = std::panic::catch_unwind(|| manifest.validate_shape())
        .expect_err("surface status body/status mismatches must fail closed");
    let message = panic_message(panic);
    assert!(message.contains("body.status must match status"), "{message}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_missing_target_projection() {
    let mut manifest = manifest_with_base("base_001_create");
    manifest
        .live_http_fixture
        .target_projections
        .remove("old-meta");

    let panic = std::panic::catch_unwind(|| manifest.validate_shape())
        .expect_err("missing target projection keys must fail closed");
    let message = panic_message(panic);
    assert!(message.contains("target_projections must be closed"), "{message}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_unknown_projection_fixture_refs() {
    let mut manifest = manifest_with_base("base_001_create");
    manifest
        .live_http_fixture
        .target_projections
        .get_mut("new-meta")
        .unwrap()
        .object = "legacy_constant".to_string();

    let panic = std::panic::catch_unwind(|| manifest.validate_shape())
        .expect_err("projection refs must resolve to fixture-owned object/query data");
    let message = panic_message(panic);
    assert!(message.contains("unknown object fixture"), "{message}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_unresolved_scenario_projection() {
    let mut manifest = manifest_with_base("base_001_create");
    manifest.scenarios[0].visible.object = "legacy-branch".to_string();

    let panic = std::panic::catch_unwind(|| manifest.validate_shape())
        .expect_err("scenario visible projections must resolve through live_http_fixture");
    let message = panic_message(panic);
    assert!(message.contains("unknown loadable object projection legacy-branch"), "{message}");
}

#[test]
fn live_http_fixture_manifest_contract_rejects_duplicate_map_keys() {
    let raw = r#"{
        "target_index": "products",
        "control_index": "control_products",
        "target_object": {
            "object_id": "new-widget",
            "body": {"objectID": "new-widget"},
            "expected_highlight": {}
        },
        "old_target_object": {
            "object_id": "old-widget",
            "body": {"objectID": "old-widget"},
            "expected_highlight": {}
        },
        "control_object": {
            "object_id": "control-widget",
            "body": {"objectID": "control-widget"},
            "expected_highlight": {}
        },
        "target_query": {"text": "modern", "ordered_hit_ids": ["new-widget"]},
        "old_target_query": {"text": "legacy", "ordered_hit_ids": ["old-widget"]},
        "control_query": {"text": "control", "ordered_hit_ids": ["control-widget"]},
        "surface_statuses": {
            "object_absent": {"status": 404, "body": {"status": 404, "message": "missing"}},
            "object_absent": {"status": 410, "body": {"status": 410, "message": "gone"}},
            "object_unavailable": {"status": 503, "body": {"status": 503, "message": "unavailable"}},
            "search_unavailable": {"status": 503, "body": {"status": 503, "message": "unavailable"}}
        },
        "target_projections": {
            "new-meta": {"object": "target_object", "query": "target_query"},
            "old-meta": {"object": "old_target_object", "query": "old_target_query"}
        },
        "expectations": {
            "target_present": {"target": "present", "object": "present", "search": "present"},
            "target_absent": {"target": "absent", "object": "absent", "search": "absent"},
            "target_unavailable": {"target": "absent", "object": "unavailable", "search": "unavailable"},
            "control_present": {"target": "present", "object": "present", "search": "present"}
        }
    }"#;

    let error = serde_json::from_str::<repair_cli_manifest::LiveHttpFixture>(raw)
        .expect_err("duplicate live fixture map keys must fail during parsing");

    assert!(error.to_string().contains("duplicate key object_absent"), "{error}");
}

#[tokio::test]
async fn generated_old_target_projection_comes_from_manifest_fixture() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut manifest = load_scenario_manifest();
    let scenario = manifest
        .scenarios
        .iter_mut()
        .find(|scenario| scenario.id == "base_018_replacement")
        .unwrap();
    scenario.visible.object = "old-meta".to_string();
    scenario.visible.search = "loadable".to_string();
    manifest.live_http_fixture.old_target_object.body["title"] =
        serde_json::json!("manifest-owned old title");

    let layout = generate_base_scenario(tmp.path(), scenario);
    let error = assert_generated_case_matches_manifest_with_fixture(
        &case_root_for(tmp.path(), &scenario.id),
        scenario,
        &layout,
        &manifest.live_http_fixture,
    )
    .expect_err("mutating the manifest old-target body must change the generated projection expectation");

    assert!(error.contains("object body mismatch"), "{error}");
}

#[tokio::test]
async fn generated_target_projection_uses_manifest_projection_map() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut manifest = load_scenario_manifest();
    let scenario = manifest
        .scenarios
        .iter_mut()
        .find(|scenario| scenario.id == "base_002_create")
        .unwrap();
    scenario.visible.object = "new-meta".to_string();
    scenario.visible.search = "loadable".to_string();
    manifest
        .live_http_fixture
        .target_projections
        .get_mut("new-meta")
        .unwrap()
        .object = "old_target_object".to_string();
    manifest
        .live_http_fixture
        .target_projections
        .get_mut("new-meta")
        .unwrap()
        .query = "old_target_query".to_string();

    let layout = generate_base_scenario(tmp.path(), scenario);
    let error = assert_generated_case_matches_manifest_with_fixture(
        &case_root_for(tmp.path(), &scenario.id),
        scenario,
        &layout,
        &manifest.live_http_fixture,
    )
    .expect_err("rewiring the manifest projection map must change the target expectation");

    assert!(error.contains("old-widget object was absent"), "{error}");
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = panic.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    "non-string panic".to_string()
}

fn manifest_with_base(id: &str) -> ScenarioManifest {
    serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "layout_count": 1,
        "live_http_fixture": live_http_fixture_json(),
        "scenarios": [{
            "id": id,
            "kind": "base",
            "activation": "create",
            "tenant": "products",
            "transaction": "txn_001",
            "journal_phase": "prepared",
            "boundary": {"identity": "create|sync_dir:.publication|1"},
            "policy_keys": [],
            "digests": {
                "old": "absent",
                "new": "sha256:35820c78a8b1cb061ab3b7356634b956cb18ca51479d1c0a1fe96ea6c6c6acf7",
                "target": "sha256:35820c78a8b1cb061ab3b7356634b956cb18ca51479d1c0a1fe96ea6c6c6acf7",
                "staging": "absent",
                "backup": "absent"
            },
            "sidecars": {
                "query_suggestions": {
                    "old": "absent",
                    "new": "absent",
                    "target": "absent",
                    "staging": "absent",
                    "backup": "absent"
                },
                "analytics": {
                    "old": "absent",
                    "new": "absent",
                    "target": "absent",
                    "staging": "absent",
                    "backup": "absent"
                }
            },
            "disposition": "commit",
            "cli": {"status": "clean", "action": "none", "exit_code": 0},
            "visible": {"target": "products", "object": "new-meta", "search": "loadable"},
            "residue": {
                "staging": "absent",
                "backup": "absent",
                "journal": "present",
                "quarantine": "absent"
            }
        }]
    }))
    .unwrap()
}

fn live_http_fixture_json() -> serde_json::Value {
    serde_json::json!({
        "target_index": "products",
        "control_index": "control_products",
        "target_object": {
            "object_id": "new-widget",
            "body": {
                "objectID": "new-widget",
                "title": "modern waffle iron",
                "body": "new repair guide",
                "generation": "new"
            },
            "expected_highlight": {
                "title": "modern waffle iron",
                "body": "new repair guide",
                "generation": "new"
            }
        },
        "old_target_object": {
            "object_id": "old-widget",
            "body": {
                "objectID": "old-widget",
                "title": "legacy waffle iron",
                "body": "old repair guide",
                "generation": "old"
            },
            "expected_highlight": {
                "title": "legacy waffle iron",
                "body": "old repair guide",
                "generation": "old"
            }
        },
        "control_object": {
            "object_id": "control-widget",
            "body": {
                "objectID": "control-widget",
                "title": "control waffle iron",
                "body": "unchanged control guide",
                "generation": "control"
            },
            "expected_highlight": {
                "title": "control waffle iron",
                "body": "unchanged control guide",
                "generation": "control"
            }
        },
        "target_query": {
            "text": "modern",
            "ordered_hit_ids": ["new-widget"]
        },
        "old_target_query": {
            "text": "legacy",
            "ordered_hit_ids": ["old-widget"]
        },
        "control_query": {
            "text": "control",
            "ordered_hit_ids": ["control-widget"]
        },
        "surface_statuses": {
            "object_absent": {
                "status": 404,
                "body": {"status": 404, "message": "Object not found"}
            },
            "object_unavailable": {
                "status": 503,
                "body": {"status": 503, "message": "Index unavailable"}
            },
            "search_unavailable": {
                "status": 503,
                "body": {"status": 503, "message": "Index unavailable"}
            },
            "index_absent": {
                "status": 404,
                "body": {"status": 404, "message": "Index 'products' does not exist"}
            }
        },
        "target_projections": {
            "new-meta": {
                "object": "target_object",
                "query": "target_query"
            },
            "old-meta": {
                "object": "old_target_object",
                "query": "old_target_query"
            }
        },
        "expectations": {
            "target_present": {
                "target": "present",
                "object": "present",
                "search": "present"
            },
            "target_absent": {
                "target": "absent",
                "object": "absent",
                "search": "absent"
            },
            "target_unavailable": {
                "target": "absent",
                "object": "unavailable",
                "search": "unavailable"
            },
            "control_present": {
                "target": "present",
                "object": "present",
                "search": "present"
            }
        }
    })
}
