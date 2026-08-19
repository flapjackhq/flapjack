//! Tests for the analytics merge module, covering each merge strategy (top-K, rates, weighted averages, histograms, category counts, HLL user counts, currency revenue, overview) and verifying correct endpoint-to-strategy routing.

use super::hll::HllSketch;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Sort a Vec of JSON objects by their "date" string field (chronological).
fn sort_by_date(dates: &mut [Value]) {
    dates.sort_by(|a, b| {
        let da = a.get("date").and_then(|v| v.as_str()).unwrap_or("");
        let db = b.get("date").and_then(|v| v.as_str()).unwrap_or("");
        da.cmp(db)
    });
}

/// Merge top-K results by summing counts for the same key, then re-sorting.
/// Used by: searches, noResults, noClicks, hits, filters, filter_values, geo_top_searches.
///
/// Expects each input to be a JSON object with a results array field. The `results_key`
/// identifies which field holds the array (e.g., "searches", "hits", "filters").
/// Each result item must have a key field (e.g., "search", "hit", "attribute") and a "count" field.
pub fn merge_top_k(results: &[Value], results_key: &str, key_field: &str, limit: usize) -> Value {
    // Track the search count plus the nbHits numerator/denominator per key.
    // nbHits is an average in the public contract, so node averages must never
    // be added together. Query results carry the exact private components; the
    // avg*count fallback keeps older peer payloads weighted during upgrades.
    let mut counts: HashMap<String, (i64, i64, i64, bool, Value)> = HashMap::new();

    for result in results {
        if let Some(items) = result.get(results_key).and_then(|v| v.as_array()) {
            for item in items {
                let key = item
                    .get(key_field)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let count = item.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                let nb_hits = item.get("nbHits").and_then(|v| v.as_i64());
                let private_sum = item.get("_nbHitsSum").and_then(|v| v.as_i64());
                let private_count = item.get("_nbHitsCount").and_then(|v| v.as_i64());
                let (nb_hits_sum, nb_hits_count) = match (private_sum, private_count) {
                    (Some(sum), Some(denominator)) => (sum, denominator),
                    _ => nb_hits
                        .map(|average| (average.saturating_mul(count), count))
                        .unwrap_or((0, 0)),
                };

                let entry = counts
                    .entry(key)
                    .or_insert_with(|| (0, 0, 0, false, item.clone()));
                entry.0 += count;
                entry.1 += nb_hits_sum;
                entry.2 += nb_hits_count;
                entry.3 |= nb_hits.is_some() || private_sum.is_some();
            }
        }
    }

    // Build merged array, updating count and nbHits in each item
    let mut merged: Vec<Value> = counts
        .into_iter()
        .map(
            |(
                _key,
                (total_count, total_nb_hits, total_nb_hits_count, has_nb_hits, mut template),
            )| {
                if let Some(obj) = template.as_object_mut() {
                    obj.insert("count".to_string(), json!(total_count));
                    if has_nb_hits {
                        let average = if total_nb_hits_count > 0 {
                            total_nb_hits / total_nb_hits_count
                        } else {
                            0
                        };
                        obj.insert("nbHits".to_string(), json!(average));
                    }
                    obj.remove("_nbHitsSum");
                    obj.remove("_nbHitsCount");
                }
                template
            },
        )
        .collect();

    // Sort by count descending, then by the endpoint's logical key ascending.
    // HashMap iteration order must never become observable in tied rows.
    merged.sort_by(|a, b| {
        let ca = a.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        let cb = b.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        cb.cmp(&ca).then_with(|| {
            let ka = a.get(key_field).and_then(|v| v.as_str()).unwrap_or("");
            let kb = b.get(key_field).and_then(|v| v.as_str()).unwrap_or("");
            ka.cmp(kb)
        })
    });

    merged.truncate(limit);

    // Use the first result as template for other fields, replace the results array
    let mut base = results.first().cloned().unwrap_or(json!({}));
    if let Some(obj) = base.as_object_mut() {
        obj.insert(results_key.to_string(), json!(merged));
    }
    base
}

