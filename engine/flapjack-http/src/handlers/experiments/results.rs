//! Stub summary for results.rs.
use super::*;
use flapjack::experiments::{metrics, stats};
use std::collections::HashMap;
use std::path::PathBuf;

/// Returns statistical results for an experiment addressed by numeric ID or UUID.
#[utoipa::path(
    get,
    path = "/2/abtests/{id}/results",
    tag = "experiments",
    params(
        ("id" = String, Path, description = "Experiment identifier (numeric ID or UUID)")
    ),
    responses(
        (status = 200, description = "Experiment statistical results", body = ResultsResponse),
        (status = 500, description = "Experiment missing numeric ID mapping"),
        (status = 404, description = "Experiment not found"),
        (status = 503, description = "Experiment store unavailable")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_experiment_results(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let (store, uuid, _numeric_id) = match resolve_store_and_experiment_id(&state, &id) {
        Ok(values) => values,
        Err(response) => return response,
    };

    let experiment = match store.get(&uuid) {
        Ok(exp) => exp,
        Err(err) => return experiment_error_to_response(err),
    };

    let analytics_data_dir = resolve_analytics_data_dir(&state);
    let index_names = resolve_experiment_index_names(&experiment);
    let experiment_metrics =
        fetch_experiment_metrics(&experiment, analytics_data_dir.as_ref(), &index_names).await;
    let covariates = fetch_cuped_covariates(&experiment, analytics_data_dir.as_ref()).await;
    let interleaving_metrics =
        fetch_interleaving_metrics(&experiment, analytics_data_dir.as_ref(), &index_names).await;

    // Compute gate, stats, and build response
    let response = build_results_response(
        &experiment,
        experiment_metrics.as_ref(),
        covariates.as_ref(),
        interleaving_metrics.as_ref(),
    );
    Json(response).into_response()
}

fn resolve_analytics_data_dir(state: &AppState) -> Option<PathBuf> {
    state
        .analytics_engine
        .as_ref()
        .map(|engine| engine.config().data_dir.clone())
}

pub(super) fn resolve_experiment_index_names(experiment: &Experiment) -> Vec<String> {
    let mut index_names = vec![experiment.index_name.clone()];
    if let Some(variant_index) = experiment.variant.index_name.as_ref() {
        if variant_index != &experiment.index_name {
            index_names.push(variant_index.clone());
        }
    }
    index_names
}

pub(super) fn index_name_refs(index_names: &[String]) -> Vec<&str> {
    index_names.iter().map(String::as_str).collect()
}

/// Queries analytics data to compute A/B experiment metrics (click rate, conversion rate) for the experiment period and indexes.
async fn fetch_experiment_metrics(
    experiment: &Experiment,
    analytics_data_dir: Option<&PathBuf>,
    index_names: &[String],
) -> Option<metrics::ExperimentMetrics> {
    let data_dir = analytics_data_dir?;
    let index_name_refs = index_name_refs(index_names);

    match metrics::get_experiment_metrics(
        &experiment.id,
        &index_name_refs,
        data_dir,
        experiment.winsorization_cap,
    )
    .await
    {
        Ok(metrics) => Some(metrics),
        Err(error) => {
            tracing::warn!("Failed to fetch experiment metrics: {}", error);
            None
        }
    }
}

/// Fetches pre-experiment baseline metrics used as CUPED covariates to reduce variance in A/B test statistical analysis.
async fn fetch_cuped_covariates(
    experiment: &Experiment,
    analytics_data_dir: Option<&PathBuf>,
) -> Option<HashMap<String, f64>> {
    let data_dir = analytics_data_dir?;
    let started_at = experiment.started_at?;

    match metrics::get_pre_experiment_covariates(
        &experiment.index_name,
        data_dir,
        &experiment.primary_metric,
        started_at,
        14, // 14-day lookback window (industry standard)
    )
    .await
    {
        Ok(covariates) if !covariates.is_empty() => Some(covariates),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!("Failed to fetch CUPED covariates: {}", error);
            None
        }
    }
}

