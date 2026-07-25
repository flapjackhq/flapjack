use super::*;
use crate::dictionaries::{BatchAction, BatchDictionaryRequest, BatchRequest};
use tempfile::TempDir;

fn make_manager() -> (TempDir, DictionaryManager) {
    let tmp = TempDir::new().unwrap();
    let mgr = DictionaryManager::new(tmp.path());
    (tmp, mgr)
}

fn add_entry_req(body: serde_json::Value) -> BatchRequest {
    BatchRequest {
        action: BatchAction::AddEntry,
        body,
    }
}

fn delete_entry_req(object_id: &str) -> BatchRequest {
    BatchRequest {
        action: BatchAction::DeleteEntry,
        body: serde_json::json!({ "objectID": object_id }),
    }
}

// ── Batch: add and search ─────────────────────────────────────────

/// Verify that batch-added stopword entries are persisted and discoverable via search.
#[test]
fn test_batch_add_then_search_stopwords() {
    let (_tmp, mgr) = make_manager();

    let req = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![
            add_entry_req(serde_json::json!({
                "objectID": "sw-1",
                "language": "en",
                "word": "the",
                "state": "enabled",
                "type": "custom"
            })),
            add_entry_req(serde_json::json!({
                "objectID": "sw-2",
                "language": "en",
                "word": "and",
                "state": "enabled",
                "type": "custom"
            })),
        ],
    };
    let resp = mgr
        .batch("tenant1", DictionaryName::Stopwords, &req)
        .unwrap();
    assert!(resp.task_id > 0);

    let search_resp = mgr
        .search(
            "tenant1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "the".into(),
                language: None,
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    assert_eq!(search_resp.nb_hits, 1);
    assert_eq!(search_resp.hits[0]["word"], "the");
}

// ── Batch: upsert (add with existing objectID) ────────────────────

/// Verify that adding an entry with a duplicate objectID replaces the previous entry rather than creating a second one.
#[test]
fn test_batch_upsert_overwrites() {
    let (_tmp, mgr) = make_manager();

    let req1 = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![add_entry_req(serde_json::json!({
            "objectID": "sw-1",
            "language": "en",
            "word": "old",
            "state": "enabled",
            "type": "custom"
        }))],
    };
    mgr.batch("t1", DictionaryName::Stopwords, &req1).unwrap();

    let req2 = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![add_entry_req(serde_json::json!({
            "objectID": "sw-1",
            "language": "en",
            "word": "new",
            "state": "enabled",
            "type": "custom"
        }))],
    };
    mgr.batch("t1", DictionaryName::Stopwords, &req2).unwrap();

    let search = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    assert_eq!(search.nb_hits, 1);
    assert_eq!(search.hits[0]["word"], "new");
}

// ── Batch: delete ─────────────────────────────────────────────────

/// Verify that a delete action removes the targeted entry while leaving others intact.
#[test]
fn test_batch_delete() {
    let (_tmp, mgr) = make_manager();

    let req = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![
            add_entry_req(serde_json::json!({
                "objectID": "sw-1",
                "language": "en",
                "word": "the",
                "state": "enabled",
                "type": "custom"
            })),
            add_entry_req(serde_json::json!({
                "objectID": "sw-2",
                "language": "en",
                "word": "and",
                "state": "enabled",
                "type": "custom"
            })),
        ],
    };
    mgr.batch("t1", DictionaryName::Stopwords, &req).unwrap();

    let del_req = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![delete_entry_req("sw-1")],
    };
    mgr.batch("t1", DictionaryName::Stopwords, &del_req)
        .unwrap();

    let search = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    assert_eq!(search.nb_hits, 1);
    assert_eq!(search.hits[0]["objectID"], "sw-2");
}

// ── Batch: delete missing is no-op ────────────────────────────────

#[test]
fn test_batch_delete_missing_noop() {
    let (_tmp, mgr) = make_manager();

    let req = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![delete_entry_req("nonexistent")],
    };
    // Should not error
    mgr.batch("t1", DictionaryName::Stopwords, &req).unwrap();
}

// ── Batch: clear existing ─────────────────────────────────────────