/// Merge count + daily breakdown by summing totals and per-date counts.
/// Used by: searches/count.
pub fn merge_count_with_daily(results: &[Value]) -> Value {
    let mut total: i64 = 0;
    let mut daily: HashMap<String, i64> = HashMap::new();

    for result in results {
        total += result.get("count").and_then(|v| v.as_i64()).unwrap_or(0);

        if let Some(dates) = result.get("dates").and_then(|v| v.as_array()) {
            for entry in dates {
                let date = entry
                    .get("date")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let count = entry.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                *daily.entry(date).or_insert(0) += count;
            }
        }
    }

    let mut dates: Vec<Value> = daily
        .into_iter()
        .map(|(date, count)| json!({"date": date, "count": count}))
        .collect();
    sort_by_date(&mut dates);

    json!({
        "count": total,
        "dates": dates,
    })
}

/// Merge rates by summing numerators and denominators separately, then dividing.
/// CRITICAL: Never average rates. Always sum components first.
/// Used by: noResultRate, noClickRate, CTR, conversionRate.
///
/// The caller must specify which JSON fields hold the numerator and denominator.
pub fn merge_rates(
    results: &[Value],
    numerator_field: &str,
    denominator_field: &str,
    rate_field: &str,
) -> Value {
    let mut total_num: i64 = 0;
    let mut total_den: i64 = 0;
    let mut daily_num: HashMap<String, i64> = HashMap::new();
    let mut daily_den: HashMap<String, i64> = HashMap::new();

    for result in results {
        total_num += result
            .get(numerator_field)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        total_den += result
            .get(denominator_field)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        if let Some(dates) = result.get("dates").and_then(|v| v.as_array()) {
            for entry in dates {
                let date = entry
                    .get("date")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let num = entry
                    .get(numerator_field)
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let den = entry
                    .get(denominator_field)
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                *daily_num.entry(date.clone()).or_insert(0) += num;
                *daily_den.entry(date).or_insert(0) += den;
            }
        }
    }

    let rate = if total_den > 0 {
        json!(total_num as f64 / total_den as f64)
    } else {
        Value::Null
    };

    let mut dates: Vec<Value> = daily_num
        .iter()
        .map(|(date, &num)| {
            let den = daily_den.get(date).copied().unwrap_or(0);
            let r = if den > 0 {
                json!(num as f64 / den as f64)
            } else {
                Value::Null
            };
            json!({
                "date": date,
                rate_field: r,
                numerator_field: num,
                denominator_field: den,
            })
        })
        .collect();
    sort_by_date(&mut dates);

    json!({
        rate_field: rate,
        numerator_field: total_num,
        denominator_field: total_den,
        "dates": dates,
    })
}

/// Merge weighted averages: sum(avg * count) / sum(count).
/// Used by: averageClickPosition.
pub fn merge_weighted_avg(results: &[Value], avg_field: &str, count_field: &str) -> Value {
    let mut total_sum: f64 = 0.0;
    let mut total_count: i64 = 0;
    let mut daily_sum: HashMap<String, f64> = HashMap::new();
    let mut daily_count: HashMap<String, i64> = HashMap::new();

    for result in results {
        let avg = result
            .get(avg_field)
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let count = result
            .get(count_field)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        total_sum += avg * count as f64;
        total_count += count;

        if let Some(dates) = result.get("dates").and_then(|v| v.as_array()) {
            for entry in dates {
                let date = entry
                    .get("date")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let a = entry.get(avg_field).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let c = entry.get(count_field).and_then(|v| v.as_i64()).unwrap_or(0);
                *daily_sum.entry(date.clone()).or_insert(0.0) += a * c as f64;
                *daily_count.entry(date).or_insert(0) += c;
            }
        }
    }

    let avg = if total_count > 0 {
        total_sum / total_count as f64
    } else {
        0.0
    };

    let mut dates: Vec<Value> = daily_sum
        .iter()
        .map(|(date, &sum)| {
            let count = daily_count.get(date).copied().unwrap_or(0);
            let a = if count > 0 { sum / count as f64 } else { 0.0 };
            json!({
                "date": date,
                avg_field: a,
                count_field: count,
            })
        })
        .collect();
    sort_by_date(&mut dates);

    json!({
        avg_field: avg,
        count_field: total_count,
        "dates": dates,
    })
}

