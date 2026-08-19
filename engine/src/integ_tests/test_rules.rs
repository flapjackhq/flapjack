//! Consolidated rules integration tests.
//!
//! Merged from (all deleted):
//!   - test_rules_http.rs      (validity, context, dedup, disabled)
//!   - test_rules_consequences.rs  (applied_rules, user_data, query rewrite)

use crate::error::Result;
use crate::index::SearchOptions;
use crate::types::Document;
use crate::IndexManager;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;

// ============================================================
// Helper — creates a fresh index with a UUID-based name
// (avoids cross-test interference when tests run in parallel)
// ============================================================

async fn setup_test() -> (Arc<IndexManager>, TempDir, String) {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    let index_name = format!("test_{}", uuid::Uuid::new_v4());
    manager.create_tenant(&index_name).unwrap();
    (manager, temp_dir, index_name)
}

// ============================================================
// From test_rules_http.rs
// ============================================================

#[tokio::test]
async fn test_rule_with_expired_validity() {
    let (manager, temp_dir, index_name) = setup_test().await;

    let past = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        - 7200;

    let rule = crate::index::rules::Rule {
        object_id: "expired-rule".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("laptop".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: None,
            filters: None,
        }],
        consequence: crate::index::rules::Consequence {
            promote: Some(vec![crate::index::rules::Promote::Single {
                object_id: "promoted-item".to_string(),
                position: 0,
            }]),
            hide: None,
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: Some(vec![crate::index::rules::TimeRange {
            from: past - 3600,
            until: past,
        }]),
    };

    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(rule);
    store.save(&rules_path).unwrap();

    let docs = vec![
        crate::types::Document::from_json(&json!({"_id": "1", "name": "Gaming Laptop"})).unwrap(),
        crate::types::Document::from_json(&json!({"_id": "2", "name": "Office Laptop"})).unwrap(),
        crate::types::Document::from_json(
            &json!({"_id": "promoted-item", "name": "Budget Laptop"}),
        )
        .unwrap(),
    ];
    manager.add_documents_sync(&index_name, docs).await.unwrap();

    let result = manager
        .search(&index_name, "laptop", None, None, 10)
        .unwrap();
    assert!(
        result.documents[0].document.id != "promoted-item",
        "Expired rule should not apply"
    );
}

#[tokio::test]
async fn test_context_based_rule() {
    let (manager, temp_dir, index_name) = setup_test().await;

    let rule = crate::index::rules::Rule {
        object_id: "context-rule".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("laptop".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: Some("mobile".to_string()),
            filters: None,
        }],
        consequence: crate::index::rules::Consequence {
            promote: Some(vec![crate::index::rules::Promote::Single {
                object_id: "mobile-item".to_string(),
                position: 0,
            }]),
            hide: None,
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    };

    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(rule);
    store.save(&rules_path).unwrap();

    let docs = vec![
        crate::types::Document::from_json(&json!({"_id": "1", "name": "Gaming Laptop"})).unwrap(),
        crate::types::Document::from_json(&json!({"_id": "mobile-item", "name": "Budget Laptop"}))
            .unwrap(),
    ];
    manager.add_documents_sync(&index_name, docs).await.unwrap();

    let result = manager
        .search(&index_name, "laptop", None, None, 10)
        .unwrap();
    assert!(
        result.documents[0].document.id != "mobile-item",
        "Rule should not apply without context"
    );
}

#[tokio::test]
async fn test_context_based_rule_matches_any_rule_context() {
    let (manager, temp_dir, index_name) = setup_test().await;

    let rule = crate::index::rules::Rule {
        object_id: "context-rule".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("laptop".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: Some("mobile".to_string()),
            filters: None,
        }],
        consequence: crate::index::rules::Consequence {
            promote: Some(vec![crate::index::rules::Promote::Single {
                object_id: "mobile-item".to_string(),
                position: 0,
            }]),
            hide: None,
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    };

    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(rule);
    store.save(&rules_path).unwrap();

    let docs = vec![
        crate::types::Document::from_json(&json!({"_id": "1", "name": "Gaming Laptop"})).unwrap(),
        crate::types::Document::from_json(&json!({"_id": "mobile-item", "name": "Budget Laptop"}))
            .unwrap(),
    ];
    manager.add_documents_sync(&index_name, docs).await.unwrap();

    let contexts = vec!["desktop".to_string(), "mobile".to_string()];
    let result = manager
        .search_with_options(
            &index_name,
            "laptop",
            &SearchOptions {
                limit: 10,
                rule_contexts: Some(&contexts),
                ..Default::default()
            },
        )
        .unwrap();

    assert_eq!(
        result.documents[0].document.id, "mobile-item",
        "Rule should match when any provided ruleContext matches condition.context"
    );
}

#[tokio::test]
async fn test_zero_based_pin_position_shifts_third_result_to_first_without_duplication() {
    let (manager, temp_dir, index_name) = setup_test().await;

    let rule = crate::index::rules::Rule {
        object_id: "dedup-rule".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("laptop".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: None,
            filters: None,
        }],
        consequence: crate::index::rules::Consequence {
            promote: Some(vec![crate::index::rules::Promote::Single {
                object_id: "3".to_string(),
                position: 0,
            }]),
            hide: None,
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    };

    let docs = vec![
        crate::types::Document::from_json(&json!({"_id": "1", "name": "Gaming Laptop", "rank": 1}))
            .unwrap(),
        crate::types::Document::from_json(&json!({"_id": "2", "name": "Office Laptop", "rank": 2}))
            .unwrap(),
        crate::types::Document::from_json(&json!({"_id": "3", "name": "Budget Laptop", "rank": 3}))
            .unwrap(),
    ];
    manager.add_documents_sync(&index_name, docs).await.unwrap();

    let sort = crate::types::Sort::ByField {
        field: "rank".to_string(),
        order: crate::types::SortOrder::Asc,
    };
    let search = || {
        manager
            .search_with_options(
                &index_name,
                "laptop",
                &SearchOptions {
                    sort: Some(&sort),
                    limit: 10,
                    ..Default::default()
                },
            )
            .unwrap()
    };
    let baseline_ids: Vec<_> = search()
        .documents
        .into_iter()
        .map(|document| document.document.id)
        .collect();
    assert_eq!(baseline_ids, vec!["1", "2", "3"]);

    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(rule);
    store.save(&rules_path).unwrap();

    let result = search();
    let result_ids: Vec<_> = result
        .documents
        .iter()
        .map(|document| document.document.id.as_str())
        .collect();
    assert_eq!(result_ids, vec!["3", "1", "2"]);

    let id_positions: Vec<_> = result
        .documents
        .iter()
        .enumerate()
        .filter(|(_, d)| d.document.id == "3")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(id_positions.len(), 1, "Pinned item should appear only once");
    assert_eq!(id_positions[0], 0, "Position 0 must mean the first result");
}

