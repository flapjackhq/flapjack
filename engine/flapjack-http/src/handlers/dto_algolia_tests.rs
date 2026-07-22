//! Test conversion and serialization of Algolia A/B test request DTOs to internal Experiment types, including field mapping, validation, and timestamp handling.
use super::*;
use flapjack::experiments::config::{
    Experiment, ExperimentArm, ExperimentStatus, PrimaryMetric, QueryOverrides,
};

/// Construct a sample Experiment with running status, control and variant arms with query overrides, and typical test values.
fn sample_experiment() -> Experiment {
    Experiment {
        id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
        name: "CTR boost test".to_string(),
        index_name: "products".to_string(),
        status: ExperimentStatus::Running,
        traffic_split: 0.5,
        control: ExperimentArm {
            name: "control".to_string(),
            query_overrides: None,
            index_name: None,
        },
        variant: ExperimentArm {
            name: "variant".to_string(),
            query_overrides: Some(QueryOverrides {
                enable_synonyms: Some(false),
                ..Default::default()
            }),
            index_name: None,
        },
        primary_metric: PrimaryMetric::Ctr,
        created_at: 1700000000000,
        started_at: Some(1700000100000),
        ended_at: None,
        stopped_at: None,
        minimum_days: 14,
        winsorization_cap: None,
        conclusion: None,
        interleaving: None,
    }
}

// -- Status mapping tests --

#[test]
fn status_running_maps_to_active() {
    assert_eq!(
        status_to_algolia(&ExperimentStatus::Running, None),
        "active"
    );
}

#[test]
fn status_stopped_maps_to_stopped() {
    assert_eq!(
        status_to_algolia(&ExperimentStatus::Stopped, None),
        "stopped"
    );
}

#[test]
fn status_concluded_maps_to_stopped() {
    assert_eq!(
        status_to_algolia(&ExperimentStatus::Concluded, None),
        "stopped"
    );
}

#[test]
fn status_draft_maps_to_active() {
    assert_eq!(status_to_algolia(&ExperimentStatus::Draft, None), "active");
}

#[test]
fn status_draft_with_past_end_maps_to_expired() {
    // An ended_at in the past
    assert_eq!(
        status_to_algolia(&ExperimentStatus::Draft, Some(1000)),
        "expired"
    );
}

// -- Timestamp conversion tests --

#[test]
fn epoch_ms_to_rfc3339_known_value() {
    // 2023-11-14T22:13:20Z
    assert_eq!(epoch_ms_to_rfc3339(1700000000000), "2023-11-14T22:13:20Z");
}

#[test]
fn epoch_ms_to_rfc3339_zero() {
    assert_eq!(epoch_ms_to_rfc3339(0), "1970-01-01T00:00:00Z");
}

#[test]
fn rfc3339_to_epoch_ms_roundtrip() {
    let original = 1700000000000i64;
    let rfc = epoch_ms_to_rfc3339(original);
    let back = rfc3339_to_epoch_ms(&rfc).unwrap();
    assert_eq!(back, original);
}

#[test]
fn rfc3339_to_epoch_ms_invalid_returns_error() {
    assert!(rfc3339_to_epoch_ms("not-a-date").is_err());
}

#[test]
fn rfc3339_to_epoch_ms_with_offset() {
    let ms = rfc3339_to_epoch_ms("2023-06-17T00:00:00+00:00").unwrap();
    assert_eq!(ms, 1686960000000);
}

// -- Experiment → AlgoliaAbTest conversion tests --

#[test]
fn experiment_to_algolia_sets_ab_test_id() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 42);
    assert_eq!(algolia.ab_test_id, 42);
}

#[test]
fn experiment_to_algolia_maps_name() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    assert_eq!(algolia.name, "CTR boost test");
}

#[test]
fn experiment_to_algolia_maps_status() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    assert_eq!(algolia.status, "active");
}

#[test]
fn experiment_to_algolia_creates_two_variants() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    assert_eq!(algolia.variants.len(), 2);
}

#[test]
fn experiment_to_algolia_control_traffic() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    assert_eq!(algolia.variants[0].traffic_percentage, 50);
    assert_eq!(algolia.variants[1].traffic_percentage, 50);
}

#[test]
fn experiment_to_algolia_variant_indices() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    // Both use the main index name since it's Mode A
    assert_eq!(algolia.variants[0].index, "products");
    assert_eq!(algolia.variants[1].index, "products");
}

