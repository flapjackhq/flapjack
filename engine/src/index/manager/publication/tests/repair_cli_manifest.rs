use super::super::executor;
use super::*;
use crate::analytics::AnalyticsConfig;
use crate::query_suggestions::QsConfigStore;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const SCENARIO_MANIFEST: &str = "tests/publication_repair_cli_scenarios.json";
const ABSENT: &str = "absent";

pub(super) const SUPPORTED_MUTATIONS: [&str; 3] = [
    "corrupt_journal",
    "missing_staging",
    "ambiguous_target_and_staging",
];

#[derive(Debug, serde::Deserialize)]
pub(super) struct ScenarioManifest {
    pub(super) schema_version: u32,
    pub(super) layout_count: usize,
    pub(super) live_http_fixture: LiveHttpFixture,
    pub(super) scenarios: Vec<Scenario>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct Scenario {
    pub(super) id: String,
    pub(super) kind: ScenarioKind,
    #[serde(default)]
    pub(super) activation: Option<ActivationKind>,
    #[serde(default)]
    pub(super) base: Option<String>,
    #[serde(default)]
    pub(super) mutation: Option<String>,
    #[serde(default)]
    pub(super) tenant: Option<String>,
    #[serde(default)]
    pub(super) transaction: Option<String>,
    #[serde(default)]
    pub(super) journal_phase: Option<String>,
    #[serde(default)]
    pub(super) clean_report_phase: Option<String>,
    #[serde(default)]
    pub(super) boundary: Option<BoundaryOracle>,
    #[serde(default)]
    pub(super) policy_keys: Vec<String>,
    pub(super) digests: BTreeMap<String, String>,
    pub(super) sidecars: BTreeMap<String, BTreeMap<String, String>>,
    pub(super) disposition: String,
    pub(super) cli: CliOracle,
    pub(super) visible: VisibleOracle,
    pub(super) residue: ResidueOracle,
}

#[derive(Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ScenarioKind {
    Base,
    Mutation,
}

impl ScenarioKind {
    pub(super) fn is_base(&self) -> bool {
        self == &Self::Base
    }
}

#[derive(Debug, serde::Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ActivationKind {
    Create,
    Replacement,
}