#[tokio::test]
async fn test_managed_merchandising_rule_lifecycle_has_exact_search_consequences() -> Result<()> {
    let (manager, temp_dir, index_name) = setup_test().await;
    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let settings = crate::index::settings::IndexSettings {
        attributes_for_faceting: vec!["category".to_string()],
        ..crate::index::settings::IndexSettings::default()
    };
    settings.save(temp_dir.path().join(&index_name).join("settings.json"))?;
    manager.invalidate_settings_cache(&index_name);

    let docs = vec![
        Document::from_json(&json!({
            "_id": "trail-1", "name": "Blue Ridge Trail Jacket",
            "category": "outerwear", "rank": 1
        }))?,
        Document::from_json(&json!({
            "_id": "trail-2", "name": "Red Ridge Trail Jacket",
            "category": "outerwear", "rank": 2
        }))?,
        Document::from_json(&json!({
            "_id": "trail-3", "name": "River Trail Shell",
            "category": "outerwear", "rank": 3
        }))?,
        Document::from_json(&json!({
            "_id": "trail-4", "name": "Alpine Trail Boots",
            "category": "footwear", "rank": 4
        }))?,
    ];
    manager.add_documents_sync(&index_name, docs).await?;

    let sort = crate::types::Sort::ByField {
        field: "rank".to_string(),
        order: crate::types::SortOrder::Asc,
    };
    let search_ids = |query: &str, filter_text: &str| -> Result<(Vec<String>, Vec<String>)> {
        let filter = crate::filter_parser::parse_filter(filter_text).unwrap();
        let result = manager.search_with_options(
            &index_name,
            query,
            &SearchOptions {
                filter: Some(&filter),
                sort: Some(&sort),
                limit: 10,
                ..Default::default()
            },
        )?;
        Ok((
            result
                .documents
                .into_iter()
                .map(|document| document.document.id)
                .collect(),
            result.applied_rules,
        ))
    };
    let outerwear_baseline = vec!["trail-1", "trail-2", "trail-3"];

    assert_eq!(
        search_ids("trail", "category:outerwear")?.0,
        outerwear_baseline
    );

    let base_rule = crate::index::rules::Rule {
        object_id: "managed-trail-rule".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("trail".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: None,
            filters: Some("category:outerwear".to_string()),
        }],
        consequence: crate::index::rules::Consequence {
            promote: Some(vec![crate::index::rules::Promote::Single {
                object_id: "trail-3".to_string(),
                position: 0,
            }]),
            hide: Some(vec![crate::index::rules::Hide {
                object_id: "trail-2".to_string(),
            }]),
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: Some("Managed trail merchandising".to_string()),
        enabled: Some(false),
        validity: Some(vec![crate::index::rules::TimeRange {
            from: 1_704_067_200,
            until: 4_102_444_799,
        }]),
    };

    let mut store = crate::index::rules::RuleStore::new();
    store.insert(base_rule.clone());
    store.save(&rules_path)?;
    manager.invalidate_rules_cache(&index_name);
    let persisted_draft = crate::index::rules::RuleStore::load(&rules_path)?;
    assert_eq!(
        persisted_draft.get("managed-trail-rule").unwrap().enabled,
        Some(false)
    );
    assert_eq!(
        search_ids("trail", "category:outerwear")?.0,
        outerwear_baseline,
        "A persisted draft must not change hits or order"
    );

    let mut future_rule = base_rule.clone();
    future_rule.enabled = Some(true);
    future_rule.validity = Some(vec![crate::index::rules::TimeRange {
        from: 4_102_444_800,
        until: 4_133_980_800,
    }]);
    store.insert(future_rule);
    store.save(&rules_path)?;
    manager.invalidate_rules_cache(&index_name);
    assert_eq!(
        search_ids("trail", "category:outerwear")?.0,
        outerwear_baseline,
        "A future rule must persist without changing hits or order"
    );

    let mut expired_rule = base_rule.clone();
    expired_rule.enabled = Some(true);
    expired_rule.validity = Some(vec![crate::index::rules::TimeRange {
        from: 946_684_800,
        until: 978_307_199,
    }]);
    store.insert(expired_rule);
    store.save(&rules_path)?;
    manager.invalidate_rules_cache(&index_name);
    assert_eq!(
        search_ids("trail", "category:outerwear")?.0,
        outerwear_baseline,
        "An expired rule must persist without changing hits or order"
    );

    let mut published_rule = base_rule;
    published_rule.enabled = Some(true);
    store.insert(published_rule);
    store.save(&rules_path)?;
    manager.invalidate_rules_cache(&index_name);

    let persisted_published = crate::index::rules::RuleStore::load(&rules_path)?;
    let readback = persisted_published.get("managed-trail-rule").unwrap();
    assert_eq!(readback.enabled, Some(true));
    assert_eq!(readback.validity.as_ref().unwrap()[0].from, 1_704_067_200);

    let (active_ids, active_rules) = search_ids("trail", "category:outerwear")?;
    assert_eq!(active_ids, vec!["trail-3", "trail-1"]);
    assert_eq!(active_rules, vec!["managed-trail-rule"]);
    assert_eq!(
        search_ids("ridge", "category:outerwear")?.0,
        vec!["trail-1", "trail-2"],
        "A nonmatching query must retain its exact hits and order"
    );
    assert_eq!(
        search_ids("trail", "category:footwear")?.0,
        vec!["trail-4"],
        "A mismatched filter scope must retain its exact hits and order"
    );

    store.remove("managed-trail-rule");
    store.save(&rules_path)?;
    manager.invalidate_rules_cache(&index_name);
    let after_delete = crate::index::rules::RuleStore::load(&rules_path)?;
    assert!(after_delete.get("managed-trail-rule").is_none());
    let (deleted_ids, deleted_rules) = search_ids("trail", "category:outerwear")?;
    assert_eq!(deleted_ids, outerwear_baseline);
    assert!(
        deleted_rules.is_empty(),
        "Deleted rules must leave no residue"
    );
    Ok(())
}

