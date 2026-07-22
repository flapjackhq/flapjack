use super::super::repair_cli_manifest::{
    self, load_scenario_manifest, load_scenario_manifest_from_path, ActivationKind, Scenario,
    ScenarioKind,
    ScenarioManifest,
};
use super::super::super::fault::PublicationFaultHook;
use super::super::*;
use crate::{Document, FlapjackError, IndexManager};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

const MANIFEST_ENV: &str = "PUBLICATION_REPAIR_CLI_MANIFEST";
const ARTIFACT_DIR_ENV: &str = "PUBLICATION_REPAIR_CLI_ARTIFACT_DIR";
const WORKER_ENV: &str = "PUBLICATION_REPAIR_CLI_WORKER";
const WORKER_CASE_ID_ENV: &str = "PUBLICATION_REPAIR_CLI_WORKER_CASE_ID";
const WORKER_BOUNDARY_ENV: &str = "PUBLICATION_REPAIR_CLI_WORKER_BOUNDARY";
const WORKER_CASE_ROOT_ENV: &str = "PUBLICATION_REPAIR_CLI_WORKER_CASE_ROOT";
const WORKER_REPORT_ENV: &str = "PUBLICATION_REPAIR_CLI_WORKER_REPORT";
const PAUSE_WAIT: Duration = Duration::from_secs(10);

#[cfg(test)]
#[path = "repair_cli_generator_tests.rs"]
mod tests;

#[derive(Debug, Clone, Deserialize, Serialize)]
struct GeneratedLayout {
    scenario_id: String,
    kind: String,
    activation: Option<String>,
    base: Option<String>,
    mutation: Option<String>,
    tenant: Option<String>,
    transaction: Option<String>,
    journal_phase: Option<String>,
    policy_keys: Vec<String>,
    digests: BTreeMap<String, String>,
    sidecars: BTreeMap<String, BTreeMap<String, String>>,
    disposition: String,
    cli: GeneratedCliOracle,
    visible: GeneratedVisibleOracle,
    residue: GeneratedResidueOracle,
    boundaries: Vec<String>,
}

impl GeneratedLayout {
    fn from_scenario(scenario: &Scenario, boundaries: Vec<String>) -> Self {
        Self {
            scenario_id: scenario.id.clone(),
            kind: match scenario.kind {
                ScenarioKind::Base => "base",
                ScenarioKind::Mutation => "mutation",
            }
            .to_string(),
            activation: scenario.activation.map(|activation| activation.as_str().to_string()),
            base: scenario.base.clone(),
            mutation: scenario.mutation.clone(),
            tenant: scenario.tenant.clone(),
            transaction: scenario.transaction.clone(),
            journal_phase: scenario.journal_phase.clone(),
            policy_keys: scenario.policy_keys.clone(),
            digests: scenario.digests.clone(),
            sidecars: scenario.sidecars.clone(),
            disposition: scenario.disposition.clone(),
            cli: GeneratedCliOracle::from_manifest(&scenario.cli),
            visible: GeneratedVisibleOracle::from_manifest(&scenario.visible),
            residue: GeneratedResidueOracle::from_manifest(&scenario.residue),
            boundaries,
        }
    }

    fn base(id: impl Into<String>, boundary: impl Into<String>) -> Self {
        Self::minimal(id.into(), "base", vec![boundary.into()])
    }

    fn mutation(id: impl Into<String>) -> Self {
        Self::minimal(id.into(), "mutation", Vec::new())
    }

    fn with_boundary(mut self, boundary: impl Into<String>) -> Self {
        self.boundaries.push(boundary.into());
        self
    }

