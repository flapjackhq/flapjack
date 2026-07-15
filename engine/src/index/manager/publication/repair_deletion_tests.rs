use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn committed_sidecar_deletion_remains_valid_during_repair() {
    let temp_dir = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("move_txn").unwrap();
    let paths = PublicationPaths::new(temp_dir.path(), &target, &transaction);
    std::fs::create_dir_all(&paths.target).unwrap();
    std::fs::create_dir_all(&paths.staging).unwrap();
    std::fs::write(paths.target.join("index_meta.json"), b"old").unwrap();
    std::fs::write(paths.staging.join("index_meta.json"), b"new").unwrap();

    let sidecar_root = temp_dir.path().join(".query_suggestions");
    std::fs::create_dir_all(&sidecar_root).unwrap();
    std::fs::write(sidecar_root.join("products.json"), b"old-sidecar").unwrap();
    let manifest = PublicationArtifactManifest::new([PublicationArtifactManifestEntry::journaled(
        "query_suggestions",
        PublicationArtifactRoot::QuerySuggestions,
        PathBuf::from("products.json"),
        PathBuf::from("publication_move_txn.json"),
        sidecar_root,
    )])
    .unwrap();
    let inventory = TantivyManagedInventory::new([PathBuf::from("index_meta.json")]).unwrap();

    activate_publication(
        &paths,
        target.clone(),
        transaction.clone(),
        PublicationGenerationEvidence::new("move_generation").unwrap(),
        manifest.clone(),
        &inventory,
    )
    .unwrap();

    assert_eq!(
        repair_publication(temp_dir.path(), target, transaction, manifest, &inventory,).unwrap(),
        RepairDecision::None
    );
}
