//! Stub summary for evaluation.rs.
use super::*;
use std::collections::HashMap;

struct RuleEvaluationContext<'a> {
    context: Option<&'a [String]>,
    active_filters: Option<&'a Filter>,
    synonyms: Option<&'a SynonymStore>,
}

struct RuleEvaluationState {
    effects: RuleEffects,
    merged_rule_rendering_content: Option<serde_json::Value>,
    current_query: String,
    stop_processing: bool,
}

impl RuleEvaluationState {
    fn new(query_text: &str) -> Self {
        Self {
            effects: RuleEffects::default(),
            merged_rule_rendering_content: None,
            current_query: query_text.to_string(),
            stop_processing: false,
        }
    }

    fn finish(mut self) -> RuleEffects {
        self.effects.pins.sort_by_key(|(_, pos)| *pos);
        self.effects.rendering_content = self.merged_rule_rendering_content;
        self.effects
    }
}

impl RuleStore {
    /// Get rules sorted by objectID for deterministic processing order.
    fn sorted_rules(&self) -> Vec<&Rule> {
        let mut rules: Vec<&Rule> = self.rules.values().collect();
        rules.sort_by(|a, b| a.object_id.cmp(&b.object_id));
        rules
    }

    /// Evaluate all enabled, valid rules against the query in objectID-sorted order and accumulate their effects.
    ///
    /// Apply first-match-wins semantics for scalar params (query edits, filters, hits_per_page, geo params). Accumulate list params (pins, hides, user data, facet/numeric/optional/tag filters). Merge rendering content across rules with later rules overriding conflicting keys. Conditionless rules skip promotes, query edits, and automatic facet filters. Query edits mutate the working query for downstream rule pattern matching.
    fn evaluate_rules(
        &self,
        query_text: &str,
        context: Option<&[String]>,
        active_filters: Option<&Filter>,
        synonyms: Option<&SynonymStore>,
    ) -> RuleEffects {
        let evaluation_context = RuleEvaluationContext {
            context,
            active_filters,
            synonyms,
        };
        let mut state = RuleEvaluationState::new(query_text);
        for rule in self.sorted_rules() {
            if state.stop_processing {
                break;
            }

            if !rule.matches(
                &state.current_query,
                evaluation_context.context,
                evaluation_context.active_filters,
                evaluation_context.synonyms,
            ) {
                continue;
            }

            self.apply_matching_rule(rule, &evaluation_context, &mut state);
        }

        state.finish()
    }

    /// Apply a matching rule's consequence to the search context: promotes, parameter
    /// overrides, query edits, facet captures, and automatic facet filters.
    /// Mutates `RuleEffects` in place.
    fn apply_matching_rule(
        &self,
        rule: &Rule,
        evaluation_context: &RuleEvaluationContext<'_>,
        state: &mut RuleEvaluationState,
    ) {
        state.effects.applied_rules.push(rule.object_id.clone());
        let is_conditionless = rule.conditions.is_empty();

        self.apply_rule_promotes(rule, is_conditionless, &mut state.effects);
        self.apply_rule_hides_and_user_data(rule, &mut state.effects);

        if let Some(params) = &rule.consequence.params {
            self.apply_rule_params(rule, params, evaluation_context, state, is_conditionless);
        }
    }

    /// Append promoted object IDs and first-match filter-promotes to
    /// `RuleEffects`; hide/user-data consequences are handled separately.
    fn apply_rule_promotes(&self, rule: &Rule, is_conditionless: bool, effects: &mut RuleEffects) {
        if is_conditionless {
            return;
        }

        if effects.filter_promotes.is_none() {
            effects.filter_promotes = rule.consequence.filter_promotes;
        }

        if let Some(promote) = &rule.consequence.promote {
            for promotion in promote {
                match promotion {
                    Promote::Single {
                        object_id,
                        position,
                    } => effects.pins.push((object_id.clone(), *position)),
                    Promote::Multiple {
                        object_ids,
                        position,
                    } => {
                        for (index, object_id) in object_ids.iter().enumerate() {
                            effects.pins.push((object_id.clone(), position + index));
                        }
                    }
                }
            }
        }
    }