impl ActivationKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Replacement => "replacement",
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct BoundaryOracle {
    pub(super) identity: String,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct CliOracle {
    pub(super) status: String,
    pub(super) action: String,
    pub(super) exit_code: i32,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct VisibleOracle {
    pub(super) target: String,
    pub(super) object: String,
    pub(super) search: String,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct ResidueOracle {
    pub(super) staging: String,
    pub(super) backup: String,
    pub(super) journal: String,
    pub(super) quarantine: String,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct LiveHttpFixture {
    pub(super) target_index: String,
    pub(super) control_index: String,
    pub(super) target_object: LiveHttpObject,
    pub(super) control_object: LiveHttpObject,
    pub(super) target_query: LiveHttpQuery,
    pub(super) control_query: LiveHttpQuery,
    pub(super) expectations: BTreeMap<String, LiveHttpVisibility>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct LiveHttpObject {
    pub(super) object_id: String,
    pub(super) body: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct LiveHttpQuery {
    pub(super) text: String,
    pub(super) ordered_hit_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct LiveHttpVisibility {
    pub(super) target: String,
    pub(super) object: String,
    pub(super) search: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ObservedBoundary {
    pub(super) identity: String,
}

impl ScenarioManifest {
    pub(super) fn validate_ids(&self) {
        let mut ids = BTreeSet::new();
        for scenario in &self.scenarios {
            assert!(
                ids.insert(scenario.id.as_str()),
                "duplicate scenario id {}",
                scenario.id
            );
        }
    }

    pub(super) fn validate_shape(&self) {
        assert_eq!(self.schema_version, 1);
        validate_live_http_fixture(&self.live_http_fixture);
        let base_ids = self
            .scenarios
            .iter()
            .filter(|scenario| scenario.kind == ScenarioKind::Base)
            .map(|scenario| scenario.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut mutation_ids = BTreeSet::new();

        for scenario in &self.scenarios {
            validate_common_oracles(scenario);
            match scenario.kind {
                ScenarioKind::Base => validate_base_scenario(scenario),
                ScenarioKind::Mutation => {
                    validate_mutation_scenario(scenario, &base_ids, &mut mutation_ids);
                }
            }
        }
    }
}

pub(super) fn load_scenario_manifest() -> ScenarioManifest {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(SCENARIO_MANIFEST);
    load_scenario_manifest_from_path(&path)
}

pub(super) fn load_scenario_manifest_from_path(path: &Path) -> ScenarioManifest {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

pub(super) fn is_supported_mutation(value: &str) -> bool {
    SUPPORTED_MUTATIONS.contains(&value)
}

pub(super) fn base_boundary_claims(manifest: &ScenarioManifest) -> BTreeSet<ObservedBoundary> {
    manifest
        .scenarios
        .iter()
        .filter(|scenario| scenario.kind == ScenarioKind::Base)
        .map(|scenario| ObservedBoundary {
            identity: scenario
                .boundary
                .as_ref()
                .expect("base scenario boundary must be validated before inventory comparison")
                .identity
                .clone(),
        })
        .collect()
}

pub(super) fn observed_publication_boundaries() -> BTreeSet<ObservedBoundary> {
    [
        record_activation(ActivationKind::Create),
        record_activation(ActivationKind::Replacement),
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub(super) fn observed_boundaries(
    activation: ActivationKind,
    base: &Path,
    operations: &[PublicationOperation],
) -> Vec<ObservedBoundary> {
    let mut occurrences = BTreeMap::<String, usize>::new();
    let mut observed = Vec::new();
    for operation in operations {
        let Some(operation_key) = normalized_operation(base, operation) else {
            continue;
        };
        let occurrence = occurrences.entry(operation_key.clone()).or_default();
        *occurrence += 1;
        observed.push(ObservedBoundary {
            identity: format!("{}|{}|{}", activation.as_str(), operation_key, occurrence),
        });
    }
    observed
}

pub(super) fn normalized_operation(base: &Path, operation: &PublicationOperation) -> Option<String> {
    match operation {
        PublicationOperation::Rename { from, to } => Some(format!(
            "rename:{}->{}",
            relative_operation_path(base, from),
            relative_operation_path(base, to)
        )),
        PublicationOperation::SyncFile(path) => {
            Some(format!("sync_file:{}", relative_operation_path(base, path)))
        }
        PublicationOperation::SyncDirectory(path) => {
            Some(format!("sync_dir:{}", relative_operation_path(base, path)))
        }
        PublicationOperation::Checkpoint(_)
        | PublicationOperation::CreateDirectory(_)
        | PublicationOperation::Digest(_)
        | PublicationOperation::WriteFile(_)
        | PublicationOperation::CopyFile { .. }
        | PublicationOperation::Remove(_) => None,
    }
}

pub(super) fn owner_resolved_manifest(fixture: &ActivationFixture) -> PublicationArtifactManifest {
    let query_suggestions = QsConfigStore::new(fixture.base());
    let analytics = contract_analytics_config(fixture.base());
    let staging_key = format!("publication_{}", fixture.transaction.as_str());
    PublicationArtifactManifest::from_resolved_artifacts(
        Some((
            query_suggestions
                .target_artifact_paths(fixture.target.as_str())
                .unwrap(),
            query_suggestions.target_artifact_paths(&staging_key).unwrap(),
        )),
        Some((
            analytics.target_artifact_paths(fixture.target.as_str()),
            analytics.target_artifact_paths(&staging_key),
        )),
    )
    .unwrap()
}

pub(super) fn write_owner_resolved_sidecars(
    fixture: &ActivationFixture,
    old: &str,
    new: &str,
) {
    let query_suggestions = QsConfigStore::new(fixture.base());
    let analytics = contract_analytics_config(fixture.base());
    let staging_key = format!("publication_{}", fixture.transaction.as_str());
    let original_qs = query_suggestions
        .target_artifact_paths(fixture.target.as_str())
        .unwrap();
    let promoted_qs = query_suggestions.target_artifact_paths(&staging_key).unwrap();
    for path in [
        original_qs.config_path,
        original_qs.log_path,
        original_qs.status_path,
    ] {
        write_file_sidecar_path(&path, old);
    }
    for path in [
        promoted_qs.config_path,
        promoted_qs.log_path,
        promoted_qs.status_path,
    ] {
        write_file_sidecar_path(&path, new);
    }
    write_directory_sidecar_path(
        &analytics
            .target_artifact_paths(fixture.target.as_str())
            .index_root,
        old,
    );
    write_directory_sidecar_path(&analytics.target_artifact_paths(&staging_key).index_root, new);
}

pub(super) fn assert_source_oracles(scenario: &Scenario) -> std::result::Result<(), String> {
    let oracles = owner_source_oracles();
    assert_tenant_source_digest(scenario, "old", Some(oracles.old_tenant.as_str()))?;
    assert_tenant_source_digest(scenario, "new", Some(oracles.new_tenant.as_str()))?;
    for field in ["target", "staging", "backup"] {
        assert_tenant_source_digest(
            scenario,
            field,
            Some(oracles.old_tenant.as_str())
                .filter(|_| {
                    scenario
                        .digests
                        .get(field)
                        .is_some_and(|value| value == oracles.old_tenant.as_str())
                })
                .or(Some(oracles.new_tenant.as_str())),
        )?;
    }
    for (name, sidecar) in &oracles.sidecars {
        let values = scenario
            .sidecars
            .get(name)
            .ok_or_else(|| format!("{} missing {name} sidecar oracle", scenario.id))?;
        assert_sidecar_source_digest(scenario, name, values, "old", Some(&sidecar.old))?;
        assert_sidecar_source_digest(scenario, name, values, "new", Some(&sidecar.new))?;
        for field in ["target", "staging", "backup"] {
            assert_sidecar_observed_digest(scenario, name, values, field, sidecar)?;
        }
    }
    Ok(())
}

struct OwnerSourceOracles {
    old_tenant: String,
    new_tenant: String,
    sidecars: BTreeMap<String, SidecarSourceOracle>,
}

struct SidecarSourceOracle {
    old: String,
    new: String,
}

fn owner_source_oracles() -> OwnerSourceOracles {
    let fixture = ActivationFixture::new();
    let old_root = fixture.base().join("oracle_old");
    let new_root = fixture.base().join("oracle_new");
    fixture.write_old_tree(&old_root);
    fixture.write_new_tree(&new_root);
    write_owner_resolved_sidecars(&fixture, "old", "new");
    let manifest = owner_resolved_manifest(&fixture);
    let mut sidecars: BTreeMap<String, SidecarSourceOracle> = BTreeMap::new();
    for entry in &manifest.entries {
        let (original, promoted) = executor::resolved_artifact_paths(entry);
        let oracle = SidecarSourceOracle {
            old: executor::artifact_digest(&original)
                .unwrap()
                .as_str()
                .to_string(),
            new: executor::artifact_digest(&promoted)
                .unwrap()
                .as_str()
                .to_string(),
        };
        match sidecars.get(&entry.policy_key) {
            Some(existing) => {
                assert_eq!(existing.old, oracle.old);
                assert_eq!(existing.new, oracle.new);
            }
            None => {
                sidecars.insert(entry.policy_key.clone(), oracle);
            }
        }
    }
    OwnerSourceOracles {
        old_tenant: canonical_tenant_tree_digest(&old_root, &fixture.inventory)
            .unwrap()
            .as_str()
            .to_string(),
        new_tenant: canonical_tenant_tree_digest(&new_root, &fixture.inventory)
            .unwrap()
            .as_str()
            .to_string(),
        sidecars,
    }
}

fn assert_tenant_source_digest(
    scenario: &Scenario,
    field: &str,
    strict_expected: Option<&str>,
) -> std::result::Result<(), String> {
    let Some(actual) = scenario.digests.get(field).map(String::as_str) else {
        return Err(format!("{} missing {field} digest", scenario.id));
    };
    if actual == ABSENT {
        return Ok(());
    }
    if let Some(expected) = strict_expected {
        if actual != expected {
            return Err(format!(
                "{} {field} digest {actual} did not match owner source {expected}",
                scenario.id
            ));
        }
    }
    Ok(())
}

fn assert_sidecar_source_digest(
    scenario: &Scenario,
    name: &str,
    values: &BTreeMap<String, String>,
    field: &str,
    strict_expected: Option<&str>,
) -> std::result::Result<(), String> {
    let Some(actual) = values.get(field).map(String::as_str) else {
        return Err(format!("{} missing {name}.{field}", scenario.id));
    };
    if actual == ABSENT {
        return Ok(());
    }
    if let Some(expected) = strict_expected {
        if actual != expected {
            return Err(format!(
                "{} {name}.{field} digest {actual} did not match owner source {expected}",
                scenario.id
            ));
        }
    }
    Ok(())
}

fn assert_sidecar_observed_digest(
    scenario: &Scenario,
    name: &str,
    values: &BTreeMap<String, String>,
    field: &str,
    source: &SidecarSourceOracle,
) -> std::result::Result<(), String> {
    let Some(actual) = values.get(field).map(String::as_str) else {
        return Err(format!("{} missing {name}.{field}", scenario.id));
    };
    if actual == ABSENT || actual == source.old || actual == source.new {
        return Ok(());
    }
    Err(format!(
        "{} {name}.{field} digest {actual} did not match owner old/new sources",
        scenario.id
    ))
}

fn record_activation(kind: ActivationKind) -> Vec<ObservedBoundary> {
    let fixture = ActivationFixture::new();
    if kind == ActivationKind::Replacement {
        fixture.write_old_target();
        write_owner_resolved_sidecars(&fixture, "old", "new");
    }
    fixture.write_new_staging();
    let manifest = if kind == ActivationKind::Replacement {
        owner_resolved_manifest(&fixture)
    } else {
        PublicationArtifactManifest::default()
    };
    let recording = PublicationFaultScript::recording();
    activate_publication_with_faults_for_test(
        &fixture.paths,
        fixture.target.clone(),
        fixture.transaction.clone(),
        PublicationGenerationEvidence::new("generation_1").unwrap(),
        manifest,
        &fixture.inventory,
        &recording,
    )
    .unwrap();
    observed_boundaries(kind, fixture.base(), &recording.operations())
}

fn validate_common_oracles(scenario: &Scenario) {
    assert!(
        matches!(
            scenario.disposition.as_str(),
            "commit" | "rollback" | "absent-create" | "quarantine"
        ),
        "{} has unknown disposition {}",
        scenario.id,
        scenario.disposition
    );
    for required in ["old", "new", "target", "staging", "backup"] {
        let value = scenario
            .digests
            .get(required)
            .unwrap_or_else(|| panic!("{} missing {required} digest oracle", scenario.id));
        validate_digest_or_absence(&scenario.id, required, value);
    }
    for required in ["query_suggestions", "analytics"] {
        let oracle = scenario
            .sidecars
            .get(required)
            .unwrap_or_else(|| panic!("{} missing {required} sidecar oracle", scenario.id));
        for key in ["old", "new", "target", "staging", "backup"] {
            let value = oracle
                .get(key)
                .unwrap_or_else(|| panic!("{} missing {required}.{key}", scenario.id));
            validate_digest_or_absence(&scenario.id, &format!("{required}.{key}"), value);
        }
    }
    assert!(!scenario.visible.target.is_empty(), "{}", scenario.id);
    assert!(!scenario.visible.object.is_empty(), "{}", scenario.id);
    assert!(!scenario.visible.search.is_empty(), "{}", scenario.id);
    for (field, value) in [
        ("staging", &scenario.residue.staging),
        ("backup", &scenario.residue.backup),
        ("journal", &scenario.residue.journal),
        ("quarantine", &scenario.residue.quarantine),
    ] {
        assert!(
            matches!(value.as_str(), "present" | "absent"),
            "{} residue.{field} must be present or absent",
            scenario.id
        );
    }
}

fn validate_base_scenario(scenario: &Scenario) {
    let activation = scenario
        .activation
        .unwrap_or_else(|| panic!("{} missing activation kind", scenario.id));
    let tenant = scenario
        .tenant
        .as_deref()
        .unwrap_or_else(|| panic!("{} missing tenant", scenario.id));
    let transaction = scenario
        .transaction
        .as_deref()
        .unwrap_or_else(|| panic!("{} missing transaction", scenario.id));
    PublicationTarget::new(tenant).unwrap_or_else(|error| {
        panic!("{} tenant is not accepted by PublicationTarget: {error}", scenario.id)
    });
    PublicationTransactionId::new(transaction).unwrap_or_else(|error| {
        panic!(
            "{} transaction is not accepted by PublicationTransactionId: {error}",
            scenario.id
        )
    });
    let phase = scenario
        .journal_phase
        .as_deref()
        .unwrap_or_else(|| panic!("{} missing journal phase", scenario.id));
    assert!(
        matches!(phase, "absent" | "prepared" | "committed" | "rolled_back"),
        "{} has invalid journal phase {phase}",
        scenario.id
    );
    if let Some(clean_report_phase) = scenario.clean_report_phase.as_deref() {
        assert!(
            matches!(
                clean_report_phase,
                "absent" | "prepared" | "committed" | "rolled_back"
            ),
            "{} has invalid clean report phase {clean_report_phase}",
            scenario.id
        );
    }
    if phase == "absent" {
        assert!(
            scenario
                .boundary
                .as_ref()
                .is_some_and(|boundary| boundary.identity.starts_with(activation.as_str())),
            "{} absent journal phase must still name its authentic boundary",
            scenario.id
        );
    }
    assert!(
        scenario.boundary.is_some(),
        "{} must claim exactly one boundary",
        scenario.id
    );
    assert!(scenario.base.is_none(), "{} base scenario restates base", scenario.id);
    assert!(
        scenario.mutation.is_none(),
        "{} base scenario must not define a mutation",
        scenario.id
    );
    assert!(
        create_backup_expectations_are_coherent(activation, scenario),
        "{} has incoherent create/replacement old and backup expectations",
        scenario.id
    );
}

fn validate_mutation_scenario<'a>(
    scenario: &'a Scenario,
    base_ids: &BTreeSet<&str>,
    mutation_ids: &mut BTreeSet<&'a str>,
) {
    assert!(
        mutation_ids.insert(scenario.id.as_str()),
        "duplicate mutation id {}",
        scenario.id
    );
    let base = scenario
        .base
        .as_deref()
        .unwrap_or_else(|| panic!("{} missing base scenario reference", scenario.id));
    assert!(
        base_ids.contains(base),
        "{} references unknown base scenario {base}",
        scenario.id
    );
    assert!(
        scenario.mutation.as_deref().is_some_and(is_supported_mutation),
        "{} missing supported mutation",
        scenario.id
    );
    let mutation = scenario.mutation.as_deref().unwrap();
    assert_eq!(
        scenario.disposition,
        expected_mutation_disposition(mutation),
        "{} {mutation} mutation has unexpected disposition",
        scenario.id
    );
    assert!(scenario.activation.is_none(), "{} restates activation", scenario.id);
    assert!(scenario.tenant.is_none(), "{} restates tenant", scenario.id);
    assert!(
        scenario.transaction.is_none(),
        "{} restates transaction",
        scenario.id
    );
    assert!(scenario.boundary.is_none(), "{} owns a second boundary", scenario.id);
}

fn expected_mutation_disposition(mutation: &str) -> &'static str {
    match mutation {
        "corrupt_journal" | "ambiguous_target_and_staging" => "quarantine",
        "missing_staging" => "rollback",
        _ => panic!("unsupported mutation {mutation}"),
    }
}

fn validate_live_http_fixture(fixture: &LiveHttpFixture) {
    PublicationTarget::new(&fixture.target_index)
        .unwrap_or_else(|error| panic!("live_http_fixture target_index invalid: {error}"));
    PublicationTarget::new(&fixture.control_index)
        .unwrap_or_else(|error| panic!("live_http_fixture control_index invalid: {error}"));
    assert_ne!(
        fixture.target_index, fixture.control_index,
        "live_http_fixture target/control indexes must be distinct"
    );
    validate_live_http_object("target_object", &fixture.target_object);
    validate_live_http_object("control_object", &fixture.control_object);
    validate_live_http_query("target_query", &fixture.target_query, &fixture.target_object);
    validate_live_http_query(
        "control_query",
        &fixture.control_query,
        &fixture.control_object,
    );

    let expected_keys = ["target_absent", "target_present", "target_unavailable", "control_present"]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let actual_keys = fixture
        .expectations
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_keys, expected_keys,
        "live_http_fixture expectations must be closed over known keys"
    );
    for (key, visibility) in &fixture.expectations {
        validate_live_http_visibility(key, visibility);
    }
}

fn validate_live_http_object(name: &str, object: &LiveHttpObject) {
    assert!(!object.object_id.is_empty(), "live_http_fixture {name} missing object_id");
    let body = object
        .body
        .as_object()
        .unwrap_or_else(|| panic!("live_http_fixture {name}.body must be an object"));
    assert_eq!(
        body.get("objectID").and_then(serde_json::Value::as_str),
        Some(object.object_id.as_str()),
        "live_http_fixture {name}.body.objectID must match object_id"
    );
}

fn validate_live_http_query(name: &str, query: &LiveHttpQuery, object: &LiveHttpObject) {
    assert!(!query.text.trim().is_empty(), "live_http_fixture {name}.text missing");
    assert!(
        !query.ordered_hit_ids.is_empty(),
        "live_http_fixture {name}.ordered_hit_ids missing"
    );
    assert!(
        query
            .ordered_hit_ids
            .iter()
            .any(|hit| hit == &object.object_id),
        "live_http_fixture {name}.ordered_hit_ids must include its object"
    );
}

fn validate_live_http_visibility(key: &str, visibility: &LiveHttpVisibility) {
    for (field, value) in [
        ("target", visibility.target.as_str()),
        ("object", visibility.object.as_str()),
        ("search", visibility.search.as_str()),
    ] {
        assert!(
            matches!(value, "present" | "absent" | "unavailable"),
            "live_http_fixture expectations.{key}.{field} has unknown value {value}"
        );
    }
}

fn validate_digest_or_absence(scenario_id: &str, field: &str, value: &str) {
    if value == ABSENT {
        return;
    }
    ContentDigest::new(value).unwrap_or_else(|error| {
        panic!("{scenario_id} {field} must be a canonical ContentDigest or absent: {error}")
    });
}

fn create_backup_expectations_are_coherent(activation: ActivationKind, scenario: &Scenario) -> bool {
    let old = scenario.digests.get("old").map(String::as_str);
    let backup = scenario.digests.get("backup").map(String::as_str);
    match activation {
        ActivationKind::Create => old == Some(ABSENT) && backup == Some(ABSENT),
        ActivationKind::Replacement => old.is_some_and(|value| value != ABSENT),
    }
}

fn relative_operation_path(base: &Path, path: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn write_file_sidecar_path(path: &Path, label: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("{label}:query_suggestions")).unwrap();
}

fn write_directory_sidecar_path(path: &Path, label: &str) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("events.jsonl"), format!("{label}:events")).unwrap();
}

fn contract_analytics_config(base: &Path) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: base.join("analytics"),
        flush_interval_secs: 1,
        flush_size: 1,
        retention_days: 1,
    }
}
