use axum::http::{Method, StatusCode};
use serde_json::json;
use std::collections::HashSet;

mod common;

const ADMIN_KEY: &str = "test-admin-key-parity";

async fn set_browse_filterable_attributes(app: &axum::Router, index_name: &str) {
    common::put_settings_and_wait(
        app,
        index_name,
        ADMIN_KEY,
        json!({"attributesForFaceting": ["category"]}),
        false,
    )
    .await;
}

#[tokio::test]
async fn browse_attributes_to_retrieve_returns_only_requested_fields_plus_object_id() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    common::seed_docs(
        &app,
        "products",
        ADMIN_KEY,
        vec![json!({"objectID": "p1", "title": "Alpha", "price": 199, "category": "books"})],
    )
    .await;

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({
            "attributesToRetrieve": ["title"],
            "hitsPerPage": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    let hit = hits[0].as_object().unwrap();
    assert_eq!(hit.get("objectID"), Some(&json!("p1")));
    assert_eq!(hit.get("title"), Some(&json!("Alpha")));
    assert_eq!(
        hit.len(),
        2,
        "hit should only include objectID and title: {hit:?}"
    );
}

#[tokio::test]
async fn browse_query_filters_matching_documents() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    common::seed_docs(
        &app,
        "products",
        ADMIN_KEY,
        vec![
            json!({"objectID": "m1", "title": "Whole Milk"}),
            json!({"objectID": "c1", "title": "Dark Chocolate"}),
        ],
    )
    .await;

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({"query": "milk", "hitsPerPage": 10})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["nbHits"], json!(1));
    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["objectID"], json!("m1"));
}

#[tokio::test]
async fn browse_query_and_filters_match_same_result_set() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    set_browse_filterable_attributes(&app, "products").await;
    common::seed_docs(
        &app,
        "products",
        ADMIN_KEY,
        vec![
            json!({"objectID": "d1", "title": "Whole Milk", "category": "dairy"}),
            json!({"objectID": "d2", "title": "Oat Milk", "category": "dairy"}),
            json!({"objectID": "s1", "title": "Milk Chocolate", "category": "snack"}),
            json!({"objectID": "h1", "title": "Desk Lamp", "category": "home"}),
        ],
    )
    .await;

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({
            "query": "milk",
            "filters": "category:dairy",
            "hitsPerPage": 10
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["nbHits"], json!(2));

    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    for hit in hits {
        assert_eq!(hit["category"], json!("dairy"));
        assert!(
            hit["title"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .contains("milk"),
            "combined query + filter browse should keep only dairy milk hits: {body}"
        );
    }
}

#[tokio::test]
async fn browse_params_string_is_applied_and_overrides_top_level_fields() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    common::seed_docs(
        &app,
        "products",
        ADMIN_KEY,
        vec![
            json!({"objectID": "a1", "title": "Alpha"}),
            json!({"objectID": "b1", "title": "Beta"}),
        ],
    )
    .await;

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({
            "query": "beta",
            "params": "query=alpha&hitsPerPage=1&attributesToRetrieve=%5B%22title%22%5D"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["nbHits"], json!(1));
    assert_eq!(body["hitsPerPage"], json!(1));

    let hits = body["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["objectID"], json!("a1"));
    assert_eq!(hits[0]["title"], json!("Alpha"));
    let hit = hits[0].as_object().unwrap();
    assert_eq!(hit.len(), 2, "params attributesToRetrieve must be applied");
}

#[tokio::test]
async fn browse_cursor_pagination_and_response_shape_match_algolia() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    common::seed_docs(
        &app,
        "products",
        ADMIN_KEY,
        vec![
            json!({"objectID": "p1", "title": "Doc 1"}),
            json!({"objectID": "p2", "title": "Doc 2"}),
            json!({"objectID": "p3", "title": "Doc 3"}),
        ],
    )
    .await;

    let (status, page1) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({"hitsPerPage": 2})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page1["page"], json!(0));
    assert_eq!(page1["nbHits"], json!(3));
    assert_eq!(page1["nbPages"], json!(2));
    assert_eq!(page1["hitsPerPage"], json!(2));

    let page1_hits = page1["hits"].as_array().unwrap();
    assert_eq!(page1_hits.len(), 2);
    for hit in page1_hits {
        assert!(
            hit.get("objectID").is_some(),
            "every hit must include objectID"
        );
    }

    let cursor = page1["cursor"]
        .as_str()
        .expect("first page must include cursor");
    assert!(!cursor.is_empty());

    let (status2, page2) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({"cursor": cursor})),
    )
    .await;
    assert_eq!(status2, StatusCode::OK);
    assert_eq!(page2["nbHits"], json!(3));
    assert_eq!(page2["page"], json!(0));
    assert_eq!(page2["nbPages"], json!(2));
    assert_eq!(page2["hitsPerPage"], json!(2));

    let page2_hits = page2["hits"].as_array().unwrap();
    assert_eq!(page2_hits.len(), 1);
    assert!(
        page2.get("cursor").is_some_and(|v| v.is_null()),
        "last page must include explicit cursor:null"
    );
}

