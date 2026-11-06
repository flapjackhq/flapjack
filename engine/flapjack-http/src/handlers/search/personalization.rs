use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use flapjack::personalization::{
    extract_facet_values, PersonalizationProfile, PersonalizationProfileStore,
    PersonalizationStrategy,
};
use flapjack::types::{Document, ScoredDocument};

use crate::dto::SearchRequest;
use crate::handlers::AppState;

pub(super) const PERSONALIZATION_FILTER_AFFINITY: u32 = 20;
pub(super) const PERSONALIZATION_RERANK_BUFFER: usize = 50;

#[derive(Debug, Clone)]
pub(super) struct PersonalizationContext {
    pub(super) affinity_scores: HashMap<String, HashMap<String, u32>>,
    pub(super) impact_multiplier: f32,
}

pub(super) fn personalization_strategy_path(state: &AppState) -> PathBuf {
    PersonalizationProfileStore::new(&*state.manager.base_path).strategy_path()
}

pub(super) fn parse_personalization_filter(filter: &str) -> Option<(String, String)> {
    let (facet, value) = filter.trim().split_once(':')?;
    let facet = facet.trim();
    let value = value.trim().trim_matches('"').trim_matches('\'');
    if facet.is_empty() || value.is_empty() {
        return None;
    }
    Some((facet.to_string(), value.to_string()))
}

pub(super) fn personalization_filters_to_affinity_map(
    filters: &[String],
) -> HashMap<String, HashMap<String, u32>> {
    let mut scores: HashMap<String, HashMap<String, u32>> = HashMap::new();
    for filter in filters {
        let Some((facet, value)) = parse_personalization_filter(filter) else {
            continue;
        };
        scores
            .entry(facet)
            .or_default()
            .insert(value, PERSONALIZATION_FILTER_AFFINITY);
    }
    scores
}

pub(super) fn personalization_affinity_for_document(
    doc: &Document,
    affinity_scores: &HashMap<String, HashMap<String, u32>>,
) -> f32 {
    let mut total_affinity = 0f32;
    for (facet, values_scores) in affinity_scores {
        let extracted = extract_facet_values(doc, facet);
        for value in extracted {
            if let Some(score) = values_scores.get(&value) {
                total_affinity += *score as f32;
            }
        }
    }
    total_affinity
}

