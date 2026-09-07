//! Query integration tests moved inline from engine/tests/test_query.rs.
//!
//! Covers: plurals, stopwords, synonym store persistence, highlighter regression,
//! and JSON prefix search. The 2 tests that depend on flapjack_http::dto::SearchRequest
//! remain in engine/tests/test_query.rs.

use crate::index::settings::IndexSettings;
use crate::index::synonyms::{Synonym, SynonymStore};
use crate::index::{get_global_budget, Index, SearchOptions};
use crate::query::plurals::IgnorePluralsValue;
use crate::query::stopwords::RemoveStopWordsValue;
use crate::types::{Document, FieldValue};
use crate::IndexManager;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

fn doc(id: &str, fields: Vec<(&str, FieldValue)>) -> Document {
    let f: HashMap<String, FieldValue> = fields
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    Document {
        id: id.to_string(),
        fields: f,
    }
}

fn text(s: &str) -> FieldValue {
    FieldValue::Text(s.to_string())
}

fn result_ids(result: &crate::types::SearchResult) -> Vec<&str> {
    result
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect()
}

// ============================================================
// Shared fixtures
// ============================================================

struct PluralFixture {
    _tmp: TempDir,
    mgr: Arc<IndexManager>,
}

static PLURAL_FIXTURE: tokio::sync::OnceCell<PluralFixture> = tokio::sync::OnceCell::const_new();

async fn get_plural_fixture() -> &'static PluralFixture {
    PLURAL_FIXTURE
        .get_or_init(|| async {
            let temp_dir = TempDir::new().unwrap();
            let manager = IndexManager::new(temp_dir.path());
            manager.create_tenant("test").unwrap();

            let settings = IndexSettings {
                ignore_plurals: IgnorePluralsValue::All,
                query_languages: vec!["en".to_string()],
                ..Default::default()
            };
            settings
                .save(temp_dir.path().join("test/settings.json"))
                .unwrap();
            manager.invalidate_settings_cache("test");

            let docs = vec![
                doc("1", vec![("name", text("car"))]),
                doc("2", vec![("name", text("cars"))]),
                doc("3", vec![("name", text("child"))]),
                doc("4", vec![("name", text("children"))]),
                doc("5", vec![("name", text("battery"))]),
                doc("6", vec![("name", text("batteries"))]),
                doc("7", vec![("name", text("church"))]),
                doc("8", vec![("name", text("churches"))]),
                doc("9", vec![("name", text("knife"))]),
                doc("10", vec![("name", text("knives"))]),
                doc("11", vec![("name", text("person"))]),
                doc("12", vec![("name", text("people"))]),
            ];
            manager.add_documents_sync("test", docs).await.unwrap();

            PluralFixture {
                _tmp: temp_dir,
                mgr: manager,
            }
        })
        .await
}

struct StopwordFixture {
    _tmp: TempDir,
    mgr: Arc<IndexManager>,
}

static STOPWORD_ENABLED_FIXTURE: tokio::sync::OnceCell<StopwordFixture> =
    tokio::sync::OnceCell::const_new();
static STOPWORD_DISABLED_FIXTURE: tokio::sync::OnceCell<StopwordFixture> =
    tokio::sync::OnceCell::const_new();
static STOPWORD_LANG_EN_FIXTURE: tokio::sync::OnceCell<StopwordFixture> =
    tokio::sync::OnceCell::const_new();
static STOPWORD_LANG_XX_FIXTURE: tokio::sync::OnceCell<StopwordFixture> =
    tokio::sync::OnceCell::const_new();

async fn make_stopword_fixture(rsw: RemoveStopWordsValue) -> StopwordFixture {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        remove_stop_words: rsw,
        ..IndexSettings::default()
    };
    settings
        .save(temp_dir.path().join("test/settings.json"))
        .unwrap();
    manager.invalidate_settings_cache("test");

    let docs = vec![
        doc("1", vec![("title", text("best search engine"))]),
        doc("2", vec![("title", text("the best search tool"))]),
        doc("3", vec![("title", text("how to build a search engine"))]),
        doc("4", vec![("title", text("search and discover"))]),
        doc("5", vec![("title", text("is this a test"))]),
    ];
    manager.add_documents_sync("test", docs).await.unwrap();

    StopwordFixture {
        _tmp: temp_dir,
        mgr: manager,
    }
}

async fn get_stopword_enabled() -> &'static StopwordFixture {
    STOPWORD_ENABLED_FIXTURE
        .get_or_init(|| make_stopword_fixture(RemoveStopWordsValue::All))
        .await
}

async fn get_stopword_disabled() -> &'static StopwordFixture {
    STOPWORD_DISABLED_FIXTURE
        .get_or_init(|| make_stopword_fixture(RemoveStopWordsValue::Disabled))
        .await
}

async fn get_stopword_lang_en() -> &'static StopwordFixture {
    STOPWORD_LANG_EN_FIXTURE
        .get_or_init(|| {
            make_stopword_fixture(RemoveStopWordsValue::Languages(vec!["en".to_string()]))
        })
        .await
}

async fn get_stopword_lang_xx() -> &'static StopwordFixture {
    STOPWORD_LANG_XX_FIXTURE
        .get_or_init(|| {
            make_stopword_fixture(RemoveStopWordsValue::Languages(vec!["xx".to_string()]))
        })
        .await
}

fn search_result_ids(mgr: &IndexManager, query: &str) -> Vec<String> {
    mgr.search("test", query, None, None, 20)
        .unwrap()
        .documents
        .iter()
        .map(|d| d.document.id.clone())
        .collect()
}

async fn setup_language_fixture(
    query_language: &str,
    query_type: &str,
    docs: Vec<Document>,
) -> (TempDir, Arc<IndexManager>) {
    let settings = IndexSettings {
        query_languages: vec![query_language.to_string()],
        index_languages: vec![query_language.to_string()],
        query_type: query_type.to_string(),
        ..Default::default()
    };
    setup_tenant_fixture(settings, docs, false).await
}

async fn setup_tenant_fixture(
    settings: IndexSettings,
    docs: Vec<Document>,
    unload_before_invalidate: bool,
) -> (TempDir, Arc<IndexManager>) {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test").unwrap();

    settings
        .save(temp_dir.path().join("test/settings.json"))
        .unwrap();
    if unload_before_invalidate {
        manager.unload_tenant("test");
    }
    manager.invalidate_settings_cache("test");

    manager.add_documents_sync("test", docs).await.unwrap();
    (temp_dir, manager)
}

// ============================================================
// PLURAL TESTS (shared fixture) — 12 tests
// ============================================================

mod plurals {
    use super::*;

    #[tokio::test]
    async fn car_finds_cars() {
        let f = get_plural_fixture().await;
        let ids = search_result_ids(&f.mgr, "car");
        assert!(ids.contains(&"1".to_string()));
        assert!(ids.contains(&"2".to_string()));
    }

    #[tokio::test]
    async fn cars_finds_car() {
        let f = get_plural_fixture().await;
        let ids = search_result_ids(&f.mgr, "cars");
        assert!(ids.contains(&"1".to_string()));
        assert!(ids.contains(&"2".to_string()));
    }

    #[tokio::test]
    async fn child_finds_children() {
        let f = get_plural_fixture().await;
        let ids = search_result_ids(&f.mgr, "child");
        assert!(ids.contains(&"3".to_string()));
        assert!(ids.contains(&"4".to_string()));
    }

    #[tokio::test]
    async fn children_finds_child() {
        let f = get_plural_fixture().await;
        let ids = search_result_ids(&f.mgr, "children");
        assert!(ids.contains(&"3".to_string()));
        assert!(ids.contains(&"4".to_string()));
    }

    #[tokio::test]
    async fn battery_finds_batteries() {
        let f = get_plural_fixture().await;
        let ids = search_result_ids(&f.mgr, "battery");
        assert!(ids.contains(&"5".to_string()));
        assert!(ids.contains(&"6".to_string()));
    }