#[tokio::test]
async fn browse_cursor_preserves_query_context_when_following_with_cursor_only() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    common::seed_docs(
        &app,
        "products",
        ADMIN_KEY,
        vec![
            json!({"objectID": "m1", "title": "milk chocolate"}),
            json!({"objectID": "x1", "title": "desk lamp"}),
            json!({"objectID": "m2", "title": "oat milk"}),
        ],
    )
    .await;

    let (status, page1) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({"query": "milk", "hitsPerPage": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page1["nbHits"], json!(2));
    assert_eq!(page1["nbPages"], json!(2));
    assert_eq!(page1["hitsPerPage"], json!(1));
    let cursor = page1["cursor"]
        .as_str()
        .expect("first page must include cursor");

    let (status2, page2) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({"cursor": cursor})),
    )
    .await;
    assert_eq!(status2, StatusCode::OK);
    assert_eq!(
        page2["nbHits"],
        json!(2),
        "cursor follow-up must preserve original query context"
    );
    assert_eq!(page2["nbPages"], json!(2));
    assert_eq!(page2["hitsPerPage"], json!(1));
    let hits = page2["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0]["title"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("milk"),
        "cursor follow-up should continue query-filtered results: {page2}"
    );
}

#[tokio::test]
async fn browse_cursor_preserves_query_and_filters_when_following_with_cursor_only() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    set_browse_filterable_attributes(&app, "products").await;
    common::seed_docs(
        &app,
        "products",
        ADMIN_KEY,
        vec![
            json!({"objectID": "d1", "title": "whole milk", "category": "dairy"}),
            json!({"objectID": "s1", "title": "milk chocolate", "category": "snack"}),
            json!({"objectID": "d2", "title": "oat milk", "category": "dairy"}),
            json!({"objectID": "h1", "title": "desk lamp", "category": "home"}),
        ],
    )
    .await;

    let (status, page1) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({
            "query": "milk",
            "filters": "category:dairy",
            "hitsPerPage": 1
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page1["nbHits"], json!(2));
    assert_eq!(page1["nbPages"], json!(2));
    assert_eq!(page1["hitsPerPage"], json!(1));
    let cursor = page1["cursor"]
        .as_str()
        .expect("first page must include cursor");

    let (status2, page2) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({"cursor": cursor})),
    )
    .await;
    assert_eq!(status2, StatusCode::OK);
    assert_eq!(
        page2["nbHits"],
        json!(2),
        "cursor follow-up must preserve original query + filters"
    );
    assert_eq!(page2["nbPages"], json!(2));
    assert_eq!(page2["hitsPerPage"], json!(1));
    assert_eq!(page2["query"], json!("milk"));
    assert!(
        page2["params"]
            .as_str()
            .unwrap_or("")
            .contains("filters=category%3Adairy"),
        "cursor follow-up should echo the preserved filters in params: {page2}"
    );

    let hits = page2["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["category"], json!("dairy"));
    assert!(
        hits[0]["title"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("milk"),
        "cursor follow-up should continue query + filter scoped results: {page2}"
    );
}

#[tokio::test]
async fn browse_empty_index_and_invalid_cursor_error_shape() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    let (status, _) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes",
        ADMIN_KEY,
        Some(json!({"uid": "empty"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (empty_status, empty_body) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/empty/browse",
        ADMIN_KEY,
        Some(json!({})),
    )
    .await;
    assert_eq!(empty_status, StatusCode::OK);
    assert_eq!(empty_body["hits"], json!([]));
    assert_eq!(empty_body["nbHits"], json!(0));
    assert_eq!(empty_body["page"], json!(0));
    assert_eq!(empty_body["nbPages"], json!(0));
    assert_eq!(empty_body["hitsPerPage"], json!(1000));
    assert!(
        empty_body.get("cursor").is_some_and(|v| v.is_null()),
        "empty browse result must include explicit cursor:null"
    );

    let (bad_status, err) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/empty/browse",
        ADMIN_KEY,
        Some(json!({"cursor": "not-base64"})),
    )
    .await;
    assert_eq!(bad_status, StatusCode::BAD_REQUEST);
    assert_eq!(err["status"], json!(400));
    assert!(
        err["message"]
            .as_str()
            .unwrap_or("")
            .contains("Invalid cursor"),
        "unexpected invalid-cursor error: {err}"
    );
}

#[tokio::test]
async fn browse_rejects_hits_per_page_below_one() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    common::seed_docs(
        &app,
        "products",
        ADMIN_KEY,
        vec![json!({"objectID": "p1", "title": "Alpha"})],
    )
    .await;

    let (status, err) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/products/browse",
        ADMIN_KEY,
        Some(json!({"hitsPerPage": 0})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["status"], json!(400));
    assert!(
        err["message"]
            .as_str()
            .unwrap_or("")
            .contains("hitsPerPage must be between 1 and 1000"),
        "unexpected hitsPerPage validation error: {err}"
    );
}

