use super::*;

async fn fixture(
    docs: Vec<serde_json::Value>,
    consequence: serde_json::Value,
) -> (Arc<IndexManager>, TempDir, String) {
    let (manager, dir, index) = setup_test().await;
    let settings = crate::index::settings::IndexSettings {
        attributes_for_faceting: vec!["brand".into()],
        ..Default::default()
    };
    settings
        .save(dir.path().join(&index).join("settings.json"))
        .unwrap();
    manager.invalidate_settings_cache(&index);
    manager
        .add_documents_sync(
            &index,
            docs.iter()
                .map(|doc| Document::from_json(doc).unwrap())
                .collect(),
        )
        .await
        .unwrap();
    let mut store = crate::index::rules::RuleStore::new();
    store.insert(
        serde_json::from_value(json!({
            "objectID":"count-rule", "conditions":[{"pattern":"laptop","anchoring":"is"}],
            "consequence":consequence
        }))
        .unwrap(),
    );
    store
        .save(&dir.path().join(&index).join("rules.json"))
        .unwrap();
    (manager, dir, index)
}

#[tokio::test]
async fn duplicate_and_missing_promotions_do_not_inflate_total() {
    let (manager, _dir, index) = fixture(
        vec![json!({"_id":"phone","name":"phone"})],
        json!({"promote":[
            {"objectID":"phone","position":0},
            {"objectID":"phone","position":1},
            {"objectID":"missing","position":2}
        ]}),
    )
    .await;
    let result = manager.search(&index, "laptop", None, None, 20).unwrap();
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.total, 1);
}

