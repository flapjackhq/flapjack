//! Define Algolia-compatible request/response DTOs and bidirectional conversion between Algolia and internal experiment formats.

use flapjack::experiments::config::{Experiment, ExperimentArm, ExperimentStatus, QueryOverrides};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const DEFAULT_MINIMUM_DAYS: u32 = 14;

// ---------------------------------------------------------------------------
// Response DTOs
// ---------------------------------------------------------------------------

/// Top-level A/B test object returned by GET /2/abtests/{id} and inside list responses.
#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaAbTest {
    #[serde(rename = "abTestID")]
    pub ab_test_id: i64,
    pub name: String,
    pub status: String,
    pub end_at: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<String>,
    pub variants: Vec<AlgoliaVariant>,
    pub configuration: AlgoliaConfiguration,
    pub click_significance: Option<f64>,
    pub conversion_significance: Option<f64>,
    pub add_to_cart_significance: Option<f64>,
    pub purchase_significance: Option<f64>,
    #[schema(value_type = Option<Object>)]
    pub revenue_significance: Option<serde_json::Value>,
}

/// A single variant in an A/B test.
#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaVariant {
    pub index: String,
    pub traffic_percentage: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub custom_search_parameters: Option<serde_json::Value>,
    // Metric stats — null when no analytics data exists.
    pub add_to_cart_count: Option<i64>,
    pub add_to_cart_rate: Option<f64>,
    pub average_click_position: Option<f64>,
    pub click_count: Option<i64>,
    pub click_through_rate: Option<f64>,
    pub conversion_count: Option<i64>,
    pub conversion_rate: Option<f64>,
    #[schema(value_type = HashMap<String, Object>)]
    pub currencies: HashMap<String, serde_json::Value>,
    pub estimated_sample_size: i64,
    pub filter_effects: Option<AlgoliaFilterEffects>,
    pub no_result_count: Option<i64>,
    pub purchase_count: Option<i64>,
    pub purchase_rate: Option<f64>,
    pub search_count: Option<i64>,
    pub tracked_search_count: Option<i64>,
    pub user_count: Option<i64>,
    pub tracked_user_count: Option<i64>,
}

/// Filter effects metadata (outlier + empty-search exclusion stats).
#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaFilterEffects {
    pub outliers: Option<AlgoliaFilterEffectsEntry>,
    pub empty_search: Option<AlgoliaFilterEffectsEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaFilterEffectsEntry {
    pub users_count: i64,
    pub tracked_searches_count: i64,
}

/// Configuration object for an A/B test.
#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaConfiguration {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_detectable_effect: Option<AlgoliaMinimumDetectableEffect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outliers: Option<AlgoliaOutliersSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_search: Option<AlgoliaEmptySearchSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature_filters: Option<AlgoliaFeatureFilters>,
}