/// Verify that `clear_existing_dictionary_entries` discards all prior entries before applying the new batch.
#[test]
fn test_batch_clear_existing() {
    let (_tmp, mgr) = make_manager();

    // Add initial entries
    let req1 = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![
            add_entry_req(serde_json::json!({
                "objectID": "sw-1", "language": "en", "word": "the",
                "state": "enabled", "type": "custom"
            })),
            add_entry_req(serde_json::json!({
                "objectID": "sw-2", "language": "en", "word": "and",
                "state": "enabled", "type": "custom"
            })),
        ],
    };
    mgr.batch("t1", DictionaryName::Stopwords, &req1).unwrap();

    // Clear and add new
    let req2 = BatchDictionaryRequest {
        clear_existing_dictionary_entries: true,
        requests: vec![add_entry_req(serde_json::json!({
            "objectID": "sw-3", "language": "fr", "word": "le",
            "state": "enabled", "type": "custom"
        }))],
    };
    mgr.batch("t1", DictionaryName::Stopwords, &req2).unwrap();

    let search = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    assert_eq!(search.nb_hits, 1);
    assert_eq!(search.hits[0]["objectID"], "sw-3");
}

// ── Batch: missing objectID rejected ──────────────────────────────

/// Verify that an entry body missing the `objectID` field is rejected with an appropriate error.
#[test]
fn test_batch_missing_object_id_rejected() {
    let (_tmp, mgr) = make_manager();

    let req = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![add_entry_req(serde_json::json!({
            "language": "en",
            "word": "the"
        }))],
    };
    let result = mgr.batch("t1", DictionaryName::Stopwords, &req);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("objectID"),
        "error should mention objectID: {}",
        err_msg
    );
}

// ── Search: language filter ───────────────────────────────────────

/// Verify that the optional language parameter restricts search results to the specified language.
#[test]
fn test_search_language_filter() {
    let (_tmp, mgr) = make_manager();

    let req = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![
            add_entry_req(serde_json::json!({
                "objectID": "sw-1", "language": "en", "word": "the",
                "state": "enabled", "type": "custom"
            })),
            add_entry_req(serde_json::json!({
                "objectID": "sw-2", "language": "fr", "word": "le",
                "state": "enabled", "type": "custom"
            })),
        ],
    };
    mgr.batch("t1", DictionaryName::Stopwords, &req).unwrap();

    let search = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: Some("fr".into()),
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    assert_eq!(search.nb_hits, 1);
    assert_eq!(search.hits[0]["language"], "fr");
}

// ── Search: pagination ────────────────────────────────────────────

/// Verify correct page slicing, `nb_pages` calculation, and partial last-page behavior.
#[test]
fn test_search_pagination() {
    let (_tmp, mgr) = make_manager();

    let mut requests = Vec::new();
    for i in 0..5 {
        requests.push(add_entry_req(serde_json::json!({
            "objectID": format!("sw-{:03}", i),
            "language": "en",
            "word": format!("word{}", i),
            "state": "enabled",
            "type": "custom"
        })));
    }
    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests,
        },
    )
    .unwrap();

    let page0 = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: Some(0),
                hits_per_page: Some(2),
            },
        )
        .unwrap();
    assert_eq!(page0.hits.len(), 2);
    assert_eq!(page0.nb_hits, 5);
    assert_eq!(page0.nb_pages, 3);
    assert_eq!(page0.page, 0);

    let page2 = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: Some(2),
                hits_per_page: Some(2),
            },
        )
        .unwrap();
    assert_eq!(page2.hits.len(), 1); // last page has 1 item
    assert_eq!(page2.page, 2);
}

#[test]
fn test_search_pagination_extreme_page_does_not_panic() {
    let (_tmp, mgr) = make_manager();
    let requests = (0..5)
        .map(|i| {
            add_entry_req(serde_json::json!({
                "objectID": format!("sw-{i:03}"),
                "language": "en",
                "word": format!("word{i}"),
                "state": "enabled",
                "type": "custom"
            }))
        })
        .collect();

    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests,
        },
    )
    .unwrap();

    let response = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: Some(usize::MAX),
                hits_per_page: Some(2),
            },
        )
        .unwrap();

    assert_eq!(response.nb_hits, 5);
    assert_eq!(response.nb_pages, 3);
    assert_eq!(response.page, usize::MAX);
    assert!(response.hits.is_empty());
}