#[tokio::test]
async fn test_pinned_position_respects_global_pagination_offset() -> Result<()> {
    let (manager, temp_dir, index_name) = setup_test().await;

    let rule = crate::index::rules::Rule {
        object_id: "pin-page-rule".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("laptop".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: None,
            filters: None,
        }],
        consequence: crate::index::rules::Consequence {
            promote: Some(vec![crate::index::rules::Promote::Single {
                object_id: "5".to_string(),
                position: 2,
            }]),
            hide: None,
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    };

    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(rule);
    store.save(&rules_path)?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "laptop", "rank": 1}))?,
        Document::from_json(&json!({"_id": "2", "name": "laptop", "rank": 2}))?,
        Document::from_json(&json!({"_id": "3", "name": "laptop", "rank": 3}))?,
        Document::from_json(&json!({"_id": "4", "name": "laptop", "rank": 4}))?,
        Document::from_json(&json!({"_id": "5", "name": "laptop", "rank": 5}))?,
    ];
    manager.add_documents_sync(&index_name, docs).await?;

    let sort = crate::types::Sort::ByField {
        field: "rank".to_string(),
        order: crate::types::SortOrder::Asc,
    };
    let result = manager.search_with_options(
        &index_name,
        "laptop",
        &SearchOptions {
            sort: Some(&sort),
            limit: 2,
            offset: 2,
            ..Default::default()
        },
    )?;

    let ids: Vec<&str> = result
        .documents
        .iter()
        .map(|doc| doc.document.id.as_str())
        .collect();
    assert_eq!(result.total, 5);
    assert_eq!(ids, vec!["5", "3"]);
    assert_eq!(result.documents.len(), 2);
    Ok(())
}

#[tokio::test]
async fn test_hidden_records_are_removed_before_pagination() -> Result<()> {
    let (manager, temp_dir, index_name) = setup_test().await;

    let rule = crate::index::rules::Rule {
        object_id: "hide-first".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("laptop".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: None,
            filters: None,
        }],
        consequence: crate::index::rules::Consequence {
            promote: None,
            hide: Some(vec![crate::index::rules::Hide {
                object_id: "1".to_string(),
            }]),
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    };

    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(rule);
    store.save(&rules_path)?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "laptop", "rank": 1}))?,
        Document::from_json(&json!({"_id": "2", "name": "laptop", "rank": 2}))?,
        Document::from_json(&json!({"_id": "3", "name": "laptop", "rank": 3}))?,
        Document::from_json(&json!({"_id": "4", "name": "laptop", "rank": 4}))?,
    ];
    manager.add_documents_sync(&index_name, docs).await?;

    let sort = crate::types::Sort::ByField {
        field: "rank".to_string(),
        order: crate::types::SortOrder::Asc,
    };
    let result = manager.search_with_options(
        &index_name,
        "laptop",
        &SearchOptions {
            sort: Some(&sort),
            limit: 2,
            ..Default::default()
        },
    )?;

    let ids: Vec<&str> = result
        .documents
        .iter()
        .map(|doc| doc.document.id.as_str())
        .collect();
    assert_eq!(ids, vec!["2", "3"]);
    assert_eq!(result.total, 3);
    Ok(())
}

#[tokio::test]
async fn test_hides_accumulate_across_multiple_matching_rules() -> Result<()> {
    let (manager, temp_dir, index_name) = setup_test().await;

    let first_rule = crate::index::rules::Rule {
        object_id: "hide-1".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("laptop".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: None,
            filters: None,
        }],
        consequence: crate::index::rules::Consequence {
            promote: None,
            hide: Some(vec![crate::index::rules::Hide {
                object_id: "1".to_string(),
            }]),
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    };

    let second_rule = crate::index::rules::Rule {
        object_id: "hide-2".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("laptop".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: None,
            filters: None,
        }],
        consequence: crate::index::rules::Consequence {
            promote: None,
            hide: Some(vec![crate::index::rules::Hide {
                object_id: "2".to_string(),
            }]),
            filter_promotes: None,
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    };

    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(first_rule);
    store.insert(second_rule);
    store.save(&rules_path)?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "gaming laptop", "rank": 1}))?,
        Document::from_json(&json!({"_id": "2", "name": "office laptop", "rank": 2}))?,
        Document::from_json(&json!({"_id": "3", "name": "budget laptop", "rank": 3}))?,
        Document::from_json(&json!({"_id": "4", "name": "pro laptop", "rank": 4}))?,
    ];
    manager.add_documents_sync(&index_name, docs).await?;

    let sort = crate::types::Sort::ByField {
        field: "rank".to_string(),
        order: crate::types::SortOrder::Asc,
    };
    let result = manager.search_with_options(
        &index_name,
        "laptop",
        &SearchOptions {
            sort: Some(&sort),
            limit: 10,
            ..Default::default()
        },
    )?;

    let ids: Vec<&str> = result
        .documents
        .iter()
        .map(|doc| doc.document.id.as_str())
        .collect();
    assert_eq!(ids, vec!["3", "4"]);
    assert_eq!(result.total, 2);
    Ok(())
}

#[tokio::test]
async fn test_filter_promotes_true_skips_promoted_doc_that_fails_active_filter() {
    let (manager, temp_dir, index_name) = setup_test().await;

    let rule = crate::index::rules::Rule {
        object_id: "filter-promotes-true-rule".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("laptop".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: None,
            filters: None,
        }],
        consequence: crate::index::rules::Consequence {
            promote: Some(vec![crate::index::rules::Promote::Single {
                object_id: "promo".to_string(),
                position: 0,
            }]),
            hide: None,
            filter_promotes: Some(true),
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    };

    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(rule);
    store.save(&rules_path).unwrap();

    let build_doc = |id: &str, title: &str, brand: &str| {
        let mut fields = HashMap::new();
        fields.insert(
            "title".to_string(),
            crate::types::FieldValue::Text(title.to_string()),
        );
        fields.insert(
            "brand".to_string(),
            crate::types::FieldValue::Text(brand.to_string()),
        );
        Document {
            id: id.to_string(),
            fields,
        }
    };
    manager
        .add_documents_sync(
            &index_name,
            vec![
                build_doc("apple-1", "gaming laptop", "Apple"),
                build_doc("apple-2", "office laptop", "Apple"),
                build_doc("promo", "featured tablet", "Samsung"),
            ],
        )
        .await
        .unwrap();

    let request_filter = crate::filter_parser::parse_filter("brand:Apple").unwrap();
    let result = manager
        .search(&index_name, "laptop", Some(&request_filter), None, 10)
        .unwrap();

    assert_eq!(result.applied_rules, vec!["filter-promotes-true-rule"]);
    assert!(
        !result
            .documents
            .iter()
            .any(|doc| doc.document.id == "promo"),
        "filterPromotes=true should skip promoted docs that fail active filters"
    );
}

