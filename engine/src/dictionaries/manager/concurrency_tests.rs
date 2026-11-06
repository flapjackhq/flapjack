use super::*;
use crate::dictionaries::BatchRequest;
use std::cell::RefCell;
use std::sync::{mpsc, Arc};
use std::time::Duration;

thread_local! {
    static AFTER_READ: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

pub(super) fn after_batch_read() {
    AFTER_READ.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

fn addition(name: DictionaryName, id: &str) -> BatchDictionaryRequest {
    let body = match name {
        DictionaryName::Stopwords => {
            serde_json::json!({"objectID": id, "language": "en", "word": id})
        }
        DictionaryName::Plurals => {
            serde_json::json!({"objectID": id, "language": "en", "words": [id, format!("{id}s")]})
        }
        DictionaryName::Compounds => {
            serde_json::json!({"objectID": id, "language": "de", "word": id, "decomposition": ["part", "word"]})
        }
    };
    BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![BatchRequest {
            action: BatchAction::AddEntry,
            body,
        }],
    }
}

#[test]
fn concurrent_dictionary_batches_preserve_acknowledged_updates() {
    for &name in DictionaryName::all() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(DictionaryManager::new(dir.path()));
        let (read_tx, read_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first_manager = manager.clone();
        let first = std::thread::spawn(move || {
            AFTER_READ.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    read_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                }));
            });
            first_manager.batch("customer-a", name, &addition(name, "first"))
        });
        read_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let (second_tx, second_rx) = mpsc::channel();
        let second_manager = manager.clone();
        let second = std::thread::spawn(move || {
            AFTER_READ.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || second_tx.send(()).unwrap()));
            });
            second_manager.batch("customer-a", name, &addition(name, "second"))
        });
        // Wait until the second batch has either read the old file or reached
        // contention on the application lock. This fixes the interleaving;
        // the timeout only prevents a broken test from hanging.
        second_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        release_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();

        let reloaded = DictionaryManager::new(dir.path());
        let result = reloaded
            .search(
                "customer-a",
                name,
                &DictionarySearchRequest {
                    query: String::new(),
                    language: None,
                    page: None,
                    hits_per_page: Some(100),
                },
            )
            .unwrap();
        let mut ids: Vec<_> = result
            .hits
            .iter()
            .map(|entry| entry["objectID"].as_str().unwrap())
            .collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            ["first", "second"],
            "{name}: both successful mutations must survive reload"
        );
    }
}

#[test]
fn invalid_dictionary_batch_preserves_prior_data_including_clear_requests() {
    let dir = tempfile::tempdir().unwrap();
    let manager = DictionaryManager::new(dir.path());
    for &name in DictionaryName::all() {
        manager
            .batch("customer", name, &addition(name, "retained"))
            .unwrap();
        for clear in [false, true] {
            let mut request = addition(name, "must-not-appear");
            request.clear_existing_dictionary_entries = clear;
            request.requests.push(BatchRequest {
                action: BatchAction::DeleteEntry,
                body: serde_json::json!({"objectID": ""}),
            });
            assert!(manager.batch("customer", name, &request).is_err());
            let result = DictionaryManager::new(dir.path())
                .search(
                    "customer",
                    name,
                    &DictionarySearchRequest {
                        query: String::new(),
                        language: None,
                        page: None,
                        hits_per_page: Some(100),
                    },
                )
                .unwrap();
            assert_eq!(result.nb_hits, 1);
            assert_eq!(result.hits[0]["objectID"], "retained");
        }
    }
}

#[test]
fn blocked_dictionary_writer_does_not_block_another_application() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(DictionaryManager::new(dir.path()));
    let (read_tx, read_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first_manager = manager.clone();
    let first = std::thread::spawn(move || {
        AFTER_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                read_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }))
        });
        first_manager.batch(
            "customer-a",
            DictionaryName::Stopwords,
            &addition(DictionaryName::Stopwords, "first"),
        )
    });
    read_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let second = std::thread::spawn(move || {
        let result = manager.batch(
            "customer-b",
            DictionaryName::Stopwords,
            &addition(DictionaryName::Stopwords, "second"),
        );
        done_tx.send(()).unwrap();
        result
    });
    let completed = done_rx.recv_timeout(Duration::from_secs(2));
    release_tx.send(()).unwrap();
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    completed.expect("unrelated application must complete while first is paused");
}

thread_local! {
    static AFTER_SETTINGS: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

pub(super) fn after_effective_settings() {
    AFTER_SETTINGS.with(|hook| {
        let callback = hook.borrow_mut().take();
        if let Some(callback) = callback {
            callback();
        }
    });
}

#[test]
fn effective_dictionary_read_never_combines_different_configurations() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(DictionaryManager::new(dir.path()));
    manager
        .batch(
            "customer-a",
            DictionaryName::Stopwords,
            &addition(DictionaryName::Stopwords, "alpha"),
        )
        .unwrap();
    let writer = manager.clone();
    AFTER_SETTINGS.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let settings: DictionarySettings = serde_json::from_value(serde_json::json!({
                "disableStandardEntries": {"stopwords": {"en": true}}
            }))
            .unwrap();
            writer.set_settings("customer-a", &settings).unwrap();
            let mut update = addition(DictionaryName::Stopwords, "beta");
            update.clear_existing_dictionary_entries = true;
            writer
                .batch("customer-a", DictionaryName::Stopwords, &update)
                .unwrap();
        }));
    });
    let words = manager.effective_stopwords("customer-a", "en").unwrap();
    assert!(
        !(words.contains("the") && words.contains("beta")),
        "built-ins plus beta existed in no published configuration"
    );
    assert!(
        words.contains("alpha"),
        "the admitted read must retain its prior coherent configuration"
    );
    let later = manager.effective_stopwords("customer-a", "en").unwrap();
    assert!(later.contains("beta") && !later.contains("the") && !later.contains("alpha"));
}

#[test]
fn delivered_application_generation_is_the_dictionary_read_authority() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let manager = DictionaryManager::new(dir.path());
    let location = dir.path().join(".applications/customer-a/.dictionaries");
    std::fs::create_dir_all(&location).unwrap();
    let mut generation = serde_json::json!({
        "application_id": "customer-a", "revision": 7, "content_sha256": "",
        "settings": {"disableStandardEntries": {}}, "stopwords": [{"objectID":"delivered", "language":"en", "word":"delivered", "state":"enabled", "type":"custom"}],
        "plurals": [], "compounds": []
    });
    generation.sort_all_objects();
    generation["content_sha256"] = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&generation).unwrap())
    )
    .into();
    std::fs::write(
        location.join("configuration.json"),
        serde_json::to_vec(&generation).unwrap(),
    )
    .unwrap();
    assert!(
        manager
            .effective_stopwords("customer-a", "en")
            .unwrap()
            .contains("delivered"),
        "a delivered full generation must drive dictionary-dependent search"
    );
    assert!(!manager
        .effective_stopwords("customer-b", "en")
        .unwrap()
        .contains("delivered"));
}