/// Queries interleaving click attribution metrics for experiments that use team-draft interleaving to compare ranking variants.
async fn fetch_interleaving_metrics(
    experiment: &Experiment,
    analytics_data_dir: Option<&PathBuf>,
    index_names: &[String],
) -> Option<metrics::InterleavingMetrics> {
    if experiment.interleaving != Some(true) {
        return None;
    }
    let data_dir = analytics_data_dir?;
    let index_name_refs = index_name_refs(index_names);

    match metrics::get_interleaving_metrics(&index_name_refs, data_dir, &experiment.id).await {
        Ok(interleaving_metrics) => interleaving_metrics,
        Err(error) => {
            tracing::warn!("Failed to fetch interleaving metrics: {}", error);
            None
        }
    }
}

/// Compute the primary metric value for an arm.
fn arm_primary_metric(arm: &metrics::ArmMetrics, metric: &PrimaryMetric) -> f64 {
    match metric {
        PrimaryMetric::Ctr => arm.ctr,
        PrimaryMetric::ConversionRate => arm.conversion_rate,
        PrimaryMetric::RevenuePerSearch => arm.revenue_per_search,
        PrimaryMetric::ZeroResultRate => arm.zero_result_rate,
        PrimaryMetric::AbandonmentRate => arm.abandonment_rate,
    }
}

fn arm_delta_samples<'a>(arm: &'a metrics::ArmMetrics, metric: &PrimaryMetric) -> &'a [(f64, f64)] {
    match metric {
        PrimaryMetric::Ctr => arm.per_user_ctrs.as_slice(),
        PrimaryMetric::ConversionRate => arm.per_user_conversion_rates.as_slice(),
        PrimaryMetric::ZeroResultRate => arm.per_user_zero_result_rates.as_slice(),
        PrimaryMetric::AbandonmentRate => arm.per_user_abandonment_rates.as_slice(),
        PrimaryMetric::RevenuePerSearch => &[],
    }
}

fn metric_prefers_lower(metric: &PrimaryMetric) -> bool {
    matches!(
        metric,
        PrimaryMetric::ZeroResultRate | PrimaryMetric::AbandonmentRate
    )
}

/// Flip the sign of a stat result for metrics where lower values are better.
///
/// For `ZeroResultRate` and `AbandonmentRate`, negates z-score, relative improvement,
/// and absolute improvement, and swaps the winner label so that "variant wins" still
/// means the variant performed better in the desired direction.
fn orient_stat_for_metric(
    mut stat: flapjack::experiments::stats::StatResult,
    metric: &PrimaryMetric,
) -> flapjack::experiments::stats::StatResult {
    if metric_prefers_lower(metric) {
        stat.z_score = -stat.z_score;
        stat.relative_improvement = -stat.relative_improvement;
        stat.absolute_improvement = -stat.absolute_improvement;
        if stat.significant {
            stat.winner = stat.winner.map(|winner| {
                if winner == "variant" {
                    "control".to_string()
                } else {
                    "variant".to_string()
                }
            });
        }
    }
    stat
}

/// Compute the sample variance of per-user rates from (numerator, denominator) tuples.
fn rate_variance(samples: &[(f64, f64)]) -> f64 {
    let rates: Vec<f64> = samples
        .iter()
        .filter(|(_, d)| *d > 0.0)
        .map(|(n, d)| n / d)
        .collect();
    if rates.len() < 2 {
        return 0.0;
    }
    let mean = rates.iter().sum::<f64>() / rates.len() as f64;
    rates.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (rates.len() - 1) as f64
}