    fn minimal(scenario_id: String, kind: &str, boundaries: Vec<String>) -> Self {
        Self {
            scenario_id,
            kind: kind.to_string(),
            activation: None,
            base: None,
            mutation: None,
            tenant: None,
            transaction: None,
            journal_phase: None,
            policy_keys: Vec::new(),
            digests: BTreeMap::new(),
            sidecars: BTreeMap::new(),
            disposition: String::new(),
            cli: GeneratedCliOracle::default(),
            visible: GeneratedVisibleOracle::default(),
            residue: GeneratedResidueOracle::default(),
            boundaries,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct GeneratedCliOracle {
    status: String,
    action: String,
    exit_code: i32,
}

impl GeneratedCliOracle {
    fn from_manifest(cli: &repair_cli_manifest::CliOracle) -> Self {
        Self {
            status: cli.status.clone(),
            action: cli.action.clone(),
            exit_code: cli.exit_code,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct GeneratedVisibleOracle {
    target: String,
    object: String,
    search: String,
}

impl GeneratedVisibleOracle {
    fn from_manifest(visible: &repair_cli_manifest::VisibleOracle) -> Self {
        Self {
            target: visible.target.clone(),
            object: visible.object.clone(),
            search: visible.search.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct GeneratedResidueOracle {
    staging: String,
    backup: String,
    journal: String,
    quarantine: String,
}

impl GeneratedResidueOracle {
    fn from_manifest(residue: &repair_cli_manifest::ResidueOracle) -> Self {
        Self {
            staging: residue.staging.clone(),
            backup: residue.backup.clone(),
            journal: residue.journal.clone(),
            quarantine: residue.quarantine.clone(),
        }
    }
}

pub(super) fn generate_from_environment() {
    if std::env::var_os(WORKER_ENV).is_some() {
        run_worker_from_environment();
        return;
    }

    let manifest_path = required_env_path(MANIFEST_ENV);
    let artifact_root = required_env_path(ARTIFACT_DIR_ENV);
    assert!(
        artifact_root.is_absolute() && artifact_root.is_dir(),
        "{ARTIFACT_DIR_ENV} must name an existing absolute directory: {}",
        artifact_root.display()
    );

    let manifest = load_scenario_manifest_from_path(&manifest_path);
    manifest.validate_ids();
    manifest.validate_shape();
    let generated = manifest
        .scenarios
        .iter()
        .map(|scenario| generate_scenario(&artifact_root, &manifest, scenario))
        .collect::<Vec<_>>();
    validate_generated_layout_index(&manifest, &generated)
        .expect("generated layout index must match manifest");
    fs::write(
        artifact_root.join("generated_layouts.json"),
        serde_json::to_vec_pretty(&generated).expect("failed to encode generated layout index"),
    )
    .expect("failed to write generated layout index");
}

fn generate_scenario(
    artifact_root: &Path,
    manifest: &ScenarioManifest,
    scenario: &Scenario,
) -> GeneratedLayout {
    match scenario.kind {
        ScenarioKind::Base => generate_base_scenario_with_manifest(artifact_root, manifest, scenario),
        ScenarioKind::Mutation => {
            generate_mutation_scenario_with_manifest(artifact_root, manifest, scenario)
        }
    }
}

fn generate_base_scenario(artifact_root: &Path, scenario: &Scenario) -> GeneratedLayout {
    let manifest = load_scenario_manifest();
    generate_base_scenario_with_manifest(artifact_root, &manifest, scenario)
}

fn generate_base_scenario_with_manifest(
    artifact_root: &Path,
    manifest: &ScenarioManifest,
    scenario: &Scenario,
) -> GeneratedLayout {
    let case_root = fresh_case_root(artifact_root, &scenario.id);
    let boundary = scenario
        .boundary
        .as_ref()
        .unwrap_or_else(|| panic!("{} missing base boundary", scenario.id))
        .identity
        .clone();
    let report_path = case_root.join(".publication_repair_pause.json");
    let mut child = spawn_worker(&case_root, &scenario.id, &boundary, &report_path);
    let report = wait_for_valid_pause_report(&mut child, &report_path, &scenario.id, &boundary);
    child.kill().expect("failed to kill paused worker");
    child.wait().expect("failed to reap paused worker");
    fs::remove_file(report_path).expect("failed to remove pause report");
    let layout = GeneratedLayout::from_scenario(scenario, vec![report.boundary]);
    assert_generated_case_matches_manifest_with_fixture(
        &case_root,
        scenario,
        &layout,
        &manifest.live_http_fixture,
    )
        .expect("generated base layout must match manifest oracles");
    layout
}

fn generate_mutation_scenario(artifact_root: &Path, scenario: &Scenario) -> GeneratedLayout {
    let manifest = load_scenario_manifest();
    generate_mutation_scenario_with_manifest(artifact_root, &manifest, scenario)
}

fn generate_mutation_scenario_with_manifest(
    artifact_root: &Path,
    manifest: &ScenarioManifest,
    scenario: &Scenario,
) -> GeneratedLayout {
    let base = scenario
        .base
        .as_deref()
        .unwrap_or_else(|| panic!("{} missing mutation base", scenario.id));
    let mutation = scenario
        .mutation
        .as_deref()
        .unwrap_or_else(|| panic!("{} missing mutation name", scenario.id));
    assert!(
        repair_cli_manifest::is_supported_mutation(mutation),
        "{} has unsupported mutation {mutation}",
        scenario.id
    );
    let case_root = fresh_case_root(artifact_root, &scenario.id);
    copy_directory(&case_root_for(artifact_root, base), &case_root);
    match mutation {
        "corrupt_journal" => fs::write(
            case_root.join(".publication/products/txn_001/journal.json"),
            b"{not-valid-json",
        )
        .expect("failed to corrupt journal"),
        "missing_staging" => remove_if_exists(&case_root.join(".publication/products/txn_001/staging")),
        "ambiguous_target_and_staging" => {
            let fixture = ActivationFixture::new_at(case_root.clone());
            fixture.write_old_target();
            fixture.write_new_staging();
        }
        _ => unreachable!("supported mutation list rejected unknown mutation"),
    }
    let layout = GeneratedLayout::from_scenario(scenario, Vec::new());
    assert_generated_case_matches_manifest_with_fixture(
        &case_root,
        scenario,
        &layout,
        &manifest.live_http_fixture,
    )
        .expect("generated mutation layout must match manifest oracles");
    layout
}

fn run_worker_from_environment() {
    let case_id = required_env(WORKER_CASE_ID_ENV);
    let boundary = required_env(WORKER_BOUNDARY_ENV);
    let case_root = required_env_path(WORKER_CASE_ROOT_ENV);
    let report_path = required_env_path(WORKER_REPORT_ENV);
    let fixture = ActivationFixture::new_at(case_root.clone());
    let activation = activation_from_boundary(&boundary);
    let hook = PausingFaultHook::new(case_id, activation, case_root, boundary, report_path);
    materialize_worker_layout(&fixture, activation, &hook)
        .expect("worker completed without pausing at requested boundary");
}

fn materialize_worker_layout(
    fixture: &ActivationFixture,
    activation: ActivationKind,
    hook: &dyn PublicationFaultHook,
) -> Result<PublicationJournal> {
    let manifest = load_scenario_manifest();
    write_manifest_control_index(fixture.base(), &manifest.live_http_fixture)
        .map_err(FlapjackError::Config)?;
    if activation == ActivationKind::Replacement {
        fixture.write_old_target();
        repair_cli_manifest::write_owner_resolved_sidecars(fixture, "old", "new");
    }
    fixture.write_new_staging();
    let manifest = if activation == ActivationKind::Replacement {
        repair_cli_manifest::owner_resolved_manifest(fixture)
    } else {
        PublicationArtifactManifest::default()
    };
    activate_publication_with_faults_for_test(
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        manifest,
        &fixture.inventory,
        hook,
    )
}

struct PausingFaultHook {
    case_id: String,
    activation: ActivationKind,
    base: PathBuf,
    boundary: String,
    report_path: PathBuf,
    occurrences: std::cell::RefCell<std::collections::BTreeMap<String, usize>>,
}

impl PausingFaultHook {
    fn new(
        case_id: String,
        activation: ActivationKind,
        base: PathBuf,
        boundary: String,
        report_path: PathBuf,
    ) -> Self {
        Self {
            case_id,
            activation,
            base,
            boundary,
            report_path,
            occurrences: std::cell::RefCell::new(std::collections::BTreeMap::new()),
        }
    }

    fn pause_forever(&self) -> Result<()> {
        let report = PauseReport {
            pid: std::process::id(),
            case_id: self.case_id.clone(),
            boundary: self.boundary.clone(),
        };
        fs::write(
            &self.report_path,
            serde_json::to_vec_pretty(&report).expect("failed to encode pause report"),
        )?;
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
}

impl PublicationFaultHook for PausingFaultHook {
    fn before_operation(&self, operation: &PublicationOperation) -> Result<()> {
        let Some(operation_key) = repair_cli_manifest::normalized_operation(&self.base, operation)
        else {
            return Ok(());
        };
        let mut occurrences = self.occurrences.borrow_mut();
        let occurrence = occurrences.entry(operation_key.clone()).or_default();
        *occurrence += 1;
        let identity = format!("{}|{}|{}", self.activation.as_str(), operation_key, occurrence);
        if identity == self.boundary {
            self.pause_forever()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PauseReport {
    pid: u32,
    case_id: String,
    boundary: String,
}

fn spawn_worker(case_root: &Path, case_id: &str, boundary: &str, report_path: &Path) -> Child {
    worker_command(case_root, case_id, boundary, report_path)
        .spawn()
        .expect("failed to spawn publication repair CLI worker")
}

fn worker_command(case_root: &Path, case_id: &str, boundary: &str, report_path: &Path) -> Command {
    let mut command =
        Command::new(std::env::current_exe().expect("failed to locate current test binary"));
    command
        .arg("publication_repair_cli_generates_owner_authentic_layouts")
        .arg("--ignored")
        .arg("--nocapture")
        .env(WORKER_ENV, "1")
        .env(WORKER_CASE_ID_ENV, case_id)
        .env(WORKER_BOUNDARY_ENV, boundary)
        .env(WORKER_CASE_ROOT_ENV, case_root)
        .env(WORKER_REPORT_ENV, report_path);
    command
}

fn wait_for_valid_pause_report(
    child: &mut Child,
    report_path: &Path,
    case_id: &str,
    boundary: &str,
) -> PauseReport {
    let report = wait_for_pause_report_with_timeout(child, report_path, PAUSE_WAIT)
        .unwrap_or_else(|error| panic!("{error}"));
    validate_pause_report(&report, child.id(), case_id, boundary)
        .unwrap_or_else(|error| panic!("{error}"));
    report
}

fn wait_for_pause_report_with_timeout(
    child: &mut Child,
    report_path: &Path,
    wait: Duration,
) -> std::result::Result<PauseReport, String> {
    let deadline = Instant::now() + wait;
    let mut last_parse_error = None;
    loop {
        if report_path.exists() {
            let raw = fs::read_to_string(report_path).expect("failed to read pause report");
            match parse_pause_report(&raw) {
                Ok(report) => return Ok(report),
                Err(error) => last_parse_error = Some(error),
            }
        }
        if let Some(status) = child.try_wait().expect("failed to poll worker") {
            if let Some(error) = last_parse_error {
                return Err(error);
            }
            return Err(format!("worker exited before pause report: {status}"));
        }
        if Instant::now() >= deadline {
            return Err(last_parse_error
                .map(|error| format!("timed out waiting for valid pause report: {error}"))
                .unwrap_or_else(|| "timed out waiting for pause report".to_string()));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn parse_pause_report(raw: &str) -> std::result::Result<PauseReport, String> {
    serde_json::from_str(raw).map_err(|error| format!("failed to parse pause report: {error}"))
}

fn validate_pause_report(
    report: &PauseReport,
    child_pid: u32,
    case_id: &str,
    boundary: &str,
) -> std::result::Result<(), String> {
    if report.pid == 0 {
        return Err("pause report missing worker PID".to_string());
    }
    if report.pid != child_pid {
        return Err(format!(
            "worker PID mismatch: report {} child {child_pid}",
            report.pid
        ));
    }
    if report.case_id != case_id {
        return Err(format!(
            "worker case mismatch: report {} expected {case_id}",
            report.case_id
        ));
    }
    if report.boundary != boundary {
        return Err(format!(
            "worker boundary mismatch: report {} expected {boundary}",
            report.boundary
        ));
    }
    Ok(())
}

fn validate_generated_layout_index(
    manifest: &ScenarioManifest,
    generated: &[GeneratedLayout],
) -> std::result::Result<(), String> {
    let mut generated_ids = std::collections::BTreeSet::new();
    for layout in generated {
        if !generated_ids.insert(layout.scenario_id.as_str()) {
            return Err(format!("duplicate generated scenario id {}", layout.scenario_id));
        }
    }

    let manifest_ids = manifest
        .scenarios
        .iter()
        .map(|scenario| scenario.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for scenario in &manifest.scenarios {
        if !generated_ids.contains(scenario.id.as_str()) {
            return Err(format!("missing generated layout {}", scenario.id));
        }
    }
    for layout in generated {
        if !manifest_ids.contains(layout.scenario_id.as_str()) {
            return Err(format!(
                "generated layout {} is not present in manifest",
                layout.scenario_id
            ));
        }
    }
    if generated.len() != manifest.layout_count {
        return Err(format!(
            "generated {} layouts but manifest declares {}",
            generated.len(),
            manifest.layout_count
        ));
    }

    let mut boundary_counts = std::collections::BTreeMap::<&str, usize>::new();
    for layout in generated {
        for boundary in &layout.boundaries {
            *boundary_counts.entry(boundary.as_str()).or_default() += 1;
        }
    }
    for scenario in &manifest.scenarios {
        if scenario.kind != ScenarioKind::Base {
            continue;
        }
        let boundary = scenario
            .boundary
            .as_ref()
            .ok_or_else(|| format!("{} missing manifest boundary", scenario.id))?;
        match boundary_counts.get(boundary.identity.as_str()).copied() {
            Some(1) => {}
            Some(count) => {
                return Err(format!(
                    "{} boundary {} observed {count} times",
                    scenario.id, boundary.identity
                ));
            }
            None => {
                return Err(format!(
                    "{} missing generated boundary {}",
                    scenario.id, boundary.identity
                ));
            }
        }
    }
    Ok(())
}

fn assert_generated_case_matches_manifest(
    case_root: &Path,
    scenario: &Scenario,
    layout: &GeneratedLayout,
) -> std::result::Result<(), String> {
    let manifest = load_scenario_manifest();
    assert_generated_case_matches_manifest_with_fixture(
        case_root,
        scenario,
        layout,
        &manifest.live_http_fixture,
    )
}

fn assert_generated_case_matches_manifest_with_fixture(
    case_root: &Path,
    scenario: &Scenario,
    layout: &GeneratedLayout,
    fixture: &repair_cli_manifest::LiveHttpFixture,
) -> std::result::Result<(), String> {
    assert_layout_preserves_manifest_oracles(scenario, layout)?;
    assert_manifest_source_oracles(scenario)?;
    assert_manifest_path_policy_and_residue_oracles(case_root, scenario, layout)?;
    assert_materialized_journal_matches_manifest_identity(case_root, scenario)?;
    assert_repaired_projection_matches_manifest(case_root, scenario, fixture)?;
    Ok(())
}

fn assert_layout_preserves_manifest_oracles(
    scenario: &Scenario,
    layout: &GeneratedLayout,
) -> std::result::Result<(), String> {
    if layout.scenario_id != scenario.id {
        return Err(format!("{} layout ID mismatch", scenario.id));
    }
    if layout.digests != scenario.digests {
        return Err(format!("{} digest oracles were rewritten", scenario.id));
    }
    if layout.sidecars != scenario.sidecars {
        return Err(format!("{} sidecar oracles were rewritten", scenario.id));
    }
    if layout.cli.status != scenario.cli.status
        || layout.cli.action != scenario.cli.action
        || layout.cli.exit_code != scenario.cli.exit_code
    {
        return Err(format!("{} CLI oracle was rewritten", scenario.id));
    }
    if layout.visible.target != scenario.visible.target
        || layout.visible.object != scenario.visible.object
        || layout.visible.search != scenario.visible.search
    {
        return Err(format!("{} visible oracle was rewritten", scenario.id));
    }
    if layout.residue.staging != scenario.residue.staging
        || layout.residue.backup != scenario.residue.backup
        || layout.residue.journal != scenario.residue.journal
        || layout.residue.quarantine != scenario.residue.quarantine
    {
        return Err(format!("{} residue oracle was rewritten", scenario.id));
    }
    Ok(())
}

fn assert_repaired_projection_matches_manifest(
    case_root: &Path,
    scenario: &Scenario,
    fixture: &repair_cli_manifest::LiveHttpFixture,
) -> std::result::Result<(), String> {
    let repaired = tempfile::TempDir::new().map_err(|error| error.to_string())?;
    copy_directory(case_root, repaired.path());
    let target = scenario.tenant.as_deref().unwrap_or(fixture.target_index.as_str());
    scan_and_repair_publication_target(
        repaired.path(),
        &AnalyticsConfig::for_data_dir(repaired.path()),
        PublicationTarget::new(target).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("{} repair projection failed: {error}", scenario.id))?;

    assert_index_projection(
        repaired.path(),
        ExpectedIndexProjection::from_live_control(fixture),
    )
    .map_err(|error| format!("{} control query projection mismatch: {error}", scenario.id))?;

    if scenario.visible.search == "loadable" {
        assert_index_projection(
            repaired.path(),
            ExpectedIndexProjection::from_visible_target(fixture, scenario)?,
        )
        .map_err(|error| format!("{} loadable target query projection mismatch: {error}", scenario.id))?;
    }
    Ok(())
}

#[derive(Debug)]
struct ExpectedIndexProjection {
    index: String,
    object_id: String,
    object_body: serde_json::Value,
    query: String,
    hit_ids: Vec<String>,
}

impl ExpectedIndexProjection {
    fn from_live_control(fixture: &repair_cli_manifest::LiveHttpFixture) -> Self {
        Self {
            index: fixture.control_index.clone(),
            object_id: fixture.control_object.object_id.clone(),
            object_body: index_manager_document_body(
                &fixture.control_object.body,
                &fixture.control_object.object_id,
            ),
            query: fixture.control_query.text.clone(),
            hit_ids: fixture.control_query.ordered_hit_ids.clone(),
        }
    }

    fn from_visible_target(
        fixture: &repair_cli_manifest::LiveHttpFixture,
        scenario: &Scenario,
    ) -> std::result::Result<Self, String> {
        let projection = fixture.resolve_target_projection(&scenario.id, &scenario.visible)?;
        Ok(Self {
            index: fixture.target_index.clone(),
            object_id: projection.object.object_id.clone(),
            object_body: index_manager_document_body(
                &projection.object.body,
                &projection.object.object_id,
            ),
            query: projection.query.text.clone(),
            hit_ids: projection.query.ordered_hit_ids.clone(),
        })
    }
}

fn index_manager_document_body(body: &serde_json::Value, object_id: &str) -> serde_json::Value {
    let mut body = body.clone();
    body.as_object_mut()
        .expect("live HTTP object body shape is validated before generation")
        .insert("_id".to_string(), serde_json::Value::String(object_id.to_string()));
    body
}

fn assert_index_projection(
    base: &Path,
    expected: ExpectedIndexProjection,
) -> std::result::Result<(), String> {
    let base = base.to_path_buf();
    thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
        runtime.block_on(async move {
            let manager = IndexManager::new(&base);
            let document = manager
                .get_document(&expected.index, &expected.object_id)
                .map_err(|error| {
                    format!(
                        "{}/{} object lookup failed: {error}",
                        expected.index, expected.object_id
                    )
                })?
                .ok_or_else(|| {
                    format!(
                        "{}/{} object was absent",
                        expected.index, expected.object_id
                    )
                })?;
            if document.to_json() != expected.object_body {
                return Err(format!(
                    "{}/{} object body mismatch",
                    expected.index, expected.object_id
                ));
            }
            let hits = manager
                .search(&expected.index, &expected.query, None, None, 10)
                .map_err(|error| {
                    format!(
                        "{} query {:?} failed: {error}",
                        expected.index, expected.query
                    )
                })?
                .documents
                .into_iter()
                .map(|hit| hit.document.id)
                .collect::<Vec<_>>();
            if hits != expected.hit_ids {
                return Err(format!(
                    "{} query {:?} hits {:?} did not match {:?}",
                    expected.index, expected.query, hits, expected.hit_ids
                ));
            }
            manager.graceful_shutdown().await;
            Ok(())
        })
    })
    .join()
    .map_err(|_| "projection validation thread panicked".to_string())?
}

fn write_manifest_control_index(
    base: &Path,
    fixture: &repair_cli_manifest::LiveHttpFixture,
) -> std::result::Result<(), String> {
    let base = base.to_path_buf();
    let index = fixture.control_index.clone();
    let body = fixture.control_object.body.clone();
    thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
        runtime.block_on(async move {
            let manager = IndexManager::new(&base);
            manager.create_tenant(&index).map_err(|error| error.to_string())?;
            manager
                .add_documents_sync(
                    &index,
                    vec![Document::from_json(&body).map_err(|error| error.to_string())?],
                )
                .await
                .map_err(|error| error.to_string())?;
            manager.graceful_shutdown().await;
            Ok(())
        })
    })
    .join()
    .map_err(|_| "control index writer thread panicked".to_string())?
}

fn assert_manifest_source_oracles(scenario: &Scenario) -> std::result::Result<(), String> {
    repair_cli_manifest::assert_source_oracles(scenario)
}

fn assert_manifest_path_policy_and_residue_oracles(
    case_root: &Path,
    scenario: &Scenario,
    layout: &GeneratedLayout,
) -> std::result::Result<(), String> {
    let target = scenario.tenant.as_deref().unwrap_or("products");
    let transaction = scenario.transaction.as_deref().unwrap_or("txn_001");
    let target = PublicationTarget::new(target).map_err(|error| error.to_string())?;
    let transaction =
        PublicationTransactionId::new(transaction).map_err(|error| error.to_string())?;
    let paths = PublicationPaths::new(case_root, &target, &transaction);
    if !paths.target.ends_with(target.as_str()) {
        return Err(format!("{} target path mismatch", scenario.id));
    }
    if !paths.journal.ends_with("journal.json") {
        return Err(format!("{} journal path mismatch", scenario.id));
    }

    let known_policy_keys = artifact_policy_table()
        .iter()
        .filter(|policy| {
            matches!(
                policy.disposition,
                ArtifactDisposition::Preserve | ArtifactDisposition::Journal
            )
        })
        .map(|policy| policy.key)
        .collect::<BTreeSet<_>>();
    for key in &scenario.policy_keys {
        if !known_policy_keys.contains(key.as_str()) {
            return Err(format!("{} unknown policy key {key}", scenario.id));
        }
    }

    // These residue values are manifest repair oracles consumed by Stage 4; the
    // paused crash tree can be earlier than that repaired end state.
    if layout.residue.staging != scenario.residue.staging
        || layout.residue.backup != scenario.residue.backup
        || layout.residue.journal != scenario.residue.journal
        || layout.residue.quarantine != scenario.residue.quarantine
    {
        return Err(format!("{} residue oracle mismatch", scenario.id));
    }
    Ok(())
}

fn assert_materialized_journal_matches_manifest_identity(
    case_root: &Path,
    scenario: &Scenario,
) -> std::result::Result<(), String> {
    let Some(expected) = scenario.journal_phase.as_deref() else {
        return Ok(());
    };
    let target = scenario.tenant.as_deref().unwrap_or("products");
    let transaction = scenario.transaction.as_deref().unwrap_or("txn_001");
    let paths = PublicationPaths::new(
        case_root,
        &PublicationTarget::new(target).map_err(|error| error.to_string())?,
        &PublicationTransactionId::new(transaction).map_err(|error| error.to_string())?,
    );
    if expected == "absent" {
        if paths.journal.exists() {
            return Err(format!("{} journal should be absent", scenario.id));
        }
        return Ok(());
    }
    let journal_path = crash_phase_journal_path(&paths, scenario)
        .ok_or_else(|| format!("{} missing journal", scenario.id))?;
    let raw = fs::read_to_string(&journal_path)
        .map_err(|error| format!("{} missing journal: {error}", scenario.id))?;
    let journal = PublicationJournal::from_json(&raw)
        .map_err(|error| format!("{} invalid journal: {error}", scenario.id))?;
    if journal.target.as_str() != target {
        return Err(format!("{} journal target mismatch", scenario.id));
    }
    if journal.transaction_id.as_str() != transaction {
        return Err(format!("{} journal transaction mismatch", scenario.id));
    }
    if journal.phase.as_str() != expected {
        return Err(format!(
            "{} journal phase {} did not match manifest {expected}",
            scenario.id,
            journal.phase.as_str()
        ));
    }
    Ok(())
}

fn crash_phase_journal_path(paths: &PublicationPaths, scenario: &Scenario) -> Option<PathBuf> {
    if paths.journal.exists() {
        return Some(paths.journal.clone());
    }
    let boundary = scenario.boundary.as_ref()?.identity.as_str();
    if boundary.contains("rename:.publication/products/txn_001/journal.json.tmp->.publication/products/txn_001/journal.json|1")
    {
        return Some(paths.journal.with_extension("json.tmp"));
    }
    None
}

fn fresh_case_root(artifact_root: &Path, scenario_id: &str) -> PathBuf {
    let case_root = case_root_for(artifact_root, scenario_id);
    remove_if_exists(&case_root);
    fs::create_dir_all(&case_root).expect("failed to create case root");
    case_root
}

fn case_root_for(artifact_root: &Path, scenario_id: &str) -> PathBuf {
    assert!(
        !scenario_id.is_empty()
            && !scenario_id.contains('/')
            && !scenario_id.contains('\\')
            && scenario_id != "."
            && scenario_id != "..",
        "unsafe scenario id {scenario_id}"
    );
    let case_root = artifact_root.join(scenario_id);
    assert!(
        case_root.starts_with(artifact_root),
        "case root escaped artifact root"
    );
    case_root
}

fn copy_directory(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("failed to create copied directory");
    for entry in fs::read_dir(from).expect("failed to read copied directory") {
        let entry = entry.expect("failed to read copied directory entry");
        let source = entry.path();
        let target = to.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source).expect("failed to inspect copied path");
        if metadata.is_dir() {
            copy_directory(&source, &target);
        } else if metadata.is_file() {
            fs::copy(&source, &target).expect("failed to copy file");
        } else {
            panic!("unsupported copied path {}", source.display());
        }
    }
}

fn remove_if_exists(path: &Path) {
    if !path.exists() {
        return;
    }
    let metadata = fs::symlink_metadata(path).expect("failed to inspect path for removal");
    if metadata.is_dir() {
        fs::remove_dir_all(path).expect("failed to remove directory");
    } else {
        fs::remove_file(path).expect("failed to remove file");
    }
}

fn activation_from_boundary(boundary: &str) -> ActivationKind {
    if boundary.starts_with("create|") {
        ActivationKind::Create
    } else if boundary.starts_with("replacement|") {
        ActivationKind::Replacement
    } else {
        panic!("boundary does not encode activation kind: {boundary}");
    }
}

fn required_env_path(name: &str) -> PathBuf {
    PathBuf::from(required_env(name))
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set"))
}