    fn apply_rule_hides_and_user_data(&self, rule: &Rule, effects: &mut RuleEffects) {
        if let Some(hidden_documents) = &rule.consequence.hide {
            for hidden in hidden_documents.iter().take(MAX_HIDDEN_OBJECT_IDS_PER_RULE) {
                effects.hidden.push(hidden.object_id.clone());
            }
        }

        if let Some(user_data) = &rule.consequence.user_data {
            effects.user_data.push(user_data.clone());
        }
    }

    /// Apply the rule's `params` object to `RuleEffects`: scalar fields (query,
    /// filters) use first-match-wins semantics; list fields (optionalFilters,
    /// facetFilters) accumulate across rules.
    fn apply_rule_params(
        &self,
        rule: &Rule,
        params: &ConsequenceParams,
        evaluation_context: &RuleEvaluationContext<'_>,
        state: &mut RuleEvaluationState,
        is_conditionless: bool,
    ) {
        if !is_conditionless {
            self.apply_rule_query_edit(params, state);
            self.apply_rule_facet_captures(rule, params, evaluation_context, state);
        }

        self.apply_rule_scalar_params(params, &mut state.effects);
        self.merge_rule_rendering_content(params, &mut state.merged_rule_rendering_content);
    }

    /// If the rule has a non-empty `query` in `params`, apply it as a query replacement.
    /// Otherwise, fall back to pattern-based remove/replace edits derived from the
    /// rule's condition.
    fn apply_rule_query_edit(&self, params: &ConsequenceParams, state: &mut RuleEvaluationState) {
        if state.effects.query_edits.is_some() {
            return;
        }

        let Some(query) = &params.query else {
            return;
        };

        state.effects.query_edits = Some(query.clone());
        match query {
            ConsequenceQuery::Literal(query_literal) => {
                if query_literal != &state.current_query {
                    state.effects.rewritten_query = Some(query_literal.clone());
                }
                state.current_query = query_literal.clone();
                state.stop_processing = true;
            }
            ConsequenceQuery::Edits { remove, edits } => {
                let next_query = apply_query_edits_to_text(
                    &state.current_query,
                    remove.as_deref(),
                    edits.as_deref(),
                );
                if next_query != state.current_query {
                    state.effects.rewritten_query = Some(next_query.clone());
                }
                state.current_query = next_query;
            }
        }
    }

    /// Convert `{facet:attr}` placeholder captures into automatic facet filters and
    /// add them to `RuleEffects.automatic_facet_filters`.
    fn apply_rule_facet_captures(
        &self,
        rule: &Rule,
        params: &ConsequenceParams,
        evaluation_context: &RuleEvaluationContext<'_>,
        state: &mut RuleEvaluationState,
    ) {
        let has_auto_facet_filters = params.automatic_facet_filters.is_some()
            || params.automatic_optional_facet_filters.is_some();
        let rule_captures = if has_auto_facet_filters {
            rule.extract_matching_facet_captures(
                &state.current_query,
                evaluation_context.context,
                evaluation_context.active_filters,
                evaluation_context.synonyms,
            )
        } else {
            HashMap::new()
        };

        self.apply_automatic_facet_filters(params, &rule_captures, &mut state.effects);
        for (attribute, value) in rule_captures {
            state
                .effects
                .facet_captures
                .entry(attribute)
                .or_insert(value);
        }
    }

    /// Parse the rule's `automaticFacetFilters` field and add each entry to
    /// `RuleEffects`. Each filter specifies an attribute, whether it is disjunctive,
    /// and an optional score.
    fn apply_automatic_facet_filters(
        &self,
        params: &ConsequenceParams,
        rule_captures: &HashMap<String, String>,
        effects: &mut RuleEffects,
    ) {
        if let Some(automatic_facet_filters) = &params.automatic_facet_filters {
            effects
                .automatic_facet_filters
                .extend(automatic_facet_filters.clone());

            for facet_filter in automatic_facet_filters {
                if let Some(captured_value) = rule_captures.get(&facet_filter.facet) {
                    let expression = if facet_filter.negative == Some(true) {
                        format!("NOT {}:{}", facet_filter.facet, captured_value)
                    } else {
                        format!("{}:{}", facet_filter.facet, captured_value)
                    };
                    effects.generated_facet_filters.push(GeneratedFacetFilter {
                        expression,
                        disjunctive: facet_filter.disjunctive.unwrap_or(false),
                    });
                }
            }
        }

        if let Some(automatic_optional_facet_filters) = &params.automatic_optional_facet_filters {
            effects
                .automatic_optional_facet_filters
                .extend(automatic_optional_facet_filters.clone());

            for optional_filter in automatic_optional_facet_filters {
                if let Some(captured_value) = rule_captures.get(&optional_filter.facet) {
                    effects.generated_optional_facet_filters.push((
                        optional_filter.facet.clone(),
                        captured_value.clone(),
                        optional_filter.score.unwrap_or(1),
                    ));
                }
            }
        }
    }