/// Attempt CUPED variance reduction on per-user ratio metric samples.
///
/// Returns `(cuped_applied, adjusted_control, adjusted_variant)`.
/// Falls back to raw if covariates unavailable, insufficient matched users,
/// or adjusted variance >= raw variance (Statsig safety check).
#[allow(clippy::type_complexity)]
fn try_cuped_adjustment(
    raw_control: &[(f64, f64)],
    raw_variant: &[(f64, f64)],
    control_ids: &[String],
    variant_ids: &[String],
    covariates: Option<&HashMap<String, f64>>,
) -> (bool, Option<Vec<(f64, f64)>>, Option<Vec<(f64, f64)>>) {
    let covs = match covariates {
        Some(c) if !c.is_empty() => c,
        _ => return (false, None, None),
    };

    // Require CUPED coverage threshold in BOTH arms; asymmetrical adjustment biases comparisons.
    let matched_count = |samples: &[(f64, f64)], ids: &[String]| -> usize {
        if samples.len() != ids.len() {
            return 0;
        }
        ids.iter()
            .enumerate()
            .filter(|(idx, uid)| samples[*idx].1 > 0.0 && covs.contains_key(uid.as_str()))
            .count()
    };
    let control_matched = matched_count(raw_control, control_ids);
    let variant_matched = matched_count(raw_variant, variant_ids);
    if control_matched < stats::CUPED_MIN_MATCHED_USERS
        || variant_matched < stats::CUPED_MIN_MATCHED_USERS
    {
        return (false, None, None);
    }

    let adj_control = stats::cuped_adjust(raw_control, control_ids, covs);
    let adj_variant = stats::cuped_adjust(raw_variant, variant_ids, covs);

    // Safety check: only use CUPED-adjusted values when adjusted variance is lower.
    // If CUPED increases variance (weak covariate correlation), fall back to raw.
    let raw_var = rate_variance(raw_control) + rate_variance(raw_variant);
    let adj_var = rate_variance(&adj_control) + rate_variance(&adj_variant);

    if adj_var < raw_var {
        (true, Some(adj_control), Some(adj_variant))
    } else {
        (false, None, None)
    }
}

/// Compute statistical gate state and progress response for the experiment.
///
/// Derives baseline rate from control arm metrics, computes required sample size,
/// elapsed time, and progress percentage. Returns the raw `StatGate` (needed by
/// downstream significance computation) alongside the fully-constructed `GateResponse`.
fn compute_gate_and_progress(
    experiment: &Experiment,
    metrics: Option<&metrics::ExperimentMetrics>,
) -> (stats::StatGate, GateResponse) {
    let baseline_rate = match metrics {
        Some(m) => arm_primary_metric(&m.control, &experiment.primary_metric).max(0.001),
        None => 0.1, // default baseline estimate
    };
    let sample_estimate = stats::required_sample_size(
        baseline_rate,
        0.05, // 5% MDE
        0.05, // alpha
        0.80, // power
        experiment.traffic_split,
    );

    let elapsed_days = experiment.started_at.map_or(0.0, |started| {
        let now_ms = chrono::Utc::now().timestamp_millis();
        (now_ms - started) as f64 / (1000.0 * 60.0 * 60.0 * 24.0)
    });

    let control_searches = metrics.map_or(0, |m| m.control.searches);
    let variant_searches = metrics.map_or(0, |m| m.variant.searches);
    let min_searches = control_searches.min(variant_searches);

    let gate = stats::StatGate::new(
        control_searches,
        variant_searches,
        sample_estimate.per_arm,
        elapsed_days,
        experiment.minimum_days,
    );

    let progress_pct = if sample_estimate.per_arm > 0 {
        ((min_searches as f64 / sample_estimate.per_arm as f64) * 100.0).min(100.0)
    } else {
        100.0
    };

    let estimated_days_remaining = if elapsed_days > 0.0 && min_searches > 0 && !gate.ready_to_read
    {
        let daily_rate = min_searches as f64 / elapsed_days;
        if daily_rate > 0.0 {
            let remaining_n = sample_estimate.per_arm.saturating_sub(min_searches);
            let days_for_n = remaining_n as f64 / daily_rate;
            let days_for_min = (experiment.minimum_days as f64 - elapsed_days).max(0.0);
            Some(days_for_n.max(days_for_min))
        } else {
            None
        }
    } else {
        None
    };

    let gate_response = GateResponse {
        minimum_n_reached: gate.minimum_n_reached,
        minimum_days_reached: gate.minimum_days_reached,
        ready_to_read: gate.ready_to_read,
        required_searches_per_arm: sample_estimate.per_arm,
        current_searches_per_arm: min_searches,
        progress_pct,
        estimated_days_remaining,
    };

    (gate, gate_response)
}