/// Merge fixed-bucket histograms by summing each bucket.
/// Used by: clicks/positions.
pub fn merge_histogram(results: &[Value], buckets_key: &str) -> Value {
    // Bucket key is position range [lo, hi], value is clickCount
    let mut bucket_counts: HashMap<String, i64> = HashMap::new();
    let mut bucket_order: Vec<(String, Value)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for result in results {
        if let Some(buckets) = result.get(buckets_key).and_then(|v| v.as_array()) {
            for bucket in buckets {
                // Use the position array as key
                let position = bucket.get("position").cloned().unwrap_or(json!([]));
                let key = position.to_string();
                let count = bucket
                    .get("clickCount")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);

                if seen.insert(key.clone()) {
                    bucket_order.push((key.clone(), position));
                }
                *bucket_counts.entry(key).or_insert(0) += count;
            }
        }
    }

    let buckets: Vec<Value> = bucket_order
        .iter()
        .map(|(key, position)| {
            let count = bucket_counts.get(key).copied().unwrap_or(0);
            json!({
                "position": position,
                "clickCount": count,
            })
        })
        .collect();

    json!({ buckets_key: buckets })
}

/// Merge category counts by summing per category.
/// Used by: devices, geo, geo/regions.
///
/// `items_key` is the JSON array key (e.g. "platforms", "countries", "regions").
/// `name_field` is the category label field (e.g. "platform", "country", "region").
/// `count_field` is the count field (e.g. "count").
pub fn merge_category_counts(
    results: &[Value],
    items_key: &str,
    name_field: &str,
    count_field: &str,
) -> Value {
    let mut counts: HashMap<String, i64> = HashMap::new();

    for result in results {
        if let Some(items) = result.get(items_key).and_then(|v| v.as_array()) {
            for item in items {
                let name = item
                    .get(name_field)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let count = item.get(count_field).and_then(|v| v.as_i64()).unwrap_or(0);
                if !name.is_empty() {
                    *counts.entry(name).or_insert(0) += count;
                }
            }
        }
    }

    let mut items: Vec<Value> = counts
        .into_iter()
        .map(|(name, count)| json!({name_field: name, count_field: count}))
        .collect();
    items.sort_by(|a, b| {
        let ca = a.get(count_field).and_then(|v| v.as_i64()).unwrap_or(0);
        let cb = b.get(count_field).and_then(|v| v.as_i64()).unwrap_or(0);
        cb.cmp(&ca)
    });

    // Recompute "total" if present (e.g., geo endpoint sums all country counts)
    let recomputed_total: i64 = items
        .iter()
        .filter_map(|item| item.get(count_field).and_then(|v| v.as_i64()))
        .sum();

    // Preserve other top-level fields from the first result (e.g. "country" for regions)
    let mut base = results.first().cloned().unwrap_or(json!({}));
    if let Some(obj) = base.as_object_mut() {
        obj.insert(items_key.to_string(), json!(items));
        if obj.contains_key("total") {
            obj.insert("total".to_string(), json!(recomputed_total));
        }
    }
    base
}

/// Merge user count results using HLL sketches.
/// Each result should include an `hll_sketch` field (base64) when in cluster mode.
/// Falls back to summing counts if no sketches available.
pub fn merge_user_counts(results: &[Value]) -> Value {
    let mut sketches: Vec<HllSketch> = Vec::new();
    let mut daily_sketches: HashMap<String, Vec<HllSketch>> = HashMap::new();
    let mut fallback_count: i64 = 0;

    for result in results {
        // Try to get HLL sketch
        if let Some(sketch_b64) = result.get("hll_sketch").and_then(|v| v.as_str()) {
            if let Some(sketch) = HllSketch::from_base64(sketch_b64) {
                sketches.push(sketch);
            }
        } else {
            // Fallback: sum counts (less accurate, double-counts shared users)
            fallback_count += result.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
        }

        // Daily sketches
        if let Some(daily) = result.get("daily_sketches").and_then(|v| v.as_object()) {
            for (date, sketch_val) in daily {
                if let Some(b64) = sketch_val.as_str() {
                    if let Some(sketch) = HllSketch::from_base64(b64) {
                        daily_sketches.entry(date.clone()).or_default().push(sketch);
                    }
                }
            }
        }
    }

    let count = if !sketches.is_empty() {
        let merged = HllSketch::merge_all(&sketches);
        let hll_count = merged.cardinality() as i64;
        if fallback_count > 0 {
            tracing::warn!(
                "[HA-analytics] mixed HLL/non-HLL user counts: {} nodes with sketches, adding {} fallback count",
                sketches.len(),
                fallback_count
            );
            hll_count + fallback_count
        } else {
            hll_count
        }
    } else {
        fallback_count
    };

    let mut dates: Vec<Value> = daily_sketches
        .iter()
        .map(|(date, day_sketches)| {
            let merged = HllSketch::merge_all(day_sketches);
            json!({"date": date, "count": merged.cardinality()})
        })
        .collect();
    sort_by_date(&mut dates);

    json!({
        "count": count,
        "dates": dates,
    })
}

