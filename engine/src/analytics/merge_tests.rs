use super::*;

/// Verify that top-K merging sums counts for overlapping keys across two nodes and includes all distinct entries.
#[test]
fn test_merge_top_k_overlapping() {
    let r1 = json!({"searches": [
        {"search": "iphone", "count": 100},
        {"search": "samsung", "count": 50},
    ]});
    let r2 = json!({"searches": [
        {"search": "iphone", "count": 80},
        {"search": "pixel", "count": 60},
    ]});

    let merged = merge_top_k(&[r1, r2], "searches", "search", 10);
    let searches = merged["searches"].as_array().unwrap();

    assert_eq!(searches[0]["search"], "iphone");
    assert_eq!(searches[0]["count"], 180); // 100 + 80
                                           // pixel (60) and samsung (50)
    assert_eq!(searches.len(), 3);
}

#[test]
fn test_merge_top_k_ties_are_sorted_by_key() {
    let node_a = json!({"searches": [
        {"search": "zebra", "count": 1, "nbHits": 2},
        {"search": "alpha", "count": 2, "nbHits": 3},
    ]});
    let node_b = json!({"searches": [
        {"search": "zebra", "count": 1, "nbHits": 2},
        {"search": "beta", "count": 2, "nbHits": 4},
    ]});

    let merged = merge_top_k(&[node_a, node_b], "searches", "search", 10);
    let queries: Vec<&str> = merged["searches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["search"].as_str().unwrap())
        .collect();

    assert_eq!(queries, vec!["alpha", "beta", "zebra"]);
}

#[test]
fn test_merge_top_k_uses_global_weighted_nb_hits_average() {
    // Node A saw one 100-hit search. Node B saw nine 10-hit searches.
    // The global average is (100 + 90) / 10 = 19, not 100 + 10 = 110
    // and not the unweighted node average 55.
    let node_a = json!({"searches": [{
        "search": "trail",
        "count": 1,
        "nbHits": 100,
        "_nbHitsSum": 100,
        "_nbHitsCount": 1
    }]});
    let node_b = json!({"searches": [{
        "search": "trail",
        "count": 9,
        "nbHits": 10,
        "_nbHitsSum": 90,
        "_nbHitsCount": 9
    }]});

    let merged = merge_top_k(&[node_a, node_b], "searches", "search", 10);
    let trail = &merged["searches"][0];
    assert_eq!(trail["count"], 10);
    assert_eq!(trail["nbHits"], 19);
    assert!(trail.get("_nbHitsSum").is_none());
    assert!(trail.get("_nbHitsCount").is_none());
}

#[test]
fn test_merge_rates_never_average() {
    // Node A: 1 no-result out of 4 searches = 25%
    // Node B: 2 no-results out of 6 searches = 33%
    // Correct: 3/10 = 30%, NOT (25%+33%)/2 = 29%
    let r1 = json!({"noResults": 1, "count": 4, "rate": 0.25});
    let r2 = json!({"noResults": 2, "count": 6, "rate": 0.333});

    let merged = merge_rates(&[r1, r2], "noResults", "count", "rate");
    let rate = merged["rate"].as_f64().unwrap();
    assert!(
        (rate - 0.3).abs() < 0.001,
        "rate should be 0.3, got {}",
        rate
    );
}

#[test]
fn test_merge_rates_returns_null_when_denominator_is_zero() {
    let r1 = json!({"noResults": 0, "count": 0, "rate": 0.0, "dates": []});
    let r2 = json!({"noResults": 0, "count": 0, "rate": 0.0, "dates": []});
    let merged = merge_rates(&[r1, r2], "noResults", "count", "rate");
    assert!(merged["rate"].is_null());
}

#[test]
fn test_merge_weighted_avg() {
    // Node A: avg=3, n=10 → sum=30
    // Node B: avg=7, n=20 → sum=140
    // Correct: 170/30 ≈ 5.67
    let r1 = json!({"average": 3.0, "clickCount": 10});
    let r2 = json!({"average": 7.0, "clickCount": 20});

    let merged = merge_weighted_avg(&[r1, r2], "average", "clickCount");
    let avg = merged["average"].as_f64().unwrap();
    assert!(
        (avg - 5.667).abs() < 0.01,
        "avg should be ~5.67, got {}",
        avg
    );
}

/// Verify that count-with-daily merging sums the overall total and per-date counts, including dates that appear in only one node.
#[test]
fn test_merge_count_with_daily() {
    let r1 = json!({"count": 100, "dates": [
        {"date": "2026-02-10", "count": 60},
        {"date": "2026-02-11", "count": 40},
    ]});
    let r2 = json!({"count": 80, "dates": [
        {"date": "2026-02-10", "count": 30},
        {"date": "2026-02-12", "count": 50},
    ]});

    let merged = merge_count_with_daily(&[r1, r2]);
    assert_eq!(merged["count"], 180);
    let dates = merged["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 3);
    // Dates are sorted chronologically
    assert_eq!(dates[0]["date"], "2026-02-10");
    assert_eq!(dates[0]["count"], 90); // 60+30
    assert_eq!(dates[1]["date"], "2026-02-11");
    assert_eq!(dates[1]["count"], 40); // only r1
    assert_eq!(dates[2]["date"], "2026-02-12");
    assert_eq!(dates[2]["count"], 50); // only r2
}

#[test]
fn test_merge_with_empty_input() {
    // merge_results handles empty by returning {}
    let merged = merge_results("searches", &[], 10);
    assert_eq!(merged, json!({}));
}

// C1: test_merge_histogram
/// Verify that histogram merging sums `clickCount` for matching position buckets and preserves buckets that appear in only one node.
#[test]
fn test_merge_histogram() {
    let r1 = json!({"positions": [
        {"position": [1, 3], "clickCount": 10},
        {"position": [4, 10], "clickCount": 20},
    ]});
    let r2 = json!({"positions": [
        {"position": [1, 3], "clickCount": 5},
        {"position": [11, 20], "clickCount": 15},
    ]});

    let merged = merge_histogram(&[r1, r2], "positions");
    let positions = merged["positions"].as_array().unwrap();

    assert_eq!(positions.len(), 3);
    // [1,3] should be summed: 10 + 5 = 15
    assert_eq!(positions[0]["position"], json!([1, 3]));
    assert_eq!(positions[0]["clickCount"], 15);
    // [4,10] only in r1
    assert_eq!(positions[1]["position"], json!([4, 10]));
    assert_eq!(positions[1]["clickCount"], 20);
    // [11,20] only in r2
    assert_eq!(positions[2]["position"], json!([11, 20]));
    assert_eq!(positions[2]["clickCount"], 15);
}

// C2: test_merge_category_counts_preserves_field_names
/// Verify that category count merging preserves endpoint-specific field names (`platform`, `country`, `region`) and recomputes the `total` field rather than copying it from the first node.
#[test]
fn test_merge_category_counts_preserves_field_names() {
    // Devices: "platforms" array with "platform" field
    let r1 = json!({"platforms": [
        {"platform": "desktop", "count": 100},
        {"platform": "mobile", "count": 50},
    ]});
    let r2 = json!({"platforms": [
        {"platform": "desktop", "count": 60},
        {"platform": "tablet", "count": 30},
    ]});
    let merged = merge_category_counts(&[r1, r2], "platforms", "platform", "count");
    let platforms = merged["platforms"].as_array().unwrap();
    assert_eq!(platforms.len(), 3);
    // Should be sorted by count desc: desktop=160, mobile=50, tablet=30
    assert_eq!(platforms[0]["platform"], "desktop");
    assert_eq!(platforms[0]["count"], 160);
    // Ensure "platform" field name is preserved (not "name")
    assert!(platforms[0].get("name").is_none());

    // Geo: "countries" array with "country" field + "total"
    let g1 = json!({"countries": [{"country": "US", "count": 200}], "total": 200});
    let g2 = json!({"countries": [{"country": "US", "count": 100}, {"country": "DE", "count": 50}], "total": 150});
    let merged = merge_category_counts(&[g1, g2], "countries", "country", "count");
    let countries = merged["countries"].as_array().unwrap();
    assert_eq!(countries[0]["country"], "US");
    assert_eq!(countries[0]["count"], 300);
    assert_eq!(countries[1]["country"], "DE");
    assert_eq!(countries[1]["count"], 50);
    // Total must be recomputed (300+50=350), NOT preserved from first node (200)
    assert_eq!(
        merged["total"], 350,
        "total should be recomputed, not from first node"
    );

    // Regions: "regions" array with "region" field
    let rg1 = json!({"country": "US", "regions": [{"region": "CA", "count": 80}]});
    let rg2 = json!({"country": "US", "regions": [{"region": "CA", "count": 40}, {"region": "NY", "count": 60}]});
    let merged = merge_category_counts(&[rg1, rg2], "regions", "region", "count");
    let regions = merged["regions"].as_array().unwrap();
    assert_eq!(regions[0]["region"], "CA");
    assert_eq!(regions[0]["count"], 120);
    assert_eq!(regions[1]["region"], "NY");
    assert_eq!(regions[1]["count"], 60);
    // Preserves "country" from base
    assert_eq!(merged["country"], "US");
}

// C3: test_merge_user_counts_with_hll
/// Verify that HLL sketch merging deduplicates overlapping user sets, producing a count near the true cardinality rather than the naive sum.
#[test]
fn test_merge_user_counts_with_hll() {
    use super::super::hll::HllSketch;

    // Create two sketches with known overlap
    let items1: Vec<String> = (0..5000).map(|i| format!("user_{}", i)).collect();
    let items2: Vec<String> = (3000..8000).map(|i| format!("user_{}", i)).collect();
    let s1 = HllSketch::from_items(items1.iter().map(|s| s.as_str()));
    let s2 = HllSketch::from_items(items2.iter().map(|s| s.as_str()));

    let r1 = json!({"count": 5000, "hll_sketch": s1.to_base64(), "dates": []});
    let r2 = json!({"count": 5000, "hll_sketch": s2.to_base64(), "dates": []});

    let merged = merge_user_counts(&[r1, r2]);
    let count = merged["count"].as_i64().unwrap();
    // True unique = 8000, HLL p=14 should be within ~3%
    let error = (count as f64 - 8000.0).abs() / 8000.0;
    assert!(
        error < 0.05,
        "HLL merged count {} has {}% error (expected ~8000)",
        count,
        error * 100.0
    );
    // Must be substantially less than naive sum of 10000
    assert!(count < 8500, "count {} too close to naive sum 10000", count);
}

// C4: test_merge_user_counts_fallback_no_sketches
#[test]
fn test_merge_user_counts_fallback_no_sketches() {
    let r1 = json!({"count": 500, "dates": []});
    let r2 = json!({"count": 300, "dates": []});

    let merged = merge_user_counts(&[r1, r2]);
    assert_eq!(merged["count"], 800);
}

// C5: test_merge_overview
/// Verify that overview merging sums `totalSearches` and `uniqueUsers`, merges per-index statistics by name, nulls out rates, and combines daily counts.
#[test]
fn test_merge_overview() {
    let r1 = json!({
        "totalSearches": 1000,
        "uniqueUsers": 200,
        "noResultRate": 0.1,
        "clickThroughRate": 0.5,
        "indices": [
            {"index": "products", "searches": 800, "noResults": 80},
            {"index": "blog", "searches": 200, "noResults": 20},
        ],
        "dates": [
            {"date": "2026-02-10", "count": 600},
            {"date": "2026-02-11", "count": 400},
        ]
    });
    let r2 = json!({
        "totalSearches": 500,
        "uniqueUsers": 100,
        "noResultRate": 0.2,
        "clickThroughRate": 0.3,
        "indices": [
            {"index": "products", "searches": 400, "noResults": 40},
        ],
        "dates": [
            {"date": "2026-02-10", "count": 300},
            {"date": "2026-02-12", "count": 200},
        ]
    });

    let merged = merge_overview(&[r1, r2]);

    // totalSearches summed
    assert_eq!(merged["totalSearches"], 1500);
    // uniqueUsers summed (approximate)
    assert_eq!(merged["uniqueUsers"], 300);
    // rates are null (can't recompute without components)
    assert!(merged["noResultRate"].is_null());
    assert!(merged["clickThroughRate"].is_null());

    // indices merged by name
    let indices = merged["indices"].as_array().unwrap();
    assert_eq!(indices.len(), 2);
    // products: 800+400=1200 searches
    let products = indices.iter().find(|i| i["index"] == "products").unwrap();
    assert_eq!(products["searches"], 1200);
    assert_eq!(products["noResults"], 120);
    // blog: 200 searches (only in r1)
    let blog = indices.iter().find(|i| i["index"] == "blog").unwrap();
    assert_eq!(blog["searches"], 200);

    // dates merged
    let dates = merged["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 3);
    let feb10 = dates.iter().find(|d| d["date"] == "2026-02-10").unwrap();
    assert_eq!(feb10["count"], 900); // 600+300
}

// C6: test_merge_results_routing
/// Verify that `merge_results` dispatches each endpoint to the correct merge strategy (TopK, CountWithDaily, Rate, WeightedAvg, Histogram, CategoryCounts, Overview).
#[test]
fn test_merge_results_routing() {
    // Each endpoint type should dispatch to the correct merge function.
    // We use inputs that produce recognizably different outputs per strategy.

    // TopK: searches
    let r1 = json!({"searches": [{"search": "a", "count": 10}]});
    let r2 = json!({"searches": [{"search": "a", "count": 5}]});
    let merged = merge_results("searches", &[r1, r2], 10);
    assert_eq!(merged["searches"][0]["count"], 15);

    // CountWithDaily: searches/count
    let r1 = json!({"count": 10, "dates": []});
    let r2 = json!({"count": 20, "dates": []});
    let merged = merge_results("searches/count", &[r1, r2], 100);
    assert_eq!(merged["count"], 30);

    // Rate: searches/noResultRate (uses corrected field names)
    let r1 = json!({"noResults": 3, "count": 10, "rate": 0.3, "dates": []});
    let r2 = json!({"noResults": 2, "count": 10, "rate": 0.2, "dates": []});
    let merged = merge_results("searches/noResultRate", &[r1, r2], 100);
    assert_eq!(merged["noResults"], 5);
    assert_eq!(merged["count"], 20);

    // Rate: searches/noClickRate (uses corrected field names)
    let r1 = json!({"noClickCount": 5, "trackedSearchCount": 20, "rate": 0.25, "dates": []});
    let r2 = json!({"noClickCount": 3, "trackedSearchCount": 10, "rate": 0.3, "dates": []});
    let merged = merge_results("searches/noClickRate", &[r1, r2], 100);
    assert_eq!(merged["noClickCount"], 8);
    assert_eq!(merged["trackedSearchCount"], 30);

    // WeightedAvg: clicks/averageClickPosition
    let r1 = json!({"average": 2.0, "clickCount": 10, "dates": []});
    let r2 = json!({"average": 4.0, "clickCount": 10, "dates": []});
    let merged = merge_results("clicks/averageClickPosition", &[r1, r2], 100);
    let avg = merged["average"].as_f64().unwrap();
    assert!((avg - 3.0).abs() < 0.01);

    // Histogram: clicks/positions
    let r1 = json!({"positions": [{"position": [1,3], "clickCount": 10}]});
    let r2 = json!({"positions": [{"position": [1,3], "clickCount": 5}]});
    let merged = merge_results("clicks/positions", &[r1, r2], 100);
    assert_eq!(merged["positions"][0]["clickCount"], 15);

    // CategoryCounts: devices (corrected to "platforms")
    let r1 = json!({"platforms": [{"platform": "desktop", "count": 10}]});
    let r2 = json!({"platforms": [{"platform": "desktop", "count": 5}]});
    let merged = merge_results("devices", &[r1, r2], 100);
    assert_eq!(merged["platforms"][0]["count"], 15);
    assert_eq!(merged["platforms"][0]["platform"], "desktop");

    // CategoryCounts: geo (countries)
    let r1 = json!({"countries": [{"country": "US", "count": 100}], "total": 100});
    let r2 = json!({"countries": [{"country": "US", "count": 50}], "total": 50});
    let merged = merge_results("geo", &[r1, r2], 100);
    assert_eq!(merged["countries"][0]["count"], 150);
    assert_eq!(merged["countries"][0]["country"], "US");

    // Overview
    let r1 = json!({"totalSearches": 100, "uniqueUsers": 50, "indices": [], "dates": []});
    let r2 = json!({"totalSearches": 200, "uniqueUsers": 80, "indices": [], "dates": []});
    let merged = merge_results("overview", &[r1, r2], 100);
    assert_eq!(merged["totalSearches"], 300);
}

// C7: test_merge_top_k_respects_limit
/// Verify that top-K merging truncates the result array to the requested limit after summing and re-sorting.
#[test]
fn test_merge_top_k_respects_limit() {
    let r1 = json!({"searches": [
        {"search": "a", "count": 100},
        {"search": "b", "count": 80},
        {"search": "c", "count": 60},
        {"search": "d", "count": 40},
        {"search": "e", "count": 20},
    ]});
    let r2 = json!({"searches": [
        {"search": "a", "count": 50},
        {"search": "f", "count": 90},
        {"search": "g", "count": 70},
        {"search": "h", "count": 30},
        {"search": "i", "count": 10},
    ]});

    let merged = merge_top_k(&[r1, r2], "searches", "search", 3);
    let searches = merged["searches"].as_array().unwrap();
    assert_eq!(searches.len(), 3);
    // Top 3 by count: a=150, f=90, b=80
    assert_eq!(searches[0]["count"], 150);
    assert_eq!(searches[0]["search"], "a");
}

// C8: test_merge_rates_with_daily
/// Verify that rate merging sums numerators and denominators per date and recomputes daily rates correctly across three distinct dates from two nodes.
#[test]
fn test_merge_rates_with_daily() {
    let r1 = json!({
        "noResults": 3, "count": 10, "rate": 0.3,
        "dates": [
            {"date": "2026-02-10", "noResults": 2, "count": 6, "rate": 0.333},
            {"date": "2026-02-11", "noResults": 1, "count": 4, "rate": 0.25},
        ]
    });
    let r2 = json!({
        "noResults": 2, "count": 8, "rate": 0.25,
        "dates": [
            {"date": "2026-02-10", "noResults": 1, "count": 4, "rate": 0.25},
            {"date": "2026-02-12", "noResults": 1, "count": 4, "rate": 0.25},
        ]
    });

    let merged = merge_rates(&[r1, r2], "noResults", "count", "rate");

    // Overall: 5/18 ≈ 0.278
    let rate = merged["rate"].as_f64().unwrap();
    assert!(
        (rate - 5.0 / 18.0).abs() < 0.001,
        "overall rate wrong: {}",
        rate
    );

    let dates = merged["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 3);

    // Feb 10: (2+1)/(6+4) = 3/10 = 0.3
    let feb10 = dates.iter().find(|d| d["date"] == "2026-02-10").unwrap();
    let r10 = feb10["rate"].as_f64().unwrap();
    assert!((r10 - 0.3).abs() < 0.001, "feb10 rate wrong: {}", r10);
    // Verify daily entries preserve numerator/denominator field names and values
    assert_eq!(feb10["noResults"], 3, "feb10 noResults should be 2+1=3");
    assert_eq!(feb10["count"], 10, "feb10 count should be 6+4=10");

    // Feb 11: 1/4 = 0.25 (only from r1)
    let feb11 = dates.iter().find(|d| d["date"] == "2026-02-11").unwrap();
    let r11 = feb11["rate"].as_f64().unwrap();
    assert!((r11 - 0.25).abs() < 0.001, "feb11 rate wrong: {}", r11);
    assert_eq!(feb11["noResults"], 1);
    assert_eq!(feb11["count"], 4);
}

// C9: test_merge_histogram_no_duplicates_on_zero
/// Regression: verify that a bucket with `clickCount: 0` in one node does not create a duplicate entry when merged with a nonzero count from another node.
#[test]
fn test_merge_histogram_no_duplicates_on_zero() {
    // B1 regression: first result has clickCount=0 for a bucket
    let r1 = json!({"positions": [
        {"position": [1, 3], "clickCount": 0},
    ]});
    let r2 = json!({"positions": [
        {"position": [1, 3], "clickCount": 5},
    ]});

    let merged = merge_histogram(&[r1, r2], "positions");
    let positions = merged["positions"].as_array().unwrap();

    // Should be exactly 1 bucket, not 2
    assert_eq!(
        positions.len(),
        1,
        "expected 1 bucket, got {}: {:?}",
        positions.len(),
        positions
    );
    assert_eq!(positions[0]["clickCount"], 5);
}

// B2 regression: nbHits must be globally weighted, not added across nodes.
/// Regression: verify that `nbHits` is weighted by node search count and that entries lacking `nbHits` do not gain the field.
#[test]
fn test_merge_top_k_weights_legacy_nb_hits() {
    let r1 = json!({"searches": [
        {"search": "iphone", "count": 100, "nbHits": 5000},
        {"search": "samsung", "count": 50, "nbHits": 2000},
    ]});
    let r2 = json!({"searches": [
        {"search": "iphone", "count": 80, "nbHits": 3000},
        {"search": "pixel", "count": 60},
    ]});

    let merged = merge_top_k(&[r1, r2], "searches", "search", 10);
    let searches = merged["searches"].as_array().unwrap();

    let iphone = searches.iter().find(|s| s["search"] == "iphone").unwrap();
    assert_eq!(iphone["count"], 180);
    // Weighted legacy fallback: (5000*100 + 3000*80) / 180 = 4111.
    assert_eq!(
        iphone["nbHits"], 4111,
        "nbHits should be globally weighted across nodes"
    );

    let samsung = searches.iter().find(|s| s["search"] == "samsung").unwrap();
    assert_eq!(
        samsung["nbHits"], 2000,
        "samsung nbHits should be preserved"
    );

    // pixel has no nbHits in input — should NOT gain an nbHits field
    let pixel = searches.iter().find(|s| s["search"] == "pixel").unwrap();
    assert!(
        pixel.get("nbHits").is_none(),
        "pixel should not gain nbHits field"
    );
}

// B3 regression: mixed HLL/non-HLL should add fallback, not drop it
/// Regression: verify that when some nodes supply HLL sketches and others do not, the fallback count is added to the HLL cardinality rather than being silently dropped.
#[test]
fn test_merge_user_counts_mixed_hll_and_fallback() {
    use super::super::hll::HllSketch;

    let items: Vec<String> = (0..1000).map(|i| format!("user_{}", i)).collect();
    let sketch = HllSketch::from_items(items.iter().map(|s| s.as_str()));

    // Node 1 has HLL sketch, Node 2 only has count (no sketch)
    let r1 = json!({"count": 1000, "hll_sketch": sketch.to_base64(), "dates": []});
    let r2 = json!({"count": 500, "dates": []});

    let merged = merge_user_counts(&[r1, r2]);
    let count = merged["count"].as_i64().unwrap();

    // Should be HLL(~1000) + 500 fallback ≈ 1500
    // HLL for 1000 items at p=14 should be 970-1030, so total should be ~1470-1530
    assert!(
        (1400..=1600).contains(&count),
        "count {} should be ~1500 (HLL ~1000 + fallback 500)",
        count
    );
}

// Single result passthrough — merge_results must return it unchanged
#[test]
fn test_merge_results_single_result_passthrough() {
    let single = json!({
        "searches": [{"search": "test", "count": 42, "nbHits": 100}],
        "extraField": "preserved"
    });
    let merged = merge_results("searches", std::slice::from_ref(&single), 10);
    assert_eq!(
        merged, single,
        "single result should pass through unchanged"
    );
}

// Routing: CTR uses correct field names (clickCount, trackedSearchCount)
/// Verify that click-through-rate routing uses the correct field names (`clickCount`, `trackedSearchCount`) for both overall and daily entries.
#[test]
fn test_merge_results_ctr_field_names() {
    let r1 = json!({
        "clickCount": 10, "trackedSearchCount": 100, "rate": 0.1,
        "dates": [{"date": "2026-02-10", "clickCount": 10, "trackedSearchCount": 100, "rate": 0.1}]
    });
    let r2 = json!({
        "clickCount": 20, "trackedSearchCount": 200, "rate": 0.1,
        "dates": [{"date": "2026-02-10", "clickCount": 20, "trackedSearchCount": 200, "rate": 0.1}]
    });
    let merged = merge_results("clicks/clickThroughRate", &[r1, r2], 100);
    assert_eq!(merged["clickCount"], 30);
    assert_eq!(merged["trackedSearchCount"], 300);
    let rate = merged["rate"].as_f64().unwrap();
    assert!((rate - 0.1).abs() < 0.001);

    let dates = merged["dates"].as_array().unwrap();
    let d = &dates[0];
    assert_eq!(d["clickCount"], 30);
    assert_eq!(d["trackedSearchCount"], 300);
}

// Routing: conversion rate uses correct field names
#[test]
fn test_merge_results_conversion_rate_field_names() {
    let r1 = json!({"conversionCount": 5, "trackedSearchCount": 50, "rate": 0.1, "dates": []});
    let r2 = json!({"conversionCount": 15, "trackedSearchCount": 150, "rate": 0.1, "dates": []});
    let merged = merge_results("conversions/conversionRate", &[r1, r2], 100);
    assert_eq!(merged["conversionCount"], 20);
    assert_eq!(merged["trackedSearchCount"], 200);
    let rate = merged["rate"].as_f64().unwrap();
    assert!((rate - 0.1).abs() < 0.001);
}

#[test]
fn test_merge_results_add_to_cart_rate_field_names() {
    let r1 = json!({"addToCartCount": 7, "trackedSearchCount": 70, "rate": 0.1, "dates": []});
    let r2 = json!({"addToCartCount": 5, "trackedSearchCount": 50, "rate": 0.1, "dates": []});
    let merged = merge_results("conversions/addToCartRate", &[r1, r2], 100);
    assert_eq!(merged["addToCartCount"], 12);
    assert_eq!(merged["trackedSearchCount"], 120);
    let rate = merged["rate"].as_f64().unwrap();
    assert!((rate - 0.1).abs() < 0.001);
}

#[test]
fn test_merge_results_purchase_rate_field_names() {
    let r1 = json!({"purchaseCount": 3, "trackedSearchCount": 30, "rate": 0.1, "dates": []});
    let r2 = json!({"purchaseCount": 9, "trackedSearchCount": 90, "rate": 0.1, "dates": []});
    let merged = merge_results("conversions/purchaseRate", &[r1, r2], 100);
    assert_eq!(merged["purchaseCount"], 12);
    assert_eq!(merged["trackedSearchCount"], 120);
    let rate = merged["rate"].as_f64().unwrap();
    assert!((rate - 0.1).abs() < 0.001);
}

// Routing: filter_values dispatches to TopK with ("values", "value")
#[test]
fn test_merge_results_filter_values_routing() {
    let r1 = json!({"attribute": "brand", "values": [
        {"value": "Apple", "count": 100},
        {"value": "Samsung", "count": 50},
    ]});
    let r2 = json!({"attribute": "brand", "values": [
        {"value": "Apple", "count": 60},
    ]});
    let merged = merge_results("filters/brand", &[r1, r2], 10);
    let values = merged["values"].as_array().unwrap();
    let apple = values.iter().find(|v| v["value"] == "Apple").unwrap();
    assert_eq!(apple["count"], 160);
    // Preserves "attribute" from base
    assert_eq!(merged["attribute"], "brand");
}

// Routing: geo/<country>/regions dispatches to CategoryCounts with ("regions", "region")
/// Verify that geo region endpoints (e.g. `geo/US/regions`) route to `CategoryCounts` with the correct field mapping and preserve the `country` field from the base result.
#[test]
fn test_merge_results_geo_regions_routing() {
    let r1 = json!({"country": "US", "regions": [
        {"region": "CA", "count": 100},
    ]});
    let r2 = json!({"country": "US", "regions": [
        {"region": "CA", "count": 50},
        {"region": "NY", "count": 80},
    ]});
    let merged = merge_results("geo/US/regions", &[r1, r2], 100);
    let regions = merged["regions"].as_array().unwrap();
    let ca = regions.iter().find(|r| r["region"] == "CA").unwrap();
    assert_eq!(ca["count"], 150);
    let ny = regions.iter().find(|r| r["region"] == "NY").unwrap();
    assert_eq!(ny["count"], 80);
    assert_eq!(merged["country"], "US");
}

// Routing: geo/<country> (top searches) dispatches to TopK with ("searches", "search")
#[test]
fn test_merge_results_geo_top_searches_routing() {
    let r1 = json!({"country": "US", "searches": [{"search": "iphone", "count": 50}]});
    let r2 = json!({"country": "US", "searches": [{"search": "iphone", "count": 30}]});
    let merged = merge_results("geo/US", &[r1, r2], 10);
    assert_eq!(merged["searches"][0]["count"], 80);
    assert_eq!(merged["country"], "US");
}

// Weighted avg daily breakdown
/// Verify that weighted average merging correctly computes `sum(avg * count) / sum(count)` for both overall and per-date breakdowns.
#[test]
fn test_merge_weighted_avg_with_daily() {
    let r1 = json!({
        "average": 3.0, "clickCount": 10,
        "dates": [
            {"date": "2026-02-10", "average": 2.0, "clickCount": 6},
            {"date": "2026-02-11", "average": 5.0, "clickCount": 4},
        ]
    });
    let r2 = json!({
        "average": 5.0, "clickCount": 10,
        "dates": [
            {"date": "2026-02-10", "average": 4.0, "clickCount": 4},
        ]
    });

    let merged = merge_weighted_avg(&[r1, r2], "average", "clickCount");

    // Overall: (30 + 50) / 20 = 4.0
    let avg = merged["average"].as_f64().unwrap();
    assert!(
        (avg - 4.0).abs() < 0.01,
        "overall avg should be 4.0, got {}",
        avg
    );
    assert_eq!(merged["clickCount"], 20);

    let dates = merged["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 2);

    // Feb 10: (2*6 + 4*4) / (6+4) = (12+16)/10 = 2.8
    let feb10 = dates.iter().find(|d| d["date"] == "2026-02-10").unwrap();
    let a10 = feb10["average"].as_f64().unwrap();
    assert!(
        (a10 - 2.8).abs() < 0.01,
        "feb10 avg should be 2.8, got {}",
        a10
    );
    assert_eq!(feb10["clickCount"], 10);

    // Feb 11: 5*4/4 = 5.0 (only from r1)
    let feb11 = dates.iter().find(|d| d["date"] == "2026-02-11").unwrap();
    let a11 = feb11["average"].as_f64().unwrap();
    assert!(
        (a11 - 5.0).abs() < 0.01,
        "feb11 avg should be 5.0, got {}",
        a11
    );
    assert_eq!(feb11["clickCount"], 4);
}

// Routing: hits uses ("hits", "hit") key mapping
#[test]
fn test_merge_results_hits_routing() {
    let r1 = json!({"hits": [{"hit": "obj123", "count": 10}]});
    let r2 = json!({"hits": [{"hit": "obj123", "count": 5}, {"hit": "obj456", "count": 3}]});
    let merged = merge_results("hits", &[r1, r2], 10);
    let hits = merged["hits"].as_array().unwrap();
    let obj123 = hits.iter().find(|h| h["hit"] == "obj123").unwrap();
    assert_eq!(obj123["count"], 15);
    assert_eq!(hits.len(), 2);
}

// Routing: filters uses ("filters", "attribute") key mapping
#[test]
fn test_merge_results_filters_routing() {
    let r1 = json!({"filters": [{"attribute": "brand:Apple", "count": 100}]});
    let r2 = json!({"filters": [{"attribute": "brand:Apple", "count": 50}, {"attribute": "color:red", "count": 30}]});
    let merged = merge_results("filters", &[r1, r2], 10);
    let filters = merged["filters"].as_array().unwrap();
    let apple = filters
        .iter()
        .find(|f| f["attribute"] == "brand:Apple")
        .unwrap();
    assert_eq!(apple["count"], 150);
    assert_eq!(filters.len(), 2);
}

// Routing: filters/noResults uses same ("filters", "attribute") mapping
#[test]
fn test_merge_results_filters_no_results_routing() {
    let r1 = json!({"filters": [{"attribute": "size:XXL", "count": 20}]});
    let r2 = json!({"filters": [{"attribute": "size:XXL", "count": 10}]});
    let merged = merge_results("filters/noResults", &[r1, r2], 10);
    assert_eq!(merged["filters"][0]["attribute"], "size:XXL");
    assert_eq!(merged["filters"][0]["count"], 30);
}

// Routing: users/count dispatches to UserCountHll
#[test]
fn test_merge_results_users_count_routing() {
    // Without HLL sketches, falls back to summing counts
    let r1 = json!({"count": 100, "dates": [{"date": "2026-02-10", "count": 100}]});
    let r2 = json!({"count": 200, "dates": [{"date": "2026-02-10", "count": 200}]});
    let merged = merge_results("users/count", &[r1, r2], 100);
    assert_eq!(merged["count"], 300);
    let dates = merged["dates"].as_array().unwrap();
    // Without daily sketches, dates come from daily_sketches map (empty) not from fallback
    // So dates will be empty — this verifies the routing hit merge_user_counts, not merge_count_with_daily
    // (merge_count_with_daily would return dates with summed counts)
    assert_eq!(dates.len(), 0, "users/count should route to merge_user_counts, which doesn't merge date arrays without sketches");
}

// Routing: searches/noResults and searches/noClicks go through TopK
#[test]
fn test_merge_results_no_results_and_no_clicks_routing() {
    let r1 = json!({"searches": [{"search": "iphone", "count": 10}]});
    let r2 = json!({"searches": [{"search": "iphone", "count": 5}]});
    let merged = merge_results("searches/noResults", &[r1, r2], 10);
    assert_eq!(merged["searches"][0]["count"], 15);
    assert_eq!(merged["searches"][0]["search"], "iphone");

    let r1 = json!({"searches": [{"search": "galaxy", "count": 20}]});
    let r2 = json!({"searches": [{"search": "galaxy", "count": 8}]});
    let merged = merge_results("searches/noClicks", &[r1, r2], 10);
    assert_eq!(merged["searches"][0]["count"], 28);
    assert_eq!(merged["searches"][0]["search"], "galaxy");
}

// Verify geo merge through merge_results recomputes total
#[test]
fn test_merge_results_geo_recomputes_total() {
    let r1 = json!({"countries": [{"country": "US", "count": 200}], "total": 200});
    let r2 = json!({"countries": [{"country": "US", "count": 100}, {"country": "DE", "count": 50}], "total": 150});
    let merged = merge_results("geo", &[r1, r2], 100);
    // total = 300 (US) + 50 (DE) = 350, NOT 200 (from first node)
    assert_eq!(merged["total"], 350);
}

// Regression: nbHits must be preserved when first node lacks it but second has it
/// Regression: verify that `nbHits` is preserved when the first node lacks the field but a subsequent node includes it.
#[test]
fn test_merge_top_k_nb_hits_first_node_missing() {
    let r1 = json!({"searches": [
        {"search": "iphone", "count": 50},
    ]});
    let r2 = json!({"searches": [
        {"search": "iphone", "count": 30, "nbHits": 200},
    ]});

    let merged = merge_top_k(&[r1, r2], "searches", "search", 10);
    let searches = merged["searches"].as_array().unwrap();
    let iphone = searches.iter().find(|s| s["search"] == "iphone").unwrap();
    assert_eq!(iphone["count"], 80);
    // nbHits must be present even though first node lacked it
    assert_eq!(
        iphone["nbHits"], 200,
        "nbHits from second node must not be dropped"
    );
}

// Daily HLL sketch merge path coverage
/// Verify that daily HLL sketches are merged per date, producing accurate per-day cardinality estimates and chronologically sorted output.
#[test]
fn test_merge_user_counts_daily_sketches() {
    use super::super::hll::HllSketch;

    // Day 1: node A has users 0-999, node B has users 500-1499 -> true unique = 1500
    // Day 2: node A has users 0-499 only -> true unique = 500
    let day1_a: Vec<String> = (0..1000).map(|i| format!("user_{}", i)).collect();
    let day1_b: Vec<String> = (500..1500).map(|i| format!("user_{}", i)).collect();
    let day2_a: Vec<String> = (0..500).map(|i| format!("user_{}", i)).collect();

    let sketch_a = HllSketch::from_items(day1_a.iter().chain(day2_a.iter()).map(|s| s.as_str()));
    let sketch_b = HllSketch::from_items(day1_b.iter().map(|s| s.as_str()));

    let s_day1_a = HllSketch::from_items(day1_a.iter().map(|s| s.as_str()));
    let s_day1_b = HllSketch::from_items(day1_b.iter().map(|s| s.as_str()));
    let s_day2_a = HllSketch::from_items(day2_a.iter().map(|s| s.as_str()));

    let r1 = json!({
        "count": 1500,
        "hll_sketch": sketch_a.to_base64(),
        "dates": [],
        "daily_sketches": {
            "2026-02-10": s_day1_a.to_base64(),
            "2026-02-11": s_day2_a.to_base64(),
        }
    });
    let r2 = json!({
        "count": 1000,
        "hll_sketch": sketch_b.to_base64(),
        "dates": [],
        "daily_sketches": {
            "2026-02-10": s_day1_b.to_base64(),
        }
    });

    let merged = merge_user_counts(&[r1, r2]);

    // Daily dates should be present and sorted
    let dates = merged["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 2, "should have 2 daily entries");
    assert_eq!(dates[0]["date"], "2026-02-10");
    assert_eq!(dates[1]["date"], "2026-02-11");

    // Day 1: true unique ~1500, HLL should be within 5%
    let day1_count = dates[0]["count"].as_u64().unwrap();
    let day1_error = (day1_count as f64 - 1500.0).abs() / 1500.0;
    assert!(
        day1_error < 0.05,
        "day1 count {} has {}% error (expected ~1500)",
        day1_count,
        day1_error * 100.0
    );

    // Day 2: true unique ~500, only from node A
    let day2_count = dates[1]["count"].as_u64().unwrap();
    let day2_error = (day2_count as f64 - 500.0).abs() / 500.0;
    assert!(
        day2_error < 0.05,
        "day2 count {} has {}% error (expected ~500)",
        day2_count,
        day2_error * 100.0
    );
}

// MergeStrategy::None: status and unknown endpoints return first result unchanged
/// Verify that endpoints mapped to `MergeStrategy::None` (including unknown endpoints) return the first result unchanged.
#[test]
fn test_merge_results_none_strategy_passthrough() {
    let r1 = json!({"status": "ok", "node": "node-1", "extra": 42});
    let r2 = json!({"status": "ok", "node": "node-2", "extra": 99});
    // "status" endpoint -> MergeStrategy::None
    let merged = merge_results("status", &[r1.clone(), r2], 100);
    assert_eq!(
        merged, r1,
        "MergeStrategy::None should return first result unchanged"
    );

    // Unknown endpoint also -> MergeStrategy::None
    let r1 = json!({"foo": "bar"});
    let r2 = json!({"foo": "baz"});
    let merged = merge_results("totally/unknown/endpoint", &[r1.clone(), r2], 100);
    assert_eq!(
        merged, r1,
        "unknown endpoint should return first result unchanged"
    );
}

// ── Revenue merge tests ──────────────────────────────────────────────

/// Verify that single-currency revenue is summed correctly across nodes for both overall totals and daily breakdowns.
#[test]
fn test_merge_currency_revenue_single_currency() {
    let r1 = json!({
        "currencies": { "USD": { "currency": "USD", "revenue": 100.0 } },
        "dates": [{ "date": "2026-02-20", "currencies": { "USD": { "currency": "USD", "revenue": 60.0 } } },
                  { "date": "2026-02-21", "currencies": { "USD": { "currency": "USD", "revenue": 40.0 } } }]
    });
    let r2 = json!({
        "currencies": { "USD": { "currency": "USD", "revenue": 50.0 } },
        "dates": [{ "date": "2026-02-20", "currencies": { "USD": { "currency": "USD", "revenue": 30.0 } } },
                  { "date": "2026-02-21", "currencies": { "USD": { "currency": "USD", "revenue": 20.0 } } }]
    });
    let merged = merge_results("conversions/revenue", &[r1, r2], 100);
    let usd = &merged["currencies"]["USD"];
    assert_eq!(usd["currency"], "USD");
    assert!((usd["revenue"].as_f64().unwrap() - 150.0).abs() < 0.01);

    let dates = merged["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 2);
    let day0_usd = &dates[0]["currencies"]["USD"];
    assert!((day0_usd["revenue"].as_f64().unwrap() - 90.0).abs() < 0.01);
    let day1_usd = &dates[1]["currencies"]["USD"];
    assert!((day1_usd["revenue"].as_f64().unwrap() - 60.0).abs() < 0.01);
}

/// Verify that revenue merging handles multiple currencies (USD, EUR, GBP) across nodes, summing per-currency totals and daily breakdowns independently.
#[test]
fn test_merge_currency_revenue_multi_currency() {
    let r1 = json!({
        "currencies": {
            "USD": { "currency": "USD", "revenue": 100.0 },
            "EUR": { "currency": "EUR", "revenue": 50.0 }
        },
        "dates": [{ "date": "2026-02-20", "currencies": {
            "USD": { "currency": "USD", "revenue": 100.0 },
            "EUR": { "currency": "EUR", "revenue": 50.0 }
        }}]
    });
    let r2 = json!({
        "currencies": {
            "EUR": { "currency": "EUR", "revenue": 30.0 },
            "GBP": { "currency": "GBP", "revenue": 20.0 }
        },
        "dates": [{ "date": "2026-02-20", "currencies": {
            "EUR": { "currency": "EUR", "revenue": 30.0 },
            "GBP": { "currency": "GBP", "revenue": 20.0 }
        }}]
    });
    let merged = merge_results("conversions/revenue", &[r1, r2], 100);
    let currencies = merged["currencies"].as_object().unwrap();
    assert_eq!(currencies.len(), 3, "Should merge USD, EUR, GBP");
    assert!((currencies["USD"]["revenue"].as_f64().unwrap() - 100.0).abs() < 0.01);
    assert!((currencies["EUR"]["revenue"].as_f64().unwrap() - 80.0).abs() < 0.01);
    assert!((currencies["GBP"]["revenue"].as_f64().unwrap() - 20.0).abs() < 0.01);

    let dates = merged["dates"].as_array().unwrap();
    let day_currencies = dates[0]["currencies"].as_object().unwrap();
    assert_eq!(day_currencies.len(), 3);
}

#[test]
fn test_merge_currency_revenue_empty() {
    let r1 = json!({ "currencies": {}, "dates": [] });
    let r2 = json!({ "currencies": {}, "dates": [] });
    let merged = merge_results("conversions/revenue", &[r1, r2], 100);
    assert!(merged["currencies"].as_object().unwrap().is_empty());
    assert!(merged["dates"].as_array().unwrap().is_empty());
}

// ── Countries merge tests ────────────────────────────────────────────

#[test]
fn test_merge_results_countries_routing() {
    let r1 =
        json!({"countries": [{"country": "US", "count": 100}, {"country": "DE", "count": 30}]});
    let r2 = json!({"countries": [{"country": "US", "count": 50}, {"country": "FR", "count": 20}]});
    let merged = merge_results("countries", &[r1, r2], 100);
    let countries = merged["countries"].as_array().unwrap();
    assert_eq!(countries.len(), 3);
    assert_eq!(countries[0]["country"], "US");
    assert_eq!(countries[0]["count"], 150);
    // No "total" field in merge output for countries
    // (merge_category_counts preserves "total" only if present in input)
    assert!(merged.get("total").is_none());
}
