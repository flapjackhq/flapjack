//! Stub summary for batch_federation.rs.
use super::*;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchSearchRequestWithFederation {
    requests: Vec<SearchRequest>,
    strategy: Option<String>,
    federation: Option<crate::federation::FederationConfig>,
}
/// TODO: Document batch_request_parses_federation_defaults_and_optional_presence.
#[test]
fn batch_request_parses_federation_defaults_and_optional_presence() {
    let with_federation: BatchSearchRequestWithFederation = serde_json::from_value(json!({
        "federation": {},
        "requests": [{ "indexName": "products", "query": "shoe" }]
    }))
    .expect("request with federation should deserialize");

    let federation = with_federation
        .federation
        .expect("federation object should be present");
    assert_eq!(federation.offset, 0);
    assert_eq!(federation.limit, 20);
    assert!(federation.merge_facets.is_none());
    assert_eq!(with_federation.requests.len(), 1);
    assert!(with_federation.strategy.is_none());

    let without_federation: BatchSearchRequestWithFederation = serde_json::from_value(json!({
        "requests": [{ "indexName": "products", "query": "shoe" }]
    }))
    .expect("request without federation should deserialize");
    assert!(
        without_federation.federation.is_none(),
        "federation should be None when omitted"
    );
}
/// TODO: Document batch_search_federation_two_indexes_returns_flat_hits_with_metadata.
#[tokio::test]
async fn batch_search_federation_two_indexes_returns_flat_hits_with_metadata() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_products",
        vec![vec![("title", "shoe product")]],
    )
    .await;
    create_index_with_docs(
        &state,
        "fed_articles",
        vec![vec![("title", "shoe article")]],
    )
    .await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "federation": {},
            "requests": [
                {"indexName": "fed_products", "query": "shoe"},
                {"indexName": "fed_articles", "query": "shoe"}
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert!(
        body.get("results").is_none(),
        "federated response must not include results array"
    );

    let hits = body["hits"]
        .as_array()
        .expect("federated response must include top-level hits array");
    assert_eq!(hits.len(), 2);

    for hit in hits {
        let federation = &hit["_federation"];
        assert!(
            federation["indexName"].is_string(),
            "federated hit must include _federation.indexName"
        );
        assert!(
            federation["queriesPosition"].is_u64(),
            "federated hit must include _federation.queriesPosition"
        );
        assert!(
            federation["weightedRankingScore"].is_number(),
            "federated hit must include _federation.weightedRankingScore"
        );
    }
}
/// TODO: Document batch_search_federation_weight_boosting_promotes_boosted_index_hits.
#[tokio::test]
async fn batch_search_federation_weight_boosting_promotes_boosted_index_hits() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_weight_normal",
        vec![vec![("title", "shoe normal")]],
    )
    .await;
    create_index_with_docs(
        &state,
        "fed_weight_boosted",
        vec![vec![("title", "shoe boosted")]],
    )
    .await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "federation": {},
            "requests": [
                {"indexName": "fed_weight_normal", "query": "shoe"},
                {
                    "indexName": "fed_weight_boosted",
                    "query": "shoe",
                    "federationOptions": {"weight": 2.0}
                }
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    let hits = body["hits"]
        .as_array()
        .expect("federated response must include top-level hits array");
    let top_index = hits[0]["_federation"]["indexName"]
        .as_str()
        .expect("top hit must include federation index name");
    assert_eq!(
        top_index, "fed_weight_boosted",
        "higher federationOptions.weight should promote boosted index results"
    );
}
/// TODO: Document batch_search_federation_pagination_uses_offset_limit_and_estimated_total_hits.
#[tokio::test]
async fn batch_search_federation_pagination_uses_offset_limit_and_estimated_total_hits() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_page_alpha",
        vec![
            vec![("title", "item one")],
            vec![("title", "item two")],
            vec![("title", "item three")],
        ],
    )
    .await;
    create_index_with_docs(
        &state,
        "fed_page_beta",
        vec![
            vec![("title", "item four")],
            vec![("title", "item five")],
            vec![("title", "item six")],
        ],
    )
    .await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "federation": {"offset": 1, "limit": 2},
            "requests": [
                {"indexName": "fed_page_alpha", "query": "item"},
                {"indexName": "fed_page_beta", "query": "item"}
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    let hits = body["hits"]
        .as_array()
        .expect("federated response must include top-level hits array");
    assert_eq!(
        hits.len(),
        2,
        "offset=1 limit=2 should return exactly 2 hits"
    );
    assert_eq!(body["offset"], 1);
    assert_eq!(body["limit"], 2);
    assert_eq!(
        body["estimatedTotalHits"], 6,
        "estimatedTotalHits should sum nbHits from each federated query result"
    );
}
/// TODO: Document batch_search_federation_ignores_per_query_page_and_hits_per_page.
#[tokio::test]
async fn batch_search_federation_ignores_per_query_page_and_hits_per_page() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_window_alpha",
        vec![
            vec![("title", "window item one")],
            vec![("title", "window item two")],
            vec![("title", "window item three")],
        ],
    )
    .await;
    create_index_with_docs(
        &state,
        "fed_window_beta",
        vec![vec![("title", "window item four")]],
    )
    .await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "federation": {"limit": 3},
            "requests": [
                {"indexName": "fed_window_alpha", "query": "window", "page": 1, "hitsPerPage": 1},
                {"indexName": "fed_window_beta", "query": "window"}
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    let hits = body["hits"]
        .as_array()
        .expect("federated response must include top-level hits array");
    assert_eq!(hits.len(), 3);
    assert_eq!(
        hits[0]["objectID"], "doc_fed_window_alpha_0",
        "federation must rank from each query's top hits, not the request's page window"
    );
}
/// TODO: Document batch_search_federation_ignores_per_query_response_fields.
#[tokio::test]
async fn batch_search_federation_ignores_per_query_response_fields() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_response_fields_alpha",
        vec![vec![("title", "response field item one")]],
    )
    .await;
    create_index_with_docs(
        &state,
        "fed_response_fields_beta",
        vec![vec![("title", "response field item two")]],
    )
    .await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "federation": {},
            "requests": [
                {"indexName": "fed_response_fields_alpha", "query": "response", "responseFields": ["page"]},
                {"indexName": "fed_response_fields_beta", "query": "response", "responseFields": ["page"]}
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(
        body["estimatedTotalHits"], 2,
        "federation must still compute estimatedTotalHits when subqueries request custom responseFields"
    );
    assert!(
        body["hits"].is_array(),
        "federation must ignore per-query responseFields that would remove hits"
    );
}
/// TODO: Document batch_search_without_federation_keeps_legacy_results_shape.
#[tokio::test]
async fn batch_search_without_federation_keeps_legacy_results_shape() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_backcompat",
        vec![vec![("title", "legacy request shape")]],
    )
    .await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "requests": [
                {"indexName": "fed_backcompat", "query": "legacy"}
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(body["results"].is_array());
    assert!(
        body.get("hits").is_none(),
        "non-federated batch response must remain results[]-based"
    );
}
/// TODO: Document batch_search_rejects_federation_with_stop_if_enough_matches_strategy.
#[tokio::test]
async fn batch_search_rejects_federation_with_stop_if_enough_matches_strategy() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_strategy_reject",
        vec![vec![("title", "shoe one")], vec![("title", "shoe two")]],
    )
    .await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "federation": {},
            "strategy": "stopIfEnoughMatches",
            "requests": [
                {"indexName": "fed_strategy_reject", "query": "shoe"}
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("strategy=stopIfEnoughMatches is not supported with federation"),
        "unexpected error: {body}"
    );
}
/// TODO: Document batch_search_rejects_federation_with_type_facet_queries.
#[tokio::test]
async fn batch_search_rejects_federation_with_type_facet_queries() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_facet_reject",
        vec![vec![("title", "shoe"), ("category", "footwear")]],
    )
    .await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "federation": {},
            "requests": [
                {
                    "indexName": "fed_facet_reject",
                    "type": "facet",
                    "facet": "category",
                    "facetQuery": "foot"
                }
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    let message = body["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("Facet queries (type=facet) are not supported with federation"),
        "unexpected error: {body}"
    );
}
/// TODO: Document batch_search_rejects_federation_merge_facets_until_supported.
#[tokio::test]
async fn batch_search_rejects_federation_merge_facets_until_supported() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_merge_facets_reject",
        vec![vec![("title", "shoe")]],
    )
    .await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "federation": {"mergeFacets": {}},
            "requests": [
                {"indexName": "fed_merge_facets_reject", "query": "shoe"}
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(
        body["message"].as_str(),
        Some("Facet merging in federated search is not yet supported"),
        "mergeFacets rejection message must stay exact for contract stability"
    );

    let facet_query_response = post_batch_search(
        &app,
        json!({
            "requests": [
                {
                    "indexName": "fed_merge_facets_reject",
                    "type": "facet",
                    "facet": "category",
                    "facetQuery": "sho"
                }
            ]
        }),
    )
    .await;
    assert_eq!(
        facet_query_response.status(),
        StatusCode::OK,
        "non-federated facet queries must continue using the legacy batch path"
    );
    let facet_body = body_json(facet_query_response).await;
    assert!(
        facet_body["results"][0]["facetHits"].is_array(),
        "non-federated facet queries must still return facetHits under results[]"
    );
}
/// TODO: Document batch_search_federation_still_enforces_per_query_index_permissions.
#[tokio::test]
async fn batch_search_federation_still_enforces_per_query_index_permissions() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(&state, "fed_auth_allowed", vec![vec![("title", "shoe")]]).await;
    create_index_with_docs(&state, "fed_auth_forbidden", vec![vec![("title", "shoe")]]).await;
    let app = batch_router(state);

    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/1/indexes/*/queries")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "federation": {},
                "requests": [
                    {"indexName": "fed_auth_allowed", "query": "shoe"},
                    {"indexName": "fed_auth_forbidden", "query": "shoe"}
                ]
            })
            .to_string(),
        ))
        .unwrap();
    request.extensions_mut().insert(crate::auth::ApiKey {
        hash: "test-hash".to_string(),
        salt: "test-salt".to_string(),
        hmac_key: None,
        created_at: 0,
        acl: vec!["search".to_string()],
        description: "federation test key".to_string(),
        indexes: vec!["fed_auth_allowed".to_string()],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: Vec::new(),
        restrict_sources: None,
        validity: 0,
    });

    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(response).await,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );
}

/// Empty-string queries across multiple indexes should return all indexed documents
/// (the federation equivalent of a "browse all" query).
#[tokio::test]
async fn batch_search_federation_empty_query_returns_all_documents() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_empty_alpha",
        vec![vec![("title", "alpha one")], vec![("title", "alpha two")]],
    )
    .await;
    create_index_with_docs(&state, "fed_empty_beta", vec![vec![("title", "beta one")]]).await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "federation": {},
            "requests": [
                {"indexName": "fed_empty_alpha", "query": ""},
                {"indexName": "fed_empty_beta", "query": ""}
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    let hits = body["hits"]
        .as_array()
        .expect("federated response must include hits array");
    // All 3 documents should appear in the merged result.
    assert_eq!(
        hits.len(),
        3,
        "empty query federation should return all documents from all indexes"
    );
    assert_eq!(body["estimatedTotalHits"], 3);
}

/// Single-index federation should produce a valid federated response shape
/// (flat hits array, no results array) with the same documents as a non-federated
/// query against that index.
#[tokio::test]
async fn batch_search_federation_single_index_matches_non_federated_results() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_single_compat",
        vec![
            vec![("title", "compat shoe one")],
            vec![("title", "compat shoe two")],
        ],
    )
    .await;
    let app = batch_router(state);

    // Federated single-index query.
    let fed_response = post_batch_search(
        &app,
        json!({
            "federation": {},
            "requests": [
                {"indexName": "fed_single_compat", "query": "shoe"}
            ]
        }),
    )
    .await;
    assert_eq!(fed_response.status(), StatusCode::OK);
    let fed_body = body_json(fed_response).await;

    // Non-federated query against the same index.
    let legacy_response = post_batch_search(
        &app,
        json!({
            "requests": [
                {"indexName": "fed_single_compat", "query": "shoe"}
            ]
        }),
    )
    .await;
    assert_eq!(legacy_response.status(), StatusCode::OK);
    let legacy_body = body_json(legacy_response).await;

    // Both should return the same documents (by objectID).
    let fed_hits = fed_body["hits"]
        .as_array()
        .expect("federated response must include hits");
    let legacy_hits = legacy_body["results"][0]["hits"]
        .as_array()
        .expect("legacy response must include results[0].hits");

    let mut fed_ids: Vec<String> = fed_hits
        .iter()
        .filter_map(|h| h["objectID"].as_str().map(String::from))
        .collect();
    let mut legacy_ids: Vec<String> = legacy_hits
        .iter()
        .filter_map(|h| h["objectID"].as_str().map(String::from))
        .collect();
    fed_ids.sort();
    legacy_ids.sort();

    assert_eq!(
        fed_ids, legacy_ids,
        "single-index federation must return same documents as non-federated query"
    );
}

/// Federation with offset=0, limit=0 should return zero hits but still compute
/// estimatedTotalHits correctly.
#[tokio::test]
async fn batch_search_federation_limit_zero_returns_count_only() {
    let tmp = TempDir::new().unwrap();
    let state = make_basic_search_state(&tmp);
    create_index_with_docs(
        &state,
        "fed_limit_zero",
        vec![vec![("title", "shoe one")], vec![("title", "shoe two")]],
    )
    .await;
    let app = batch_router(state);

    let response = post_batch_search(
        &app,
        json!({
            "federation": {"offset": 0, "limit": 0},
            "requests": [
                {"indexName": "fed_limit_zero", "query": "shoe"}
            ]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    let hits = body["hits"]
        .as_array()
        .expect("federated response must include hits array");
    assert!(hits.is_empty(), "limit=0 should return zero hits");
    // estimatedTotalHits should still reflect the actual match count.
    assert_eq!(
        body["estimatedTotalHits"], 2,
        "estimatedTotalHits should still be computed with limit=0"
    );
}
