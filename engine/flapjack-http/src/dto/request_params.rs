//! Stub summary for request_params.rs.
use super::HybridSearchParams;

impl super::SearchRequest {
    /// Parse the URL-encoded `params` string and merge each key into the corresponding struct field.
    ///
    /// The params string always overrides top-level JSON values (verified against live Algolia API).
    /// Consumes `self.params` via `take()`, so calling twice is a no-op.
    pub fn apply_params_string(&mut self) {
        let params_str = match self.params.take() {
            Some(s) if !s.is_empty() => s,
            _ => return,
        };
        // Params string always overrides top-level JSON values.
        // Verified against live Algolia API (2026-02-23).
        for (key, value) in url::form_urlencoded::parse(params_str.as_bytes()) {
            let k = key.as_ref();
            let v = value.as_ref();
            match k {
                "query" => self.query = v.to_string(),
                _ => {
                    let _ = self.apply_pagination_param(k, v)
                        || self.apply_filter_param(k, v)
                        || self.apply_facet_search_param(k, v)
                        || self.apply_highlight_param(k, v)
                        || self.apply_ranking_param(k, v)
                        || self.apply_geo_param(k, v)
                        || self.apply_analytics_param(k, v)
                        || self.apply_personalization_param(k, v)
                        || self.apply_retrieval_param(k, v)
                        || self.apply_feature_toggle_param(k, v)
                        || self.apply_language_param(k, v)
                        || self.apply_search_mode_param(k, v);
                }
            }
        }
    }

