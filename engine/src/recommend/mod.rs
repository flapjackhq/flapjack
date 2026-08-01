//! Recommendation engine for trending items, facets, related products, and bought-together models using analytics insight events.

/// Extract object IDs from a JSON row, handling both string-encoded and array formats.
///
/// # Arguments
///
/// `row` — A JSON value typically containing an "object_ids" field.
///
/// # Returns
///
/// A vector of string object IDs. If the field is missing, parsing fails, or array elements are non-string, returns an empty vector.
#[cfg(feature = "analytics")]
fn parse_object_ids(row: &serde_json::Value) -> Vec<String> {
    let Some(raw_object_ids) = row.get("object_ids") else {
        return Vec::new();
    };

    if let Some(value) = raw_object_ids.as_str() {
        return serde_json::from_str::<Vec<String>>(value).unwrap_or_default();
    }

    raw_object_ids
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(feature = "analytics")]
pub mod cooccurrence;
pub mod looking_similar;
pub mod rules;
#[cfg(feature = "analytics")]
pub mod trending;

/// Default window (in days) for trending item/facet computation.
/// Algolia uses 15-30 days; we use 7 for on-demand computation.
pub const TRENDING_WINDOW_DAYS: u64 = 7;
pub const TRENDING_WINDOW_DAYS_ENV_VAR: &str = "FLAPJACK_TRENDING_WINDOW_DAYS";

/// Lookback window (in days) for co-occurrence computation (related-products, bought-together).
/// Matches Algolia's default 30-day event window.
pub const CO_OCCURRENCE_LOOKBACK_DAYS: u64 = 30;

/// Valid recommendation model names (Algolia REST API values).
pub const VALID_MODELS: &[&str] = &[
    "trending-items",
    "trending-facets",
    "related-products",
    "bought-together",
    "looking-similar",
];

/// Models that require an `objectID` in the request.
pub const MODELS_REQUIRING_OBJECT_ID: &[&str] =
    &["related-products", "bought-together", "looking-similar"];

/// Models that support `queryParameters` and `fallbackParameters`.
pub const MODELS_SUPPORTING_QUERY_PARAMS: &[&str] = &[
    "trending-items",
    "related-products",
    "bought-together",
    "looking-similar",
];

/// Default and max for `maxRecommendations`.
pub const MAX_RECOMMENDATIONS_DEFAULT: u32 = 30;
pub const MAX_RECOMMENDATIONS_DEFAULT_ENV_VAR: &str = "FLAPJACK_RECOMMEND_MAX_RESULTS";
pub const MAX_RECOMMENDATIONS_MIN: u32 = 1;
pub const MAX_RECOMMENDATIONS_MAX: u32 = 30;

/// Threshold range (0-100).
pub const THRESHOLD_MIN: u32 = 0;
pub const THRESHOLD_MAX: u32 = 100;

/// Runtime recommendation configuration loaded from environment variables.
#[derive(Debug, Clone)]
pub struct RecommendConfig {
    pub trending_window_days: u64,
    pub max_recommendations_default: u32,
}

impl Default for RecommendConfig {
    fn default() -> Self {
        Self {
            trending_window_days: TRENDING_WINDOW_DAYS,
            max_recommendations_default: MAX_RECOMMENDATIONS_DEFAULT,
        }
    }
}

