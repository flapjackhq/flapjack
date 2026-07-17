use super::repair_cli_manifest::{self,
    base_boundary_claims, load_scenario_manifest, observed_publication_boundaries,
    SUPPORTED_MUTATIONS,
};
use super::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[path = "repair_cli_generator.rs"]
mod generator;

#[test]
fn publication_repair_cli_manifest_is_closed_over_recorded_activation_boundaries() {
    let manifest = load_scenario_manifest();
    manifest.validate_ids();
    manifest.validate_shape();

    let observed = observed_publication_boundaries();
    let base_boundaries = base_boundary_claims(&manifest);

    assert_eq!(
        base_boundaries.len(),
        observed.len(),
        "base scenario count must equal the observed boundary count: {observed:#?}"
    );
    assert_eq!(
        base_boundaries, observed,
        "manifest base boundary ownership must be a bijection over recorded Rename/SyncFile/SyncDirectory operations"
    );
    assert_eq!(
        manifest.layout_count,
        manifest.scenarios.len(),
        "later generated layout count must include base and mutation scenarios"
    );
}

#[test]
fn publication_repair_cli_manifest_uses_current_policy_and_cli_vocabularies() {
    let manifest = load_scenario_manifest();
    let base_policy_keys = manifest
        .scenarios
        .iter()
        .filter(|scenario| scenario.kind.is_base())
        .flat_map(|scenario| scenario.policy_keys.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let expected_policy_keys = artifact_policy_table()
        .iter()
        .filter(|policy| {
            matches!(
                policy.disposition,
                ArtifactDisposition::Preserve | ArtifactDisposition::Journal
            )
        })
        .map(|policy| policy.key)
        .collect::<BTreeSet<_>>();

    assert_eq!(
        base_policy_keys, expected_policy_keys,
        "base scenarios must represent every preserve/journal artifact policy and no unknown policy"
    );

    let valid_statuses = [
        PublicationRepairStatus::Clean,
        PublicationRepairStatus::Repaired,
        PublicationRepairStatus::Quarantined,
        PublicationRepairStatus::Unresolved,
    ]
    .into_iter()
    .map(PublicationRepairStatus::as_str)
    .collect::<BTreeSet<_>>();
    let valid_actions = [
        PublicationScanAction::Clean,
        PublicationScanAction::Repaired(RepairDecision::Complete),
        PublicationScanAction::Repaired(RepairDecision::Rollback),
        PublicationScanAction::Repaired(RepairDecision::Cleanup),
        PublicationScanAction::Quarantined,
        PublicationScanAction::Unresolved,
    ]
    .into_iter()
    .map(PublicationScanAction::as_str)
    .collect::<BTreeSet<_>>();
    let mut dispositions = BTreeSet::new();

    for scenario in &manifest.scenarios {
        assert!(
            valid_statuses.contains(scenario.cli.status.as_str()),
            "{} uses unknown CLI status {}",
            scenario.id,
            scenario.cli.status
        );
        assert!(
            valid_actions.contains(scenario.cli.action.as_str()),
            "{} uses unknown CLI action {}",
            scenario.id,
            scenario.cli.action
        );
        assert!(
            matches!(scenario.cli.exit_code, 0 | 2),
            "{} uses unsupported CLI exit code {}",
            scenario.id,
            scenario.cli.exit_code
        );
        dispositions.insert(scenario.disposition.as_str());
    }

    assert_eq!(
        dispositions,
        BTreeSet::from(["commit", "quarantine", "rollback"]),
        "manifest must include executable coverage for every shipped repair disposition"
    );
}

#[test]
fn publication_repair_cli_shared_manifest_helper_owns_mutations_and_boundaries() {
    assert_eq!(
        SUPPORTED_MUTATIONS,
        [
            "corrupt_journal",
            "missing_staging",
            "ambiguous_target_and_staging"
        ]
    );
    let operation = PublicationOperation::SyncDirectory(PathBuf::from("/tmp/root/.publication"));
    assert_eq!(
        repair_cli_manifest::normalized_operation(Path::new("/tmp/root"), &operation).as_deref(),
        Some("sync_dir:.publication")
    );
}

#[test]
#[ignore]
fn publication_repair_cli_generates_owner_authentic_layouts() {
    generator::generate_from_environment();
}