/// Compute Bayesian probability that the variant outperforms control.
///
/// Uses the primary metric's count data for beta-binomial computation.
/// For lower-is-better metrics (zero result rate, abandonment rate), the
/// probability is flipped so that higher values always mean "variant is better."
fn compute_bayesian(
    metrics: Option<&metrics::ExperimentMetrics>,
    primary_metric: &PrimaryMetric,
) -> Option<BayesianResponse> {
    metrics.map(|m| {
        let (a_success, a_total, b_success, b_total) = match primary_metric {
            PrimaryMetric::Ctr => (
                m.control.clicks,
                m.control.searches,
                m.variant.clicks,
                m.variant.searches,
            ),
            PrimaryMetric::ConversionRate => (
                m.control.conversions,
                m.control.searches,
                m.variant.conversions,
                m.variant.searches,
            ),
            PrimaryMetric::ZeroResultRate => (
                m.control.zero_result_searches,
                m.control.searches,
                m.variant.zero_result_searches,
                m.variant.searches,
            ),
            PrimaryMetric::AbandonmentRate => {
                let ctrl_with_results = m
                    .control
                    .searches
                    .saturating_sub(m.control.zero_result_searches);
                let var_with_results = m
                    .variant
                    .searches
                    .saturating_sub(m.variant.zero_result_searches);
                (
                    m.control.abandoned_searches,
                    ctrl_with_results,
                    m.variant.abandoned_searches,
                    var_with_results,
                )
            }
            PrimaryMetric::RevenuePerSearch => {
                // No natural count data for beta-binomial; fall back to CTR as directional signal
                (
                    m.control.clicks,
                    m.control.searches,
                    m.variant.clicks,
                    m.variant.searches,
                )
            }
        };
        let prob = stats::beta_binomial_prob_b_greater_a(a_success, a_total, b_success, b_total);
        let prob_variant_better = if metric_prefers_lower(primary_metric) {
            1.0 - prob
        } else {
            prob
        };
        BayesianResponse {
            prob_variant_better,
        }
    })
}

/// Compute frequentist significance and recommendation when sample gate is reached.
///
/// Applies CUPED variance reduction for ratio metrics when covariates are available,
/// falls back to raw delta-method z-test or Welch t-test (revenue). Returns the
/// significance response, a human-readable recommendation string, and whether
/// CUPED adjustment was applied.
fn compute_significance_and_recommendation(
    experiment: &Experiment,
    metrics: Option<&metrics::ExperimentMetrics>,
    covariates: Option<&HashMap<String, f64>>,
    gate: &stats::StatGate,
    srm: bool,
) -> (Option<SignificanceResponse>, Option<String>, bool) {
    if gate.minimum_n_reached {
        if let Some(m) = metrics {
            // Try CUPED adjustment for ratio metrics (not revenue, which uses Welch t-test)
            let (cuped_applied, adj_ctrl, adj_var) = match experiment.primary_metric {
                PrimaryMetric::RevenuePerSearch => (false, None, None),
                _ => try_cuped_adjustment(
                    arm_delta_samples(&m.control, &experiment.primary_metric),
                    arm_delta_samples(&m.variant, &experiment.primary_metric),
                    &m.control.per_user_ids,
                    &m.variant.per_user_ids,
                    covariates,
                ),
            };

            let raw_stat = match experiment.primary_metric {
                PrimaryMetric::RevenuePerSearch => {
                    stats::welch_t_test(&m.control.per_user_revenues, &m.variant.per_user_revenues)
                }
                _ => {
                    let ctrl_samples = adj_ctrl.as_deref().unwrap_or_else(|| {
                        arm_delta_samples(&m.control, &experiment.primary_metric)
                    });
                    let var_samples = adj_var.as_deref().unwrap_or_else(|| {
                        arm_delta_samples(&m.variant, &experiment.primary_metric)
                    });
                    stats::delta_method_z_test(ctrl_samples, var_samples)
                }
            };
            let stat = orient_stat_for_metric(raw_stat, &experiment.primary_metric);

            let rec = if srm {
                Some("Sample ratio mismatch detected — investigate assignment before declaring a winner.".to_string())
            } else if stat.significant {
                stat.winner.as_ref().map(|w| {
                    format!(
                        "Statistically significant result: {} arm wins on {}.",
                        w,
                        primary_metric_label(&experiment.primary_metric)
                    )
                })
            } else {
                Some(
                    "Not yet statistically significant. Consider continuing the experiment."
                        .to_string(),
                )
            };

            (
                Some(SignificanceResponse {
                    z_score: stat.z_score,
                    p_value: stat.p_value,
                    confidence: stat.confidence,
                    significant: stat.significant,
                    relative_improvement: stat.relative_improvement,
                    winner: stat.winner,
                }),
                rec,
                cuped_applied,
            )
        } else {
            (None, None, false)
        }
    } else {
        // Gate not ready: SRM warning as recommendation if detected, no significance yet.
        let rec = if srm {
            Some("Sample ratio mismatch detected — investigate assignment before declaring a winner.".to_string())
        } else {
            None
        };
        (None, rec, false)
    }
}