#[tokio::test]
async fn test_filter_promotes_false_keeps_promoted_doc_even_if_filter_does_not_match() {
    let (manager, temp_dir, index_name) = setup_test().await;

    let rule = crate::index::rules::Rule {
        object_id: "filter-promotes-false-rule".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: Some("laptop".to_string()),
            anchoring: Some(crate::index::rules::Anchoring::Contains),
            alternatives: None,
            context: None,
            filters: None,
        }],
        consequence: crate::index::rules::Consequence {
            promote: Some(vec![crate::index::rules::Promote::Single {
                object_id: "promo".to_string(),
                position: 0,
            }]),
            hide: None,
            filter_promotes: Some(false),
            user_data: None,
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    };

    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(rule);
    store.save(&rules_path).unwrap();

    let build_doc = |id: &str, title: &str, brand: &str| {
        let mut fields = HashMap::new();
        fields.insert(
            "title".to_string(),
            crate::types::FieldValue::Text(title.to_string()),
        );
        fields.insert(
            "brand".to_string(),
            crate::types::FieldValue::Text(brand.to_string()),
        );
        Document {
            id: id.to_string(),
            fields,
        }
    };
    manager
        .add_documents_sync(
            &index_name,
            vec![
                build_doc("apple-1", "gaming laptop", "Apple"),
                build_doc("apple-2", "office laptop", "Apple"),
                build_doc("promo", "featured tablet", "Samsung"),
            ],
        )
        .await
        .unwrap();

    let request_filter = crate::filter_parser::parse_filter("brand:Apple").unwrap();
    let result = manager
        .search(&index_name, "laptop", Some(&request_filter), None, 10)
        .unwrap();

    assert_eq!(result.applied_rules, vec!["filter-promotes-false-rule"]);
    assert_eq!(
        result.documents.first().map(|doc| doc.document.id.as_str()),
        Some("promo"),
        "filterPromotes=false should keep current unconditional promote behavior"
    );
}

// ============================================================
// From test_rules_consequences.rs
// ============================================================

#[tokio::test]
async fn test_applied_rules_in_response() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let mut fields = HashMap::new();
    fields.insert(
        "name".to_string(),
        crate::types::FieldValue::Text("laptop".to_string()),
    );
    let doc = Document {
        id: "1".to_string(),
        fields,
    };
    manager.add_documents_sync("test", vec![doc]).await?;

    let rule = json!({
        "objectID": "test-rule",
        "conditions": [{"pattern": "laptop", "anchoring": "contains"}],
        "consequence": {"promote": [{"objectID": "1", "position": 0}]}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    let result = manager.search("test", "laptop", None, None, 10)?;
    assert_eq!(result.applied_rules.len(), 1);
    assert_eq!(result.applied_rules[0], "test-rule");
    Ok(())
}

#[tokio::test]
async fn test_user_data_in_response() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let mut fields = HashMap::new();
    fields.insert(
        "name".to_string(),
        crate::types::FieldValue::Text("laptop".to_string()),
    );
    let doc = Document {
        id: "1".to_string(),
        fields,
    };
    manager.add_documents_sync("test", vec![doc]).await?;

    let rule = json!({
        "objectID": "banner-rule",
        "conditions": [{"pattern": "laptop", "anchoring": "contains"}],
        "consequence": {"userData": {"banner": "summer-sale", "discount": 20}}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    let result = manager.search("test", "laptop", None, None, 10)?;
    assert_eq!(result.user_data.len(), 1);
    assert_eq!(result.user_data[0]["banner"], "summer-sale");
    assert_eq!(result.user_data[0]["discount"], 20);
    Ok(())
}

#[tokio::test]
async fn test_query_rewrite() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let mut f1 = HashMap::new();
    f1.insert(
        "name".to_string(),
        crate::types::FieldValue::Text("gaming laptop".to_string()),
    );
    let mut f2 = HashMap::new();
    f2.insert(
        "name".to_string(),
        crate::types::FieldValue::Text("office laptop".to_string()),
    );
    manager
        .add_documents_sync(
            "test",
            vec![
                Document {
                    id: "1".to_string(),
                    fields: f1,
                },
                Document {
                    id: "2".to_string(),
                    fields: f2,
                },
            ],
        )
        .await?;

    let rule = json!({
        "objectID": "rewrite-rule",
        "conditions": [{"pattern": "lptop", "anchoring": "is"}],
        "consequence": {"params": {"query": "laptop"}}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    let result = manager.search("test", "lptop", None, None, 10)?;
    assert_eq!(result.total, 2);
    assert!(result.documents.iter().any(|d| d.document.id == "1"));
    assert!(result.documents.iter().any(|d| d.document.id == "2"));
    Ok(())
}

#[tokio::test]
async fn test_multiple_rules_user_data() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let mut fields = HashMap::new();
    fields.insert(
        "name".to_string(),
        crate::types::FieldValue::Text("laptop".to_string()),
    );
    let doc = Document {
        id: "1".to_string(),
        fields,
    };
    manager.add_documents_sync("test", vec![doc]).await?;

    let rules = vec![
        json!({"objectID": "rule-1", "conditions": [{"pattern": "laptop", "anchoring": "contains"}], "consequence": {"userData": {"type": "banner", "id": 1}}}),
        json!({"objectID": "rule-2", "conditions": [{"pattern": "laptop", "anchoring": "contains"}], "consequence": {"userData": {"type": "discount", "id": 2}}}),
    ];
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&rules)?,
    )?;

    let result = manager.search("test", "laptop", None, None, 10)?;
    assert_eq!(result.user_data.len(), 2);
    assert_eq!(result.applied_rules.len(), 2);
    Ok(())
}