/// Adjusts document scores by personalization affinity (facet-value match scores
/// weighted by strategy and query impact), then re-sorts preserving tie-break stability.
pub(super) fn apply_personalization_boost_in_tiers(
    documents: &mut [ScoredDocument],
    context: &PersonalizationContext,
) {
    if documents.len() < 2 {
        return;
    }

    // The engine already enforces hard ranking tiers (typo bucket, attribute
    // bucket, exact-vs-prefix) via bucket-sort.  After those tiers, documents
    // are ordered by tuned BM25 which varies per document even within the same
    // bucket.  We apply personalization as a score adjustment across all
    // results — the engine's tier boundaries remain intact because the boost
    // magnitude is small relative to inter-tier score differences.
    let mut scored: Vec<(usize, ScoredDocument, f32)> = documents
        .iter()
        .cloned()
        .enumerate()
        .map(|(idx, doc)| {
            let affinity =
                personalization_affinity_for_document(&doc.document, &context.affinity_scores);
            let adjusted_score = doc.score + (affinity * context.impact_multiplier);
            (idx, doc, adjusted_score)
        })
        .collect();

    scored.sort_by(|a, b| {
        b.2.partial_cmp(&a.2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    for (offset, (_, doc, _)) in scored.into_iter().enumerate() {
        documents[offset] = doc;
    }
}

pub(super) fn index_allows_personalization(
    settings: Option<&flapjack::index::settings::IndexSettings>,
) -> bool {
    settings
        .and_then(|s| s.enable_personalization)
        .unwrap_or(true)
}

fn query_personalization_impact(
    req: &SearchRequest,
    index_settings: Option<&flapjack::index::settings::IndexSettings>,
) -> Option<u32> {
    if req.enable_personalization != Some(true) {
        return None;
    }
    if !index_allows_personalization(index_settings) {
        return None;
    }

    let query_impact = req.personalization_impact.unwrap_or(100);
    (query_impact > 0).then_some(query_impact)
}

fn load_personalization_strategy(state: &Arc<AppState>) -> Option<PersonalizationStrategy> {
    let strategy_data = std::fs::read_to_string(personalization_strategy_path(state)).ok()?;
    serde_json::from_str(&strategy_data).ok()
}

pub(super) fn compute_personalization_impact_multiplier(
    strategy_impact: u32,
    query_impact: u32,
) -> Option<f32> {
    if strategy_impact == 0 || query_impact == 0 {
        return None;
    }

    let multiplier = (strategy_impact as f32 / 100.0) * (query_impact as f32 / 100.0);
    (multiplier > 0.0).then_some(multiplier)
}

pub(super) fn convert_profile_scores_to_affinity_map(
    profile_scores: BTreeMap<String, BTreeMap<String, u32>>,
) -> HashMap<String, HashMap<String, u32>> {
    profile_scores
        .into_iter()
        .map(|(facet, values)| (facet, values.into_iter().collect::<HashMap<_, _>>()))
        .collect::<HashMap<_, _>>()
}

/// Loads a user's personalization profile: first attempts to compute a fresh profile
/// from analytics data, falling back to the on-disk cached profile if computation fails.
async fn load_profile_from_engine_or_cache(
    state: &Arc<AppState>,
    strategy: &PersonalizationStrategy,
    user_token: &str,
    store: &PersonalizationProfileStore,
) -> Option<PersonalizationProfile> {
    if let Some(analytics_engine) = state.analytics_engine.as_ref() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        match flapjack::personalization::profile::compute_and_cache_profile(
            store,
            &state.manager,
            analytics_engine,
            strategy,
            user_token,
            now_ms,
        )
        .await
        {
            Ok(computed_profile) => return Some(computed_profile),
            Err(err) => {
                tracing::warn!(
                    "failed to compute personalization profile for '{}': {}",
                    user_token,
                    err
                );
            }
        }
    }

    match store.load_profile(user_token) {
        Ok(cached_profile) => cached_profile,
        Err(err) => {
            tracing::warn!(
                "failed to load cached personalization profile for '{}': {}",
                user_token,
                err
            );
            None
        }
    }
}

async fn resolve_profile_affinity_scores(
    state: &Arc<AppState>,
    strategy: &PersonalizationStrategy,
    user_token: &str,
) -> Option<HashMap<String, HashMap<String, u32>>> {
    let store = PersonalizationProfileStore::new(&*state.manager.base_path);
    let profile = load_profile_from_engine_or_cache(state, strategy, user_token, &store).await?;
    if profile.scores.is_empty() {
        return None;
    }

    Some(convert_profile_scores_to_affinity_map(profile.scores))
}

/// Resolves the full personalization context for a search: checks enablement, loads
/// the strategy, computes impact multiplier, and resolves affinity scores from
/// explicit filters or the user's computed profile.
pub(super) async fn resolve_personalization_context(
    state: &Arc<AppState>,
    req: &SearchRequest,
    index_settings: Option<&flapjack::index::settings::IndexSettings>,
) -> Option<PersonalizationContext> {
    let query_impact = query_personalization_impact(req, index_settings)?;
    let strategy = load_personalization_strategy(state)?;
    let impact_multiplier =
        compute_personalization_impact_multiplier(strategy.personalization_impact, query_impact)?;

    if let Some(filters) = req.personalization_filters.as_ref() {
        let filters_affinity = personalization_filters_to_affinity_map(filters);
        return (!filters_affinity.is_empty()).then_some(PersonalizationContext {
            affinity_scores: filters_affinity,
            impact_multiplier,
        });
    }

    let user_token = req.user_token.as_deref()?;
    let affinity_scores = resolve_profile_affinity_scores(state, &strategy, user_token).await?;

    Some(PersonalizationContext {
        affinity_scores,
        impact_multiplier,
    })
}