// ──────────────────────────────────────────────────────────────────
// Bulk-seed smoke test (10K docs)
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "smoke test for seed_docs_bulk — run manually"]
async fn browse_bulk_seed_10k_smoke() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    common::seed_docs_bulk(&app, "bulk10k", ADMIN_KEY, 10_000).await;

    let (status, body) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/bulk10k/browse",
        ADMIN_KEY,
        Some(json!({"hitsPerPage": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["nbHits"], json!(10_000), "expected 10K docs indexed");
}

// ──────────────────────────────────────────────────────────────────
// Cursor invalidation under index mutation
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn browse_cursor_invalidation_on_mutation() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    common::seed_docs_bulk(&app, "inval", ADMIN_KEY, 10_000).await;

    // Page 1
    let (status, page1) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/inval/browse",
        ADMIN_KEY,
        Some(json!({"hitsPerPage": 100})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cursor = page1["cursor"]
        .as_str()
        .expect("first page should have a cursor");

    // Page 2 — should work (no mutation yet)
    let (status, page2) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/inval/browse",
        ADMIN_KEY,
        Some(json!({"cursor": cursor})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cursor2 = page2["cursor"]
        .as_str()
        .expect("second page should have a cursor");

    // Mutate the index — add a new doc (triggers commit, changes segment UUIDs)
    common::seed_docs(
        &app,
        "inval",
        ADMIN_KEY,
        vec![json!({"objectID": "new_doc", "v": 99999})],
    )
    .await;

    // Page 3 with old cursor — should fail with generation mismatch
    let (status, err) = common::send_json(
        &app,
        Method::POST,
        "/1/indexes/inval/browse",
        ADMIN_KEY,
        Some(json!({"cursor": cursor2})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "expected 400 after mutation"
    );
    assert!(
        err["message"]
            .as_str()
            .unwrap_or("")
            .contains("Cursor is not valid anymore"),
        "expected generation-mismatch error, got: {err}"
    );
}

// ──────────────────────────────────────────────────────────────────
// 1M-document full-scan browse with latency + memory profiling
// ──────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "1M doc stress test with RSS assertions — run manually with cargo test -- --ignored"]
async fn browse_1m_docs_full_scan_slow() {
    const DOC_COUNT: usize = 1_000_000;
    const HITS_PER_PAGE: usize = 1_000;

    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    eprintln!("[1M-browse] Indexing {DOC_COUNT} docs in batches of 5,000...");
    let t_index_start = std::time::Instant::now();
    common::seed_docs_bulk(&app, "million", ADMIN_KEY, DOC_COUNT).await;
    eprintln!(
        "[1M-browse] Indexing complete in {:.1}s",
        t_index_start.elapsed().as_secs_f64()
    );

    let baseline_rss_kb = common::sample_rss_kb();
    let mut peak_rss_kb = baseline_rss_kb;
    let mut visited = HashSet::with_capacity(DOC_COUNT);
    let mut page_count: usize = 0;
    let mut cursor: Option<String> = None;
    let t_browse_start = std::time::Instant::now();
    let mut t_checkpoint = t_browse_start;

    loop {
        let payload = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({"hitsPerPage": HITS_PER_PAGE}),
        };

        let (status, body) = common::send_json(
            &app,
            Method::POST,
            "/1/indexes/million/browse",
            ADMIN_KEY,
            Some(payload),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "browse failed on page {page_count}");

        let hits = body["hits"].as_array().expect("hits should be an array");
        for hit in hits {
            let oid = hit["objectID"]
                .as_str()
                .expect("objectID must be a string")
                .to_string();
            visited.insert(oid);
        }

        page_count += 1;

        // Latency checkpoint every 100 pages
        if page_count.is_multiple_of(100) {
            let elapsed = t_checkpoint.elapsed();
            let avg_ms_per_page = elapsed.as_secs_f64() * 1000.0 / 100.0;
            eprintln!(
                "[1M-browse] pages {}-{}: {:.1}ms total ({:.1}ms/page), visited={}",
                page_count - 100,
                page_count,
                elapsed.as_secs_f64() * 1000.0,
                avg_ms_per_page,
                visited.len()
            );
            t_checkpoint = std::time::Instant::now();
        }

        // Memory checkpoint every 200 pages
        if page_count.is_multiple_of(200) {
            let rss = common::sample_rss_kb();
            if rss > peak_rss_kb {
                peak_rss_kb = rss;
            }
            eprintln!(
                "[1M-browse] page {page_count}: RSS={:.0}MB (peak={:.0}MB, baseline={:.0}MB)",
                rss as f64 / 1024.0,
                peak_rss_kb as f64 / 1024.0,
                baseline_rss_kb as f64 / 1024.0,
            );
        }

        cursor = body["cursor"].as_str().map(|s| s.to_string());
        if cursor.is_none() {
            break;
        }
    }

    let browse_secs = t_browse_start.elapsed().as_secs_f64();
    eprintln!(
        "[1M-browse] Complete: {page_count} pages, {:.1}s total, {} unique docs visited",
        browse_secs,
        visited.len()
    );

    // Final memory sample
    let final_rss = common::sample_rss_kb();
    if final_rss > peak_rss_kb {
        peak_rss_kb = final_rss;
    }
    let rss_growth_mb = peak_rss_kb.saturating_sub(baseline_rss_kb) as f64 / 1024.0;
    eprintln!(
        "[1M-browse] Memory: baseline={:.0}MB, peak={:.0}MB, growth={:.0}MB",
        baseline_rss_kb as f64 / 1024.0,
        peak_rss_kb as f64 / 1024.0,
        rss_growth_mb,
    );

    // Assertions
    assert_eq!(
        visited.len(),
        DOC_COUNT,
        "expected exactly {DOC_COUNT} unique docs visited, got {}",
        visited.len()
    );
    assert!(
        cursor.is_none(),
        "expected no cursor on final page (all docs exhausted)"
    );

    // Soft memory bound: 2GB growth above baseline catches catastrophic blowup
    let rss_growth_gb = rss_growth_mb / 1024.0;
    assert!(
        rss_growth_gb < 2.0,
        "RSS grew by {rss_growth_gb:.1}GB — exceeds 2GB soft bound"
    );

    // Observed profile on 2026-03-02 in this environment:
    // - Full scan completed in 741.85s with 1,000,000 unique docs visited.
    // - Peak RSS growth stayed within the 2GB soft bound.
    // Per these results, browse is bounded enough for now; no follow-up mitigation ticket opened.
}

#[tokio::test]
async fn browse_query_uses_authenticated_application_dictionary() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    common::seed_docs(
        &app,
        "products",
        ADMIN_KEY,
        vec![json!({"objectID":"delta", "title":"delta"})],
    )
    .await;
    common::put_settings_and_wait(
        &app,
        "products",
        ADMIN_KEY,
        json!({
            "removeStopWords":["en"], "queryLanguages":["en"],
            "queryType":"prefixNone", "removeWordsIfNoResults":"none"
        }),
        false,
    )
    .await;
    for (app_id, word) in [("customer-a", "alpha"), ("_default", "beta")] {
        let (status, _) = common::send_authed(&app, Method::POST, "/1/dictionaries/stopwords/batch", ADMIN_KEY, app_id, &[], Some(json!({
            "requests":[{"action":"addEntry","body":{"objectID":"custom","language":"en","word":word}}]
        }))).await;
        assert_eq!(status, StatusCode::OK);
    }
    for endpoint in ["query", "browse"] {
        let (status, body) = common::send_authed(
            &app,
            Method::POST,
            &format!("/1/indexes/products/{endpoint}"),
            ADMIN_KEY,
            "customer-a",
            &[],
            Some(json!({"query":"alpha delta"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["hits"].as_array().unwrap().len(),
            1,
            "{endpoint}: authenticated application stopword must apply"
        );
        assert_eq!(body["hits"][0]["objectID"], "delta");
    }
}