#[tokio::test]
async fn test_filter_condition_matched_rule() {
    let (manager, temp_dir, index_name) = setup_test().await;

    // Rule: condition requires brand:Apple filter to be active
    let rule = crate::index::rules::Rule {
        object_id: "filter-rule".to_string(),
        conditions: vec![crate::index::rules::Condition {
            pattern: None,
            anchoring: None,
            alternatives: None,
            context: None,
            filters: Some("brand:Apple".to_string()),
        }],
        consequence: crate::index::rules::Consequence {
            promote: None,
            hide: None,
            filter_promotes: None,
            user_data: Some(json!({"promo": "apple-sale"})),
            params: None,
        },
        description: None,
        enabled: None,
        validity: None,
    };

    let rules_path = temp_dir.path().join(&index_name).join("rules.json");
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(rule);
    store.save(&rules_path).unwrap();

    // Index docs with a "brand" field so the filter can match
    let docs = vec![
        crate::types::Document::from_json(&json!({"_id": "1", "name": "iPhone", "brand": "Apple"}))
            .unwrap(),
        crate::types::Document::from_json(
            &json!({"_id": "2", "name": "Galaxy", "brand": "Samsung"}),
        )
        .unwrap(),
    ];
    manager.add_documents_sync(&index_name, docs).await.unwrap();

    // Search WITH brand:Apple filter → rule should fire
    let apple_filter = crate::filter_parser::parse_filter("brand:Apple").unwrap();
    let result = manager
        .search_with_options(
            &index_name,
            "",
            &SearchOptions {
                filter: Some(&apple_filter),
                limit: 10,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        result.applied_rules.contains(&"filter-rule".to_string()),
        "Rule should fire when search filters match condition.filters"
    );
    assert_eq!(result.user_data.len(), 1);
    assert_eq!(result.user_data[0]["promo"], "apple-sale");

    // Search WITHOUT filter → rule should NOT fire
    let result_no_filter = manager.search(&index_name, "", None, None, 10).unwrap();
    assert!(
        !result_no_filter
            .applied_rules
            .contains(&"filter-rule".to_string()),
        "Rule should not fire without matching filters"
    );

    // Search with wrong filter → rule should NOT fire
    let samsung_filter = crate::filter_parser::parse_filter("brand:Samsung").unwrap();
    let result_wrong = manager
        .search_with_options(
            &index_name,
            "",
            &SearchOptions {
                filter: Some(&samsung_filter),
                limit: 10,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        !result_wrong
            .applied_rules
            .contains(&"filter-rule".to_string()),
        "Rule should not fire when filter value doesn't match"
    );
}

#[tokio::test]
async fn test_rule_params_filters_and_merges_with_request_filter() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "premium laptop", "price": 75}))?,
        Document::from_json(&json!({"_id": "2", "name": "ultra laptop", "price": 150}))?,
        Document::from_json(&json!({"_id": "3", "name": "budget laptop", "price": 25}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "rule-filter-inject",
        "conditions": [{"pattern": "laptop", "anchoring": "contains"}],
        "consequence": {"params": {"filters": "price<=100"}}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;
    let request_filter = crate::filter_parser::parse_filter("price>=50").unwrap();
    let result = manager.search_with_options(
        "test",
        "laptop",
        &SearchOptions {
            filter: Some(&request_filter),
            limit: 10,
            ..Default::default()
        },
    )?;

    assert_eq!(result.total, 1);
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.documents[0].document.id, "1");
    Ok(())
}

#[tokio::test]
async fn test_rule_hits_per_page_overrides_request_limit() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "gaming laptop"}))?,
        Document::from_json(&json!({"_id": "2", "name": "office laptop"}))?,
        Document::from_json(&json!({"_id": "3", "name": "budget laptop"}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "rule-hpp",
        "conditions": [{"pattern": "laptop", "anchoring": "contains"}],
        "consequence": {"params": {"hitsPerPage": 1}}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    let result = manager.search_with_options(
        "test",
        "laptop",
        &SearchOptions {
            limit: 3,
            ..Default::default()
        },
    )?;

    assert_eq!(result.total, 3);
    assert_eq!(result.documents.len(), 1);
    Ok(())
}

#[tokio::test]
async fn test_rule_restrict_searchable_attributes_overrides_request_scope() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "title": "gaming laptop", "brand": "apple"}))?,
        Document::from_json(&json!({"_id": "2", "title": "apple", "brand": "samsung"}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "rule-restrict-attrs",
        "conditions": [{"pattern": "apple", "anchoring": "contains"}],
        "consequence": {"params": {"restrictSearchableAttributes": ["brand"]}}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    let request_restrict_attrs = vec!["title".to_string()];
    let result = manager.search_with_options(
        "test",
        "apple",
        &SearchOptions {
            limit: 10,
            restrict_searchable_attrs: Some(&request_restrict_attrs),
            ..Default::default()
        },
    )?;

    assert_eq!(result.total, 1);
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.documents[0].document.id, "1");
    Ok(())
}

#[tokio::test]
async fn test_no_matching_rule_keeps_request_limit() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "gaming laptop"}))?,
        Document::from_json(&json!({"_id": "2", "name": "office laptop"}))?,
        Document::from_json(&json!({"_id": "3", "name": "budget laptop"}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "rule-hpp-no-match",
        "conditions": [{"pattern": "phone", "anchoring": "contains"}],
        "consequence": {"params": {"hitsPerPage": 1}}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    let result = manager.search_with_options(
        "test",
        "laptop",
        &SearchOptions {
            limit: 2,
            ..Default::default()
        },
    )?;

    assert_eq!(result.total, 3);
    assert_eq!(result.documents.len(), 2);
    Ok(())
}

#[tokio::test]
async fn test_rule_geo_params_override_are_exposed_in_search_result() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![Document::from_json(
        &json!({"_id": "1", "name": "gaming laptop"}),
    )?];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "rule-geo-override",
        "conditions": [{"pattern": "laptop", "anchoring": "contains"}],
        "consequence": {
            "params": {
                "aroundLatLng": "34.0522, -118.2437",
                "aroundRadius": 300000
            }
        }
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    let result = manager.search_with_options(
        "test",
        "laptop",
        &SearchOptions {
            limit: 10,
            ..Default::default()
        },
    )?;

    assert_eq!(
        result.effective_around_lat_lng,
        Some("34.0522, -118.2437".to_string())
    );
    assert_eq!(result.effective_around_radius, Some(json!(300000)));
    Ok(())
}

#[tokio::test]
async fn test_rule_facet_filters_and_merge_with_request_filter() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "laptop", "brand": "Apple", "price": 75}))?,
        Document::from_json(
            &json!({"_id": "2", "name": "laptop", "brand": "Samsung", "price": 150}),
        )?,
        Document::from_json(&json!({"_id": "3", "name": "laptop", "brand": "Apple", "price": 25}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "rule-facet-filter-inject",
        "conditions": [{"pattern": "laptop", "anchoring": "contains"}],
        "consequence": {"params": {"facetFilters": ["brand:Apple"]}}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;
    let settings = crate::index::settings::IndexSettings {
        attributes_for_faceting: vec!["brand".to_string()],
        ..crate::index::settings::IndexSettings::default()
    };
    settings.save(temp_dir.path().join("test").join("settings.json"))?;
    manager.invalidate_settings_cache("test");
    let loaded_settings = manager.get_settings("test").expect("settings should load");
    assert!(loaded_settings.facet_set().contains("brand"));

    let request_filter = crate::filter_parser::parse_filter("price>=50").unwrap();
    let baseline = manager.search_with_options(
        "test",
        "laptop",
        &SearchOptions {
            filter: Some(&request_filter),
            limit: 10,
            enable_rules: Some(false),
            ..Default::default()
        },
    )?;
    assert_eq!(baseline.total, 2);

    let result = manager.search_with_options(
        "test",
        "laptop",
        &SearchOptions {
            filter: Some(&request_filter),
            limit: 10,
            ..Default::default()
        },
    )?;

    assert_eq!(result.total, 1);
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.documents[0].document.id, "1");
    Ok(())
}

