//! Click-through rate and position analytics queries for search events.
use super::*;

impl super::AnalyticsQueryEngine {
    /// Calculate the click-through rate for an index over a date range with daily breakdown.
    ///
    /// Queries tracked searches (those with query_id) from the searches table and click events from the events table for the specified date window. Returns aggregate CTR, total click and search counts, and daily breakdown records.
    ///
    /// # Arguments
    ///
    /// * `index_name` - Index name
    /// * `start_date` - Start date
    /// * `end_date` - End date
    ///
    /// # Returns
    ///
    /// JSON object containing:
    /// - `rate`: aggregate CTR (clicks / tracked searches), or null if no tracked searches
    /// - `clickCount`: total clicks in period
    /// - `trackedSearchCount`: total searches with query IDs in period
    /// - `dates`: daily breakdown array, each with `date`, `rate`, `clickCount`, `trackedSearchCount`
    ///
    /// # Errors
    ///
    /// Returns `Err` on date parsing failure or SQL execution failure.
    pub async fn click_through_rate(
        &self,
        index_name: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<serde_json::Value, String> {
        let start_ms = date_to_start_ms(start_date)?;
        let end_ms = date_to_end_ms(end_date)?;

        // Get tracked search count (searches with queryID)
        let ctx = self
            .create_session_with_searches_and_events(index_name)
            .await?;
        let search_sql = format!(
            "SELECT COUNT(*) as count FROM searches \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} AND query_id IS NOT NULL",
            start_ms, end_ms
        );
        let df = ctx
            .sql(&search_sql)
            .await
            .map_err(|e| format!("SQL error: {}", e))?;
        let batches = df
            .collect()
            .await
            .map_err(|e| format!("Exec error: {}", e))?;
        let tracked_searches = batches_to_json(&batches)?
            .first()
            .and_then(|r| r.get("count"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        // Daily tracked searches
        let daily_search_sql = format!(
            "SELECT CAST(timestamp_ms / 86400000 * 86400000 AS BIGINT) as day_ms, \
             COUNT(*) as count \
             FROM searches \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} AND query_id IS NOT NULL \
             GROUP BY day_ms ORDER BY day_ms",
            start_ms, end_ms
        );
        let df = ctx
            .sql(&daily_search_sql)
            .await
            .map_err(|e| format!("SQL error: {}", e))?;
        let batches = df
            .collect()
            .await
            .map_err(|e| format!("Exec error: {}", e))?;
        let daily_searches = batches_to_json(&batches)?;

        // Get click count + daily clicks
        let click_sql = format!(
            "SELECT COUNT(*) as count FROM events \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} AND event_type = 'click'",
            start_ms, end_ms
        );
        let click_count = match ctx.sql(&click_sql).await {
            Ok(df) => {
                let batches = df
                    .collect()
                    .await
                    .map_err(|e| format!("Exec error: {}", e))?;
                batches_to_json(&batches)?
                    .first()
                    .and_then(|r| r.get("count"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
            }
            Err(_) => 0,
        };

        let daily_click_sql = format!(
            "SELECT CAST(timestamp_ms / 86400000 * 86400000 AS BIGINT) as day_ms, \
             COUNT(*) as count \
             FROM events \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} AND event_type = 'click' \
             GROUP BY day_ms ORDER BY day_ms",
            start_ms, end_ms
        );
        let daily_clicks: std::collections::HashMap<i64, i64> =
            match ctx.sql(&daily_click_sql).await {
                Ok(df) => {
                    let batches = df
                        .collect()
                        .await
                        .map_err(|e| format!("Exec error: {}", e))?;
                    batches_to_json(&batches)?
                        .iter()
                        .filter_map(|r| {
                            let ms = r.get("day_ms")?.as_i64()?;
                            let c = r.get("count")?.as_i64()?;
                            Some((ms, c))
                        })
                        .collect()
                }
                Err(_) => std::collections::HashMap::new(),
            };

        let dates: Vec<serde_json::Value> = daily_searches
            .iter()
            .filter_map(|row| {
                let ms = row.get("day_ms")?.as_i64()?;
                let tracked = row.get("count")?.as_i64()?;
                let clicks = daily_clicks.get(&ms).copied().unwrap_or(0);
                Some(serde_json::json!({
                    "date": ms_to_date_string(ms),
                    "rate": rounded_rate_or_null(clicks, tracked),
                    "clickCount": clicks,
                    "trackedSearchCount": tracked
                }))
            })
            .collect();

        Ok(serde_json::json!({
            "rate": rounded_rate_or_null(click_count, tracked_searches),
            "clickCount": click_count,
            "trackedSearchCount": tracked_searches,
            "dates": dates
        }))
    }

    /// Average click position with daily breakdown.
    pub async fn average_click_position(
        &self,
        index_name: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<serde_json::Value, String> {
        let events_ctx = self.create_session_with_events(index_name).await?;
        let start_ms = date_to_start_ms(start_date)?;
        let end_ms = date_to_end_ms(end_date)?;

        // Read raw position data and compute in Rust (positions is JSON array)
        let sql = format!(
            "SELECT positions, timestamp_ms \
             FROM events \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} \
               AND event_type = 'click' AND positions IS NOT NULL",
            start_ms, end_ms
        );

        match events_ctx.sql(&sql).await {
            Ok(df) => {
                let batches = df
                    .collect()
                    .await
                    .map_err(|e| format!("Exec error: {}", e))?;
                let rows = batches_to_json(&batches)?;

                let mut total_sum: f64 = 0.0;
                let mut total_count: i64 = 0;
                let mut daily: std::collections::BTreeMap<i64, (f64, i64)> =
                    std::collections::BTreeMap::new();

                for row in &rows {
                    let pos_str = row
                        .get("positions")
                        .and_then(|v| v.as_str())
                        .unwrap_or("[]");
                    let ts = row
                        .get("timestamp_ms")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let day_ms = ts / 86400000 * 86400000;
                    let positions: Vec<f64> = serde_json::from_str(pos_str).unwrap_or_default();
                    for &p in &positions {
                        total_sum += p;
                        total_count += 1;
                        let entry = daily.entry(day_ms).or_insert((0.0, 0));
                        entry.0 += p;
                        entry.1 += 1;
                    }
                }

                let avg = if total_count > 0 {
                    total_sum / total_count as f64
                } else {
                    0.0
                };

                let dates: Vec<serde_json::Value> = daily
                    .iter()
                    .map(|(&ms, &(sum, count))| {
                        let day_avg = if count > 0 { sum / count as f64 } else { 0.0 };
                        serde_json::json!({
                            "date": ms_to_date_string(ms),
                            "average": (day_avg * 10.0).round() / 10.0,
                            "clickCount": count
                        })
                    })
                    .collect();

                Ok(serde_json::json!({
                    "average": (avg * 10.0).round() / 10.0,
                    "clickCount": total_count,
                    "dates": dates
                }))
            }
            Err(_) => Ok(serde_json::json!({
                "average": 0,
                "clickCount": 0,
                "dates": []
            })),
        }
    }

    /// Click position distribution histogram (Algolia-style buckets).
    pub async fn click_positions(
        &self,
        index_name: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<serde_json::Value, String> {
        let events_ctx = self.create_session_with_events(index_name).await?;
        let start_ms = date_to_start_ms(start_date)?;
        let end_ms = date_to_end_ms(end_date)?;

        let sql = format!(
            "SELECT positions FROM events \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} \
               AND event_type = 'click' AND positions IS NOT NULL",
            start_ms, end_ms
        );

        // Algolia-style position buckets
        let buckets: Vec<(i32, i32)> =
            vec![(1, 1), (2, 2), (3, 4), (5, 8), (9, 16), (17, 20), (21, -1)];
        let mut bucket_counts: Vec<i64> = vec![0; buckets.len()];
        let mut total_clicks: i64 = 0;

        if let Ok(df) = events_ctx.sql(&sql).await {
            let batches = df
                .collect()
                .await
                .map_err(|e| format!("Exec error: {}", e))?;
            let rows = batches_to_json(&batches)?;

            for row in &rows {
                let pos_str = row
                    .get("positions")
                    .and_then(|v| v.as_str())
                    .unwrap_or("[]");
                let positions: Vec<i32> = serde_json::from_str(pos_str).unwrap_or_default();
                for &p in &positions {
                    total_clicks += 1;
                    for (i, &(lo, hi)) in buckets.iter().enumerate() {
                        if hi == -1 {
                            if p >= lo {
                                bucket_counts[i] += 1;
                            }
                        } else if p >= lo && p <= hi {
                            bucket_counts[i] += 1;
                        }
                    }
                }
            }
        }

        let positions: Vec<serde_json::Value> = buckets
            .iter()
            .zip(bucket_counts.iter())
            .map(|(&(lo, hi), &count)| {
                serde_json::json!({
                    "position": [lo, hi],
                    "clickCount": count
                })
            })
            .collect();

        Ok(serde_json::json!({
            "positions": positions,
            "clickCount": total_clicks
        }))
    }

    /// Top clicked object IDs ranked by click count.
    pub async fn top_hits(
        &self,
        index_name: &str,
        start_date: &str,
        end_date: &str,
        limit: usize,
    ) -> Result<serde_json::Value, String> {
        let events_ctx = self.create_session_with_events(index_name).await?;
        let start_ms = date_to_start_ms(start_date)?;
        let end_ms = date_to_end_ms(end_date)?;

        let sql = format!(
            "SELECT object_ids as hit, COUNT(*) as count \
             FROM events \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} AND event_type = 'click' \
             GROUP BY object_ids \
             ORDER BY count DESC \
             LIMIT {}",
            start_ms, end_ms, limit
        );

        match events_ctx.sql(&sql).await {
            Ok(df) => {
                let batches = df
                    .collect()
                    .await
                    .map_err(|e| format!("Exec error: {}", e))?;
                let rows = batches_to_json(&batches)?;
                Ok(serde_json::json!({"hits": rows}))
            }
            Err(_) => Ok(serde_json::json!({"hits": []})),
        }
    }

    /// Return click counts for a set of object IDs on one index.
    ///
    /// Uses all-time click counts (no date window) for Stage 5 MVP reranking.
    pub async fn get_click_counts_for_objects(
        &self,
        index_name: &str,
        object_ids: &[String],
    ) -> Result<HashMap<String, u64>, String> {
        let mut counts: HashMap<String, u64> = HashMap::new();
        if object_ids.is_empty() {
            return Ok(counts);
        }

        let target_ids: HashSet<&str> = object_ids.iter().map(|id| id.as_str()).collect();
        let events_ctx = self.create_session_with_events(index_name).await?;
        let sql = "SELECT object_ids, COUNT(*) as count \
                   FROM events \
                   WHERE event_type = 'click' \
                   GROUP BY object_ids";
        let df = events_ctx
            .sql(sql)
            .await
            .map_err(|e| format!("SQL error: {}", e))?;
        let batches = df
            .collect()
            .await
            .map_err(|e| format!("Exec error: {}", e))?;
        let rows = batches_to_json(&batches)?;

        for row in rows {
            let object_ids_json = match row.get("object_ids").and_then(|v| v.as_str()) {
                Some(value) => value,
                None => continue,
            };
            let row_count = row
                .get("count")
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_i64().and_then(|raw| u64::try_from(raw).ok()))
                })
                .unwrap_or(0);
            if row_count == 0 {
                continue;
            }

            let parsed_ids: Vec<String> = match serde_json::from_str(object_ids_json) {
                Ok(ids) => ids,
                Err(_) => continue,
            };
            for object_id in parsed_ids {
                if target_ids.contains(object_id.as_str()) {
                    *counts.entry(object_id).or_insert(0) += row_count;
                }
            }
        }

        Ok(counts)
    }
}