/// Check all metrics for guard-rail violations (>20% regression in variant vs control).
fn compute_guard_rail_alerts(
    metrics: Option<&metrics::ExperimentMetrics>,
) -> Vec<GuardRailAlertResponse> {
    let Some(m) = metrics else {
        return Vec::new();
    };

    const GUARD_RAIL_THRESHOLD: f64 = 0.20;

    let metric_checks: Vec<(&str, f64, f64, bool)> = vec![
        ("ctr", m.control.ctr, m.variant.ctr, false),
        (
            "conversionRate",
            m.control.conversion_rate,
            m.variant.conversion_rate,
            false,
        ),
        (
            "revenuePerSearch",
            m.control.revenue_per_search,
            m.variant.revenue_per_search,
            false,
        ),
        (
            "zeroResultRate",
            m.control.zero_result_rate,
            m.variant.zero_result_rate,
            true,
        ),
        (
            "abandonmentRate",
            m.control.abandonment_rate,
            m.variant.abandonment_rate,
            true,
        ),
    ];

    metric_checks
        .into_iter()
        .filter_map(|(name, ctrl, var, lower_is_better)| {
            stats::check_guard_rail(name, ctrl, var, lower_is_better, GUARD_RAIL_THRESHOLD).map(
                |alert| GuardRailAlertResponse {
                    metric_name: alert.metric_name,
                    control_value: alert.control_value,
                    variant_value: alert.variant_value,
                    drop_pct: alert.drop_pct,
                },
            )
        })
        .collect()
}

/// Build interleaving preference response for team-draft interleaving experiments.
fn compute_interleaving_response(
    experiment: &Experiment,
    interleaving_metrics: Option<&metrics::InterleavingMetrics>,
) -> Option<InterleavingResponse> {
    if experiment.interleaving != Some(true) {
        return None;
    }
    interleaving_metrics.map(|m| InterleavingResponse {
        delta_ab: m.preference.delta_ab,
        wins_control: m.preference.wins_a,
        wins_variant: m.preference.wins_b,
        ties: m.preference.ties,
        p_value: m.preference.p_value,
        significant: m.preference.p_value < 0.05,
        total_queries: m.total_queries,
        data_quality_ok: m.first_team_a_ratio >= 0.45 && m.first_team_a_ratio <= 0.55,
    })
}