/// Verify that a `hits_per_page` of zero is clamped to 1, returning at least one result per page.
#[test]
fn test_search_hits_per_page_zero_is_clamped() {
    let (_tmp, mgr) = make_manager();

    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![add_entry_req(serde_json::json!({
                "objectID": "sw-1",
                "language": "en",
                "word": "the",
                "state": "enabled",
                "type": "custom"
            }))],
        },
    )
    .unwrap();

    let search = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: Some(0),
                hits_per_page: Some(0),
            },
        )
        .unwrap();

    assert_eq!(search.nb_hits, 1);
    assert_eq!(search.nb_pages, 1);
    assert_eq!(search.hits.len(), 1);
}

/// Verify that a `hits_per_page` exceeding the 1000-entry ceiling is clamped, producing correct multi-page results.
#[test]
fn test_search_hits_per_page_above_max_is_clamped() {
    let (_tmp, mgr) = make_manager();

    let requests: Vec<BatchRequest> = (0..1001)
        .map(|i| {
            add_entry_req(serde_json::json!({
                "objectID": format!("sw-{i:04}"),
                "language": "en",
                "word": format!("word{i}"),
                "state": "enabled",
                "type": "custom"
            }))
        })
        .collect();

    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests,
        },
    )
    .unwrap();

    let page0 = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: Some(0),
                hits_per_page: Some(5000),
            },
        )
        .unwrap();
    assert_eq!(page0.nb_hits, 1001);
    assert_eq!(page0.nb_pages, 2);
    assert_eq!(page0.hits.len(), 1000);

    let page1 = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: Some(1),
                hits_per_page: Some(5000),
            },
        )
        .unwrap();
    assert_eq!(page1.hits.len(), 1);
}

/// Verify that search results are returned in ascending lexicographic order of `objectID`.
#[test]
fn test_search_results_sorted_by_object_id() {
    let (_tmp, mgr) = make_manager();

    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![
                add_entry_req(serde_json::json!({
                    "objectID": "sw-c",
                    "language": "en",
                    "word": "gamma",
                    "state": "enabled",
                    "type": "custom"
                })),
                add_entry_req(serde_json::json!({
                    "objectID": "sw-a",
                    "language": "en",
                    "word": "alpha",
                    "state": "enabled",
                    "type": "custom"
                })),
                add_entry_req(serde_json::json!({
                    "objectID": "sw-b",
                    "language": "en",
                    "word": "beta",
                    "state": "enabled",
                    "type": "custom"
                })),
            ],
        },
    )
    .unwrap();

    let search = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    let object_ids: Vec<String> = search
        .hits
        .iter()
        .map(|hit| hit["objectID"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        object_ids,
        vec!["sw-a".to_string(), "sw-b".to_string(), "sw-c".to_string()]
    );
}

// ── Search: empty results ─────────────────────────────────────────

/// Verify that searching with no matching entries returns zero hits and zero pages.
#[test]
fn test_search_empty() {
    let (_tmp, mgr) = make_manager();

    let search = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "nothing".into(),
                language: None,
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    assert_eq!(search.nb_hits, 0);
    assert_eq!(search.nb_pages, 0);
    assert!(search.hits.is_empty());
}

/// Verify that an unrecognized language code in a batch request produces a validation error.
#[test]
fn test_batch_rejects_unsupported_language() {
    let (_tmp, mgr) = make_manager();

    let req = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![add_entry_req(serde_json::json!({
            "objectID": "sw-1",
            "language": "xx",
            "word": "the",
            "state": "enabled",
            "type": "custom"
        }))],
    };
    let err = mgr
        .batch("t1", DictionaryName::Stopwords, &req)
        .expect_err("unsupported language should be rejected");
    assert!(
        err.to_string().contains("language"),
        "error should mention language: {err}"
    );
}

/// Verify that uppercase language codes (e.g. "EN") are normalized to lowercase and matched correctly in subsequent searches.
#[test]
fn test_batch_normalizes_language_code() {
    let (_tmp, mgr) = make_manager();

    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![add_entry_req(serde_json::json!({
                "objectID": "sw-1",
                "language": "EN",
                "word": "the",
                "state": "enabled",
                "type": "custom"
            }))],
        },
    )
    .unwrap();

    let search = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: Some("en".into()),
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    assert_eq!(
        search.nb_hits, 1,
        "language should be canonicalized to lowercase"
    );
}