#[test]
fn experiment_to_algolia_mode_b_variant_has_different_index() {
    let mut exp = sample_experiment();
    exp.variant.query_overrides = None;
    exp.variant.index_name = Some("products_v2".to_string());
    let algolia = experiment_to_algolia(&exp, 1);
    assert_eq!(algolia.variants[0].index, "products");
    assert_eq!(algolia.variants[1].index, "products_v2");
}

#[test]
fn experiment_to_algolia_variant_has_custom_search_params() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    // Control should have no custom params
    assert!(algolia.variants[0].custom_search_parameters.is_none());
    // Variant should have the query overrides serialized
    let params = algolia.variants[1]
        .custom_search_parameters
        .as_ref()
        .unwrap();
    assert_eq!(params["enableSynonyms"], false);
}

#[test]
fn experiment_to_algolia_created_at_is_rfc3339() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    assert_eq!(algolia.created_at, "2023-11-14T22:13:20Z");
}

#[test]
fn experiment_to_algolia_updated_at_uses_started_at_when_present() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    assert_eq!(algolia.updated_at, epoch_ms_to_rfc3339(1700000100000));
}

#[test]
fn experiment_to_algolia_stopped_experiment_has_stopped_at() {
    let mut exp = sample_experiment();
    exp.status = ExperimentStatus::Stopped;
    exp.ended_at = Some(1893456000000);
    exp.stopped_at = Some(1700001000000);
    let algolia = experiment_to_algolia(&exp, 1);
    assert!(algolia.stopped_at.is_some());
    assert_eq!(
        algolia.stopped_at.unwrap(),
        epoch_ms_to_rfc3339(1700001000000)
    );
    assert_eq!(algolia.end_at, epoch_ms_to_rfc3339(1893456000000));
}

#[test]
fn experiment_to_algolia_running_experiment_no_stopped_at() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    assert!(algolia.stopped_at.is_none());
}

#[test]
fn experiment_to_algolia_variant_stats_default_to_null() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    for v in &algolia.variants {
        assert!(v.search_count.is_none());
        assert!(v.click_count.is_none());
        assert!(v.conversion_count.is_none());
        assert!(v.user_count.is_none());
        assert!(v.click_through_rate.is_none());
    }
}

#[test]
fn experiment_to_algolia_significance_fields_default_to_none() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    assert!(algolia.click_significance.is_none());
    assert!(algolia.conversion_significance.is_none());
    assert!(algolia.add_to_cart_significance.is_none());
    assert!(algolia.purchase_significance.is_none());
    assert!(algolia.revenue_significance.is_none());
}

#[test]
fn experiment_to_algolia_winsorization_maps_to_outliers_config() {
    let mut exp = sample_experiment();
    exp.winsorization_cap = Some(0.01);
    let algolia = experiment_to_algolia(&exp, 1);
    assert!(algolia.configuration.outliers.as_ref().unwrap().exclude);
}

// -- JSON serialization tests --

#[test]
fn algolia_ab_test_serializes_to_correct_field_names() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 99);
    let json = serde_json::to_string(&algolia).unwrap();
    // Must use "abTestID" not "abTestId"
    assert!(json.contains("\"abTestID\":99"));
    assert!(json.contains("\"endAt\":"));
    assert!(json.contains("\"createdAt\":"));
    assert!(json.contains("\"updatedAt\":"));
    assert!(json.contains("\"variants\":"));
    assert!(json.contains("\"trafficPercentage\":"));
    assert!(json.contains("\"clickThroughRate\":"));
    assert!(json.contains("\"searchCount\":"));
    assert!(!json.contains("\"ab_test_id\"")); // no snake_case leaking
}

#[test]
fn algolia_ab_test_stopped_at_omitted_when_none() {
    let exp = sample_experiment();
    let algolia = experiment_to_algolia(&exp, 1);
    let json = serde_json::to_string(&algolia).unwrap();
    assert!(!json.contains("stoppedAt"));
}

#[test]
fn create_response_serializes_correct_field_names() {
    let resp = AlgoliaCreateAbTestResponse {
        ab_test_id: 42,
        index: "products".to_string(),
        task_id: 12345,
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("\"abTestID\":42"));
    assert!(json.contains("\"taskID\":12345"));
    assert!(json.contains("\"index\":\"products\""));
}

#[test]
fn action_response_serializes_correct_field_names() {
    let resp = AlgoliaAbTestActionResponse {
        ab_test_id: 7,
        index: "products".to_string(),
        task_id: 99999,
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("\"abTestID\":7"));
    assert!(json.contains("\"taskID\":99999"));
}