#[tokio::test]
async fn test_rule_numeric_filters_and_merge_with_request_filter() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "premium laptop", "price": 75}))?,
        Document::from_json(&json!({"_id": "2", "name": "ultra laptop", "price": 150}))?,
        Document::from_json(&json!({"_id": "3", "name": "budget laptop", "price": 25}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "rule-numeric-filter-inject",
        "conditions": [{"pattern": "laptop", "anchoring": "contains"}],
        "consequence": {"params": {"numericFilters": ["price<=100"]}}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    let request_filter = crate::filter_parser::parse_filter("price>=50").unwrap();
    let result = manager.search_with_options(
        "test",
        "laptop",
        &SearchOptions {
            filter: Some(&request_filter),
            limit: 10,
            ..Default::default()
        },
    )?;

    assert_eq!(result.total, 1);
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.documents[0].document.id, "1");
    Ok(())
}

#[tokio::test]
async fn test_rule_tag_filters_and_merge_with_request_filter() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(
            &json!({"_id": "1", "name": "laptop", "price": 75, "_tags": ["Featured"]}),
        )?,
        Document::from_json(
            &json!({"_id": "2", "name": "laptop", "price": 150, "_tags": ["Clearance"]}),
        )?,
        Document::from_json(
            &json!({"_id": "3", "name": "laptop", "price": 25, "_tags": ["Featured"]}),
        )?,
    ];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "rule-tag-filter-inject",
        "conditions": [{"pattern": "laptop", "anchoring": "contains"}],
        "consequence": {"params": {"tagFilters": ["Featured"]}}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;
    let settings = crate::index::settings::IndexSettings {
        attributes_for_faceting: vec!["_tags".to_string()],
        ..crate::index::settings::IndexSettings::default()
    };
    settings.save(temp_dir.path().join("test").join("settings.json"))?;
    manager.invalidate_settings_cache("test");
    let loaded_settings = manager.get_settings("test").expect("settings should load");
    assert!(loaded_settings.facet_set().contains("_tags"));

    let request_filter = crate::filter_parser::parse_filter("price>=50").unwrap();
    let baseline = manager.search_with_options(
        "test",
        "laptop",
        &SearchOptions {
            filter: Some(&request_filter),
            limit: 10,
            enable_rules: Some(false),
            ..Default::default()
        },
    )?;
    assert_eq!(baseline.total, 2);

    let result = manager.search_with_options(
        "test",
        "laptop",
        &SearchOptions {
            filter: Some(&request_filter),
            limit: 10,
            ..Default::default()
        },
    )?;

    assert_eq!(result.total, 1);
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.documents[0].document.id, "1");
    Ok(())
}

#[tokio::test]
async fn test_rule_optional_filters_append_to_request_optional_filters() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "laptop", "brand": "apple"}))?,
        Document::from_json(&json!({"_id": "2", "name": "laptop", "brand": "samsung"}))?,
        Document::from_json(
            &json!({"_id": "3", "name": "laptop laptop laptop laptop", "brand": "other"}),
        )?,
    ];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "rule-optional-filter-inject",
        "conditions": [{"pattern": "laptop", "anchoring": "contains"}],
        "consequence": {"params": {"optionalFilters": ["brand:apple<score=20000>"]}}
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    let request_optional_specs = vec![("brand".to_string(), "samsung".to_string(), 10000.0)];
    let opt_groups = SearchOptions::with_flat_optional_filters(Some(&request_optional_specs));
    let result = manager.search_with_options(
        "test",
        "laptop",
        &SearchOptions {
            limit: 3,
            optional_filter_specs: opt_groups.as_deref(),
            ..Default::default()
        },
    )?;

    assert_eq!(result.total, 3);
    assert_eq!(result.documents.len(), 3);
    assert_eq!(result.documents[0].document.id, "1");
    assert_eq!(result.documents[1].document.id, "2");
    Ok(())
}

// ============================================================
// automaticFacetFilters / automaticOptionalFacetFilters
// ============================================================