/// Verify that an unsupported language code in the search language filter produces a validation error.
#[test]
fn test_search_rejects_unsupported_language_filter() {
    let (_tmp, mgr) = make_manager();
    let err = mgr
        .search(
            "t1",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: Some("xx".into()),
                page: None,
                hits_per_page: None,
            },
        )
        .expect_err("unsupported language filter should be rejected");
    assert!(
        err.to_string().contains("language"),
        "error should mention language: {err}"
    );
}

// ── Plurals batch + search ────────────────────────────────────────

/// Verify end-to-end batch insert and substring search for plural dictionary entries.
#[test]
fn test_plurals_batch_and_search() {
    let (_tmp, mgr) = make_manager();

    let req = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![add_entry_req(serde_json::json!({
            "objectID": "pl-1",
            "language": "en",
            "words": ["mouse", "mice"],
            "type": "custom"
        }))],
    };
    mgr.batch("t1", DictionaryName::Plurals, &req).unwrap();

    let search = mgr
        .search(
            "t1",
            DictionaryName::Plurals,
            &DictionarySearchRequest {
                query: "mouse".into(),
                language: None,
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    assert_eq!(search.nb_hits, 1);
    assert_eq!(
        search.hits[0]["words"],
        serde_json::json!(["mouse", "mice"])
    );
}

// ── Compounds batch + search ──────────────────────────────────────

/// Verify end-to-end batch insert and substring search for compound dictionary entries.
#[test]
fn test_compounds_batch_and_search() {
    let (_tmp, mgr) = make_manager();

    let req = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![add_entry_req(serde_json::json!({
            "objectID": "cp-1",
            "language": "de",
            "word": "Lebensversicherung",
            "decomposition": ["Leben", "Versicherung"],
            "type": "custom"
        }))],
    };
    mgr.batch("t1", DictionaryName::Compounds, &req).unwrap();

    let search = mgr
        .search(
            "t1",
            DictionaryName::Compounds,
            &DictionarySearchRequest {
                query: "Leben".into(),
                language: None,
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    assert_eq!(search.nb_hits, 1);
}

// ── Settings get/set ──────────────────────────────────────────────

/// Verify that dictionary settings survive a save/load round-trip and that `is_standard_disabled` reflects the persisted state.
#[test]
fn test_settings_get_set_roundtrip() {
    let (_tmp, mgr) = make_manager();

    // Default empty
    let settings = mgr.get_settings("t1").unwrap();
    assert!(settings.disable_standard_entries.is_empty());

    // Set
    let mut new_settings = DictionarySettings::default();
    new_settings.disable_standard_entries.insert(
        DictionaryName::Stopwords,
        [("fr".to_string(), true)].into_iter().collect(),
    );
    let resp = mgr.set_settings("t1", &new_settings).unwrap();
    assert!(resp.task_id > 0);
    assert!(
        chrono::DateTime::parse_from_rfc3339(&resp.updated_at).is_ok(),
        "updated_at should be RFC3339: {}",
        resp.updated_at
    );

    // Get
    let loaded = mgr.get_settings("t1").unwrap();
    assert!(loaded.is_standard_disabled(DictionaryName::Stopwords, "fr"));
    assert!(!loaded.is_standard_disabled(DictionaryName::Stopwords, "en"));
}

// ── Languages listing ─────────────────────────────────────────────

/// Verify that `list_languages` aggregates per-language custom entry counts across dictionary types.
#[test]
fn test_languages_listing() {
    let (_tmp, mgr) = make_manager();

    // Add entries across languages
    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![
                add_entry_req(serde_json::json!({
                    "objectID": "sw-1", "language": "en", "word": "the",
                    "state": "enabled", "type": "custom"
                })),
                add_entry_req(serde_json::json!({
                    "objectID": "sw-2", "language": "en", "word": "a",
                    "state": "enabled", "type": "custom"
                })),
            ],
        },
    )
    .unwrap();
    mgr.batch(
        "t1",
        DictionaryName::Plurals,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![add_entry_req(serde_json::json!({
                "objectID": "pl-1", "language": "en",
                "words": ["cat", "cats"], "type": "custom"
            }))],
        },
    )
    .unwrap();

    let langs = mgr.list_languages("t1").unwrap();
    let en = &langs["en"];
    assert_eq!(en.stopwords.as_ref().unwrap().nb_custom_entries, 2);
    assert_eq!(en.plurals.as_ref().unwrap().nb_custom_entries, 1);
    assert!(en.compounds.is_none());
}