#[tokio::test]
async fn matching_pin_outside_collector_window_is_not_counted_twice() {
    let docs = (0..80)
        .map(|i| json!({"_id":format!("doc-{i}"),"name":"laptop","rank":i}))
        .collect();
    let (manager, _dir, index) = fixture(
        docs,
        json!({
            "promote":[{"objectID":"doc-79","position":0}]
        }),
    )
    .await;
    let sort = crate::types::Sort::ByField {
        field: "rank".into(),
        order: crate::types::SortOrder::Asc,
    };
    let result = manager
        .search_with_options(
            &index,
            "laptop",
            &SearchOptions {
                sort: Some(&sort),
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.documents[0].document.id, "doc-79");
    assert_eq!(result.total, 80);
}

#[tokio::test]
async fn hidden_match_outside_collector_window_reduces_total() {
    let docs = (0..80)
        .map(|i| json!({"_id":format!("doc-{i}"),"name":"laptop","rank":i}))
        .collect();
    let (manager, _dir, index) = fixture(
        docs,
        json!({
            "hide":[{"objectID":"doc-79"},{"objectID":"missing"}]
        }),
    )
    .await;
    let sort = crate::types::Sort::ByField {
        field: "rank".into(),
        order: crate::types::SortOrder::Asc,
    };
    let result = manager
        .search_with_options(
            &index,
            "laptop",
            &SearchOptions {
                sort: Some(&sort),
                limit: 1,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.documents[0].document.id, "doc-0");
    assert_eq!(result.total, 79);
}

#[tokio::test]
async fn promotion_adds_a_page_without_losing_the_last_organic_hit() {
    let (manager, _dir, index) = fixture(
        vec![
            json!({"_id":"organic","name":"laptop"}),
            json!({"_id":"promo","name":"phone"}),
        ],
        json!({"promote":[{"objectID":"promo","position":0}]}),
    )
    .await;
    for (offset, expected) in [(0, "promo"), (1, "organic")] {
        let result = manager
            .search_with_options(
                &index,
                "laptop",
                &SearchOptions {
                    offset,
                    limit: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(result.documents[0].document.id, expected);
        assert_eq!(result.total, 2);
    }
}

#[tokio::test]
async fn promotion_membership_uses_rule_rewrite_and_effective_filters() {
    for filter_promotes in [false, true] {
        let (manager, _dir, index) = fixture(vec![
            json!({"_id":"matching","name":"phone","brand":"Apple"}),
            json!({"_id":"excluded","name":"phone","brand":"Samsung"})
        ], json!({
            "params":{"query":"phone", "filters":"brand:Apple"},
            "filterPromotes":filter_promotes,
            "promote":[{"objectID":"matching","position":0},{"objectID":"excluded","position":1}]
        })).await;
        let result = manager.search(&index, "laptop", None, None, 20).unwrap();
        let expected = if filter_promotes { 1 } else { 2 };
        assert_eq!(result.documents.len(), expected);
        assert_eq!(result.total, expected, "filterPromotes={filter_promotes}");
    }
}

#[tokio::test]
async fn hidden_then_promoted_matching_record_is_counted_once() {
    let (manager, _dir, index) = fixture(
        vec![json!({"_id":"matching","name":"laptop"})],
        json!({"hide":[{"objectID":"matching"}],"promote":[{"objectID":"matching","position":0}]}),
    )
    .await;
    let result = manager.search(&index, "laptop", None, None, 20).unwrap();
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.total, 1);
}

#[tokio::test]
async fn synonym_variant_membership_does_not_double_count_a_promoted_match() {
    let (manager, dir, index) = fixture(
        vec![
            json!({"_id":"direct","name":"laptop"}),
            json!({"_id":"synonym","name":"notebook"}),
            json!({"_id":"extra","name":"phone"}),
        ],
        json!({"promote":[{"objectID":"synonym","position":0},{"objectID":"extra","position":1}]}),
    )
    .await;
    std::fs::write(
        dir.path().join(&index).join("synonyms.json"),
        serde_json::to_vec(&json!([
            {"objectID":"laptop-notebook","type":"synonym","synonyms":["laptop","notebook"]}
        ]))
        .unwrap(),
    )
    .unwrap();
    let baseline = manager
        .search_with_options(
            &index,
            "laptop",
            &SearchOptions {
                enable_rules: Some(false),
                limit: 20,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(baseline.total, 2);
    let result = manager.search(&index, "laptop", None, None, 20).unwrap();
    assert_eq!(result.documents.len(), 3);
    assert_eq!(result.total, 3);
}

#[tokio::test]
async fn disjoint_variant_hides_preserve_the_original_count_aggregation_basis() {
    let docs = ["laptop", "notebook"]
        .into_iter()
        .flat_map(|word| (0..80).map(move |i| json!({"_id":format!("{word}-{i}"),"name":word})))
        .collect();
    let (manager, dir, index) = fixture(docs, json!({})).await;
    // The supported limit is 50 hidden IDs per rule: use two matching rules.
    let mut rules = crate::index::rules::RuleStore::new();
    for word in ["laptop", "notebook"] {
        let hidden: Vec<_> = (0..50)
            .map(|i| json!({"objectID":format!("{word}-{i}")}))
            .collect();
        rules.insert(
            serde_json::from_value(json!({
                "objectID":format!("hide-{word}"),
                "conditions":[{"pattern":"laptop","anchoring":"is"}],
                "consequence":{"hide":hidden}
            }))
            .unwrap(),
        );
    }
    rules
        .save(&dir.path().join(&index).join("rules.json"))
        .unwrap();
    std::fs::write(
        dir.path().join(&index).join("synonyms.json"),
        serde_json::to_vec(&json!([
            {"objectID":"variants","type":"synonym","synonyms":["laptop","notebook"]}
        ]))
        .unwrap(),
    )
    .unwrap();
    let narrow = manager.search(&index, "laptop", None, None, 1).unwrap();
    assert_eq!(narrow.documents.len(), 1);
    // The existing truncated-variants count is max(per-variant totals), not
    // an exact union. Adjust each 80 by its own 50 hides, never 80 minus 100.
    assert_eq!(narrow.total, 30);
    let wide = manager.search(&index, "laptop", None, None, 1000).unwrap();
    assert_eq!(wide.documents.len(), 60);
    assert_eq!(wide.total, 60);
}

// Distinct totals count groups before rule promotion today. Preserving this
// behavior is deliberate in the ordinary-hit correction; the policy for
// replacing/suppressing/promoting distinct group members is a separate decision.
#[tokio::test]
async fn distinct_group_count_and_promoted_members_remain_unchanged() {
    let (manager, dir, index) = fixture(
        vec![
            json!({"_id":"first","name":"laptop","group":"same","rank":1}),
            json!({"_id":"second","name":"laptop","group":"same","rank":2}),
        ],
        json!({"promote":[{"objectID":"second","position":0}]}),
    )
    .await;
    let settings = crate::index::settings::IndexSettings {
        attribute_for_distinct: Some("group".into()),
        ..Default::default()
    };
    settings
        .save(dir.path().join(&index).join("settings.json"))
        .unwrap();
    manager.invalidate_settings_cache(&index);
    let sort = crate::types::Sort::ByField {
        field: "rank".into(),
        order: crate::types::SortOrder::Asc,
    };
    let result = manager
        .search_with_options(
            &index,
            "laptop",
            &SearchOptions {
                distinct: Some(1),
                sort: Some(&sort),
                limit: 20,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        result
            .documents
            .iter()
            .map(|d| d.document.id.as_str())
            .collect::<Vec<_>>(),
        vec!["second", "first"]
    );
    assert_eq!(result.total, 1);
}