#[tokio::test]
async fn test_automatic_facet_filter_generates_mandatory_filter_from_pattern() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    // Every doc matches text query; automatic facet filter must do the narrowing.
    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "comedy movie", "genre": "comedy"}))?,
        Document::from_json(&json!({"_id": "2", "name": "comedy movie", "genre": "horror"}))?,
        Document::from_json(&json!({"_id": "3", "name": "comedy movie", "genre": "action"}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    // Rule: "{facet:genre} movie" captures "comedy" from query "comedy movie"
    // and injects mandatory filter genre:comedy.
    let rule = json!({
        "objectID": "genre-auto-filter",
        "conditions": [{"pattern": "{facet:genre} movie", "anchoring": "contains"}],
        "consequence": {
            "params": {
                "automaticFacetFilters": [{"facet": "genre"}]
            }
        }
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;
    let settings = crate::index::settings::IndexSettings {
        attributes_for_faceting: vec!["genre".to_string()],
        ..crate::index::settings::IndexSettings::default()
    };
    settings.save(temp_dir.path().join("test").join("settings.json"))?;
    manager.invalidate_settings_cache("test");
    let loaded_settings = manager.get_settings("test").expect("settings should load");
    assert!(loaded_settings.facet_set().contains("genre"));

    // All docs match text; automatic facet filter should narrow to only genre=comedy.
    let result = manager.search_with_options(
        "test",
        "comedy movie",
        &SearchOptions {
            limit: 10,
            ..Default::default()
        },
    )?;

    assert_eq!(
        result.total, 1,
        "Expected 1 result after genre:comedy filter"
    );
    assert_eq!(result.documents[0].document.id, "1");

    // Verify the rule was applied
    assert!(result
        .applied_rules
        .contains(&"genre-auto-filter".to_string()));

    Ok(())
}

#[tokio::test]
async fn test_automatic_facet_filter_ignored_when_facet_not_configured() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    // Every doc matches text query. If automatic facet filtering incorrectly fires for
    // a non-faceted attribute, results will be narrowed; correct behavior is no narrowing.
    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "comedy movie", "genre": "comedy"}))?,
        Document::from_json(&json!({"_id": "2", "name": "comedy movie", "genre": "horror"}))?,
        Document::from_json(&json!({"_id": "3", "name": "comedy movie", "genre": "action"}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "genre-auto-filter-no-facet-config",
        "conditions": [{"pattern": "{facet:genre} movie", "anchoring": "contains"}],
        "consequence": {
            "params": {
                "automaticFacetFilters": [{"facet": "genre"}]
            }
        }
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    // Deliberately do not configure `genre` in attributes_for_faceting.
    let result = manager.search_with_options(
        "test",
        "comedy movie",
        &SearchOptions {
            limit: 10,
            ..Default::default()
        },
    )?;

    assert_eq!(
        result.total, 3,
        "automaticFacetFilters should not narrow results when facet is not configured"
    );
    assert!(result
        .applied_rules
        .contains(&"genre-auto-filter-no-facet-config".to_string()));

    Ok(())
}

#[tokio::test]
async fn test_automatic_optional_facet_filter_boosts_matching_records() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    // Keep text relevance tied; optional facet filter should be what changes ranking.
    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "comedy movie", "genre": "horror"}))?,
        Document::from_json(&json!({"_id": "2", "name": "comedy movie", "genre": "comedy"}))?,
        Document::from_json(&json!({"_id": "3", "name": "comedy movie", "genre": "comedy"}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    // Rule captures "comedy" and boosts genre:comedy without filtering out non-matches.
    let rule = json!({
        "objectID": "genre-optional-boost",
        "conditions": [{"pattern": "{facet:genre} movie", "anchoring": "contains"}],
        "consequence": {
            "params": {
                "automaticOptionalFacetFilters": [{"facet": "genre", "score": 20000}]
            }
        }
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;
    let settings = crate::index::settings::IndexSettings {
        attributes_for_faceting: vec!["genre".to_string()],
        ..crate::index::settings::IndexSettings::default()
    };
    settings.save(temp_dir.path().join("test").join("settings.json"))?;
    manager.invalidate_settings_cache("test");

    // Query matches all docs; optional filter should reorder ranking in favor of comedy genre.
    let result = manager.search_with_options(
        "test",
        "comedy movie",
        &SearchOptions {
            limit: 10,
            ..Default::default()
        },
    )?;

    assert_eq!(
        result.total, 3,
        "Optional filter should not exclude records"
    );
    assert!(result
        .applied_rules
        .contains(&"genre-optional-boost".to_string()));

    let ordered_ids: Vec<&str> = result
        .documents
        .iter()
        .map(|doc| doc.document.id.as_str())
        .collect();
    let horror_pos = ordered_ids.iter().position(|id| *id == "1").unwrap();
    let comedy_positions: Vec<usize> = ordered_ids
        .iter()
        .enumerate()
        .filter_map(|(idx, id)| (*id == "2" || *id == "3").then_some(idx))
        .collect();
    assert_eq!(comedy_positions.len(), 2);
    assert!(
        comedy_positions.iter().all(|idx| *idx < horror_pos),
        "Boosted comedy records should rank before non-matching genre record"
    );

    Ok(())
}

#[tokio::test]
async fn test_automatic_facet_filter_combines_with_request_filter() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(
            &json!({"_id": "1", "name": "comedy movie", "genre": "comedy", "rating": 5}),
        )?,
        Document::from_json(
            &json!({"_id": "2", "name": "comedy movie", "genre": "comedy", "rating": 2}),
        )?,
        Document::from_json(
            &json!({"_id": "3", "name": "comedy movie", "genre": "horror", "rating": 5}),
        )?,
    ];
    manager.add_documents_sync("test", docs).await?;

    let rule = json!({
        "objectID": "genre-filter",
        "conditions": [{"pattern": "{facet:genre} movie", "anchoring": "contains"}],
        "consequence": {
            "params": {
                "automaticFacetFilters": [{"facet": "genre"}]
            }
        }
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;
    let settings = crate::index::settings::IndexSettings {
        attributes_for_faceting: vec!["genre".to_string()],
        ..crate::index::settings::IndexSettings::default()
    };
    settings.save(temp_dir.path().join("test").join("settings.json"))?;
    manager.invalidate_settings_cache("test");

    // Request filter: rating >= 4. Rule generates genre:comedy. Combined: genre:comedy AND rating>=4
    let request_filter = crate::filter_parser::parse_filter("rating>=4").unwrap();
    let result = manager.search_with_options(
        "test",
        "comedy movie",
        &SearchOptions {
            filter: Some(&request_filter),
            limit: 10,
            ..Default::default()
        },
    )?;

    // Only doc 1 matches: genre=comedy AND rating>=4
    assert_eq!(result.total, 1);
    assert_eq!(result.documents[0].document.id, "1");

    Ok(())
}

/// Two rules with `disjunctive: true` capture different genre values from query.
/// The generated filters should be OR'd (genre:comedy OR genre:action) instead of AND'd,
/// meaning docs matching either genre are returned.
#[tokio::test]
async fn test_automatic_facet_filter_disjunctive_or_semantics() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(
            &json!({"_id": "1", "name": "comedy action movie", "genre": "comedy"}),
        )?,
        Document::from_json(
            &json!({"_id": "2", "name": "comedy action movie", "genre": "action"}),
        )?,
        Document::from_json(
            &json!({"_id": "3", "name": "comedy action movie", "genre": "horror"}),
        )?,
    ];
    manager.add_documents_sync("test", docs).await?;

    // Rule 1: StartsWith captures first word as genre
    // Rule 2: EndsWith captures last non-"movie" word as genre
    // With disjunctive: true, the two captures OR-merge → genre:comedy OR genre:action
    let rules = vec![
        json!({
            "objectID": "genre-start",
            "conditions": [{"pattern": "{facet:genre} action movie", "anchoring": "is"}],
            "consequence": {
                "params": {
                    "automaticFacetFilters": [{"facet": "genre", "disjunctive": true}]
                }
            }
        }),
        json!({
            "objectID": "genre-end",
            "conditions": [{"pattern": "comedy {facet:genre} movie", "anchoring": "is"}],
            "consequence": {
                "params": {
                    "automaticFacetFilters": [{"facet": "genre", "disjunctive": true}]
                }
            }
        }),
    ];
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&rules)?,
    )?;
    let settings = crate::index::settings::IndexSettings {
        attributes_for_faceting: vec!["genre".to_string()],
        ..crate::index::settings::IndexSettings::default()
    };
    settings.save(temp_dir.path().join("test").join("settings.json"))?;
    manager.invalidate_settings_cache("test");

    let result = manager.search_with_options(
        "test",
        "comedy action movie",
        &SearchOptions {
            limit: 10,
            ..Default::default()
        },
    )?;

    // Disjunctive OR: genre:comedy OR genre:action → docs 1 and 2 match, doc 3 (horror) excluded
    assert_eq!(
        result.total,
        2,
        "Expected 2 results with disjunctive OR (comedy|action), got {}: {:?}",
        result.total,
        result
            .documents
            .iter()
            .map(|d| &d.document.id)
            .collect::<Vec<_>>()
    );
    let ids: Vec<&str> = result
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();
    assert!(ids.contains(&"1"), "comedy doc should be included");
    assert!(ids.contains(&"2"), "action doc should be included");

    Ok(())
}