#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaMinimumDetectableEffect {
    pub size: f64,
    pub metric: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaOutliersSetting {
    pub exclude: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaEmptySearchSetting {
    pub exclude: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaFeatureFilters {
    #[serde(default)]
    pub dynamic_re_ranking: bool,
    #[serde(default)]
    pub ai_perso: bool,
    #[serde(default)]
    pub multi_signal_ranking: bool,
}

/// List response for GET /2/abtests.
#[derive(Debug, Serialize, Deserialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaListAbTestsResponse {
    pub abtests: Option<Vec<AlgoliaAbTest>>,
    pub count: usize,
    pub total: usize,
}

// ---------------------------------------------------------------------------
// Request DTOs
// ---------------------------------------------------------------------------

/// Request body for POST /2/abtests (create).
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaCreateAbTestRequest {
    pub name: String,
    pub variants: Vec<AlgoliaCreateVariant>,
    pub end_at: String,
    #[serde(default)]
    pub configuration: Option<AlgoliaCreateConfiguration>,
    #[serde(default)]
    pub metrics: Option<Vec<AlgoliaMetricDef>>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaCreateVariant {
    pub index: String,
    pub traffic_percentage: i64,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub custom_search_parameters: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaCreateConfiguration {
    #[serde(default)]
    pub minimum_detectable_effect: Option<AlgoliaMinimumDetectableEffect>,
    #[serde(default)]
    pub outliers: Option<AlgoliaOutliersSetting>,
    #[serde(default)]
    pub empty_search: Option<AlgoliaEmptySearchSetting>,
    #[serde(default)]
    pub feature_filters: Option<AlgoliaFeatureFilters>,
    #[serde(default)]
    pub error_correction: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaMetricDef {
    pub name: String,
    #[serde(default)]
    pub dimension: Option<String>,
}

/// Successful create response.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaCreateAbTestResponse {
    #[serde(rename = "abTestID")]
    pub ab_test_id: i64,
    pub index: String,
    #[serde(rename = "taskID")]
    pub task_id: i64,
}

/// Successful stop/delete response.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaAbTestActionResponse {
    #[serde(rename = "abTestID")]
    pub ab_test_id: i64,
    pub index: String,
    #[serde(rename = "taskID")]
    pub task_id: i64,
}

// ---------------------------------------------------------------------------
// Estimate endpoint DTOs
// ---------------------------------------------------------------------------

/// Request body for POST /2/abtests/estimate.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaEstimateRequest {
    pub configuration: AlgoliaEstimateConfiguration,
    pub variants: Vec<AlgoliaEstimateVariant>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaEstimateConfiguration {
    pub minimum_detectable_effect: AlgoliaMinimumDetectableEffect,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaEstimateVariant {
    pub index: String,
    pub traffic_percentage: i64,
}

/// Response for POST /2/abtests/estimate.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaEstimateResponse {
    pub duration_days: i64,
    pub sample_sizes: Vec<i64>,
}

// ---------------------------------------------------------------------------
// List query params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaListAbTestsQuery {
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub index_name: Option<String>,
    #[serde(default)]
    pub index_prefix: Option<String>,
    #[serde(default)]
    pub index_suffix: Option<String>,
}

// ---------------------------------------------------------------------------
// Conversion: internal → Algolia wire format
// ---------------------------------------------------------------------------

/// Convert an internal `ExperimentStatus` to the Algolia status string.
pub fn status_to_algolia(status: &ExperimentStatus, scheduled_end_at: Option<i64>) -> &'static str {
    match status {
        ExperimentStatus::Running => "active",
        ExperimentStatus::Stopped => "stopped",
        ExperimentStatus::Concluded => "stopped",
        ExperimentStatus::Draft => {
            // If a draft has an end date in the past, it's "expired".
            if let Some(end) = scheduled_end_at {
                if end < chrono::Utc::now().timestamp_millis() {
                    return "expired";
                }
            }
            // Flapjack drafts map to "active" since Algolia has no "draft" status.
            "active"
        }
    }
}

/// Convert epoch milliseconds to RFC 3339 string.
pub fn epoch_ms_to_rfc3339(epoch_ms: i64) -> String {
    use chrono::{DateTime, Utc};
    let dt = DateTime::<Utc>::from_timestamp_millis(epoch_ms)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());
    dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Convert an RFC 3339 string to epoch milliseconds.
pub fn rfc3339_to_epoch_ms(rfc: &str) -> Result<i64, String> {
    use chrono::DateTime;
    let dt = DateTime::parse_from_rfc3339(rfc)
        .map_err(|e| format!("invalid RFC 3339 timestamp '{}': {}", rfc, e))?;
    Ok(dt.timestamp_millis())
}