// ── Merge helpers ─────────────────────────────────────────────────

/// Verify that `effective_stopwords` merges built-in stopwords with custom enabled entries.
#[test]
fn test_effective_stopwords_with_custom() {
    let (_tmp, mgr) = make_manager();

    // Add a custom stopword
    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![add_entry_req(serde_json::json!({
                "objectID": "sw-custom",
                "language": "en",
                "word": "xyzzy",
                "state": "enabled",
                "type": "custom"
            }))],
        },
    )
    .unwrap();

    let stopwords = mgr.effective_stopwords("t1", "en").unwrap();
    // Should contain both built-in and custom
    assert!(stopwords.contains("the"), "should have built-in 'the'");
    assert!(stopwords.contains("xyzzy"), "should have custom 'xyzzy'");
}

/// Verify that disabling standard entries excludes built-ins while retaining custom entries.
#[test]
fn test_effective_stopwords_standard_disabled() {
    let (_tmp, mgr) = make_manager();

    // Add a custom entry
    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![add_entry_req(serde_json::json!({
                "objectID": "sw-custom",
                "language": "en",
                "word": "xyzzy",
                "state": "enabled",
                "type": "custom"
            }))],
        },
    )
    .unwrap();

    // Disable standard entries for English
    let mut settings = DictionarySettings::default();
    settings.disable_standard_entries.insert(
        DictionaryName::Stopwords,
        [("en".to_string(), true)].into_iter().collect(),
    );
    mgr.set_settings("t1", &settings).unwrap();

    let stopwords = mgr.effective_stopwords("t1", "en").unwrap();
    // Should have custom but NOT built-in
    assert!(stopwords.contains("xyzzy"), "should have custom 'xyzzy'");
    assert!(
        !stopwords.contains("the"),
        "should NOT have built-in 'the' when standard disabled"
    );
}

/// Verify that disabling standard entries for one language does not affect other languages.
#[test]
fn test_effective_stopwords_standard_disabled_is_language_scoped() {
    let (_tmp, mgr) = make_manager();

    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![add_entry_req(serde_json::json!({
                "objectID": "sw-fr-custom",
                "language": "fr",
                "word": "bonjour",
                "state": "enabled",
                "type": "custom"
            }))],
        },
    )
    .unwrap();

    let mut settings = DictionarySettings::default();
    settings.disable_standard_entries.insert(
        DictionaryName::Stopwords,
        [("fr".to_string(), true)].into_iter().collect(),
    );
    mgr.set_settings("t1", &settings).unwrap();

    let fr = mgr.effective_stopwords("t1", "fr").unwrap();
    assert!(
        fr.contains("bonjour"),
        "should include custom French stopword"
    );
    assert!(
        !fr.contains("le"),
        "should exclude built-in French stopwords when disabled"
    );

    let en = mgr.effective_stopwords("t1", "en").unwrap();
    assert!(
        en.contains("the"),
        "disabling French standard entries must not disable English stopwords"
    );
}

/// Verify that a custom entry with state `disabled` suppresses the corresponding built-in stopword.
#[test]
fn test_effective_stopwords_disabled_entry_excluded() {
    let (_tmp, mgr) = make_manager();

    // Add a disabled custom stopword
    mgr.batch(
        "t1",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![add_entry_req(serde_json::json!({
                "objectID": "sw-disabled",
                "language": "en",
                "word": "the",
                "state": "disabled",
                "type": "custom"
            }))],
        },
    )
    .unwrap();

    let stopwords = mgr.effective_stopwords("t1", "en").unwrap();
    // "the" is a built-in but the custom disabled entry should suppress it
    assert!(
        !stopwords.contains("the"),
        "disabled custom entry should suppress built-in 'the'"
    );
}