    #[tokio::test]
    async fn person_finds_people() {
        let f = get_plural_fixture().await;
        let ids = search_result_ids(&f.mgr, "person");
        assert!(ids.contains(&"11".to_string()));
        assert!(ids.contains(&"12".to_string()));
    }

    #[tokio::test]
    async fn knife_finds_knives() {
        let f = get_plural_fixture().await;
        let ids = search_result_ids(&f.mgr, "knife");
        assert!(ids.contains(&"9".to_string()));
        assert!(ids.contains(&"10".to_string()));
    }

    #[tokio::test]
    async fn disabled_no_expansion() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            ignore_plurals: IgnorePluralsValue::Disabled,
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();

        let docs = vec![
            doc("1", vec![("name", text("child"))]),
            doc("2", vec![("name", text("children"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager.search("test", "child ", None, None, 10).unwrap();
        let ids: Vec<&str> = result
            .documents
            .iter()
            .map(|d| d.document.id.as_str())
            .collect();
        assert!(ids.contains(&"1"));
        assert!(!ids.contains(&"2"));
    }

    #[tokio::test]
    async fn per_query_override_enables_plural_expansion() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings::default();
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();

        let docs = vec![
            doc("1", vec![("name", text("child"))]),
            doc("2", vec![("name", text("children"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        let ip = IgnorePluralsValue::All;
        let ql = vec!["en".to_string()];
        let result = manager
            .search_with_options(
                "test",
                "child",
                &SearchOptions {
                    limit: 10,
                    ignore_plurals: Some(&ip),
                    query_languages: Some(&ql),
                    ..SearchOptions::default()
                },
            )
            .unwrap();
        let ids: Vec<&str> = result
            .documents
            .iter()
            .map(|d| d.document.id.as_str())
            .collect();
        assert!(ids.contains(&"1"));
        assert!(ids.contains(&"2"));
    }

    #[tokio::test]
    async fn query_languages_wiring() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            ignore_plurals: IgnorePluralsValue::All,
            query_languages: vec!["xx".to_string()],
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![
            doc("1", vec![("name", text("child"))]),
            doc("2", vec![("name", text("children"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager.search("test", "child ", None, None, 10).unwrap();
        let ids: Vec<&str> = result
            .documents
            .iter()
            .map(|d| d.document.id.as_str())
            .collect();
        assert!(ids.contains(&"1"));
        assert!(!ids.contains(&"2"));
    }

    #[tokio::test]
    async fn serde_roundtrip_settings() {
        let settings = IndexSettings {
            ignore_plurals: IgnorePluralsValue::Languages(vec!["en".to_string(), "fr".to_string()]),
            query_languages: vec!["en".to_string()],
            ..Default::default()
        };
        let json = serde_json::to_string_pretty(&settings).unwrap();
        let loaded: IndexSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(
            loaded.ignore_plurals,
            IgnorePluralsValue::Languages(vec!["en".to_string(), "fr".to_string()])
        );
        assert_eq!(loaded.query_languages, vec!["en".to_string()]);
    }

    #[tokio::test]
    async fn settings_default_false() {
        let json = r#"{"queryType":"prefixLast"}"#;
        let settings: IndexSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.ignore_plurals, IgnorePluralsValue::Disabled);
        assert!(settings.query_languages.is_empty());
    }
}

// ============================================================
// STOPWORD TESTS (minus 2 SearchRequest tests) — 12 tests
// ============================================================

mod stopwords {
    use super::*;

    #[tokio::test]
    async fn disabled_matches_all_words() {
        let f = get_stopword_disabled().await;
        let result = f.mgr.search("test", "the best", None, None, 10).unwrap();
        let ids: Vec<&str> = result
            .documents
            .iter()
            .map(|d| d.document.id.as_str())
            .collect();
        assert!(
            ids.contains(&"2"),
            "should match 'the best search tool' when stop words disabled"
        );

        let result2 = f.mgr.search("test", "the", None, None, 10).unwrap();
        assert!(
            result2.total > 0,
            "'the' should match docs when stop words disabled"
        );
    }

    #[tokio::test]
    async fn enabled_strips_common_words() {
        let f = get_stopword_enabled().await;
        let with_stop = f
            .mgr
            .search("test", "the best search", None, None, 10)
            .unwrap();
        let without_stop = f.mgr.search("test", "best search", None, None, 10).unwrap();
        assert_eq!(
            with_stop
                .documents
                .iter()
                .map(|d| d.document.id.as_str())
                .collect::<Vec<_>>(),
            without_stop
                .documents
                .iter()
                .map(|d| d.document.id.as_str())
                .collect::<Vec<_>>(),
            "removing 'the' should produce same results"
        );
    }

    #[tokio::test]
    async fn all_stop_words_query_not_emptied() {
        let f = get_stopword_enabled().await;
        let result = f.mgr.search("test", "the a an", None, None, 10).unwrap();
        assert!(
            result.total > 0,
            "all-stop-word query should not be emptied"
        );
    }

    #[tokio::test]
    async fn prefix_last_preserves_last_word() {
        let f = get_stopword_enabled().await;
        let result = f.mgr.search("test", "how to the", None, None, 10).unwrap();
        assert!(
            result.total > 0,
            "last word 'the' should be preserved as prefix in prefixLast mode"
        );
    }

    #[tokio::test]
    async fn language_specific_en() {
        let f = get_stopword_lang_en().await;
        let with_stop = f
            .mgr
            .search("test", "the search engine", None, None, 10)
            .unwrap();
        let without_stop = f
            .mgr
            .search("test", "search engine", None, None, 10)
            .unwrap();
        assert_eq!(
            with_stop
                .documents
                .iter()
                .map(|d| d.document.id.as_str())
                .collect::<Vec<_>>(),
            without_stop
                .documents
                .iter()
                .map(|d| d.document.id.as_str())
                .collect::<Vec<_>>(),
            "en stop words should strip 'the'"
        );
    }

    #[tokio::test]
    async fn unsupported_language_noop() {
        let f = get_stopword_lang_xx().await;
        let result = f.mgr.search("test", "the best", None, None, 10).unwrap();
        let ids: Vec<&str> = result
            .documents
            .iter()
            .map(|d| d.document.id.as_str())
            .collect();
        assert!(
            ids.contains(&"2"),
            "unsupported lang should not strip any words"
        );
    }

    #[tokio::test]
    async fn does_not_affect_content_words() {
        let f = get_stopword_enabled().await;
        let result = f
            .mgr
            .search("test", "search engine", None, None, 10)
            .unwrap();
        let ids: Vec<&str> = result
            .documents
            .iter()
            .map(|d| d.document.id.as_str())
            .collect();
        assert!(ids.contains(&"1"));
        assert!(ids.contains(&"3"));
    }

    #[tokio::test]
    async fn empty_query_unchanged() {
        let f = get_stopword_enabled().await;
        let result = f.mgr.search("test", "", None, None, 10).unwrap();
        assert_eq!(result.total, 5);
    }

    #[tokio::test]
    async fn prefix_none_strips_all_stop_words() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            remove_stop_words: RemoveStopWordsValue::All,
            query_type: "prefixNone".to_string(),
            ..IndexSettings::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![
            doc("1", vec![("title", text("best search engine"))]),
            doc("2", vec![("title", text("the best search tool"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        let with_the = manager
            .search("test", "the best search", None, None, 10)
            .unwrap();
        let without_the = manager
            .search("test", "best search", None, None, 10)
            .unwrap();
        assert_eq!(
            with_the
                .documents
                .iter()
                .map(|d| d.document.id.as_str())
                .collect::<Vec<_>>(),
            without_the
                .documents
                .iter()
                .map(|d| d.document.id.as_str())
                .collect::<Vec<_>>(),
        );
    }

    #[tokio::test]
    async fn per_query_override_enables_stopword_removal() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let docs = vec![
            doc("1", vec![("title", text("best search engine"))]),
            doc("2", vec![("title", text("the best search tool"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        let enabled = RemoveStopWordsValue::All;
        let with_override = manager
            .search_with_options(
                "test",
                "the best search",
                &SearchOptions {
                    limit: 10,
                    remove_stop_words: Some(&enabled),
                    ..SearchOptions::default()
                },
            )
            .unwrap();
        let without_override = manager
            .search_with_options(
                "test",
                "best search",
                &SearchOptions {
                    limit: 10,
                    ..SearchOptions::default()
                },
            )
            .unwrap();
        assert_eq!(
            with_override
                .documents
                .iter()
                .map(|d| d.document.id.as_str())
                .collect::<Vec<_>>(),
            without_override
                .documents
                .iter()
                .map(|d| d.document.id.as_str())
                .collect::<Vec<_>>(),
        );
    }

    #[tokio::test]
    async fn per_query_override_trumps_setting() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            remove_stop_words: RemoveStopWordsValue::All,
            ..IndexSettings::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![doc("1", vec![("title", text("the best search engine"))])];
        manager.add_documents_sync("test", docs).await.unwrap();

        let disabled = RemoveStopWordsValue::Disabled;
        let result = manager
            .search_with_options(
                "test",
                "the",
                &SearchOptions {
                    limit: 10,
                    remove_stop_words: Some(&disabled),
                    ..SearchOptions::default()
                },
            )
            .unwrap();
        assert!(
            result.total > 0,
            "per-query disabled should override setting enabled"
        );
    }

    #[tokio::test]
    async fn setting_serialization_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("settings.json");

        let mut settings = IndexSettings {
            remove_stop_words: RemoveStopWordsValue::All,
            ..IndexSettings::default()
        };
        settings.save(&path).unwrap();
        let loaded = IndexSettings::load(&path).unwrap();
        assert_eq!(loaded.remove_stop_words, RemoveStopWordsValue::All);

        settings.remove_stop_words =
            RemoveStopWordsValue::Languages(vec!["en".to_string(), "fr".to_string()]);
        settings.save(&path).unwrap();
        let loaded2 = IndexSettings::load(&path).unwrap();
        assert_eq!(
            loaded2.remove_stop_words,
            RemoveStopWordsValue::Languages(vec!["en".to_string(), "fr".to_string()])
        );
    }

    #[tokio::test]
    async fn existing_settings_without_field_defaults_to_false() {
        let json = r#"{"queryType":"prefixAll"}"#;
        let settings: IndexSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.remove_stop_words, RemoveStopWordsValue::Disabled);
    }
}

// ============================================================
// SYNONYM STORE PERSISTENCE (unique — not covered by inline tests)
// ============================================================

mod synonyms {
    use super::*;

    #[test]
    fn store_save_load() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("synonyms.json");

        let mut store = SynonymStore::new();
        store.insert(Synonym::Regular {
            object_id: "pants-trousers".to_string(),
            synonyms: vec!["pants".to_string(), "trousers".to_string()],
        });
        store.save(&path).unwrap();

        let loaded = SynonymStore::load(&path).unwrap();
        assert!(loaded.get("pants-trousers").is_some());
    }

    #[tokio::test]
    async fn pbv5_exp_004_multi_word_regular_synonyms_change_search_bidirectionally() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let mut store = SynonymStore::new();
        store.insert(Synonym::Regular {
            object_id: "aurora-rule".to_string(),
            synonyms: vec![
                "aurora".to_string(),
                "glow".to_string(),
                "northern lights".to_string(),
            ],
        });
        store
            .save(temp_dir.path().join("test/synonyms.json"))
            .unwrap();
        manager.invalidate_synonyms_cache("test");

        manager
            .add_documents_sync(
                "test",
                vec![
                    doc("aurora-doc", vec![("title", text("Aurora field guide"))]),
                    doc("phrase-doc", vec![("title", text("Northern lights guide"))]),
                ],
            )
            .await
            .unwrap();

        let phrase = manager
            .search("test", "northern lights", None, None, 10)
            .unwrap();
        let phrase_ids = result_ids(&phrase);
        assert!(
            phrase_ids.contains(&"aurora-doc"),
            "a multi-word regular synonym must expand to its single-word peer"
        );

        let reverse = manager.search("test", "aurora", None, None, 10).unwrap();
        let reverse_ids = result_ids(&reverse);
        assert!(
            reverse_ids.contains(&"phrase-doc"),
            "the single-word peer must expand back to the multi-word regular synonym"
        );

        let single_word = manager.search("test", "glow", None, None, 10).unwrap();
        let single_word_ids = result_ids(&single_word);
        assert!(single_word_ids.contains(&"aurora-doc"));

        let unrelated = manager.search("test", "sunset", None, None, 10).unwrap();
        assert_eq!(
            unrelated.total, 0,
            "unrelated queries must remain unrelated"
        );
    }
}

// ============================================================
// HIGHLIGHT BUG REGRESSION — 1 test
// ============================================================

mod highlight_bug {
    use crate::query::highlighter::Highlighter;

    #[test]
    fn multi_word_highlighting() {
        let highlighter = Highlighter::default();
        let query_words = vec!["essence".to_string(), "mascara".to_string()];

        // Brand field: "Essence" — should only match "essence"
        let result = highlighter.highlight_text("Essence", &query_words);
        assert_eq!(result.matched_words, vec!["essence"]);
        assert!(matches!(
            result.match_level,
            crate::query::highlighter::MatchLevel::Partial
        ));

        // tags[1] field: "mascara" — should only match "mascara"
        let result2 = highlighter.highlight_text("mascara", &query_words);
        assert_eq!(result2.matched_words, vec!["mascara"]);
        assert!(matches!(
            result2.match_level,
            crate::query::highlighter::MatchLevel::Partial
        ));

        // Name field: "Essence Mascara..." — should match both
        let result3 = highlighter.highlight_text("Essence Mascara Lash Princess", &query_words);
        assert_eq!(result3.matched_words, vec!["essence", "mascara"]);
        assert!(matches!(
            result3.match_level,
            crate::query::highlighter::MatchLevel::Full
        ));
    }
}

// ============================================================
// JSON PREFIX SEARCH — 1 test (test_single_word removed as redundant
// with test_library.rs::test_schemaless_prefix_search_end_to_end)
// ============================================================

mod json_prefix_search {
    use crate::index::manager::IndexManager;
    use crate::types::Document;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_multi_word_query_structure() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());

        manager.create_tenant("products").unwrap();

        let docs = [
            json!({"_id": "1", "title": "Gaming Laptop"}),
            json!({"_id": "2", "title": "Laptop Gaming Mouse"}),
            json!({"_id": "3", "title": "Gaming Mouse"}),
        ];

        let doc_objs: Vec<Document> = docs
            .iter()
            .map(|d| Document::from_json(d).unwrap())
            .collect();

        manager
            .add_documents_sync("products", doc_objs)
            .await
            .unwrap();

        let results = manager
            .search("products", "gaming lap", None, None, 10)
            .unwrap();

        assert!(
            !results.documents.is_empty(),
            "Expected 'gaming lap' to match at least 'Gaming Laptop'"
        );
    }
}

// ============================================================
// STAGE 1 SENTINELS — intentionally red baseline tests
// ============================================================

mod stage1_sentinels {
    use super::*;

    #[tokio::test]
    async fn stemming_gap_run_should_match_running_and_runs() {
        let (_tmp, manager) = setup_language_fixture(
            "en",
            "prefixNone",
            vec![
                doc("running", vec![("title", text("running"))]),
                doc("runs", vec![("title", text("runs"))]),
            ],
        )
        .await;

        let ids = search_result_ids(&manager, "run");
        assert!(
            ids.contains(&"running".to_string()) && ids.contains(&"runs".to_string()),
            "expected stemming behavior to match running/runs for query 'run' with prefixNone; got {ids:?}"
        );
    }

    #[tokio::test]
    async fn tier1_french_stemming_gap_manger_should_match_inflections() {
        // Tests French Snowball stemmer: manger→mang, mangeait→mang, mangé→mange→mang.
        // "mangeons" (→mangeon via Snowball) is deferred to Stage 3 morphological analysis.
        let (_tmp, manager) = setup_language_fixture(
            "fr",
            "prefixNone",
            vec![
                doc("manger", vec![("title", text("manger"))]),
                doc("mangeait", vec![("title", text("mangeait"))]),
                doc("mange", vec![("title", text("mangé"))]),
            ],
        )
        .await;

        let ids = search_result_ids(&manager, "manger");
        assert!(
            ids.contains(&"manger".to_string())
                && ids.contains(&"mangeait".to_string())
                && ids.contains(&"mange".to_string()),
            "expected French stemming behavior to match manger/mangeait/mangé for query 'manger'; got {ids:?}"
        );
    }
}

// ============================================================
// STAGE 3 TIER 1 STEMMING — integration behavior
// ============================================================

mod stage3_tier1_stemming {
    use super::*;
    use crate::query::stopwords::stopwords_for_lang;

    fn search_no_typo(
        manager: &IndexManager,
        query: &str,
    ) -> crate::error::Result<crate::types::SearchResult> {
        manager.search_with_options(
            "test",
            query,
            &SearchOptions {
                limit: 10,
                typo_tolerance: Some(false),
                ..SearchOptions::default()
            },
        )
    }

    fn search_result_ids_no_typo(manager: &IndexManager, query: &str) -> Vec<String> {
        let result = search_no_typo(manager, query).unwrap();
        result_ids(&result)
            .into_iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[tokio::test]
    async fn german_stemming_laufen_matches_inflections() {
        let (_tmp, manager) = setup_language_fixture(
            "de",
            "prefixNone",
            vec![
                doc("spielen", vec![("title", text("spielen"))]),
                doc("spiele", vec![("title", text("spiele"))]),
                doc("spiel", vec![("title", text("spiel"))]),
            ],
        )
        .await;

        let ids = search_result_ids_no_typo(&manager, "spielen");
        assert!(
            ids.contains(&"spielen".to_string())
                && ids.contains(&"spiele".to_string())
                && ids.contains(&"spiel".to_string()),
            "expected German stemming to match spielen/spiele/spiel for query 'spielen'; got {ids:?}"
        );
    }

    #[tokio::test]
    async fn spanish_stemming_correr_matches_inflections() {
        let (_tmp, manager) = setup_language_fixture(
            "es",
            "prefixNone",
            vec![
                doc("hablar", vec![("title", text("hablar"))]),
                doc("hablando", vec![("title", text("hablando"))]),
                doc("hablado", vec![("title", text("hablado"))]),
            ],
        )
        .await;

        let ids = search_result_ids_no_typo(&manager, "hablar");
        assert!(
            ids.contains(&"hablar".to_string())
                && ids.contains(&"hablando".to_string())
                && ids.contains(&"hablado".to_string()),
            "expected Spanish stemming to match hablar/hablando/hablado for query 'hablar'; got {ids:?}"
        );
    }

    #[tokio::test]
    async fn portuguese_stemming_correr_matches_inflections() {
        let (_tmp, manager) = setup_language_fixture(
            "pt",
            "prefixNone",
            vec![
                doc("correr", vec![("title", text("correr"))]),
                doc("correndo", vec![("title", text("correndo"))]),
                doc("correu", vec![("title", text("correu"))]),
            ],
        )
        .await;

        let ids = search_result_ids_no_typo(&manager, "correr");
        assert!(
            ids.contains(&"correr".to_string())
                && ids.contains(&"correndo".to_string())
                && ids.contains(&"correu".to_string()),
            "expected Portuguese stemming to match correr/correndo/correu for query 'correr'; got {ids:?}"
        );
    }

    #[tokio::test]
    async fn italian_stemming_mangiare_matches_inflections() {
        let (_tmp, manager) = setup_language_fixture(
            "it",
            "prefixNone",
            vec![
                doc("mangiare", vec![("title", text("mangiare"))]),
                doc("mangiava", vec![("title", text("mangiava"))]),
                doc("mangiato", vec![("title", text("mangiato"))]),
            ],
        )
        .await;

        let ids = search_result_ids_no_typo(&manager, "mangiare");
        assert!(
            ids.contains(&"mangiare".to_string())
                && ids.contains(&"mangiava".to_string())
                && ids.contains(&"mangiato".to_string()),
            "expected Italian stemming to match mangiare/mangiava/mangiato for query 'mangiare'; got {ids:?}"
        );
    }

    #[tokio::test]
    async fn dutch_stemming_lopen_matches_inflections() {
        let (_tmp, manager) = setup_language_fixture(
            "nl",
            "prefixNone",
            vec![
                doc("spel", vec![("title", text("spel"))]),
                doc("spelen", vec![("title", text("spelen"))]),
                doc("speel", vec![("title", text("speel"))]),
            ],
        )
        .await;

        let ids = search_result_ids_no_typo(&manager, "spelen");
        assert!(
            ids.contains(&"spel".to_string())
                && ids.contains(&"spelen".to_string())
                && ids.contains(&"speel".to_string()),
            "expected Dutch stemming to match spel/spelen/speel for query 'spelen'; got {ids:?}"
        );
    }

    #[tokio::test]
    async fn pt_br_alias_uses_portuguese_stemmer() {
        let settings = IndexSettings {
            query_languages: vec!["pt-br".to_string()],
            index_languages: vec!["pt-br".to_string()],
            query_type: "prefixNone".to_string(),
            ..Default::default()
        };
        let (_tmp, manager) = setup_tenant_fixture(
            settings,
            vec![
                doc("correr", vec![("title", text("correr"))]),
                doc("correndo", vec![("title", text("correndo"))]),
                doc("correu", vec![("title", text("correu"))]),
            ],
            false,
        )
        .await;

        let ids = search_result_ids_no_typo(&manager, "correr");
        assert!(
            ids.contains(&"correr".to_string())
                && ids.contains(&"correndo".to_string())
                && ids.contains(&"correu".to_string()),
            "expected pt-br alias to use Portuguese stemmer; got {ids:?}"
        );
    }

    #[tokio::test]
    async fn cjk_zh_uses_cjk_tokenizer_no_snowball() {
        let settings = IndexSettings {
            index_languages: vec!["zh".to_string()],
            ..Default::default()
        };
        let (_tmp, manager) = setup_tenant_fixture(
            settings,
            vec![doc("zh1", vec![("title", text("北京旅游攻略"))])],
            true,
        )
        .await;

        let result = search_no_typo(&manager, "京").unwrap();
        assert!(
            result_ids(&result).contains(&"zh1"),
            "CJK tokenizer should allow single-character query for zh; got {:?}",
            result_ids(&result)
        );
    }

    #[tokio::test]
    async fn cjk_ko_uses_cjk_tokenizer_no_snowball() {
        let ko_languages = vec!["ko".to_string()];
        assert!(
            Index::needs_cjk_tokenizer(&ko_languages),
            "ko should route to CJK tokenizer"
        );
        assert!(
            Index::stemmer_language_for_index(&ko_languages).is_none(),
            "ko should not select a Snowball stemmer"
        );

        let settings = IndexSettings {
            index_languages: vec!["ko".to_string()],
            ..Default::default()
        };
        let (_tmp, _manager) = setup_tenant_fixture(
            settings,
            vec![doc("ko1", vec![("title", text("서울여행가이드"))])],
            true,
        )
        .await;
    }

    #[tokio::test]
    async fn mixed_ja_de_prefers_cjk_and_disables_snowball() {
        let stemmer = Index::stemmer_language_for_index(&["ja".to_string(), "de".to_string()]);
        assert!(
            stemmer.is_none(),
            "CJK language presence should disable Snowball stemmer selection"
        );

        let settings = IndexSettings {
            index_languages: vec!["ja".to_string(), "de".to_string()],
            ..Default::default()
        };
        let (_tmp, manager) = setup_tenant_fixture(
            settings,
            vec![doc("mix1", vec![("title", text("東京駅"))])],
            true,
        )
        .await;

        let result = search_no_typo(&manager, "京").unwrap();
        assert!(
            result_ids(&result).contains(&"mix1"),
            "mixed ja/de should still use CJK tokenization and match char queries"
        );
    }

    #[test]
    fn tier1_stopword_coverage_includes_all_languages() {
        for lang in ["en", "fr", "de", "es", "pt", "it", "nl", "ja", "zh", "ko"] {
            assert!(
                stopwords_for_lang(lang).is_some(),
                "Tier 1 stopword language '{lang}' missing from stopword data"
            );
        }
    }

    #[tokio::test]
    async fn german_stopword_removed_from_parsed_query() {
        let settings = IndexSettings {
            remove_stop_words: RemoveStopWordsValue::All,
            query_languages: vec!["de".to_string()],
            index_languages: vec!["de".to_string()],
            ..Default::default()
        };
        let (_tmp, manager) = setup_tenant_fixture(
            settings,
            vec![
                doc("de1", vec![("title", text("und suchen"))]),
                doc("de2", vec![("title", text("suchen schnell"))]),
            ],
            false,
        )
        .await;

        let result = manager
            .search("test", "und suchen", None, None, 10)
            .unwrap();
        assert_eq!(
            result.parsed_query, "suchen",
            "German stopword 'und' should be removed under queryLanguages=[de]"
        );
    }
}

// ============================================================
// DECOMPOUNDING (Stage 2) — integration behavior
// ============================================================

mod decompound {
    use super::*;

    fn search_with_decompound_flag(
        manager: &IndexManager,
        query: &str,
        decompound_query: bool,
    ) -> crate::error::Result<crate::types::SearchResult> {
        manager.search_full_with_stop_words_with_hits_per_page_cap(
            "test",
            query,
            &crate::index::SearchOptions {
                limit: 10,
                typo_tolerance: Some(false),
                decompound_query: Some(decompound_query),
                ..Default::default()
            },
        )
    }

    #[tokio::test]
    async fn decompound_query_false_disables_compound_splitting() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            query_languages: vec!["de".to_string()],
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        manager
            .add_documents_sync(
                "test",
                vec![
                    doc("compound", vec![("title", text("Hundehütte"))]),
                    doc("split", vec![("title", text("Hunde Hütte"))]),
                ],
            )
            .await
            .unwrap();

        let with_decompound = search_with_decompound_flag(&manager, "Hundehütte", true).unwrap();
        let with_ids = result_ids(&with_decompound);
        assert!(
            with_ids.contains(&"compound"),
            "compound form must match with decompounding enabled"
        );
        assert!(
            with_ids.contains(&"split"),
            "split form must match via decompounding when enabled"
        );

        let without_decompound =
            search_with_decompound_flag(&manager, "Hundehütte", false).unwrap();
        let without_ids = result_ids(&without_decompound);
        assert!(
            without_ids.contains(&"compound"),
            "exact compound form should still match with decompounding disabled"
        );
        assert!(
            !without_ids.contains(&"split"),
            "split form must not match when decompoundQuery is false"
        );
    }

    #[tokio::test]
    async fn decompound_query_true_enables_dutch_compound_splitting() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            query_languages: vec!["nl".to_string()],
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        manager
            .add_documents_sync(
                "test",
                vec![
                    doc("compound", vec![("title", text("voetbal"))]),
                    doc("split", vec![("title", text("voet bal"))]),
                ],
            )
            .await
            .unwrap();

        let with_decompound = search_with_decompound_flag(&manager, "voetbal", true).unwrap();
        let with_ids = result_ids(&with_decompound);
        assert!(
            with_ids.contains(&"compound"),
            "compound form must match with decompounding enabled"
        );
        assert!(
            with_ids.contains(&"split"),
            "split form must match via Dutch decompounding when enabled"
        );
    }

    #[tokio::test]
    async fn pbv5_exp_011_german_stemming_composes_with_query_decompounding() {
        let settings = IndexSettings {
            query_languages: vec!["de".to_string()],
            index_languages: vec!["de".to_string()],
            query_type: "prefixNone".to_string(),
            ..Default::default()
        };
        let (_tmp, manager) = setup_tenant_fixture(
            settings,
            vec![
                doc("compound", vec![("title", text("Hundehütte"))]),
                doc("split", vec![("title", text("Hunde Hütte"))]),
            ],
            true,
        )
        .await;

        let without = search_with_decompound_flag(&manager, "Hundehütte", false).unwrap();
        assert_eq!(result_ids(&without), vec!["compound"]);

        let with = search_with_decompound_flag(&manager, "Hundehütte", true).unwrap();
        let mut with_ids = result_ids(&with);
        with_ids.sort_unstable();
        assert_eq!(with_ids, vec!["compound", "split"]);

        let default = manager
            .search_full_with_stop_words_with_hits_per_page_cap(
                "test",
                "Hundehütte",
                &crate::index::SearchOptions {
                    limit: 10,
                    typo_tolerance: Some(false),
                    ..Default::default()
                },
            )
            .unwrap();
        let mut default_ids = result_ids(&default);
        default_ids.sort_unstable();
        assert_eq!(default_ids, vec!["compound", "split"]);
    }
}

mod decompounded_attributes {
    use super::*;

    fn decompounded_settings() -> IndexSettings {
        let mut decompounded_attributes = HashMap::new();
        decompounded_attributes.insert("de".to_string(), vec!["title".to_string()]);

        IndexSettings {
            query_languages: vec!["de".to_string()],
            query_type: "prefixNone".to_string(),
            decompounded_attributes: Some(decompounded_attributes),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn searches_compound_parts_when_attribute_is_configured() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        decompounded_settings()
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        manager
            .add_documents_sync(
                "test",
                vec![doc(
                    "compound",
                    vec![
                        ("title", text("Hundehütte")),
                        ("description", text("other")),
                    ],
                )],
            )
            .await
            .unwrap();

        let result = manager.search("test", "hütte", None, None, 10).unwrap();
        let ids = result_ids(&result);
        assert!(
            ids.contains(&"compound"),
            "compound title should match via decompounded index-time expansion"
        );
    }

    #[tokio::test]
    async fn search_ignores_compounds_on_non_configured_attributes() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        decompounded_settings()
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        manager
            .add_documents_sync(
                "test",
                vec![doc(
                    "only-description",
                    vec![
                        ("title", text("hand tool")),
                        ("description", text("Hundehütte")),
                    ],
                )],
            )
            .await
            .unwrap();

        let result = manager.search("test", "hütte", None, None, 10).unwrap();
        assert_eq!(
            result.documents.len(),
            0,
            "descriptions should not receive decompound expansion"
        );
    }

    #[tokio::test]
    async fn search_does_not_expand_without_decompounded_attributes_setting() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            query_languages: vec!["de".to_string()],
            query_type: "prefixNone".to_string(),
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        manager
            .add_documents_sync(
                "test",
                vec![doc("compound", vec![("title", text("Hundehütte"))])],
            )
            .await
            .unwrap();

        let result = manager.search("test", "hütte", None, None, 10).unwrap();
        assert_eq!(
            result.documents.len(),
            0,
            "without decompoundedAttributes, compound parts should not be indexed"
        );
    }

    #[tokio::test]
    async fn decompound_expansion_strips_punctuation_from_tokens() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        decompounded_settings()
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        // Title has punctuation adjacent to compound word
        manager
            .add_documents_sync(
                "test",
                vec![doc(
                    "punctuated",
                    vec![("title", text("Hundehütte, Kindergarten."))],
                )],
            )
            .await
            .unwrap();

        // Both compound parts should be findable despite punctuation in source text
        let result_huette = manager.search("test", "hütte", None, None, 10).unwrap();
        assert!(
            result_ids(&result_huette).contains(&"punctuated"),
            "compound part 'hütte' must match even when source has trailing comma: got {:?}",
            result_ids(&result_huette)
        );

        let result_garten = manager.search("test", "garten", None, None, 10).unwrap();
        assert!(
            result_ids(&result_garten).contains(&"punctuated"),
            "compound part 'garten' must match even when source has trailing period: got {:?}",
            result_ids(&result_garten)
        );
    }
}

// ============================================================
// MULTI-LANGUAGE STOPWORDS (Stage 2 C + H) — integration tests
// ============================================================

mod multilang_stopwords {
    use super::*;

    /// C.5: Index English + French docs, set removeStopWords: ["fr"],
    /// verify French stopwords removed but English stopwords preserved.
    #[tokio::test]
    async fn french_stopwords_removed_english_preserved() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        // removeStopWords only for French
        let settings = IndexSettings {
            remove_stop_words: RemoveStopWordsValue::Languages(vec!["fr".to_string()]),
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![
            doc("en1", vec![("title", text("the best search engine"))]),
            doc("en2", vec![("title", text("search appliance"))]),
            doc(
                "fr1",
                vec![("title", text("le meilleur moteur de recherche"))],
            ),
            doc("fr2", vec![("title", text("recherche rapide"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        // "le" is a French stopword — searching "le recherche" should strip "le"
        // and match on "recherche" alone.
        let result_fr_stop = manager
            .search("test", "le recherche", None, None, 10)
            .unwrap();
        assert_eq!(result_fr_stop.parsed_query, "recherche");
        let fr_ids = result_ids(&result_fr_stop);
        assert!(
            fr_ids.contains(&"fr1") && fr_ids.contains(&"fr2"),
            "French stopword 'le' should be removed, matching both 'recherche' docs"
        );

        // "the" is an English stopword but removeStopWords: ["fr"] only targets French.
        // "the" should NOT be removed.
        let result_en = manager
            .search("test", "the search", None, None, 10)
            .unwrap();
        assert_eq!(result_en.parsed_query, "the search");
        let en_ids = result_ids(&result_en);
        assert!(
            en_ids.contains(&"en1"),
            "'the search' should match the document that contains both words"
        );
        assert!(
            !en_ids.contains(&"en2"),
            "'the' must remain in query under removeStopWords=[fr], so a 'search'-only doc should not match"
        );
    }

    /// H.1: queryLanguages: ["en"] + removeStopWords: true removes English stopwords;
    /// changing to ["fr"] removes French stopwords instead.
    #[tokio::test]
    async fn query_languages_controls_stopword_language() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        // removeStopWords: true + queryLanguages: ["en"]
        let settings_en = IndexSettings {
            remove_stop_words: RemoveStopWordsValue::All,
            query_languages: vec!["en".to_string()],
            ..Default::default()
        };
        settings_en
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![
            doc("1", vec![("title", text("the search engine"))]),
            doc("2", vec![("title", text("search engine"))]),
            doc("3", vec![("title", text("le moteur de recherche"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        // With queryLanguages: ["en"], "the" (English) should be stripped
        let result_en = manager
            .search("test", "the search", None, None, 10)
            .unwrap();
        assert_eq!(
            result_en.parsed_query, "search",
            "with queryLanguages=[en], English stopword 'the' should be removed"
        );
        let result_fr_words_under_en = manager
            .search("test", "le recherche", None, None, 10)
            .unwrap();
        assert_eq!(
            result_fr_words_under_en.parsed_query, "le recherche",
            "with queryLanguages=[en], French stopwords should not be removed"
        );

        // Now switch to queryLanguages: ["fr"]
        let settings_fr = IndexSettings {
            remove_stop_words: RemoveStopWordsValue::All,
            query_languages: vec!["fr".to_string()],
            ..Default::default()
        };
        settings_fr
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        // With queryLanguages: ["fr"], "le" and "de" (French) should be stripped
        let result_fr = manager
            .search("test", "le recherche", None, None, 10)
            .unwrap();
        assert_eq!(
            result_fr.parsed_query, "recherche",
            "with queryLanguages=[fr], French stopword 'le' should be removed"
        );
        let result_en_words_under_fr = manager
            .search("test", "the search", None, None, 10)
            .unwrap();
        assert_eq!(
            result_en_words_under_fr.parsed_query, "the search",
            "with queryLanguages=[fr], English stopwords should not be removed"
        );
    }

    #[tokio::test]
    async fn pt_br_query_language_routes_to_portuguese_stopwords() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            remove_stop_words: RemoveStopWordsValue::All,
            query_languages: vec!["pt-br".to_string()],
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        manager
            .add_documents_sync(
                "test",
                vec![
                    doc("pt1", vec![("title", text("o melhor motor de busca"))]),
                    doc("pt2", vec![("title", text("busca avançada"))]),
                ],
            )
            .await
            .unwrap();

        let result = manager.search("test", "o busca", None, None, 10).unwrap();
        assert_eq!(
            result.parsed_query, "busca",
            "pt-br should route to Portuguese stopword list and strip 'o'"
        );
        let ids = result_ids(&result);
        assert!(
            ids.contains(&"pt1") && ids.contains(&"pt2"),
            "after stripping Portuguese stopword, both busca docs should match"
        );
    }

    /// H.3: Unsupported language code in queryLanguages doesn't crash,
    /// produces stable fallback results.
    #[tokio::test]
    async fn unsupported_language_code_does_not_crash() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            remove_stop_words: RemoveStopWordsValue::All,
            ignore_plurals: IgnorePluralsValue::All,
            query_languages: vec!["xx".to_string()],
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![
            doc("1", vec![("title", text("hello world"))]),
            doc("2", vec![("title", text("testing search"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        // Should not panic or crash with unknown language code
        let result = manager.search("test", "hello", None, None, 10).unwrap();
        assert_eq!(result.parsed_query, "hello");
        assert!(
            !result.documents.is_empty(),
            "search with unsupported language code should still return results"
        );

        // Searching twice should give stable (identical) results
        let result2 = manager.search("test", "hello", None, None, 10).unwrap();
        let ids1: Vec<&str> = result
            .documents
            .iter()
            .map(|d| d.document.id.as_str())
            .collect();
        let ids2: Vec<&str> = result2
            .documents
            .iter()
            .map(|d| d.document.id.as_str())
            .collect();
        assert_eq!(
            ids1, ids2,
            "results should be stable across repeated searches"
        );
    }
}

// ============================================================
// MULTI-LANGUAGE PLURALS (Stage 2 H) — integration tests
// ============================================================

mod multilang_plurals {
    use super::*;

    fn search_no_typo(
        manager: &IndexManager,
        query: &str,
    ) -> crate::error::Result<crate::types::SearchResult> {
        manager.search_with_options(
            "test",
            query,
            &SearchOptions {
                limit: 10,
                typo_tolerance: Some(false),
                ..SearchOptions::default()
            },
        )
    }

    async fn assert_language_plural_pair_matches(
        lang: &str,
        singular_id: &str,
        singular_word: &str,
        plural_id: &str,
        plural_word: &str,
        query: &str,
    ) {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            ignore_plurals: IgnorePluralsValue::All,
            query_languages: vec![lang.to_string()],
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        manager
            .add_documents_sync(
                "test",
                vec![
                    doc(singular_id, vec![("name", text(singular_word))]),
                    doc(plural_id, vec![("name", text(plural_word))]),
                ],
            )
            .await
            .unwrap();

        let result = search_no_typo(&manager, query).unwrap();
        let ids = result_ids(&result);
        assert!(
            ids.contains(&singular_id) && ids.contains(&plural_id),
            "with queryLanguages=[{}], '{}' should match both '{}' and '{}', got: {:?}",
            lang,
            query,
            singular_word,
            plural_word,
            ids
        );
    }

    /// H.2: ignorePlurals: true + queryLanguages: ["en"] expands English plurals;
    /// queryLanguages: ["fr"] expands French plurals.
    #[tokio::test]
    async fn query_languages_controls_plural_expansion() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        // English plurals
        let settings_en = IndexSettings {
            ignore_plurals: IgnorePluralsValue::All,
            query_languages: vec!["en".to_string()],
            ..Default::default()
        };
        settings_en
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![
            doc("en_s", vec![("name", text("child"))]),
            doc("en_p", vec![("name", text("children"))]),
            doc("fr_s", vec![("name", text("cheval"))]),
            doc("fr_p", vec![("name", text("chevaux"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        // With English plurals: "children" should find both "children" and "child".
        // We disable typo tolerance to avoid fuzzy false positives.
        let en_result = search_no_typo(&manager, "children").unwrap();
        let en_ids = result_ids(&en_result);
        assert!(
            en_ids.contains(&"en_s") && en_ids.contains(&"en_p"),
            "with queryLanguages=[en], 'children' should match both 'children' and 'child', got: {:?}",
            en_ids
        );
        let en_fr_probe = search_no_typo(&manager, "chevaux").unwrap();
        let en_fr_probe_ids = result_ids(&en_fr_probe);
        assert!(
            !en_fr_probe_ids.contains(&"fr_s"),
            "with queryLanguages=[en], French singular should not match 'chevaux' without FR plural expansion"
        );

        // Now switch to French
        let settings_fr = IndexSettings {
            ignore_plurals: IgnorePluralsValue::All,
            query_languages: vec!["fr".to_string()],
            ..Default::default()
        };
        settings_fr
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        // With French plurals: "chevaux" should find both "chevaux" and "cheval".
        let fr_result = search_no_typo(&manager, "chevaux").unwrap();
        let fr_ids = result_ids(&fr_result);
        assert!(
            fr_ids.contains(&"fr_s") && fr_ids.contains(&"fr_p"),
            "with queryLanguages=[fr], 'chevaux' should match both 'chevaux' and 'cheval', got: {:?}",
            fr_ids
        );
        let fr_en_probe = search_no_typo(&manager, "children").unwrap();
        let fr_en_probe_ids = result_ids(&fr_en_probe);
        assert!(
            !fr_en_probe_ids.contains(&"en_s"),
            "with queryLanguages=[fr], English singular should not match 'children' without EN plural expansion"
        );
    }

    #[tokio::test]
    async fn portuguese_ignore_plurals_matches_singular_and_plural() {
        assert_language_plural_pair_matches("pt", "pt_s", "casa", "pt_p", "casas", "casa").await;
    }

    #[tokio::test]
    async fn italian_ignore_plurals_matches_singular_and_plural() {
        assert_language_plural_pair_matches("it", "it_s", "gatto", "it_p", "gatti", "gatto").await;
    }

    #[tokio::test]
    async fn dutch_ignore_plurals_matches_singular_and_plural() {
        assert_language_plural_pair_matches("nl", "nl_s", "boek", "nl_p", "boeken", "boek").await;
    }
}

// ============================================================
// indexLanguages ROUND-TRIP (Stage 2 H.4) — integration test
// ============================================================

mod index_languages_tokenizer_wiring {
    use super::*;

    fn search_no_typo(
        manager: &IndexManager,
        query: &str,
    ) -> crate::error::Result<crate::types::SearchResult> {
        manager.search_with_options(
            "test",
            query,
            &SearchOptions {
                limit: 10,
                typo_tolerance: Some(false),
                ..SearchOptions::default()
            },
        )
    }

    /// B.3: indexLanguages with CJK language enables CJK character-level search.
    /// When indexLanguages contains "ja", individual CJK characters should be
    /// searchable because the CJK-aware tokenizer splits them.
    #[tokio::test]
    async fn cjk_language_enables_cjk_tokenizer() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        // Write settings with CJK language BEFORE indexing
        let settings = IndexSettings {
            index_languages: vec!["ja".to_string(), "en".to_string()],
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();

        // Force reload so the index picks up indexLanguages
        manager.unload_tenant("test");
        manager.invalidate_settings_cache("test");

        // Index CJK content
        let docs = vec![doc(
            "1",
            vec![("title", FieldValue::Text("東京タワー観光".to_string()))],
        )];
        manager.add_documents_sync("test", docs).await.unwrap();

        // Search for a non-prefix CJK char ("京") — should match only when split into char tokens.
        let result = search_no_typo(&manager, "京").unwrap();
        assert!(
            result.total > 0,
            "CJK char '京' should be searchable when indexLanguages includes 'ja'"
        );
        assert!(
            result_ids(&result).contains(&"1"),
            "document with CJK content should match non-prefix char query under CJK-aware mode"
        );
    }

    /// B.3: indexLanguages with only Latin languages uses Latin-only tokenizer.
    /// When indexLanguages is ["en"], CJK text is grouped as whole words instead
    /// of split into individual characters, so individual char search may not match.
    #[tokio::test]
    async fn latin_only_languages_groups_cjk_text() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        // Write settings with Latin-only languages
        let settings = IndexSettings {
            index_languages: vec!["en".to_string(), "fr".to_string()],
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();

        // Force reload so the index picks up indexLanguages
        manager.unload_tenant("test");
        manager.invalidate_settings_cache("test");

        // Index CJK content
        let docs = vec![doc(
            "1",
            vec![("title", FieldValue::Text("東京タワー観光".to_string()))],
        )];
        manager.add_documents_sync("test", docs).await.unwrap();

        // Search for a non-prefix CJK char ("京") — should NOT match with Latin-only tokenizer.
        let result = search_no_typo(&manager, "京").unwrap();
        assert_eq!(
            result.total, 0,
            "CJK char '京' should NOT be individually searchable when indexLanguages is Latin-only"
        );
    }

    /// B.3: Empty indexLanguages (default) should use CJK-aware tokenizer for
    /// backwards compatibility.
    #[tokio::test]
    async fn empty_index_languages_defaults_to_cjk_aware() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        // Default settings (empty indexLanguages)
        let docs = vec![doc(
            "1",
            vec![("title", FieldValue::Text("東京タワー".to_string()))],
        )];
        manager.add_documents_sync("test", docs).await.unwrap();

        // Should find non-prefix CJK chars with default (CJK-aware) tokenizer
        let result = search_no_typo(&manager, "京").unwrap();
        assert!(
            result.total > 0,
            "CJK char should be searchable with default (empty) indexLanguages"
        );
    }
}

mod index_languages_roundtrip {
    use super::*;

    /// H.4: indexLanguages: ["ja"] round-trips through settings and is visible
    /// when loading settings back from disk.
    #[tokio::test]
    async fn index_languages_persists_and_loads() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            index_languages: vec!["ja".to_string(), "en".to_string()],
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        // Load settings back through the manager
        let loaded = manager
            .get_settings("test")
            .expect("settings should exist after save");
        assert_eq!(
            loaded.index_languages,
            vec!["ja".to_string(), "en".to_string()],
            "indexLanguages should round-trip through save/load"
        );

        // Also verify it serializes to JSON with the right key name
        let json = serde_json::to_value(loaded.as_ref()).unwrap();
        let index_langs = json.get("indexLanguages");
        assert!(
            index_langs.is_some(),
            "indexLanguages should be present in serialized JSON"
        );
        assert_eq!(
            index_langs.unwrap(),
            &serde_json::json!(["ja", "en"]),
            "indexLanguages should serialize as expected"
        );
    }
}

mod typo_tolerance_settings {
    use super::*;

    #[tokio::test]
    async fn disable_typo_tolerance_on_words_makes_queries_exact_match_only() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            disable_typo_tolerance_on_words: Some(vec!["iphonne".to_string()]),
            ..IndexSettings::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![doc("1", vec![("title", text("iPhone 15"))])];
        manager.add_documents_sync("test", docs).await.unwrap();

        let exact_result = manager.search("test", "iPhone", None, None, 10).unwrap();
        let exact_ids = result_ids(&exact_result);
        assert_eq!(exact_ids, vec!["1"]);

        let typo_result = manager.search("test", "iphonne", None, None, 10).unwrap();
        assert_eq!(
            typo_result.total, 0,
            "disabled typo term should prevent typo-based matching"
        );
    }

    #[tokio::test]
    async fn disable_typo_tolerance_on_attributes_only_disables_fuzzy_for_listed_attributes() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            disable_typo_tolerance_on_attributes: Some(vec!["sku".to_string()]),
            query_type: "prefixNone".to_string(),
            ..IndexSettings::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![
            doc(
                "sku_only",
                vec![("sku", text("ABC123")), ("title", text("blue jacket"))],
            ),
            doc(
                "title_only",
                vec![("sku", text("XYZ999")), ("title", text("ABC123"))],
            ),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager.search("test", "ABC12", None, None, 10).unwrap();
        let ids = result_ids(&result);
        assert!(
            !ids.contains(&"sku_only"),
            "typo on disabled attribute should not match"
        );
        assert!(
            ids.contains(&"title_only"),
            "other attributes should still use typo tolerance"
        );
    }
}

mod separators_to_index {
    use super::*;

    #[tokio::test]
    async fn separators_to_index_indexes_separator_tokens() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            separators_to_index: "#+".to_string(),
            ..IndexSettings::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");
        manager.unload_tenant("test");

        let docs = vec![
            doc("plus", vec![("title", text("C++"))]),
            doc("plain", vec![("title", text("C"))]),
            doc("hash", vec![("title", text("C#"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager
            .search_with_options(
                "test",
                "C++",
                &SearchOptions {
                    limit: 10,
                    query_type: Some("prefixNone"),
                    ..SearchOptions::default()
                },
            )
            .unwrap();

        let ids = result_ids(&result);
        assert!(ids.contains(&"plus"));
        assert!(!ids.contains(&"plain"));
        assert!(!ids.contains(&"hash"));
    }
}

mod camel_case_attributes {
    use super::*;

    #[tokio::test]
    async fn defaults_do_not_split_camel_case_terms() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings::default();
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![doc("1", vec![("productName", text("macBookPro"))])];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager
            .search_with_options(
                "test",
                "book",
                &SearchOptions {
                    limit: 10,
                    query_type: Some("prefixNone"),
                    typo_tolerance: Some(false),
                    ..SearchOptions::default()
                },
            )
            .unwrap();
        assert_eq!(
            result.total, 0,
            "camelCase terms should not be split without camelCaseAttributes"
        );
    }

    #[tokio::test]
    async fn splits_listed_attributes_for_search_matching() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            camel_case_attributes: vec!["productName".to_string()],
            ..IndexSettings::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![doc("1", vec![("productName", text("macBookPro"))])];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager
            .search_with_options(
                "test",
                "book",
                &SearchOptions {
                    limit: 10,
                    query_type: Some("prefixNone"),
                    typo_tolerance: Some(false),
                    ..SearchOptions::default()
                },
            )
            .unwrap();

        let ids = result_ids(&result);
        assert!(
            ids.contains(&"1"),
            "camelCaseAttributes should split attribute value into word tokens"
        );
    }
}

mod keep_diacritics_on_characters {
    use super::*;

    #[tokio::test]
    async fn defaults_fold_diacritics_for_query_and_indexing() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            ..IndexSettings::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();
        manager.invalidate_settings_cache("test");

        let docs = vec![
            doc("folded", vec![("title", text("København"))]),
            doc("unrelated", vec![("title", text("Stockholm"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager
            .search_with_options(
                "test",
                "København",
                &SearchOptions {
                    limit: 10,
                    query_type: Some("prefixNone"),
                    typo_tolerance: Some(false),
                    ..SearchOptions::default()
                },
            )
            .unwrap();

        let ids = result_ids(&result);
        assert!(ids.contains(&"folded"));
        assert!(!ids.contains(&"unrelated"));
    }

    #[tokio::test]
    async fn keeps_selected_diacritics_when_configured() {
        let temp_dir = TempDir::new().unwrap();
        let tenant_path = temp_dir.path().join("test");
        std::fs::create_dir_all(&tenant_path).unwrap();

        let settings = IndexSettings {
            keep_diacritics_on_characters: "ø".to_string(),
            ..IndexSettings::default()
        };
        settings.save(tenant_path.join("settings.json")).unwrap();

        let schema = crate::index::schema::Schema::builder().build();
        let _ = Index::create_with_languages_indexed_separators_and_keep_diacritics(
            &tenant_path,
            schema,
            get_global_budget(),
            &[],
            &[],
            "ø",
            &[],
        )
        .unwrap();

        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let docs = vec![
            doc("kept", vec![("title", text("København"))]),
            doc("folded", vec![("title", text("Kobenhavn"))]),
        ];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager
            .search_with_options(
                "test",
                "København",
                &SearchOptions {
                    limit: 10,
                    query_type: Some("prefixNone"),
                    typo_tolerance: Some(false),
                    ..SearchOptions::default()
                },
            )
            .unwrap();

        let ids = result_ids(&result);
        assert!(ids.contains(&"kept"));
        assert!(!ids.contains(&"folded"));
    }
}

mod custom_normalization {
    use super::*;

    #[tokio::test]
    async fn custom_character_map_applies_case_insensitive_key_mapping() {
        let temp_dir = TempDir::new().unwrap();
        let tenant_path = temp_dir.path().join("test");
        std::fs::create_dir_all(&tenant_path).unwrap();

        let mut custom_script = std::collections::HashMap::new();
        let mut default_map = std::collections::HashMap::new();
        default_map.insert("Q".to_string(), "k".to_string());
        custom_script.insert("default".to_string(), default_map);

        let settings = IndexSettings {
            custom_normalization: Some(custom_script),
            ..IndexSettings::default()
        };
        let settings_path = tenant_path.join("settings.json");
        settings.save(settings_path).unwrap();
        let custom_normalization = IndexSettings::flatten_custom_normalization(&settings);
        let schema = crate::index::schema::Schema::builder().build();
        let _ = Index::create_with_languages_indexed_separators_and_keep_diacritics(
            &tenant_path,
            schema,
            get_global_budget(),
            &[],
            &[],
            "",
            &custom_normalization,
        )
        .unwrap();

        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let docs = vec![doc(
            "custom-normalized-match",
            vec![("title", text("Qatar"))],
        )];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager
            .search_with_options(
                "test",
                "katar",
                &SearchOptions {
                    limit: 10,
                    query_type: Some("prefixNone"),
                    typo_tolerance: Some(false),
                    ..SearchOptions::default()
                },
            )
            .unwrap();
        let ids = result_ids(&result);
        assert!(ids.contains(&"custom-normalized-match"));
        assert_eq!(ids.len(), 1);
    }

    #[tokio::test]
    async fn custom_character_map_lowercases_uppercase_replacement_values() {
        let temp_dir = TempDir::new().unwrap();
        let tenant_path = temp_dir.path().join("test");
        std::fs::create_dir_all(&tenant_path).unwrap();

        let mut custom_script = std::collections::HashMap::new();
        let mut default_map = std::collections::HashMap::new();
        default_map.insert("Q".to_string(), "K".to_string());
        custom_script.insert("default".to_string(), default_map);

        let settings = IndexSettings {
            custom_normalization: Some(custom_script),
            ..IndexSettings::default()
        };
        let settings_path = tenant_path.join("settings.json");
        settings.save(settings_path).unwrap();
        let custom_normalization = IndexSettings::flatten_custom_normalization(&settings);
        let schema = crate::index::schema::Schema::builder().build();
        let _ = Index::create_with_languages_indexed_separators_and_keep_diacritics(
            &tenant_path,
            schema,
            get_global_budget(),
            &[],
            &[],
            "",
            &custom_normalization,
        )
        .unwrap();

        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let docs = vec![doc(
            "custom-normalized-match",
            vec![("title", text("Qatar"))],
        )];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager
            .search_with_options(
                "test",
                "katar",
                &SearchOptions {
                    limit: 10,
                    query_type: Some("prefixNone"),
                    typo_tolerance: Some(false),
                    ..SearchOptions::default()
                },
            )
            .unwrap();
        let ids = result_ids(&result);
        assert!(ids.contains(&"custom-normalized-match"));
        assert_eq!(ids.len(), 1);
    }

    #[tokio::test]
    async fn without_custom_character_map_query_does_not_match() {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let docs = vec![doc("plain", vec![("title", text("Qatar"))])];
        manager.add_documents_sync("test", docs).await.unwrap();

        let result = manager
            .search_with_options(
                "test",
                "katar",
                &SearchOptions {
                    limit: 10,
                    query_type: Some("prefixNone"),
                    typo_tolerance: Some(false),
                    ..SearchOptions::default()
                },
            )
            .unwrap();
        let ids = result_ids(&result);
        assert!(!ids.contains(&"plain"));
        assert!(ids.is_empty());
    }
}