/// Build a default `AlgoliaVariant` with null metric stats from an internal `ExperimentArm`.
fn arm_to_algolia_variant(
    arm: &ExperimentArm,
    index_name: &str,
    traffic_pct: i64,
    custom_search_params: Option<serde_json::Value>,
) -> AlgoliaVariant {
    AlgoliaVariant {
        index: arm.index_name.as_deref().unwrap_or(index_name).to_string(),
        traffic_percentage: traffic_pct,
        description: Some(arm.name.clone()),
        custom_search_parameters: custom_search_params,
        add_to_cart_count: None,
        add_to_cart_rate: None,
        average_click_position: None,
        click_count: None,
        click_through_rate: None,
        conversion_count: None,
        conversion_rate: None,
        currencies: HashMap::new(),
        estimated_sample_size: 0,
        filter_effects: None,
        no_result_count: None,
        purchase_count: None,
        purchase_rate: None,
        search_count: None,
        tracked_search_count: None,
        user_count: None,
        tracked_user_count: None,
    }
}

/// Convert query overrides to Algolia's `customSearchParameters` JSON value.
fn query_overrides_to_custom_params(overrides: &QueryOverrides) -> Option<serde_json::Value> {
    let val = serde_json::to_value(overrides).ok()?;
    if val.as_object().is_none_or(|o| o.is_empty()) {
        None
    } else {
        Some(val)
    }
}

/// Convert an internal `Experiment` to the Algolia wire format.
///
/// `numeric_id` is the integer ID alias assigned by the store.
pub fn experiment_to_algolia(experiment: &Experiment, numeric_id: i64) -> AlgoliaAbTest {
    experiment_to_algolia_with_updated_at(experiment, numeric_id, None)
}

/// Convert an internal `Experiment` to the Algolia wire format, with optional updatedAt override.
///
/// `numeric_id` is the integer ID alias assigned by the store.
/// `updated_at_ms_override` should be the persisted "last updated" timestamp when available.
pub fn experiment_to_algolia_with_updated_at(
    experiment: &Experiment,
    numeric_id: i64,
    updated_at_ms_override: Option<i64>,
) -> AlgoliaAbTest {
    let control_traffic = ((1.0 - experiment.traffic_split) * 100.0).round() as i64;
    let variant_traffic = (experiment.traffic_split * 100.0).round() as i64;

    let control_custom_params = None; // control has no overrides
    let variant_custom_params = experiment
        .variant
        .query_overrides
        .as_ref()
        .and_then(query_overrides_to_custom_params);

    let control_variant = arm_to_algolia_variant(
        &experiment.control,
        &experiment.index_name,
        control_traffic,
        control_custom_params,
    );
    let variant_variant = arm_to_algolia_variant(
        &experiment.variant,
        &experiment.index_name,
        variant_traffic,
        variant_custom_params,
    );

    let scheduled_end_at_str = experiment.ended_at.map(epoch_ms_to_rfc3339);
    let stopped_at_ms = if experiment.status == ExperimentStatus::Stopped
        || experiment.status == ExperimentStatus::Concluded
    {
        // Backward compatibility: older records may only have ended_at populated.
        experiment.stopped_at.or(experiment.ended_at)
    } else {
        None
    };
    let stopped_at = stopped_at_ms.map(epoch_ms_to_rfc3339);
    let started_at = experiment.started_at.map(epoch_ms_to_rfc3339);

    // For endAt, use scheduled end if available, otherwise a far-future sentinel.
    let end_at = scheduled_end_at_str.unwrap_or_else(|| "2099-12-31T23:59:59Z".to_string());
    let updated_at_ms = updated_at_ms_override
        .or(stopped_at_ms)
        .or(experiment.started_at)
        .unwrap_or(experiment.created_at);

    AlgoliaAbTest {
        ab_test_id: numeric_id,
        name: experiment.name.clone(),
        status: status_to_algolia(&experiment.status, experiment.ended_at).to_string(),
        end_at,
        created_at: epoch_ms_to_rfc3339(experiment.created_at),
        updated_at: epoch_ms_to_rfc3339(updated_at_ms),
        started_at,
        stopped_at,
        variants: vec![control_variant, variant_variant],
        configuration: AlgoliaConfiguration {
            minimum_detectable_effect: None,
            outliers: Some(AlgoliaOutliersSetting {
                exclude: experiment.winsorization_cap.is_some(),
            }),
            empty_search: None,
            feature_filters: None,
        },
        click_significance: None,
        conversion_significance: None,
        add_to_cart_significance: None,
        purchase_significance: None,
        revenue_significance: None,
    }
}