#[test]
fn estimate_response_serializes_correctly() {
    let resp = AlgoliaEstimateResponse {
        duration_days: 21,
        sample_sizes: vec![23415, 23415],
    };
    let json = serde_json::to_string(&resp).unwrap();
    assert!(json.contains("\"durationDays\":21"));
    assert!(json.contains("\"sampleSizes\":[23415,23415]"));
}

// -- AlgoliaCreateAbTestRequest → Experiment conversion tests --

/// Verify that AlgoliaCreateAbTestRequest converts to Experiment with correct mode detection, indices, traffic split, and arm configuration.
#[test]
fn algolia_create_to_experiment_basic() {
    let req = AlgoliaCreateAbTestRequest {
        name: "Test AB".to_string(),
        variants: vec![
            AlgoliaCreateVariant {
                index: "products".to_string(),
                traffic_percentage: 60,
                description: Some("control".to_string()),
                custom_search_parameters: None,
            },
            AlgoliaCreateVariant {
                index: "products_v2".to_string(),
                traffic_percentage: 40,
                description: Some("variant".to_string()),
                custom_search_parameters: None,
            },
        ],
        end_at: "2025-06-17T00:00:00Z".to_string(),
        configuration: None,
        metrics: None,
    };
    let exp = algolia_create_to_experiment(&req).unwrap();
    assert_eq!(exp.name, "Test AB");
    assert_eq!(exp.index_name, "products");
    assert_eq!(exp.traffic_split, 0.4);
    assert_eq!(exp.status, ExperimentStatus::Draft);
    assert_eq!(exp.control.name, "control");
    assert_eq!(exp.variant.name, "variant");
    // Mode B: different indices
    assert!(exp.variant.query_overrides.is_none());
    assert_eq!(exp.variant.index_name.as_deref(), Some("products_v2"));
}

/// Verify that Mode A conversion (same index, different query overrides) correctly maps custom_search_parameters to query_overrides.
#[test]
fn algolia_create_mode_a_with_custom_search_params() {
    let req = AlgoliaCreateAbTestRequest {
        name: "Synonym test".to_string(),
        variants: vec![
            AlgoliaCreateVariant {
                index: "products".to_string(),
                traffic_percentage: 50,
                description: None,
                custom_search_parameters: None,
            },
            AlgoliaCreateVariant {
                index: "products".to_string(),
                traffic_percentage: 50,
                description: None,
                custom_search_parameters: Some(serde_json::json!({
                    "enableSynonyms": false
                })),
            },
        ],
        end_at: "2025-06-17T00:00:00Z".to_string(),
        configuration: None,
        metrics: None,
    };
    let exp = algolia_create_to_experiment(&req).unwrap();
    assert!(exp.variant.query_overrides.is_some());
    assert_eq!(
        exp.variant
            .query_overrides
            .as_ref()
            .unwrap()
            .enable_synonyms,
        Some(false)
    );
    assert!(exp.variant.index_name.is_none());
}

#[test]
fn algolia_create_too_few_variants_errors() {
    let req = AlgoliaCreateAbTestRequest {
        name: "Bad".to_string(),
        variants: vec![AlgoliaCreateVariant {
            index: "products".to_string(),
            traffic_percentage: 100,
            description: None,
            custom_search_parameters: None,
        }],
        end_at: "2025-06-17T00:00:00Z".to_string(),
        configuration: None,
        metrics: None,
    };
    assert!(algolia_create_to_experiment(&req).is_err());
}

/// Verify that conversion fails when more than two variants are provided.
#[test]
fn algolia_create_more_than_two_variants_errors() {
    let req = AlgoliaCreateAbTestRequest {
        name: "Too many".to_string(),
        variants: vec![
            AlgoliaCreateVariant {
                index: "a".to_string(),
                traffic_percentage: 34,
                description: None,
                custom_search_parameters: None,
            },
            AlgoliaCreateVariant {
                index: "b".to_string(),
                traffic_percentage: 33,
                description: None,
                custom_search_parameters: None,
            },
            AlgoliaCreateVariant {
                index: "c".to_string(),
                traffic_percentage: 33,
                description: None,
                custom_search_parameters: None,
            },
        ],
        end_at: "2025-06-17T00:00:00Z".to_string(),
        configuration: None,
        metrics: None,
    };
    assert!(algolia_create_to_experiment(&req).is_err());
}