impl RecommendConfig {
    /// Load recommendation config from environment variables with defaults.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let trending_window_days = std::env::var(TRENDING_WINDOW_DAYS_ENV_VAR)
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|window_days: &u64| *window_days > 0)
            .unwrap_or(defaults.trending_window_days);

        let max_recommendations_default = std::env::var(MAX_RECOMMENDATIONS_DEFAULT_ENV_VAR)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(defaults.max_recommendations_default)
            .clamp(MAX_RECOMMENDATIONS_MIN, MAX_RECOMMENDATIONS_MAX);

        Self {
            trending_window_days,
            max_recommendations_default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RecommendConfig, MAX_RECOMMENDATIONS_DEFAULT_ENV_VAR, TRENDING_WINDOW_DAYS_ENV_VAR,
    };
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarReset {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvVarReset {
        fn remove(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: Tests synchronize env mutation via `env_lock` so no concurrent reads/writes.
            unsafe { std::env::remove_var(key) };
            Self { key, original }
        }

        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: Tests synchronize env mutation via `env_lock` so no concurrent reads/writes.
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvVarReset {
        fn drop(&mut self) {
            if let Some(value) = self.original.as_deref() {
                // SAFETY: Tests synchronize env mutation via `env_lock` so no concurrent reads/writes.
                unsafe { std::env::set_var(self.key, value) };
            } else {
                // SAFETY: Tests synchronize env mutation via `env_lock` so no concurrent reads/writes.
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }

    #[test]
    fn recommend_config_from_env_uses_defaults_when_env_missing() {
        let _env_guard = env_lock().lock().unwrap();
        let _window_reset = EnvVarReset::remove(TRENDING_WINDOW_DAYS_ENV_VAR);
        let _max_results_reset = EnvVarReset::remove(MAX_RECOMMENDATIONS_DEFAULT_ENV_VAR);

        let config = RecommendConfig::from_env();

        assert_eq!(config.trending_window_days, 7);
        assert_eq!(config.max_recommendations_default, 30);
    }

    #[test]
    fn recommend_config_from_env_applies_valid_overrides() {
        let _env_guard = env_lock().lock().unwrap();
        let _window_reset = EnvVarReset::set(TRENDING_WINDOW_DAYS_ENV_VAR, "14");
        let _max_results_reset = EnvVarReset::set(MAX_RECOMMENDATIONS_DEFAULT_ENV_VAR, "20");

        let config = RecommendConfig::from_env();

        assert_eq!(config.trending_window_days, 14);
        assert_eq!(config.max_recommendations_default, 20);
    }

    #[test]
    fn recommend_config_from_env_falls_back_for_non_numeric_values() {
        let _env_guard = env_lock().lock().unwrap();
        let _window_reset = EnvVarReset::set(TRENDING_WINDOW_DAYS_ENV_VAR, "abc");
        let _max_results_reset = EnvVarReset::set(MAX_RECOMMENDATIONS_DEFAULT_ENV_VAR, "xyz");

        let config = RecommendConfig::from_env();

        assert_eq!(config.trending_window_days, 7);
        assert_eq!(config.max_recommendations_default, 30);
    }

    #[test]
    fn recommend_config_from_env_rejects_zero_window_days() {
        let _env_guard = env_lock().lock().unwrap();
        let _window_reset = EnvVarReset::set(TRENDING_WINDOW_DAYS_ENV_VAR, "0");
        let _max_results_reset = EnvVarReset::remove(MAX_RECOMMENDATIONS_DEFAULT_ENV_VAR);

        let config = RecommendConfig::from_env();

        assert_eq!(config.trending_window_days, 7);
        assert_eq!(config.max_recommendations_default, 30);
    }

    #[test]
    fn stage3_recommend_config_from_env_handles_negative_and_overflowing_numerics() {
        let _env_guard = env_lock().lock().unwrap();
        let _window_reset = EnvVarReset::set(TRENDING_WINDOW_DAYS_ENV_VAR, "-1");
        let _max_results_reset =
            EnvVarReset::set(MAX_RECOMMENDATIONS_DEFAULT_ENV_VAR, "18446744073709551616");

        let config = RecommendConfig::from_env();

        assert_eq!(
            config.trending_window_days, 7,
            "negative u64 env input must fall back to the default window"
        );
        assert_eq!(
            config.max_recommendations_default, 30,
            "overflowing u32 env input must fall back to the default max"
        );
    }

    #[test]
    fn stage3_recommend_config_from_env_clamps_max_results() {
        let _env_guard = env_lock().lock().unwrap();
        let _window_reset = EnvVarReset::remove(TRENDING_WINDOW_DAYS_ENV_VAR);
        let _max_results_reset = EnvVarReset::set(MAX_RECOMMENDATIONS_DEFAULT_ENV_VAR, "999");

        let config = RecommendConfig::from_env();

        assert_eq!(config.trending_window_days, 7);
        assert_eq!(config.max_recommendations_default, 30);
    }

    #[cfg(feature = "analytics")]
    #[test]
    fn stage3_parse_object_ids_preserves_multibyte_ids_and_rejects_malformed_rows() {
        let encoded = serde_json::json!({
            "object_ids": "[\"prod-é\",\"東京-2\"]"
        });
        assert_eq!(
            super::parse_object_ids(&encoded),
            vec!["prod-é".to_string(), "東京-2".to_string()]
        );

        let array = serde_json::json!({
            "object_ids": ["用户-1", 42, "emoji-😀"]
        });
        assert_eq!(
            super::parse_object_ids(&array),
            vec!["用户-1".to_string(), "emoji-😀".to_string()]
        );

        let malformed = serde_json::json!({
            "object_ids": "[\"unterminated\""
        });
        assert!(super::parse_object_ids(&malformed).is_empty());
    }
}