    /// Apply first-match-wins scalar parameters from a rule's `params` to `RuleEffects`.
    /// Skips parameters that have already been set by a higher-priority rule.
    fn apply_rule_scalar_params(&self, params: &ConsequenceParams, effects: &mut RuleEffects) {
        if effects.filters.is_none() {
            if let Some(rule_filters) = &params.filters {
                effects.filters = Some(rule_filters.clone());
            }
        }

        if let Some(facet_filters) = &params.facet_filters {
            effects.facet_filters.push(facet_filters.clone());
        }

        if let Some(numeric_filters) = &params.numeric_filters {
            effects.numeric_filters.push(numeric_filters.clone());
        }

        if let Some(optional_filters) = &params.optional_filters {
            effects.optional_filters.push(optional_filters.clone());
        }

        if let Some(tag_filters) = &params.tag_filters {
            effects.tag_filters.push(tag_filters.clone());
        }

        if effects.around_lat_lng.is_none() {
            if let Some(around_lat_lng) = &params.around_lat_lng {
                effects.around_lat_lng = Some(around_lat_lng.clone());
            }
        }

        if effects.around_radius.is_none() {
            if let Some(around_radius) = &params.around_radius {
                effects.around_radius = Some(around_radius.clone());
            }
        }

        if effects.hits_per_page.is_none() {
            if let Some(hits_per_page) = params.hits_per_page {
                effects.hits_per_page = Some(hits_per_page);
            }
        }

        if effects.restrict_searchable_attributes.is_none() {
            if let Some(restrict_searchable_attributes) = &params.restrict_searchable_attributes {
                effects.restrict_searchable_attributes =
                    Some(restrict_searchable_attributes.clone());
            }
        }
    }

    fn merge_rule_rendering_content(
        &self,
        params: &ConsequenceParams,
        merged_rule_rendering_content: &mut Option<serde_json::Value>,
    ) {
        let Some(rule_rendering_content) = &params.rendering_content else {
            return;
        };

        if let Some(merged) = merged_rule_rendering_content.as_mut() {
            merge_json_values(merged, rule_rendering_content);
        } else {
            *merged_rule_rendering_content = Some(rule_rendering_content.clone());
        }
    }

    pub fn apply_rules(
        &self,
        query_text: &str,
        context: Option<&[String]>,
        active_filters: Option<&Filter>,
        synonyms: Option<&SynonymStore>,
    ) -> RuleEffects {
        self.evaluate_rules(query_text, context, active_filters, synonyms)
    }

    pub fn apply_query_rewrite(
        &self,
        query_text: &str,
        context: Option<&[String]>,
        active_filters: Option<&Filter>,
        synonyms: Option<&SynonymStore>,
    ) -> Option<String> {
        self.evaluate_rules(query_text, context, active_filters, synonyms)
            .rewritten_query
    }
}

#[cfg(test)]
mod tests {
    /// TODO: Document apply_rule_promotes_doc_stays_scoped_to_promotions.
    #[test]
    fn apply_rule_promotes_doc_stays_scoped_to_promotions() {
        let source = include_str!("evaluation.rs");
        let before_fn = source
            .split("fn apply_rule_promotes(&self, rule: &Rule, is_conditionless: bool, effects: &mut RuleEffects)")
            .next()
            .unwrap_or_default();
        let doc_window = before_fn
            .lines()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            doc_window.contains("filter-promotes"),
            "apply_rule_promotes docs must describe only promote/filter-promotes behavior"
        );
        assert!(
            !doc_window.contains("hidden object IDs"),
            "apply_rule_promotes docs must not claim hidden-object handling"
        );
    }
}