/// Merge revenue results by summing revenue per currency across peers.
/// Each result has `{ "currencies": { "USD": { "currency": "USD", "revenue": N }, ... }, "dates": [...] }`.
pub fn merge_currency_revenue(results: &[Value]) -> Value {
    let mut agg_currencies: HashMap<String, f64> = HashMap::new();
    let mut daily: HashMap<String, HashMap<String, f64>> = HashMap::new(); // date -> currency -> revenue

    for result in results {
        if let Some(currencies) = result.get("currencies").and_then(|v| v.as_object()) {
            for (code, entry) in currencies {
                let rev = entry.get("revenue").and_then(|v| v.as_f64()).unwrap_or(0.0);
                *agg_currencies.entry(code.clone()).or_insert(0.0) += rev;
            }
        }
        if let Some(dates) = result.get("dates").and_then(|v| v.as_array()) {
            for day in dates {
                let date = day
                    .get("date")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if date.is_empty() {
                    continue;
                }
                if let Some(day_currencies) = day.get("currencies").and_then(|v| v.as_object()) {
                    let day_map = daily.entry(date).or_default();
                    for (code, entry) in day_currencies {
                        let rev = entry.get("revenue").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        *day_map.entry(code.clone()).or_insert(0.0) += rev;
                    }
                }
            }
        }
    }

    let currencies_obj: serde_json::Map<String, Value> = agg_currencies
        .into_iter()
        .map(|(code, revenue)| {
            (
                code.clone(),
                json!({ "currency": code, "revenue": revenue }),
            )
        })
        .collect();

    let mut sorted_dates: Vec<_> = daily.into_iter().collect();
    sorted_dates.sort_by(|a, b| a.0.cmp(&b.0));

    let dates: Vec<Value> = sorted_dates
        .into_iter()
        .map(|(date, day_currencies)| {
            let day_obj: serde_json::Map<String, Value> = day_currencies
                .into_iter()
                .map(|(code, revenue)| {
                    (
                        code.clone(),
                        json!({ "currency": code, "revenue": revenue }),
                    )
                })
                .collect();
            json!({ "date": date, "currencies": Value::Object(day_obj) })
        })
        .collect();

    json!({
        "currencies": Value::Object(currencies_obj),
        "dates": dates,
    })
}

