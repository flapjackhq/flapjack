use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use super::AnalyticsQueryEngine;
use crate::analytics::schema::RECOMMENDATION_REQUEST_EVENT_TYPE;

#[derive(Debug, Clone, PartialEq)]
pub struct RecommendAnalyticsSummary {
    pub total_users: u64,
    pub total_recommendations: u64,
    pub tracked_recommendations: u64,
    pub clicked_recommendations: u64,
    pub converted_recommendations: u64,
    pub click_through_rate: f64,
    pub conversion_rate: f64,
    pub click_position_distribution: Vec<(u32, u64)>,
    pub average_click_position: Option<f64>,
}

impl RecommendAnalyticsSummary {
    fn empty() -> Self {
        Self {
            total_users: 0,
            total_recommendations: 0,
            tracked_recommendations: 0,
            clicked_recommendations: 0,
            converted_recommendations: 0,
            click_through_rate: 0.0,
            conversion_rate: 0.0,
            click_position_distribution: Vec::new(),
            average_click_position: None,
        }
    }
}

fn string_field<'a>(row: &'a Value, field: &str) -> Option<&'a str> {
    row.get(field).and_then(Value::as_str)
}

impl AnalyticsQueryEngine {
    /// Aggregate the closed PBV5 Recommend metrics for one physical index,
    /// model, and inclusive UTC-date window represented as millisecond bounds.
    pub async fn recommendation_analytics(
        &self,
        index_name: &str,
        model: &str,
        start_timestamp_ms: i64,
        end_timestamp_exclusive_ms: i64,
    ) -> Result<RecommendAnalyticsSummary, String> {
        let event_type = super::sql_string_literal(RECOMMENDATION_REQUEST_EVENT_TYPE);
        let sql = format!(
            "SELECT event_type, event_subtype, user_token, query_id, positions FROM events \
             WHERE timestamp_ms >= {start_timestamp_ms} AND timestamp_ms < {end_timestamp_exclusive_ms} \
             AND (event_type = {event_type} OR event_type = 'click' OR event_type = 'conversion')"
        );
        let rows = self.query_events(index_name, &sql).await?;
        if rows.is_empty() {
            return Ok(RecommendAnalyticsSummary::empty());
        }

        let mut total_recommendations = 0_u64;
        let mut users = HashSet::new();
        let mut tracked = HashMap::<String, String>::new();

        for row in &rows {
            if string_field(row, "event_type") != Some(RECOMMENDATION_REQUEST_EVENT_TYPE)
                || string_field(row, "event_subtype") != Some(model)
            {
                continue;
            }
            let Some(identity) = string_field(row, "user_token") else {
                continue;
            };
            total_recommendations += 1;
            users.insert(identity.to_string());
            if let Some(query_id) = string_field(row, "query_id") {
                tracked.insert(query_id.to_string(), identity.to_string());
            }
        }

        let mut clicked = HashSet::new();
        let mut converted = HashSet::new();
        let mut positions = BTreeMap::<u32, u64>::new();
        let mut position_sum = 0_u64;
        let mut position_count = 0_u64;

        for row in &rows {
            let Some(event_kind @ ("click" | "conversion")) = string_field(row, "event_type")
            else {
                continue;
            };
            let Some(query_id) = string_field(row, "query_id") else {
                continue;
            };
            let Some(expected_identity) = tracked.get(query_id) else {
                continue;
            };
            if string_field(row, "user_token") != Some(expected_identity.as_str()) {
                continue;
            }

            if event_kind == "conversion" {
                converted.insert(query_id.to_string());
                continue;
            }

            clicked.insert(query_id.to_string());
            if let Some(encoded) = string_field(row, "positions") {
                for position in serde_json::from_str::<Vec<u32>>(encoded).unwrap_or_default() {
                    if position == 0 {
                        continue;
                    }
                    *positions.entry(position).or_default() += 1;
                    position_sum += u64::from(position);
                    position_count += 1;
                }
            }
        }

        let tracked_recommendations = tracked.len() as u64;
        let rate = |numerator: usize| {
            if tracked_recommendations == 0 {
                0.0
            } else {
                100.0 * numerator as f64 / tracked_recommendations as f64
            }
        };

        Ok(RecommendAnalyticsSummary {
            total_users: users.len() as u64,
            total_recommendations,
            tracked_recommendations,
            clicked_recommendations: clicked.len() as u64,
            converted_recommendations: converted.len() as u64,
            click_through_rate: rate(clicked.len()),
            conversion_rate: rate(converted.len()),
            click_position_distribution: positions.into_iter().collect(),
            average_click_position: (position_count > 0)
                .then_some(position_sum as f64 / position_count as f64),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::schema::{InsightEvent, RECOMMENDATION_REQUEST_EVENT_TYPE};
    use crate::analytics::{retention, writer};
    use crate::analytics::{AnalyticsCollector, AnalyticsConfig};
    use chrono::{NaiveDate, TimeZone, Utc};
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    const INDEX: &str = "products";
    const RELATED: &str = "related-products";
    const TRENDING: &str = "trending-items";
    const USER_1: &str = "00000000-0000-4000-8000-000000000001";
    const USER_2: &str = "00000000-0000-4000-8000-000000000002";
    const USER_3: &str = "00000000-0000-4000-8000-000000000003";
    const QID_1: &str = "11111111111111111111111111111111";
    const QID_2: &str = "22222222222222222222222222222222";
    const SHARED_FIXTURE_BYTES: &[u8] =
        include_bytes!("fixtures/recommend_analytics_known_answer.json");
    const SHARED_FIXTURE_SHA256: &str =
        "b97a1c46e243beaa09b47ca10690c631e6ccd97aeb99a4d16914e024a552e63a";

    fn config(temp_dir: &TempDir) -> AnalyticsConfig {
        AnalyticsConfig {
            enabled: true,
            data_dir: temp_dir.path().join("analytics"),
            flush_interval_secs: 3_600,
            flush_size: 100_000,
            retention_days: 90,
        }
    }

    fn timestamp(date: &str, hour: u32) -> i64 {
        Utc.from_utc_datetime(
            &NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .unwrap()
                .and_hms_opt(hour, 0, 0)
                .unwrap(),
        )
        .timestamp_millis()
    }

    fn event(
        event_type: &str,
        index: &str,
        user_token: &str,
        query_id: Option<&str>,
        positions: Option<Vec<u32>>,
        timestamp_ms: i64,
    ) -> InsightEvent {
        let object_count = positions.as_ref().map_or(1, Vec::len);
        InsightEvent {
            event_type: event_type.to_string(),
            event_subtype: None,
            event_name: format!("Recommend {event_type}"),
            index: index.to_string(),
            user_token: user_token.to_string(),
            authenticated_user_token: None,
            query_id: query_id.map(str::to_string),
            object_ids: (0..object_count).map(|i| format!("object-{i}")).collect(),
            object_ids_alt: Vec::new(),
            positions,
            timestamp: Some(timestamp_ms),
            value: None,
            currency: None,
            interleaving_team: None,
        }
    }

    fn request_event(
        model: &str,
        user_token: &str,
        query_id: Option<&str>,
        timestamp_ms: i64,
    ) -> InsightEvent {
        InsightEvent {
            event_type: RECOMMENDATION_REQUEST_EVENT_TYPE.to_string(),
            event_subtype: Some(model.to_string()),
            event_name: "Recommend request".to_string(),
            index: INDEX.to_string(),
            user_token: user_token.to_string(),
            authenticated_user_token: None,
            query_id: query_id.map(str::to_string),
            object_ids: Vec::new(),
            object_ids_alt: Vec::new(),
            positions: None,
            timestamp: Some(timestamp_ms),
            value: None,
            currency: None,
            interleaving_team: None,
        }
    }

    async fn summary(
        engine: &AnalyticsQueryEngine,
        model: &str,
        start: &str,
        end_exclusive: &str,
    ) -> RecommendAnalyticsSummary {
        engine
            .recommendation_analytics(
                INDEX,
                model,
                timestamp(start, 0),
                timestamp(end_exclusive, 0),
            )
            .await
            .unwrap()
    }

    fn summary_json(summary: &RecommendAnalyticsSummary) -> Value {
        serde_json::json!({
            "totalUsers": summary.total_users,
            "totalRecommendations": summary.total_recommendations,
            "trackedRecommendations": summary.tracked_recommendations,
            "clickedRecommendations": summary.clicked_recommendations,
            "convertedRecommendations": summary.converted_recommendations,
            "clickThroughRate": summary.click_through_rate,
            "conversionRate": summary.conversion_rate,
            "clickPositionDistribution": summary
                .click_position_distribution
                .iter()
                .map(|(position, count)| serde_json::json!({
                    "position": position,
                    "count": count,
                }))
                .collect::<Vec<_>>(),
            "averageClickPosition": summary.average_click_position,
        })
    }

    #[tokio::test]
    async fn shared_known_answer_survives_restart_and_rejects_attribution_mutations() {
        let temp_dir = TempDir::new().unwrap();
        let config = config(&temp_dir);
        let collector = AnalyticsCollector::new(config.clone());
        assert_eq!(
            hex::encode(Sha256::digest(SHARED_FIXTURE_BYTES)),
            SHARED_FIXTURE_SHA256
        );
        let fixture: Value = serde_json::from_slice(SHARED_FIXTURE_BYTES).unwrap();
        let fixture_index = fixture["index"].as_str().unwrap();
        let fixture_date = fixture["window"]["start_date"].as_str().unwrap();
        let at = timestamp(fixture_date, 12);

        for request in fixture["recommendation_requests"].as_array().unwrap() {
            if request["analytics"] == Value::Bool(false) {
                continue;
            }
            collector.record_recommendation_request(
                fixture_index,
                request["model"].as_str().unwrap(),
                request["userToken"].as_str(),
                request["request_ip"].as_str(),
                request["queryID"].as_str().map(str::to_string),
                at,
            );
        }
        for attributed in fixture["attributed_events"].as_array().unwrap() {
            let positions = attributed["positions"].as_array().map(|positions| {
                positions
                    .iter()
                    .map(|position| position.as_u64().unwrap() as u32)
                    .collect()
            });
            collector.record_insight(event(
                attributed["eventType"].as_str().unwrap(),
                fixture_index,
                attributed["userToken"].as_str().unwrap(),
                attributed["queryID"].as_str(),
                positions,
                at,
            ));
        }

        // Unknown, foreign-identity, cross-index, and date-boundary mutations.
        collector.record_insight(event(
            "click",
            INDEX,
            USER_1,
            Some("ffffffffffffffffffffffffffffffff"),
            Some(vec![9]),
            at,
        ));
        collector.record_insight(event(
            "click",
            INDEX,
            USER_2,
            Some(QID_1),
            Some(vec![8]),
            at,
        ));
        collector.record_insight(event(
            "click",
            "other-products",
            USER_1,
            Some(QID_1),
            Some(vec![7]),
            at,
        ));
        collector.record_insight(event(
            "click",
            INDEX,
            USER_1,
            Some(QID_1),
            Some(vec![6]),
            timestamp("2026-08-02", 0),
        ));
        collector.flush_all();
        drop(collector);

        // A newly opened query engine is the restart proof: no in-memory query-ID
        // registry or cache is needed to preserve attribution.
        let reopened = AnalyticsQueryEngine::new(config);
        let related = summary(&reopened, RELATED, "2026-08-01", "2026-08-02").await;
        assert_eq!(summary_json(&related), fixture["expected"][RELATED]);

        let trending = summary(&reopened, TRENDING, "2026-08-01", "2026-08-02").await;
        assert_eq!(summary_json(&trending), fixture["expected"][TRENDING]);
    }

    #[tokio::test]
    async fn click_deduplication_keeps_every_valid_position_in_distribution() {
        let temp_dir = TempDir::new().unwrap();
        let config = config(&temp_dir);
        let collector = AnalyticsCollector::new(config.clone());
        let at = timestamp("2026-08-01", 12);
        collector.record_recommendation_request(
            INDEX,
            RELATED,
            Some(USER_1),
            None,
            Some(QID_1.to_string()),
            at,
        );
        for positions in [vec![1, 3], vec![3]] {
            collector.record_insight(event(
                "click",
                INDEX,
                USER_1,
                Some(QID_1),
                Some(positions),
                at,
            ));
        }
        for _ in 0..2 {
            collector.record_insight(event("conversion", INDEX, USER_1, Some(QID_1), None, at));
        }
        collector.flush_all();

        let actual = summary(
            &AnalyticsQueryEngine::new(config),
            RELATED,
            "2026-08-01",
            "2026-08-02",
        )
        .await;
        assert_eq!(actual.clicked_recommendations, 1);
        assert_eq!(actual.converted_recommendations, 1);
        assert_eq!(actual.click_position_distribution, vec![(1, 1), (3, 2)]);
        assert_eq!(actual.average_click_position, Some(7.0 / 3.0));
    }

    #[tokio::test]
    async fn user_token_purge_removes_recommend_attribution_without_resurrection() {
        let temp_dir = TempDir::new().unwrap();
        let config = config(&temp_dir);
        let collector = AnalyticsCollector::new(config.clone());
        let at = timestamp("2026-08-01", 12);
        collector.record_recommendation_request(
            INDEX,
            RELATED,
            Some(USER_1),
            None,
            Some(QID_1.to_string()),
            at,
        );
        collector.record_recommendation_request(INDEX, RELATED, Some(USER_2), None, None, at);
        collector.record_insight(event(
            "click",
            INDEX,
            USER_1,
            Some(QID_1),
            Some(vec![1]),
            at,
        ));
        collector.flush_all();
        // Leave a second deleted-user request buffered so the same purge proves
        // both in-memory and already-persisted Recommend attribution ownership.
        collector.record_recommendation_request(INDEX, RELATED, Some(USER_1), None, None, at);

        assert_eq!(collector.purge_user_token(USER_1).unwrap(), 3);
        collector.flush_all();
        drop(collector);
        let actual = summary(
            &AnalyticsQueryEngine::new(config),
            RELATED,
            "2026-08-01",
            "2026-08-02",
        )
        .await;
        assert_eq!(actual.total_users, 1);
        assert_eq!(actual.total_recommendations, 1);
        assert_eq!(actual.tracked_recommendations, 0);
        assert_eq!(actual.clicked_recommendations, 0);
        assert!(actual.click_position_distribution.is_empty());
    }

    #[tokio::test]
    async fn scoped_purge_preserves_other_indexes_and_unselectable_fallback_identity() {
        let temp_dir = TempDir::new().unwrap();
        let config = config(&temp_dir);
        let collector = AnalyticsCollector::new(config.clone());
        let at = timestamp("2026-08-01", 12);

        // "anonymous" is both a valid stable token and the frozen literal for
        // requests with no usable identity. Provenance must let deletion remove
        // only the selected stable-token row.
        collector.record_recommendation_request(
            INDEX,
            RELATED,
            Some("anonymous"),
            None,
            Some(QID_1.to_string()),
            at,
        );
        collector.record_recommendation_request(INDEX, RELATED, None, None, None, at);
        collector.record_recommendation_request(
            "other-products",
            RELATED,
            Some("anonymous"),
            None,
            Some(QID_2.to_string()),
            at,
        );
        collector.flush_all();

        assert_eq!(
            collector
                .purge_user_token_where_index("anonymous", &|index| index == INDEX)
                .unwrap(),
            1
        );
        collector.flush_all();
        drop(collector);

        let reopened = AnalyticsQueryEngine::new(config);
        let selected = summary(&reopened, RELATED, "2026-08-01", "2026-08-02").await;
        assert_eq!(selected.total_recommendations, 1);
        assert_eq!(selected.total_users, 1);
        assert_eq!(selected.tracked_recommendations, 0);
        let other = reopened
            .recommendation_analytics(
                "other-products",
                RELATED,
                timestamp("2026-08-01", 0),
                timestamp("2026-08-02", 0),
            )
            .await
            .unwrap();
        assert_eq!(other.total_recommendations, 1);
        assert_eq!(other.tracked_recommendations, 1);
    }

    #[tokio::test]
    async fn thirty_day_inclusive_window_remains_queryable() {
        let temp_dir = TempDir::new().unwrap();
        let config = config(&temp_dir);
        let collector = AnalyticsCollector::new(config.clone());
        for date in ["2026-08-01", "2026-08-30"] {
            collector.record_recommendation_request(
                INDEX,
                RELATED,
                Some(USER_1),
                None,
                None,
                timestamp(date, 12),
            );
        }
        collector.flush_all();
        drop(collector);

        let actual = summary(
            &AnalyticsQueryEngine::new(config),
            RELATED,
            "2026-08-01",
            "2026-08-31",
        )
        .await;
        assert_eq!(actual.total_recommendations, 2);
        assert_eq!(actual.total_users, 1);
    }

    #[tokio::test]
    async fn minimum_retention_keeps_cutoff_and_younger_recommend_partitions_queryable() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = config(&temp_dir);
        config.retention_days = 30;

        for (date, token) in [
            ("2026-08-01", USER_1),
            ("2026-08-02", USER_2),
            ("2026-08-03", USER_3),
        ] {
            // Stage a real Parquet row, then place it in the canonical date
            // partition corresponding to the controlled retention clock.
            let staging = temp_dir.path().join(format!("staging-{date}"));
            writer::flush_insight_events(
                &[request_event(RELATED, token, None, timestamp(date, 12))],
                &staging,
            )
            .unwrap();
            let generated_partition = std::fs::read_dir(&staging)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let generated_file = std::fs::read_dir(generated_partition)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            let target_partition = config.events_dir(INDEX).join(format!("date={date}"));
            std::fs::create_dir_all(&target_partition).unwrap();
            std::fs::rename(
                &generated_file,
                target_partition.join(generated_file.file_name().unwrap()),
            )
            .unwrap();
        }

        let now = Utc.with_ymd_and_hms(2026, 9, 1, 12, 0, 0).single().unwrap();
        assert_eq!(
            retention::cleanup_old_partitions_at(&config.data_dir, config.retention_days, now)
                .unwrap(),
            1
        );
        assert!(!config.events_dir(INDEX).join("date=2026-08-01").exists());
        assert!(config.events_dir(INDEX).join("date=2026-08-02").exists());
        assert!(config.events_dir(INDEX).join("date=2026-08-03").exists());

        let actual = summary(
            &AnalyticsQueryEngine::new(config),
            RELATED,
            "2026-08-01",
            "2026-08-04",
        )
        .await;
        assert_eq!(actual.total_recommendations, 2);
        assert_eq!(actual.total_users, 2);
    }
}
