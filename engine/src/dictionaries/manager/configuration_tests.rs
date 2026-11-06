use super::*;
use crate::dictionaries::BatchRequest;

fn add(word: &str) -> DictionaryCommand {
    DictionaryCommand::Batch {
        dictionary: DictionaryName::Stopwords,
        request: BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![BatchRequest {
                action: BatchAction::AddEntry,
                body: serde_json::json!({"objectID": word, "language": "en", "word": word}),
            }],
        },
    }
}

#[test]
fn preparation_is_pure_and_delivery_survives_restart_and_subsequent_updates() {
    let source = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let manager = DictionaryManager::new(source.path());
    let empty = DictionarySnapshot::empty("a").unwrap();
    let first = manager
        .prepare_configuration("a", &empty, 1, &add("alpha"))
        .unwrap();
    assert_eq!(
        source.path().read_dir().unwrap().count(),
        0,
        "preparation must not activate or create files"
    );
    assert!(
        manager.snapshot_at_least("a", 0).is_err(),
        "missing is not a valid zero"
    );
    manager.install_configuration("a", &empty).unwrap();
    let admitted = manager.snapshot_at_least("a", 0).unwrap();
    manager.install_configuration("a", &first).unwrap();
    assert!(!admitted
        .effective_stopwords("a", "en")
        .unwrap()
        .contains("alpha"));
    assert!(manager.snapshot_at_least("a", 2).is_err());
    let reloaded = DictionaryManager::new(source.path());
    assert_eq!(reloaded.snapshot_at_least("a", 1).unwrap(), first);
    let replacement = DictionaryManager::new(target.path());
    replacement.install_configuration("a", &first).unwrap();
    replacement.install_configuration("a", &first).unwrap(); // exact retry
    let second = replacement
        .prepare_configuration("a", &first, 2, &add("beta"))
        .unwrap();
    replacement.install_configuration("a", &second).unwrap();
    let result = DictionaryManager::new(target.path())
        .snapshot_at_least("a", 2)
        .unwrap();
    let words = result.effective_stopwords("a", "en").unwrap();
    assert!(words.contains("alpha") && words.contains("beta"));
    assert!(!replacement
        .effective_stopwords("b", "en")
        .unwrap()
        .contains("alpha"));
    assert!(replacement.install_configuration("b", &second).is_err());
    assert!(
        replacement.install_configuration("a", &first).is_err(),
        "restored older authority must not overwrite newer state"
    );
    let conflict = manager
        .prepare_configuration("a", &first, 2, &add("different"))
        .unwrap();
    assert!(replacement.install_configuration("a", &conflict).is_err());
    assert_eq!(replacement.snapshot_at_least("a", 2).unwrap(), second);
}

#[test]
fn corrupt_missing_and_unversioned_configuration_are_not_silently_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let manager = DictionaryManager::new(dir.path());
    let empty = DictionarySnapshot::empty("a").unwrap();
    let DictionaryCommand::Batch {
        dictionary,
        request,
    } = add("legacy")
    else {
        unreachable!()
    };
    manager.batch("a", dictionary, &request).unwrap();
    assert!(
        manager.install_configuration("a", &empty).is_err(),
        "unbound existing data needs reconciliation"
    );
    assert!(manager
        .effective_stopwords("a", "en")
        .unwrap()
        .contains("legacy"));
    let b = DictionarySnapshot::empty("b").unwrap();
    manager.install_configuration("b", &b).unwrap();
    assert!(
        manager.batch("b", dictionary, &request).is_err(),
        "local mutation cannot bypass durable authority"
    );
    assert!(manager
        .set_settings("b", &DictionarySettings::default())
        .is_err());
    let path = dir
        .path()
        .join(".applications/b/.dictionaries/configuration.json");
    std::fs::write(&path, b"{").unwrap();
    assert!(manager.snapshot("b").is_err());
    assert!(
        manager.install_configuration("b", &b).is_err(),
        "corruption must remain observable"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"{");
    std::fs::remove_file(&path).unwrap();
    assert!(manager.snapshot_at_least("b", 0).is_err());
}

#[test]
fn invalid_generation_or_command_preserves_published_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let manager = DictionaryManager::new(dir.path());
    let empty = DictionarySnapshot::empty("a").unwrap();
    manager.install_configuration("a", &empty).unwrap();
    let mut wrong_digest = empty.clone();
    wrong_digest.revision = 3;
    assert!(manager.install_configuration("a", &wrong_digest).is_err());
    let invalid = DictionaryCommand::Batch {
        dictionary: DictionaryName::Stopwords,
        request: BatchDictionaryRequest {
            clear_existing_dictionary_entries: true,
            requests: vec![BatchRequest {
                action: BatchAction::AddEntry,
                body: serde_json::json!({"objectID":"", "language":"en", "word":"bad"}),
            }],
        },
    };
    assert!(manager
        .prepare_configuration("a", &empty, 1, &invalid)
        .is_err());
    assert_eq!(manager.snapshot_at_least("a", 0).unwrap(), empty);
}

#[test]
fn managed_configuration_identical_retry_cannot_skip_failed_directory_sync() {
    use crate::index::utils::{fail_next_directory_sync_for_test, DirectorySyncFaultPoint};
    let dir = tempfile::tempdir().unwrap();
    let manager = DictionaryManager::new(dir.path());
    let snapshot = DictionarySnapshot::empty("a").unwrap();
    let leaf = dir.path().join(".applications/a/.dictionaries");
    std::fs::create_dir_all(&leaf).unwrap();
    let fault = fail_next_directory_sync_for_test(&leaf, DirectorySyncFaultPoint::Sync);
    assert!(manager.install_configuration("a", &snapshot).is_err());
    assert!(fault.was_triggered());
    assert!(
        leaf.join("configuration.json").exists(),
        "failure follows rename"
    );
    drop(fault);
    let retry_fault = fail_next_directory_sync_for_test(&leaf, DirectorySyncFaultPoint::Sync);
    assert!(
        manager.install_configuration("a", &snapshot).is_err(),
        "visible identical bytes are not a durability receipt"
    );
    assert!(retry_fault.was_triggered());
    drop(retry_fault);
    manager.install_configuration("a", &snapshot).unwrap();
}

#[test]
fn managed_configuration_installation_requires_durable_application_ancestors() {
    use crate::index::utils::{fail_next_directory_sync_for_test, DirectorySyncFaultPoint};
    let dir = tempfile::tempdir().unwrap();
    let manager = DictionaryManager::new(dir.path());
    let snapshot = DictionarySnapshot::empty("a").unwrap();
    let fault = fail_next_directory_sync_for_test(dir.path(), DirectorySyncFaultPoint::Sync);
    assert!(
        manager.install_configuration("a", &snapshot).is_err(),
        "new application namespace entry must be durable before acknowledging"
    );
    assert!(fault.was_triggered());
}