/// Apply the appropriate merge function based on the endpoint.
pub fn merge_results(endpoint: &str, results: &[Value], limit: usize) -> Value {
    use super::types::MergeStrategy;

    if results.is_empty() {
        return json!({});
    }
    if results.len() == 1 {
        return results[0].clone();
    }

    match super::types::merge_strategy_for_endpoint(endpoint) {
        MergeStrategy::TopK => {
            // Determine the results key and key field based on endpoint
            let (results_key, key_field) = match endpoint {
                "searches" | "searches/noResults" | "searches/noClicks" => ("searches", "search"),
                "hits" => ("hits", "hit"),
                "filters" | "filters/noResults" => ("filters", "attribute"),
                _ if endpoint.starts_with("filters/") => ("values", "value"),
                _ if endpoint.starts_with("geo/") => ("searches", "search"),
                _ => ("results", "key"),
            };
            merge_top_k(results, results_key, key_field, limit)
        }
        MergeStrategy::CountWithDaily => merge_count_with_daily(results),
        MergeStrategy::Rate => {
            // Determine numerator/denominator fields based on endpoint
            let (num, den, rate) = match endpoint {
                "searches/noResultRate" => ("noResults", "count", "rate"),
                "searches/noClickRate" => ("noClickCount", "trackedSearchCount", "rate"),
                "clicks/clickThroughRate" => ("clickCount", "trackedSearchCount", "rate"),
                "conversions/conversionRate" => ("conversionCount", "trackedSearchCount", "rate"),
                "conversions/addToCartRate" => ("addToCartCount", "trackedSearchCount", "rate"),
                "conversions/purchaseRate" => ("purchaseCount", "trackedSearchCount", "rate"),
                _ => ("numerator", "denominator", "rate"),
            };
            merge_rates(results, num, den, rate)
        }
        MergeStrategy::WeightedAvg => merge_weighted_avg(results, "average", "clickCount"),
        MergeStrategy::Histogram => merge_histogram(results, "positions"),
        MergeStrategy::CategoryCounts => {
            let (items_key, name_field, count_field) = match endpoint {
                "devices" => ("platforms", "platform", "count"),
                "geo" | "countries" => ("countries", "country", "count"),
                _ if endpoint.ends_with("/regions") => ("regions", "region", "count"),
                _ => ("items", "name", "count"),
            };
            merge_category_counts(results, items_key, name_field, count_field)
        }
        MergeStrategy::CurrencyRevenue => merge_currency_revenue(results),
        MergeStrategy::UserCountHll => merge_user_counts(results),
        MergeStrategy::Overview => {
            // Overview is a multi-index summary — merge each index's data
            merge_overview(results)
        }
        MergeStrategy::None => results[0].clone(),
    }
}

/// Merge overview results (multi-index summaries).
/// Sums totalSearches, uniqueUsers (approximate), merges indices by name,
/// and merges dates by summing counts per date.
/// Rates (noResultRate, clickThroughRate) are dropped since we lack components to recompute.
fn merge_overview(results: &[Value]) -> Value {
    let mut total_searches: i64 = 0;
    let mut total_users: i64 = 0;
    let mut indices: HashMap<String, (i64, i64)> = HashMap::new(); // (searches, noResults)
    let mut daily: HashMap<String, i64> = HashMap::new();

    for result in results {
        total_searches += result
            .get("totalSearches")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        total_users += result
            .get("uniqueUsers")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        if let Some(idx_array) = result.get("indices").and_then(|v| v.as_array()) {
            for idx in idx_array {
                let name = idx
                    .get("index")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let searches = idx.get("searches").and_then(|v| v.as_i64()).unwrap_or(0);
                let no_results = idx.get("noResults").and_then(|v| v.as_i64()).unwrap_or(0);
                let entry = indices.entry(name).or_insert((0, 0));
                entry.0 += searches;
                entry.1 += no_results;
            }
        }

        if let Some(dates) = result.get("dates").and_then(|v| v.as_array()) {
            for entry in dates {
                let date = entry
                    .get("date")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let count = entry.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
                *daily.entry(date).or_insert(0) += count;
            }
        }
    }

    let mut merged_indices: Vec<Value> = indices
        .into_iter()
        .map(|(name, (searches, no_results))| {
            json!({
                "index": name,
                "searches": searches,
                "noResults": no_results,
            })
        })
        .collect();
    merged_indices.sort_by(|a, b| {
        let sa = a.get("searches").and_then(|v| v.as_i64()).unwrap_or(0);
        let sb = b.get("searches").and_then(|v| v.as_i64()).unwrap_or(0);
        sb.cmp(&sa)
    });

    let mut dates: Vec<Value> = daily
        .into_iter()
        .map(|(date, count)| json!({"date": date, "count": count}))
        .collect();
    sort_by_date(&mut dates);

    json!({
        "totalSearches": total_searches,
        "uniqueUsers": total_users,
        "noResultRate": null,
        "clickThroughRate": null,
        "indices": merged_indices,
        "dates": dates,
    })
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