// ---------------------------------------------------------------------------
// Conversion: Algolia request → internal
// ---------------------------------------------------------------------------

/// Convert an `AlgoliaCreateAbTestRequest` to the internal `Experiment` struct.
///
/// Returns the experiment (with a fresh UUID and Draft status) or an error message.
pub fn algolia_create_to_experiment(
    req: &AlgoliaCreateAbTestRequest,
) -> Result<Experiment, String> {
    if req.variants.len() != 2 {
        return Err("variants must contain exactly 2 entries".to_string());
    }

    let control_v = &req.variants[0];
    let variant_v = &req.variants[1];

    if !(1..=99).contains(&control_v.traffic_percentage) {
        return Err("control trafficPercentage must be between 1 and 99".to_string());
    }
    if !(1..=99).contains(&variant_v.traffic_percentage) {
        return Err("variant trafficPercentage must be between 1 and 99".to_string());
    }
    if control_v.traffic_percentage + variant_v.traffic_percentage != 100 {
        return Err("variants trafficPercentage values must sum to 100".to_string());
    }

    let traffic_split = variant_v.traffic_percentage as f64 / 100.0;

    let end_at_ms = rfc3339_to_epoch_ms(&req.end_at)?;

    // Determine variant mode: if customSearchParameters is set → Mode A, else → Mode B
    if variant_v.custom_search_parameters.is_some() && control_v.index != variant_v.index {
        return Err(
            "variant index must match control index when customSearchParameters is provided"
                .to_string(),
        );
    }
    let (query_overrides, variant_index_name) =
        if let Some(ref params) = variant_v.custom_search_parameters {
            let overrides: QueryOverrides = serde_json::from_value(params.clone())
                .map_err(|e| format!("invalid customSearchParameters: {}", e))?;
            (Some(overrides), None)
        } else if control_v.index != variant_v.index {
            // Different indices → Mode B
            (None, Some(variant_v.index.clone()))
        } else {
            // Same index, no custom params → Mode A with empty overrides
            (Some(QueryOverrides::default()), None)
        };

    // Determine primary metric from the metrics array if provided
    let primary_metric = req
        .metrics
        .as_ref()
        .and_then(|m| m.first())
        .map(|m| match m.name.as_str() {
            "clickThroughRate" => flapjack::experiments::config::PrimaryMetric::Ctr,
            "conversionRate" => flapjack::experiments::config::PrimaryMetric::ConversionRate,
            "revenue" => flapjack::experiments::config::PrimaryMetric::RevenuePerSearch,
            _ => flapjack::experiments::config::PrimaryMetric::Ctr,
        })
        .unwrap_or(flapjack::experiments::config::PrimaryMetric::Ctr);

    Ok(Experiment {
        id: uuid::Uuid::new_v4().to_string(),
        name: req.name.clone(),
        index_name: control_v.index.clone(),
        status: ExperimentStatus::Draft,
        traffic_split,
        control: ExperimentArm {
            name: control_v
                .description
                .clone()
                .unwrap_or_else(|| "control".to_string()),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: variant_v
                .description
                .clone()
                .unwrap_or_else(|| "variant".to_string()),
            query_overrides,
            index_name: variant_index_name,
        },
        primary_metric,
        created_at: chrono::Utc::now().timestamp_millis(),
        started_at: None,
        ended_at: Some(end_at_ms),
        stopped_at: None,
        minimum_days: DEFAULT_MINIMUM_DAYS,
        winsorization_cap: req.configuration.as_ref().and_then(|c| {
            c.outliers
                .as_ref()
                .and_then(|o| if o.exclude { Some(0.01) } else { None })
        }),
        conclusion: None,
        interleaving: None,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "dto_algolia_tests.rs"]
mod tests;