/// Verify that `custom_plural_sets` returns only the equivalence sets for the requested language.
#[test]
fn test_custom_plural_sets() {
    let (_tmp, mgr) = make_manager();

    mgr.batch(
        "t1",
        DictionaryName::Plurals,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![add_entry_req(serde_json::json!({
                "objectID": "pl-custom",
                "language": "en",
                "words": ["cactus", "cacti"],
                "type": "custom"
            }))],
        },
    )
    .unwrap();

    let plurals = mgr.custom_plural_sets("t1", "en").unwrap();
    let has_custom = plurals
        .iter()
        .any(|set| set.contains(&"cactus".to_string()) && set.contains(&"cacti".to_string()));
    assert!(
        has_custom,
        "should contain custom plural set [cactus, cacti]"
    );
}

#[test]
fn test_use_builtin_plurals() {
    let (_tmp, mgr) = make_manager();

    assert!(mgr.use_builtin_plurals("t1", "en").unwrap());

    let mut settings = DictionarySettings::default();
    settings.disable_standard_entries.insert(
        DictionaryName::Plurals,
        [("en".to_string(), true)].into_iter().collect(),
    );
    mgr.set_settings("t1", &settings).unwrap();

    assert!(!mgr.use_builtin_plurals("t1", "en").unwrap());
}

/// Verify that `effective_compounds` returns the custom decomposition map for a language.
#[test]
fn test_effective_compounds_custom() {
    let (_tmp, mgr) = make_manager();

    mgr.batch(
        "t1",
        DictionaryName::Compounds,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![add_entry_req(serde_json::json!({
                "objectID": "cp-custom",
                "language": "de",
                "word": "Haustür",
                "decomposition": ["Haus", "Tür"],
                "type": "custom"
            }))],
        },
    )
    .unwrap();

    let compounds = mgr.effective_compounds("t1", "de").unwrap();
    assert_eq!(
        compounds.get("Haustür").unwrap(),
        &vec!["Haus".to_string(), "Tür".to_string()]
    );
}

// ── is_standard_disabled convenience ──────────────────────────────

/// Verify the `is_standard_disabled` convenience method before and after toggling the setting.
#[test]
fn test_is_standard_disabled() {
    let (_tmp, mgr) = make_manager();

    assert!(!mgr
        .is_standard_disabled("t1", DictionaryName::Stopwords, "en")
        .unwrap());

    let mut settings = DictionarySettings::default();
    settings.disable_standard_entries.insert(
        DictionaryName::Stopwords,
        [("en".to_string(), true)].into_iter().collect(),
    );
    mgr.set_settings("t1", &settings).unwrap();

    assert!(mgr
        .is_standard_disabled("t1", DictionaryName::Stopwords, "en")
        .unwrap());
}

// ── Multi-tenant isolation ────────────────────────────────────────

/// Verify that entries written under one tenant ID are invisible to a different tenant.
#[test]
fn test_multi_tenant_isolation() {
    let (_tmp, mgr) = make_manager();

    mgr.batch(
        "tenant-a",
        DictionaryName::Stopwords,
        &BatchDictionaryRequest {
            clear_existing_dictionary_entries: false,
            requests: vec![add_entry_req(serde_json::json!({
                "objectID": "sw-1", "language": "en", "word": "foo",
                "state": "enabled", "type": "custom"
            }))],
        },
    )
    .unwrap();

    // tenant-b should have no entries
    let search = mgr
        .search(
            "tenant-b",
            DictionaryName::Stopwords,
            &DictionarySearchRequest {
                query: "".into(),
                language: None,
                page: None,
                hits_per_page: None,
            },
        )
        .unwrap();
    assert_eq!(search.nb_hits, 0);
}

// ── Path traversal rejection ──────────────────────────────────────

#[test]
fn test_tenant_id_rejects_path_traversal() {
    let (_tmp, mgr) = make_manager();
    let req = BatchDictionaryRequest {
        clear_existing_dictionary_entries: false,
        requests: vec![],
    };
    for bad_id in ["../escape", "foo/bar", "foo\\bar", "", "a\0b"] {
        let result = mgr.batch(bad_id, DictionaryName::Stopwords, &req);
        assert!(result.is_err(), "should reject tenant_id {:?}", bad_id);
    }
}
