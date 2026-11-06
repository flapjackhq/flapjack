use super::*;

// Contract from the official InstantSearch NoResultsBoundary: nbHits=0 hides
// Hits. This is a consumer regression, not a captured Algolia edge-case oracle.
#[tokio::test]
async fn promoted_only_hit_has_usable_counts_and_exhaustiveness() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    let index = "promoted-counts";
    state.manager.create_tenant(index).unwrap();
    state
        .manager
        .add_documents_sync(
            index,
            vec![Document::from_json(&json!({"objectID":"phone","name":"phone"})).unwrap()],
        )
        .await
        .unwrap();
    let mut rules = flapjack::index::rules::RuleStore::new();
    rules.insert(
        serde_json::from_value(json!({
            "objectID":"featured", "conditions":[{"pattern":"featured","anchoring":"is"}],
            "consequence":{"promote":[{"objectID":"phone","position":0}]}
        }))
        .unwrap(),
    );
    rules
        .save(&tmp.path().join(index).join("rules.json"))
        .unwrap();
    let app = search_router(state);

    for (request, expected_hits, expected_total, expected_pages) in [
        (json!({"query":"featured","hitsPerPage":1}), 1, 1, 1),
        (
            json!({"query":"featured","hitsPerPage":1,"page":1}),
            0,
            1,
            1,
        ),
        (json!({"query":"featured","hitsPerPage":0}), 0, 1, 0),
        (json!({"query":"featured","enableRules":false}), 0, 0, 0),
    ] {
        let response = post_search(&app, index, request.clone(), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(
            body["hits"].as_array().unwrap().len(),
            expected_hits,
            "{request}"
        );
        assert_eq!(body["nbHits"], expected_total, "{request}");
        assert_eq!(body["nbPages"], expected_pages, "{request}");
        assert_eq!(body["exhaustiveNbHits"], true);
        assert_eq!(body["exhaustive"]["nbHits"], true);
        if expected_hits > 0 {
            assert_eq!(body["hits"][0]["objectID"], "phone");
        }
    }
}