/// TODO: Document build_results_response.
pub(super) fn build_results_response(
    experiment: &Experiment,
    metrics: Option<&metrics::ExperimentMetrics>,
    covariates: Option<&HashMap<String, f64>>,
    interleaving_metrics: Option<&metrics::InterleavingMetrics>,
) -> ResultsResponse {
    let (control_arm, variant_arm) = match metrics {
        Some(m) => (arm_to_response(&m.control), arm_to_response(&m.variant)),
        None => (empty_arm_response("control"), empty_arm_response("variant")),
    };

    let (gate, gate_response) = compute_gate_and_progress(experiment, metrics);

    let bayesian = compute_bayesian(metrics, &experiment.primary_metric);

    // SRM is always computed when metrics exist (early warning, independent of gate).
    let srm = metrics.is_some_and(|m| {
        stats::check_sample_ratio_mismatch(
            m.control.searches,
            m.variant.searches,
            experiment.traffic_split,
        )
    });

    let (significance, recommendation, cuped_applied) =
        compute_significance_and_recommendation(experiment, metrics, covariates, &gate, srm);

    let start_date = experiment.started_at.map(|ms| {
        chrono::DateTime::from_timestamp_millis(ms)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default()
    });
    let ended_at = experiment.stopped_at.map(|ms| {
        chrono::DateTime::from_timestamp_millis(ms)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default()
    });

    let guard_rail_alerts = compute_guard_rail_alerts(metrics);
    let interleaving = compute_interleaving_response(experiment, interleaving_metrics);

    ResultsResponse {
        experiment_id: experiment.id.clone(),
        name: experiment.name.clone(),
        status: experiment.status.clone(),
        index_name: experiment.index_name.clone(),
        start_date,
        ended_at,
        conclusion: experiment.conclusion.clone(),
        traffic_split: experiment.traffic_split,
        gate: gate_response,
        control: control_arm,
        variant: variant_arm,
        primary_metric: experiment.primary_metric.clone(),
        significance,
        bayesian,
        sample_ratio_mismatch: srm,
        guard_rail_alerts,
        cuped_applied,
        outlier_users_excluded: metrics.map_or(0, |m| m.outlier_users_excluded),
        no_stable_id_queries: metrics.map_or(0, |m| m.no_stable_id_queries),
        recommendation,
        interleaving,
    }
}

/// Convert internal `ArmMetrics` into the API-facing `ArmResponse` DTO.
///
/// Maps all metric fields (searches, clicks, conversions, revenue, rates) directly
/// from the aggregated arm metrics.
fn arm_to_response(arm: &metrics::ArmMetrics) -> ArmResponse {
    ArmResponse {
        name: arm.arm_name.clone(),
        searches: arm.searches,
        users: arm.users,
        clicks: arm.clicks,
        conversions: arm.conversions,
        revenue: arm.revenue,
        ctr: arm.ctr,
        conversion_rate: arm.conversion_rate,
        revenue_per_search: arm.revenue_per_search,
        zero_result_rate: arm.zero_result_rate,
        abandonment_rate: arm.abandonment_rate,
        mean_click_rank: arm.mean_click_rank,
    }
}

/// Construct a zeroed-out `ArmResponse` with the given arm name.
///
/// Used as a fallback when no analytics metrics are available for an experiment.
fn empty_arm_response(name: &str) -> ArmResponse {
    ArmResponse {
        name: name.to_string(),
        searches: 0,
        users: 0,
        clicks: 0,
        conversions: 0,
        revenue: 0.0,
        ctr: 0.0,
        conversion_rate: 0.0,
        revenue_per_search: 0.0,
        zero_result_rate: 0.0,
        abandonment_rate: 0.0,
        mean_click_rank: 0.0,
    }
}

fn primary_metric_label(metric: &PrimaryMetric) -> &'static str {
    match metric {
        PrimaryMetric::Ctr => "CTR",
        PrimaryMetric::ConversionRate => "conversion rate",
        PrimaryMetric::RevenuePerSearch => "revenue per search",
        PrimaryMetric::ZeroResultRate => "zero result rate",
        PrimaryMetric::AbandonmentRate => "abandonment rate",
    }
}