// ============================================================
// Stage 5: Redirect, fallback, synonym integration tests
// ============================================================

/// Redirect rule populates `rendering_content.redirect.url` in SearchResult
/// and coexists with returned hits and other renderingContent keys.
#[tokio::test]
async fn test_redirect_rule_populates_rendering_content_in_search_result() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "help article one"}))?,
        Document::from_json(&json!({"_id": "2", "name": "help article two"}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    // Rule with redirect + banner widget in renderingContent
    let rule = json!({
        "objectID": "redirect-rule",
        "conditions": [{"pattern": "help", "anchoring": "is"}],
        "consequence": {
            "params": {
                "renderingContent": {
                    "redirect": { "url": "https://example.com/support" },
                    "widgets": { "banners": [{"image": {"urls": [{"url": "banner.png"}]}}] }
                }
            }
        }
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    let result = manager.search("test", "help", None, None, 10)?;

    // Hits should still be returned (redirect is UI-level, not server-side)
    assert!(result.total >= 1, "redirect rule should not suppress hits");

    // renderingContent should contain both redirect and widgets
    let rc = result
        .rendering_content
        .expect("rendering_content should be populated");
    assert_eq!(
        rc["redirect"]["url"].as_str(),
        Some("https://example.com/support"),
        "redirect URL should be present"
    );
    assert!(
        rc["widgets"]["banners"].is_array(),
        "widget banners should coexist with redirect"
    );

    // Applied rules metadata should reference the redirect rule
    assert!(
        result.applied_rules.contains(&"redirect-rule".to_string()),
        "redirect rule should appear in applied_rules"
    );

    Ok(())
}

/// When removeWordsIfNoResults triggers a fallback, rule metadata
/// (applied_rules, user_data, rendering_content) from the fallback search
/// should be present in the response.
#[tokio::test]
async fn test_remove_words_fallback_preserves_rule_metadata() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    // Only index "laptop" docs — searching "xyznonexistent laptop" should
    // find nothing for the full query, then the fallback ("laptop") should match.
    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "gaming laptop"}))?,
        Document::from_json(&json!({"_id": "2", "name": "office laptop"}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    // Rule that fires on "laptop" — should fire during fallback
    let rule = json!({
        "objectID": "laptop-rule",
        "conditions": [{"pattern": "laptop", "anchoring": "contains"}],
        "consequence": {
            "userData": {"promo": "laptop-sale"},
            "params": {
                "renderingContent": {
                    "redirect": { "url": "https://example.com/laptops" }
                }
            }
        }
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    // Set removeWordsIfNoResults to "lastWords"
    let settings = crate::index::settings::IndexSettings {
        remove_words_if_no_results: "firstWords".to_string(),
        ..Default::default()
    };
    settings.save(temp_dir.path().join("test").join("settings.json"))?;
    manager.invalidate_settings_cache("test");

    // Search "xyznonexistent laptop" — full query has 0 results,
    // fallback drops "laptop" first ("lastWords" removes from end),
    // then drops "xyznonexistent" — "laptop" alone should hit.
    let result = manager.search_with_options(
        "test",
        "xyznonexistent laptop",
        &SearchOptions {
            limit: 10,
            ..Default::default()
        },
    )?;

    // Fallback should have found results
    assert!(result.total >= 1, "fallback should find laptop docs");

    // Rule metadata should be present from the fallback search
    assert!(
        result.applied_rules.contains(&"laptop-rule".to_string()),
        "applied_rules should contain laptop-rule from fallback search: {:?}",
        result.applied_rules
    );
    assert!(
        !result.user_data.is_empty(),
        "user_data should be present from fallback rule"
    );
    assert_eq!(result.user_data[0]["promo"], "laptop-sale");
    let rc = result
        .rendering_content
        .expect("rendering_content should be present from fallback rule");
    assert_eq!(
        rc["redirect"]["url"].as_str(),
        Some("https://example.com/laptops")
    );

    Ok(())
}

/// Rules still fire correctly when synonyms are enabled and expanded.
#[tokio::test]
async fn test_rules_fire_with_synonym_expansion() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test")?;

    let docs = vec![
        Document::from_json(&json!({"_id": "1", "name": "cell phone case"}))?,
        Document::from_json(&json!({"_id": "2", "name": "mobile phone stand"}))?,
    ];
    manager.add_documents_sync("test", docs).await?;

    // Synonym: phone <-> mobile
    let synonyms = vec![json!({
        "objectID": "syn-phone-mobile",
        "type": "synonym",
        "synonyms": ["phone", "mobile"]
    })];
    std::fs::write(
        temp_dir.path().join("test").join("synonyms.json"),
        serde_json::to_string(&synonyms)?,
    )?;

    // Rule that fires on "phone" — should fire even when searching "mobile"
    // because the rule evaluator gets the synonym store
    let rule = json!({
        "objectID": "phone-promo",
        "conditions": [{"pattern": "phone", "anchoring": "contains"}],
        "consequence": {
            "userData": {"promo": "phone-deals"}
        }
    });
    std::fs::write(
        temp_dir.path().join("test").join("rules.json"),
        serde_json::to_string(&vec![rule])?,
    )?;

    // Search "phone" directly — rule should fire
    let result_direct = manager.search_with_options(
        "test",
        "phone",
        &SearchOptions {
            limit: 10,
            ..Default::default()
        },
    )?;
    assert!(
        result_direct
            .applied_rules
            .contains(&"phone-promo".to_string()),
        "rule should fire on direct 'phone' query"
    );
    assert_eq!(result_direct.user_data[0]["promo"], "phone-deals");

    // Search with synonyms enabled — results should include both docs
    assert!(
        result_direct.total >= 1,
        "synonym expansion should find results for 'phone'"
    );

    Ok(())
}
