use super::*;
use crate::analytics::AnalyticsConfig;
use crate::index::manager::publication::{
    fence_publication_admission, scan_and_repair_publication_target, PreStagedPublication,
    PublicationTarget, PublicationTargetDisposition,
};
use tempfile::TempDir;

fn committed_snapshot() -> (TempDir, PublicationJournal) {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let publication = PreStagedPublication::prepare(temp.path(), target).unwrap();
    std::fs::create_dir_all(&publication.paths().staging).unwrap();
    tantivy::Index::create_in_dir(
        &publication.paths().staging,
        tantivy::schema::Schema::builder().build(),
    )
    .unwrap();
    std::fs::write(publication.paths().staging.join("settings.json"), b"{}").unwrap();
    let journal = publication.activate().unwrap();
    (temp, journal)
}

#[test]
fn snapshot_adoption_requires_fence_and_exact_committed_tree() {
    let (temp, journal) = committed_snapshot();
    assert!(adopt_snapshot_publication(temp.path(), &journal).is_err());
    let _fence = fence_publication_admission(temp.path(), &journal.target).unwrap();
    std::fs::write(
        temp.path().join("products/settings.json"),
        b"changed before handoff",
    )
    .unwrap();
    assert!(adopt_snapshot_publication(temp.path(), &journal).is_err());
    assert!(!adoption_path(&PublicationPaths::new(
        temp.path(),
        &journal.target,
        &journal.transaction_id
    ))
    .exists());
}

#[test]
fn snapshot_adoption_cannot_be_rebound_to_changed_journal_or_epoch() {
    for damage in ["marker", "journal", "epoch", "symlink", "missing-target"] {
        let (temp, journal) = committed_snapshot();
        let paths = PublicationPaths::new(temp.path(), &journal.target, &journal.transaction_id);
        {
            let _fence = fence_publication_admission(temp.path(), &journal.target).unwrap();
            adopt_snapshot_publication(temp.path(), &journal).unwrap();
        }
        match damage {
            "marker" => std::fs::write(adoption_path(&paths), b"{}").unwrap(),
            "journal" => {
                let mut value = journal.to_json_value();
                value["generation"] = serde_json::json!("other_generation");
                std::fs::write(&paths.journal, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "epoch" => std::fs::write(paths.epoch_path(), b"1").unwrap(),
            "symlink" => {
                std::fs::remove_file(adoption_path(&paths)).unwrap();
                std::os::unix::fs::symlink(&paths.journal, adoption_path(&paths)).unwrap();
            }
            "missing-target" => std::fs::remove_dir_all(&paths.target).unwrap(),
            _ => unreachable!(),
        }
        let result = scan_and_repair_publication_target(
            temp.path(),
            &AnalyticsConfig::for_data_dir(temp.path()),
            journal.target,
        );
        assert!(
            result.is_err()
                || result.unwrap().disposition == PublicationTargetDisposition::Unavailable,
            "{damage}"
        );
        assert!(
            paths.journal.exists(),
            "{damage} must retain recovery evidence"
        );
    }
}

#[test]
fn unadopted_snapshot_with_changed_tree_stays_unavailable() {
    let (temp, journal) = committed_snapshot();
    std::fs::write(
        temp.path().join("products/settings.json"),
        b"unproven mutation",
    )
    .unwrap();
    let report = scan_and_repair_publication_target(
        temp.path(),
        &AnalyticsConfig::for_data_dir(temp.path()),
        journal.target,
    )
    .unwrap();
    assert_eq!(
        report.disposition,
        PublicationTargetDisposition::Unavailable
    );
}
