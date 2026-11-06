use super::query_execution::QueryRuleCount;
use crate::index::rules::RuleEffects;
use crate::types::ScoredDocument;
use std::collections::HashSet;

pub(super) struct RuleCountContext<'a> {
    pub queries: &'a [QueryRuleCount],
    pub effective_limit: usize,
}

impl RuleCountContext<'_> {
    /// Keep the existing expanded-query aggregation basis. In particular, a
    /// union of hidden IDs must never be subtracted from a max-of-variants total.
    /// This corrects rule deltas; it does not make the existing max approximation
    /// an exact union count. Distinct group counting is deliberately excluded.
    pub(super) fn adjusted_total(
        &self,
        original_ids: &HashSet<String>,
        documents: &[ScoredDocument],
        effects: &RuleEffects,
    ) -> usize {
        let pin_ids: HashSet<&str> = effects.pins.iter().map(|(id, _)| id.as_str()).collect();
        let emitted: HashSet<&str> = documents
            .iter()
            .map(|doc| doc.document.id.as_str())
            .filter(|id| pin_ids.contains(id))
            .collect();
        let removed: HashSet<&str> = effects
            .hidden
            .iter()
            .map(String::as_str)
            .filter(|id| !emitted.contains(id))
            .collect();
        let mut adjusted_variants = self.queries.iter().map(|query| {
            query.total.saturating_sub(
                query
                    .matched_rule_ids
                    .iter()
                    .filter(|id| removed.contains(id.as_str()))
                    .count(),
            )
        });
        let organic = if self.queries.len() == 1 {
            adjusted_variants.next().unwrap_or(0)
        } else if original_ids.len() < self.effective_limit {
            original_ids
                .iter()
                .filter(|id| !removed.contains(id.as_str()))
                .count()
        } else {
            adjusted_variants.max().unwrap_or(original_ids.len())
        };
        let additions = emitted
            .iter()
            .filter(|id| {
                !self
                    .queries
                    .iter()
                    .any(|query| query.matched_rule_ids.contains(**id))
            })
            .count();
        organic.saturating_add(additions)
    }
}
