use super::QueryExecutor;
use crate::error::Result;
use crate::index::rules::RuleEffects;
use crate::types::{Filter, ScoredDocument};
use tantivy::query::{BooleanQuery, Occur, Query as TantivyQuery, TermQuery};
use tantivy::schema::IndexRecordOption;
use tantivy::Searcher;

impl QueryExecutor {
    /// Apply rule effects (hide, pin, promote) to the scored result set, removing
    /// hidden docs and inserting promoted/pinned docs at their target positions.
    pub(crate) fn apply_rules_to_results(
        &self,
        searcher: &Searcher,
        mut documents: Vec<ScoredDocument>,
        effects: &RuleEffects,
        active_filter: Option<&Filter>,
    ) -> Result<Vec<ScoredDocument>> {
        if !effects.hidden.is_empty() {
            documents.retain(|doc| !effects.hidden.contains(&doc.document.id));
        }

        if effects.pins.is_empty() {
            return Ok(documents);
        }

        let compiled_filter_query = if effects.filter_promotes == Some(true) {
            active_filter
                .map(|filter| {
                    self.filter_compiler
                        .compile(filter, self.settings.as_deref())
                })
                .transpose()?
        } else {
            None
        };

        let schema = searcher.schema();
        let id_field = schema
            .get_field("_id")
            .map_err(|_| crate::error::FlapjackError::FieldNotFound("_id".to_string()))?;

        let mut pinned_docs = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        for (pin_id, target_pos) in &effects.pins {
            if seen_ids.contains(pin_id) {
                continue;
            }
            seen_ids.insert(pin_id.clone());

            if let Some(pos) = documents.iter().position(|d| &d.document.id == pin_id) {
                pinned_docs.push((documents.remove(pos), *target_pos));
            } else {
                let term = tantivy::Term::from_field_text(id_field, pin_id);
                let id_query: Box<dyn TantivyQuery> =
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic));
                let lookup_query: Box<dyn TantivyQuery> =
                    if let Some(filter_query) = compiled_filter_query.as_ref() {
                        Box::new(BooleanQuery::new(vec![
                            (Occur::Must, id_query),
                            (Occur::Must, filter_query.box_clone()),
                        ]))
                    } else {
                        id_query
                    };
                // Deliberately not routed through `QueryExecutor::search_bounded`:
                // this is a single-document lookup by `_id` that matches at most
                // one doc in one segment, so fanning it across a worker pool
                // costs more in dispatch than it saves, and it runs once per pin
                // rather than once per query phase. The bounded seam owns the
                // result-set collectors; this stays a direct single lookup.
                let top_docs = searcher.search(
                    lookup_query.as_ref(),
                    &tantivy::collector::TopDocs::with_limit(1).order_by_score(),
                )?;

                if !top_docs.is_empty() {
                    let doc_address = top_docs[0].1;
                    let retrieved_doc = searcher.doc(doc_address)?;
                    let document =
                        self.converter
                            .from_tantivy(retrieved_doc, schema, pin_id.clone())?;
                    pinned_docs.push((
                        ScoredDocument {
                            document,
                            score: f32::MAX,
                        },
                        *target_pos,
                    ));
                }
            }
        }

        pinned_docs.sort_by(|a, b| {
            a.1.cmp(&b.1).then_with(|| {
                let a_idx = effects
                    .pins
                    .iter()
                    .position(|(id, _)| id == &a.0.document.id)
                    .unwrap_or(0);
                let b_idx = effects
                    .pins
                    .iter()
                    .position(|(id, _)| id == &b.0.document.id)
                    .unwrap_or(0);
                a_idx.cmp(&b_idx)
            })
        });

        let mut result = Vec::new();
        let mut doc_iter = documents.into_iter();
        let mut next_pin_idx = 0;

        for target_pos in 0..1000 {
            while next_pin_idx < pinned_docs.len() && pinned_docs[next_pin_idx].1 == target_pos {
                result.push(pinned_docs[next_pin_idx].0.clone());
                next_pin_idx += 1;
            }

            while result.len() == target_pos {
                if let Some(doc) = doc_iter.next() {
                    result.push(doc);
                } else {
                    break;
                }
            }

            if result.len() <= target_pos {
                break;
            }
        }

        while next_pin_idx < pinned_docs.len() {
            result.push(pinned_docs[next_pin_idx].0.clone());
            next_pin_idx += 1;
        }

        result.extend(doc_iter);

        Ok(result)
    }
}