/// Verify that conversion fails when any variant has zero traffic percentage.
#[test]
fn algolia_create_invalid_traffic_percentage_errors() {
    let req = AlgoliaCreateAbTestRequest {
        name: "Bad".to_string(),
        variants: vec![
            AlgoliaCreateVariant {
                index: "a".to_string(),
                traffic_percentage: 0,
                description: None,
                custom_search_parameters: None,
            },
            AlgoliaCreateVariant {
                index: "b".to_string(),
                traffic_percentage: 0,
                description: None,
                custom_search_parameters: None,
            },
        ],
        end_at: "2025-06-17T00:00:00Z".to_string(),
        configuration: None,
        metrics: None,
    };
    assert!(algolia_create_to_experiment(&req).is_err());
}

/// Verify that conversion fails when variant traffic percentages do not sum to 100.
#[test]
fn algolia_create_traffic_percentages_must_sum_to_100() {
    let req = AlgoliaCreateAbTestRequest {
        name: "Bad traffic mix".to_string(),
        variants: vec![
            AlgoliaCreateVariant {
                index: "a".to_string(),
                traffic_percentage: 90,
                description: None,
                custom_search_parameters: None,
            },
            AlgoliaCreateVariant {
                index: "b".to_string(),
                traffic_percentage: 20,
                description: None,
                custom_search_parameters: None,
            },
        ],
        end_at: "2025-06-17T00:00:00Z".to_string(),
        configuration: None,
        metrics: None,
    };
    assert!(algolia_create_to_experiment(&req).is_err());
}

/// Verify that the first metric name in the request maps to the experiment's primary_metric.
#[test]
fn algolia_create_with_metrics_maps_primary_metric() {
    let req = AlgoliaCreateAbTestRequest {
        name: "Revenue test".to_string(),
        variants: vec![
            AlgoliaCreateVariant {
                index: "products".to_string(),
                traffic_percentage: 50,
                description: None,
                custom_search_parameters: None,
            },
            AlgoliaCreateVariant {
                index: "products_v2".to_string(),
                traffic_percentage: 50,
                description: None,
                custom_search_parameters: None,
            },
        ],
        end_at: "2025-06-17T00:00:00Z".to_string(),
        configuration: None,
        metrics: Some(vec![AlgoliaMetricDef {
            name: "revenue".to_string(),
            dimension: Some("USD".to_string()),
        }]),
    };
    let exp = algolia_create_to_experiment(&req).unwrap();
    assert_eq!(exp.primary_metric, PrimaryMetric::RevenuePerSearch);
}

/// Verify that AlgoliaCreateAbTestRequest correctly deserializes from JSON with camelCase field names mapping to internal snake_case fields.
#[test]
fn algolia_create_request_deserializes_from_json() {
    let json = serde_json::json!({
        "name": "Test",
        "variants": [
            { "index": "idx", "trafficPercentage": 60 },
            { "index": "idx_v2", "trafficPercentage": 40, "description": "v2" }
        ],
        "endAt": "2025-06-17T00:00:00Z",
        "configuration": {
            "minimumDetectableEffect": { "size": 0.05, "metric": "clickThroughRate" },
            "outliers": { "exclude": true }
        },
        "metrics": [{ "name": "clickThroughRate" }]
    });
    let req: AlgoliaCreateAbTestRequest = serde_json::from_value(json).unwrap();
    assert_eq!(req.name, "Test");
    assert_eq!(req.variants.len(), 2);
    assert_eq!(req.variants[1].traffic_percentage, 40);
    assert!(req.configuration.is_some());
    let config = req.configuration.unwrap();
    assert!(config.minimum_detectable_effect.is_some());
    assert_eq!(config.minimum_detectable_effect.unwrap().size, 0.05);
}

#[test]
fn list_query_deserializes_algolia_params() {
    let json = serde_json::json!({
        "offset": 10,
        "limit": 5,
        "indexName": "products",
        "indexPrefix": "prod",
        "indexSuffix": "_v2"
    });
    let query: AlgoliaListAbTestsQuery = serde_json::from_value(json).unwrap();
    assert_eq!(query.offset, Some(10));
    assert_eq!(query.limit, Some(5));
    assert_eq!(query.index_name.as_deref(), Some("products"));
    assert_eq!(query.index_prefix.as_deref(), Some("prod"));
    assert_eq!(query.index_suffix.as_deref(), Some("_v2"));
}