    fn apply_pagination_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "hitsPerPage" => {
                if let Ok(v) = value.parse() {
                    self.hits_per_page = Some(v);
                }
            }
            "page" => {
                self.page = value.parse().unwrap_or(0);
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_filter_param.
    fn apply_filter_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "filters" => {
                self.filters = Some(value.to_string());
            }
            "facetFilters" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                    self.facet_filters = Some(v);
                }
            }
            "numericFilters" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                    self.numeric_filters = Some(v);
                }
            }
            "tagFilters" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                    self.tag_filters = Some(v);
                }
            }
            "optionalFilters" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                    self.optional_filters = Some(v);
                }
            }
            "sumOrFiltersScores" => {
                if let Ok(v) = value.parse() {
                    self.sum_or_filters_scores = Some(v);
                }
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_facet_search_param.
    fn apply_facet_search_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "facets" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.facets = Some(v);
                } else {
                    self.facets = Some(value.split(',').map(|s| s.trim().to_string()).collect());
                }
            }
            "maxValuesPerFacet" => {
                if let Ok(v) = value.parse() {
                    self.max_values_per_facet = Some(v);
                }
            }
            "facet" => {
                self.facet = Some(value.to_string());
            }
            "facetQuery" => {
                self.facet_query = Some(value.to_string());
            }
            "maxFacetHits" => {
                if let Ok(v) = value.parse() {
                    self.max_facet_hits = Some(v);
                }
            }
            "sortFacetValuesBy" => {
                self.sort_facet_values_by = Some(value.to_string());
            }
            "facetingAfterDistinct" => {
                if let Ok(v) = value.parse() {
                    self.faceting_after_distinct = Some(v);
                }
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_highlight_param.
    fn apply_highlight_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "attributesToHighlight" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.attributes_to_highlight = Some(v);
                }
            }
            "attributesToSnippet" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.attributes_to_snippet = Some(v);
                }
            }
            "highlightPreTag" => {
                self.highlight_pre_tag = Some(value.to_string());
            }
            "highlightPostTag" => {
                self.highlight_post_tag = Some(value.to_string());
            }
            "snippetEllipsisText" => {
                self.snippet_ellipsis_text = Some(value.to_string());
            }
            "restrictHighlightAndSnippetArrays" => {
                if let Ok(v) = value.parse() {
                    self.restrict_highlight_and_snippet_arrays = Some(v);
                }
            }
            "replaceSynonymsInHighlight" => {
                if let Ok(v) = value.parse() {
                    self.replace_synonyms_in_highlight = Some(v);
                }
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_ranking_param.
    fn apply_ranking_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "queryType" => {
                self.query_type_prefix = Some(value.to_string());
            }
            "typoTolerance" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                    self.typo_tolerance = Some(v);
                } else {
                    match value {
                        "true" => self.typo_tolerance = Some(serde_json::Value::Bool(true)),
                        "false" => self.typo_tolerance = Some(serde_json::Value::Bool(false)),
                        _ => {
                            self.typo_tolerance = Some(serde_json::Value::String(value.to_string()))
                        }
                    }
                }
            }
            "advancedSyntax" => {
                if let Ok(v) = value.parse() {
                    self.advanced_syntax = Some(v);
                }
            }
            "advancedSyntaxFeatures" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.advanced_syntax_features = Some(v);
                }
            }
            "removeWordsIfNoResults" => {
                self.remove_words_if_no_results = Some(value.to_string());
            }
            "minProximity" => {
                if let Ok(v) = value.parse::<u32>() {
                    self.min_proximity = Some(v);
                }
            }
            "disableExactOnAttributes" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.disable_exact_on_attributes = Some(v);
                }
            }
            "exactOnSingleWordQuery" => {
                self.exact_on_single_word_query = Some(value.to_string());
            }
            "alternativesAsExact" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.alternatives_as_exact = Some(v);
                }
            }
            "relevancyStrictness" => {
                if let Ok(v) = value.parse::<u32>() {
                    self.relevancy_strictness = Some(v);
                }
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_geo_param.
    fn apply_geo_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "aroundLatLng" => {
                self.around_lat_lng = Some(value.to_string());
            }
            "aroundRadius" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                    self.around_radius = Some(v);
                } else if value == "all" {
                    self.around_radius = Some(serde_json::Value::String("all".to_string()));
                } else if let Ok(n) = value.parse::<u64>() {
                    self.around_radius = Some(serde_json::json!(n));
                }
            }
            "insideBoundingBox" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                    self.inside_bounding_box = Some(v);
                }
            }
            "insidePolygon" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                    self.inside_polygon = Some(v);
                }
            }
            "aroundPrecision" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                    self.around_precision = Some(v);
                } else if let Ok(n) = value.parse::<u64>() {
                    self.around_precision = Some(serde_json::json!(n));
                }
            }
            "minimumAroundRadius" => {
                if let Ok(v) = value.parse() {
                    self.minimum_around_radius = Some(v);
                }
            }
            "aroundLatLngViaIP" => {
                if let Ok(v) = value.parse() {
                    self.around_lat_lng_via_ip = Some(v);
                }
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_analytics_param.
    fn apply_analytics_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "analytics" => {
                if let Ok(v) = value.parse() {
                    self.analytics = Some(v);
                }
            }
            "clickAnalytics" => {
                if let Ok(v) = value.parse() {
                    self.click_analytics = Some(v);
                }
            }
            "analyticsTags" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.analytics_tags = Some(v);
                }
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_personalization_param.
    fn apply_personalization_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "userToken" => {
                self.user_token = Some(value.to_string());
            }
            "enablePersonalization" => {
                if let Ok(v) = value.parse() {
                    self.enable_personalization = Some(v);
                }
            }
            "enableReRanking" => {
                if let Ok(v) = value.parse() {
                    self.enable_re_ranking = Some(v);
                }
            }
            "reRankingApplyFilter" => {
                self.re_ranking_apply_filter = Some(value.to_string());
            }
            "personalizationImpact" => {
                if let Ok(v) = value.parse::<u32>() {
                    self.personalization_impact = Some(v);
                }
            }
            "personalizationFilters" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.personalization_filters = Some(v);
                } else {
                    self.personalization_filters = Some(
                        value
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect(),
                    );
                }
            }
            "sessionID" | "sessionId" => {
                self.session_id = Some(value.to_string());
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_retrieval_param.
    fn apply_retrieval_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "attributesToRetrieve" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.attributes_to_retrieve = Some(v);
                }
            }
            "restrictSearchableAttributes" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.restrict_searchable_attributes = Some(v);
                }
            }
            "responseFields" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.response_fields = Some(v);
                }
            }
            "distinct" => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                    self.distinct = Some(v);
                }
            }
            "getRankingInfo" => {
                if let Ok(v) = value.parse() {
                    self.get_ranking_info = Some(v);
                }
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_feature_toggle_param.
    fn apply_feature_toggle_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "enableSynonyms" => {
                if let Ok(v) = value.parse() {
                    self.enable_synonyms = Some(v);
                }
            }
            "enableRules" => {
                if let Ok(v) = value.parse() {
                    self.enable_rules = Some(v);
                }
            }
            "ruleContexts" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.rule_contexts = Some(v);
                }
            }
            "enableABTest" => {
                if let Ok(v) = value.parse() {
                    self.enable_ab_test = Some(v);
                }
            }
            "percentileComputation" => {
                if let Ok(v) = value.parse() {
                    self.percentile_computation = Some(v);
                }
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_language_param.
    fn apply_language_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "removeStopWords" => {
                if let Ok(v) =
                    serde_json::from_str::<flapjack::query::stopwords::RemoveStopWordsValue>(value)
                {
                    self.remove_stop_words = Some(v);
                }
            }
            "ignorePlurals" => {
                if let Ok(v) =
                    serde_json::from_str::<flapjack::query::plurals::IgnorePluralsValue>(value)
                {
                    self.ignore_plurals = Some(v);
                }
            }
            "queryLanguages" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.query_languages = Some(v);
                }
            }
            "naturalLanguages" => {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(value) {
                    self.natural_languages = Some(v);
                }
            }
            "decompoundQuery" => {
                if let Ok(v) = value.parse() {
                    self.decompound_query = Some(v);
                }
            }
            _ => return false,
        }
        true
    }

    /// TODO: Document SearchRequest.apply_search_mode_param.
    fn apply_search_mode_param(&mut self, key: &str, value: &str) -> bool {
        match key {
            "mode" => {
                self.mode = match value {
                    "neuralSearch" => Some(flapjack::index::settings::IndexMode::NeuralSearch),
                    "keywordSearch" => Some(flapjack::index::settings::IndexMode::KeywordSearch),
                    _ => None,
                };
            }
            "hybrid" => {
                if let Ok(mut h) = serde_json::from_str::<HybridSearchParams>(value) {
                    h.clamp_ratio();
                    self.hybrid = Some(h);
                }
            }
            "similarQuery" => {
                self.similar_query = Some(value.to_string());
            }
            _ => return false,
        }
        true
    }

    /// Build `GeoParams` from the request's geo-related fields.
    ///
    /// Bounding box and polygon constraints take priority over `aroundLatLng`. When `aroundLatLng`
    /// is active, `aroundRadius`, `aroundPrecision`, and `minimumAroundRadius` are resolved;
    /// otherwise they are ignored. `aroundLatLngViaIP` logs a trace warning when no upstream
    /// GeoIP resolution occurred.
    pub fn build_geo_params(&self) -> flapjack::query::geo::GeoParams {
        use flapjack::query::geo::*;

        let has_bbox = self.inside_bounding_box.is_some();
        let has_poly = self.inside_polygon.is_some();

        let bounding_boxes = self
            .inside_bounding_box
            .as_ref()
            .map(parse_bounding_boxes)
            .unwrap_or_default();

        let polygons = self
            .inside_polygon
            .as_ref()
            .map(parse_polygons)
            .unwrap_or_default();

        let around = if has_bbox || has_poly {
            None
        } else if let Some(point) = self
            .around_lat_lng
            .as_ref()
            .and_then(|s| parse_around_lat_lng(s))
        {
            Some(point)
        } else if self.around_lat_lng_via_ip == Some(true) {
            tracing::trace!("[GEO] aroundLatLngViaIP=true but was not resolved upstream — no GeoIP database or IP lookup failed");
            None
        } else {
            None
        };

        let around_radius = if around.is_some() {
            self.around_radius.as_ref().and_then(parse_around_radius)
        } else {
            None
        };

        let around_precision = self
            .around_precision
            .as_ref()
            .map(parse_around_precision)
            .unwrap_or_default();

        let minimum_around_radius = if around.is_some() && around_radius.is_none() {
            self.minimum_around_radius
        } else {
            None
        };

        GeoParams {
            around,
            around_radius,
            bounding_boxes,
            polygons,
            around_precision,
            minimum_around_radius,
        }
    }
}
