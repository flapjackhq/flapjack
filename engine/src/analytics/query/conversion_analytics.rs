use super::*;

impl super::AnalyticsQueryEngine {
    pub async fn conversion_rate(
        &self,
        index_name: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<serde_json::Value, String> {
        self.conversion_rate_with_optional_subtype(
            index_name,
            start_date,
            end_date,
            None,
            "conversionCount",
        )
        .await
    }

    /// Conversion rate filtered by a conversion event subtype.
    ///
    /// Accepted subtypes: `addToCart`, `purchase`.
    pub async fn conversion_rate_for_subtype(
        &self,
        index_name: &str,
        start_date: &str,
        end_date: &str,
        event_subtype: &str,
    ) -> Result<serde_json::Value, String> {
        let count_field_name = match event_subtype {
            "addToCart" => "addToCartCount",
            "purchase" => "purchaseCount",
            _ => return Err("eventSubtype must be addToCart or purchase".to_string()),
        };
        self.conversion_rate_with_optional_subtype(
            index_name,
            start_date,
            end_date,
            Some(event_subtype),
            count_field_name,
        )
        .await
    }

    /// Compute conversion rate with daily breakdown, optionally filtered by event subtype (e.g. `addToCart`, `purchase`). Cross-references tracked searches against conversion events and labels the count field with the caller-supplied `count_field_name`.
    async fn conversion_rate_with_optional_subtype(
        &self,
        index_name: &str,
        start_date: &str,
        end_date: &str,
        event_subtype: Option<&str>,
        count_field_name: &str,
    ) -> Result<serde_json::Value, String> {
        let start_ms = date_to_start_ms(start_date)?;
        let end_ms = date_to_end_ms(end_date)?;

        // Get tracked search count + daily
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

        let subtype_filter = event_subtype
            .map(|s| format!(" AND event_subtype = '{}'", sanitize_sql_eq(s)))
            .unwrap_or_default();

        // Get conversion count + daily
        let conv_sql = format!(
            "SELECT COUNT(*) as count FROM events \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} AND event_type = 'conversion'{}",
            start_ms, end_ms, subtype_filter
        );
        let conversion_count = match ctx.sql(&conv_sql).await {
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

        let daily_conv_sql = format!(
            "SELECT CAST(timestamp_ms / 86400000 * 86400000 AS BIGINT) as day_ms, \
             COUNT(*) as count \
             FROM events \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} AND event_type = 'conversion'{} \
             GROUP BY day_ms ORDER BY day_ms",
            start_ms, end_ms, subtype_filter
        );
        let daily_convs: std::collections::HashMap<i64, i64> = match ctx.sql(&daily_conv_sql).await
        {
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
                let convs = daily_convs.get(&ms).copied().unwrap_or(0);

                let mut day_entry = serde_json::Map::new();
                day_entry.insert("date".to_string(), serde_json::json!(ms_to_date_string(ms)));
                day_entry.insert("rate".to_string(), rounded_rate_or_null(convs, tracked));
                day_entry.insert(count_field_name.to_string(), serde_json::json!(convs));
                day_entry.insert("trackedSearchCount".to_string(), serde_json::json!(tracked));

                Some(serde_json::Value::Object(day_entry))
            })
            .collect();

        let mut result = serde_json::Map::new();
        result.insert(
            "rate".to_string(),
            rounded_rate_or_null(conversion_count, tracked_searches),
        );
        result.insert(
            count_field_name.to_string(),
            serde_json::json!(conversion_count),
        );
        result.insert(
            "trackedSearchCount".to_string(),
            serde_json::json!(tracked_searches),
        );
        result.insert("dates".to_string(), serde_json::json!(dates));

        Ok(serde_json::Value::Object(result))
    }

    /// Revenue from purchase conversion events, grouped by currency.
    ///
    /// Returns `{ "currencies": { "USD": { "currency": "USD", "revenue": 123.45 }, ... },
    ///            "dates": [{ "date": "YYYY-MM-DD", "currencies": { ... } }, ...] }`
    pub async fn revenue(
        &self,
        index_name: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<serde_json::Value, String> {
        let start_ms = date_to_start_ms(start_date)?;
        let end_ms = date_to_end_ms(end_date)?;

        let events_ctx = self.create_session_with_events(index_name).await?;

        // Aggregate totals by currency
        let agg_sql = format!(
            "SELECT currency, SUM(value) as revenue \
             FROM events \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} \
               AND event_type = 'conversion' AND event_subtype = 'purchase' \
               AND value IS NOT NULL AND currency IS NOT NULL \
             GROUP BY currency",
            start_ms, end_ms
        );
        let agg_rows = match events_ctx.sql(&agg_sql).await {
            Ok(df) => {
                let batches = df
                    .collect()
                    .await
                    .map_err(|e| format!("Exec error: {}", e))?;
                batches_to_json(&batches)?
            }
            Err(_) => Vec::new(),
        };

        let mut currencies = serde_json::Map::new();
        for row in &agg_rows {
            if let (Some(currency), Some(revenue)) = (
                row.get("currency").and_then(|v| v.as_str()),
                row.get("revenue").and_then(|v| v.as_f64()),
            ) {
                currencies.insert(
                    currency.to_string(),
                    serde_json::json!({ "currency": currency, "revenue": revenue }),
                );
            }
        }

        // Daily breakdown by (day, currency)
        let daily_sql = format!(
            "SELECT CAST(timestamp_ms / 86400000 * 86400000 AS BIGINT) as day_ms, \
             currency, SUM(value) as revenue \
             FROM events \
             WHERE timestamp_ms >= {} AND timestamp_ms <= {} \
               AND event_type = 'conversion' AND event_subtype = 'purchase' \
               AND value IS NOT NULL AND currency IS NOT NULL \
             GROUP BY day_ms, currency \
             ORDER BY day_ms",
            start_ms, end_ms
        );
        let daily_rows = match events_ctx.sql(&daily_sql).await {
            Ok(df) => {
                let batches = df
                    .collect()
                    .await
                    .map_err(|e| format!("Exec error: {}", e))?;
                batches_to_json(&batches)?
            }
            Err(_) => Vec::new(),
        };

        // Group daily rows by day_ms
        let mut daily_map: std::collections::BTreeMap<
            i64,
            serde_json::Map<String, serde_json::Value>,
        > = std::collections::BTreeMap::new();
        for row in &daily_rows {
            if let (Some(day_ms), Some(currency), Some(revenue)) = (
                row.get("day_ms").and_then(|v| v.as_i64()),
                row.get("currency").and_then(|v| v.as_str()),
                row.get("revenue").and_then(|v| v.as_f64()),
            ) {
                let day_currencies = daily_map.entry(day_ms).or_default();
                day_currencies.insert(
                    currency.to_string(),
                    serde_json::json!({ "currency": currency, "revenue": revenue }),
                );
            }
        }

        let dates: Vec<serde_json::Value> = daily_map
            .into_iter()
            .map(|(day_ms, day_currencies)| {
                serde_json::json!({
                    "date": ms_to_date_string(day_ms),
                    "currencies": serde_json::Value::Object(day_currencies),
                })
            })
            .collect();

        Ok(serde_json::json!({
            "currencies": serde_json::Value::Object(currencies),
            "dates": dates,
        }))
    }
}
