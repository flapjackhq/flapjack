//! Statistical testing utilities for A/B experiments: delta-method and Welch z/t-tests, Bayesian beta-binomial comparison, CUPED variance reduction, interleaving preference scoring, SRM detection, guard-rail alerting, and sample-size estimation.
use std::collections::{HashMap, HashSet};

// ── Result Structs ──────────────────────────────────────────────────

pub struct StatResult {
    pub z_score: f64,
    pub p_value: f64,
    pub confidence: f64,
    pub significant: bool,
    pub relative_improvement: f64,
    pub absolute_improvement: f64,
    pub winner: Option<String>,
}

pub struct StatGate {
    pub minimum_n_reached: bool,
    pub minimum_days_reached: bool,
    pub ready_to_read: bool,
}

impl StatGate {
    /// Construct a readiness gate that checks whether both minimum sample-size and minimum-duration thresholds have been met.
    ///
    /// # Arguments
    ///
    /// * `control_searches` - Observed searches in the control arm.
    /// * `variant_searches` - Observed searches in the variant arm.
    /// * `required_per_arm` - Minimum searches required in each arm.
    /// * `elapsed_days` - Days elapsed since experiment start.
    /// * `minimum_days` - Minimum calendar days before results may be read.
    pub fn new(
        control_searches: u64,
        variant_searches: u64,
        required_per_arm: u64,
        elapsed_days: f64,
        minimum_days: u32,
    ) -> Self {
        let minimum_n_reached =
            control_searches >= required_per_arm && variant_searches >= required_per_arm;
        let minimum_days_reached = elapsed_days >= minimum_days as f64;
        Self {
            minimum_n_reached,
            minimum_days_reached,
            ready_to_read: minimum_n_reached && minimum_days_reached,
        }
    }
}

pub struct SampleSizeEstimate {
    pub per_arm: u64,
    pub total: u64,
    pub estimated_days: Option<f64>,
    pub minimum_days: u32,
    pub effective_days: f64,
}

// ── Normal Survival Function (A&S 26.2.17 with Horner's method) ─────

/// Computes P(Z > z) for the standard normal distribution.
/// Uses Abramowitz & Stegun 26.2.17 rational approximation with Horner's method.
/// Caller must pass z >= 0 (use z.abs() before calling).
pub fn normal_sf(z: f64) -> f64 {
    debug_assert!(z >= 0.0, "normal_sf requires z >= 0, got {}", z);

    let t = 1.0 / (1.0 + 0.2316419 * z);
    let d = 0.3989422804014327; // 1/sqrt(2*pi)
    let p = d * (-z * z / 2.0).exp();

    // Horner's method for the polynomial
    let poly = t
        * (0.319381530
            + t * (-0.356563782 + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));

    p * poly
}

// ── Delta Method Z-Test ─────────────────────────────────────────────

/// Delta method z-test for per-user CTR comparison.
/// Each entry is (clicks_i, searches_i) for user i.
/// Skips users with zero searches.
pub fn delta_method_z_test(control: &[(f64, f64)], variant: &[(f64, f64)]) -> StatResult {
    let compute_arm = |data: &[(f64, f64)]| -> (f64, f64, usize) {
        let valid: Vec<f64> = data
            .iter()
            .filter(|(_, s)| *s > 0.0)
            .map(|(c, s)| c / s)
            .collect();
        let n = valid.len();
        if n == 0 {
            return (0.0, 0.0, 0);
        }
        let mean = valid.iter().sum::<f64>() / n as f64;
        let variance = if n > 1 {
            valid.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1) as f64
        } else {
            0.0
        };
        (mean, variance, n)
    };

    let (mean_c, var_c, n_c) = compute_arm(control);
    let (mean_v, var_v, n_v) = compute_arm(variant);

    if n_c == 0 || n_v == 0 {
        return StatResult {
            z_score: 0.0,
            p_value: 1.0,
            confidence: 0.0,
            significant: false,
            relative_improvement: 0.0,
            absolute_improvement: 0.0,
            winner: None,
        };
    }

    let se = (var_c / n_c as f64 + var_v / n_v as f64).sqrt();

    if se == 0.0 {
        return StatResult {
            z_score: 0.0,
            p_value: 1.0,
            confidence: 0.0,
            significant: false,
            relative_improvement: 0.0,
            absolute_improvement: 0.0,
            winner: None,
        };
    }

    let z = (mean_v - mean_c) / se;
    let p_value = 2.0 * normal_sf(z.abs());
    let significant = p_value < 0.05;

    let absolute_improvement = mean_v - mean_c;
    let relative_improvement = if mean_c != 0.0 {
        absolute_improvement / mean_c
    } else {
        0.0
    };

    let winner = if significant {
        if mean_v > mean_c {
            Some("variant".to_string())
        } else {
            Some("control".to_string())
        }
    } else {
        None
    };

    StatResult {
        z_score: z,
        p_value,
        confidence: 1.0 - p_value,
        significant,
        relative_improvement,
        absolute_improvement,
        winner,
    }
}

// ── Welch's T-Test ──────────────────────────────────────────────────

/// Welch's t-test for continuous metrics (e.g., RevenuePerSearch).
/// Uses normal approximation when degrees of freedom > 50.
pub fn welch_t_test(control: &[f64], variant: &[f64]) -> StatResult {
    let compute_arm = |data: &[f64]| -> (f64, f64, usize) {
        let n = data.len();
        if n == 0 {
            return (0.0, 0.0, 0);
        }
        let mean = data.iter().sum::<f64>() / n as f64;
        let variance = if n > 1 {
            data.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64
        } else {
            0.0
        };
        (mean, variance, n)
    };

    let (mean_c, var_c, n_c) = compute_arm(control);
    let (mean_v, var_v, n_v) = compute_arm(variant);

    // Welch's t-test requires at least 2 observations per arm.
    if n_c < 2 || n_v < 2 {
        return StatResult {
            z_score: 0.0,
            p_value: 1.0,
            confidence: 0.0,
            significant: false,
            relative_improvement: 0.0,
            absolute_improvement: 0.0,
            winner: None,
        };
    }

    let se = (var_c / n_c as f64 + var_v / n_v as f64).sqrt();

    if se == 0.0 {
        return StatResult {
            z_score: 0.0,
            p_value: 1.0,
            confidence: 0.0,
            significant: false,
            relative_improvement: 0.0,
            absolute_improvement: 0.0,
            winner: None,
        };
    }

    let t = (mean_v - mean_c) / se;

    // Welch-Satterthwaite degrees of freedom
    let s1_n = var_c / n_c as f64;
    let s2_n = var_v / n_v as f64;
    let df_denom = s1_n.powi(2) / (n_c - 1) as f64 + s2_n.powi(2) / (n_v - 1) as f64;
    if df_denom <= 0.0 || !df_denom.is_finite() {
        return StatResult {
            z_score: 0.0,
            p_value: 1.0,
            confidence: 0.0,
            significant: false,
            relative_improvement: 0.0,
            absolute_improvement: 0.0,
            winner: None,
        };
    }
    let df = (s1_n + s2_n).powi(2) / df_denom;

    // Use normal approximation only when df is sufficiently large.
    let p_value = if df > 50.0 {
        2.0 * normal_sf(t.abs())
    } else {
        students_t_two_tailed_p(t, df)
    }
    .clamp(0.0, 1.0);
    let significant = p_value < 0.05;

    let absolute_improvement = mean_v - mean_c;
    let relative_improvement = if mean_c != 0.0 {
        absolute_improvement / mean_c
    } else {
        0.0
    };

    let winner = if significant {
        if mean_v > mean_c {
            Some("variant".to_string())
        } else {
            Some("control".to_string())
        }
    } else {
        None
    };

    StatResult {
        z_score: t,
        p_value,
        confidence: 1.0 - p_value,
        significant,
        relative_improvement,
        absolute_improvement,
        winner,
    }
}

// ── SRM Detection ───────────────────────────────────────────────────

/// Chi-squared test for sample ratio mismatch.
/// Returns true if chi2 > 6.635 (p=0.01 threshold).
pub fn check_sample_ratio_mismatch(
    control_n: u64,
    variant_n: u64,
    expected_variant_fraction: f64,
) -> bool {
    let total = control_n + variant_n;
    if total == 0 {
        return false;
    }
    let expected_control = total as f64 * (1.0 - expected_variant_fraction);
    let expected_variant = total as f64 * expected_variant_fraction;

    if expected_control == 0.0 || expected_variant == 0.0 {
        return false;
    }

    let chi2 = (control_n as f64 - expected_control).powi(2) / expected_control
        + (variant_n as f64 - expected_variant).powi(2) / expected_variant;

    chi2 > 6.635
}

// ── Winsorization ───────────────────────────────────────────────────

/// Caps values above the threshold. Accepts a pre-computed cap (not percentile).
pub fn winsorize(values: &mut [f64], cap: f64) {
    for v in values.iter_mut() {
        if *v > cap {
            *v = cap;
        }
    }
}

// ── Outlier Detection ───────────────────────────────────────────────

/// Detects outlier users using log-normal z-score.
/// Threshold: z > 7.0 AND count > 100.
pub fn detect_outlier_users(counts: &HashMap<String, u64>) -> HashSet<String> {
    if counts.is_empty() {
        return HashSet::new();
    }

    // Compute log-transformed statistics
    let log_values: Vec<f64> = counts
        .values()
        .filter(|&&v| v > 0)
        .map(|&v| (v as f64).ln())
        .collect();

    if log_values.is_empty() {
        return HashSet::new();
    }

    let n = log_values.len() as f64;
    let mean = log_values.iter().sum::<f64>() / n;
    let variance = log_values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
    let sd = variance.sqrt();

    if sd == 0.0 {
        return HashSet::new();
    }

    counts
        .iter()
        .filter(|(_, &count)| {
            count > 100 && {
                let log_count = (count as f64).ln();
                let z = (log_count - mean) / sd;
                z > 7.0
            }
        })
        .map(|(user, _)| user.clone())
        .collect()
}

// ── Bayesian Beta-Binomial ──────────────────────────────────────────

/// Computes P(B > A) using Evan Miller's closed-form integral for Beta distributions.
/// Prior: Beta(1,1) (uniform).
/// Posterior A: Beta(a_clicks+1, a_searches-a_clicks+1)
/// Posterior B: Beta(b_clicks+1, b_searches-b_clicks+1)
pub fn beta_binomial_prob_b_greater_a(
    a_clicks: u64,
    a_searches: u64,
    b_clicks: u64,
    b_searches: u64,
) -> f64 {
    if a_clicks > a_searches || b_clicks > b_searches {
        // Invalid counts; keep downstream results bounded and non-crashing.
        return 0.5;
    }

    let alpha_a = a_clicks as f64 + 1.0;
    let beta_a = (a_searches - a_clicks) as f64 + 1.0;
    let alpha_b = b_clicks as f64 + 1.0;
    let beta_b = (b_searches - b_clicks) as f64 + 1.0;

    // Evan Miller's closed-form: sum over i and j
    // P(B > A) = sum_{i=0}^{alpha_b-1} B(alpha_a+i, beta_a+beta_b) / ((beta_b+i)*B(1+i, beta_b)*B(alpha_a, beta_a))
    // where B is the beta function.
    //
    // For numerical stability, work in log space.
    let mut total = 0.0;
    let alpha_b_int = alpha_b as u64;

    for i in 0..alpha_b_int {
        let log_num = ln_beta(alpha_a + i as f64, beta_a + beta_b);
        let log_den =
            (beta_b + i as f64).ln() + ln_beta(1.0 + i as f64, beta_b) + ln_beta(alpha_a, beta_a);
        total += (log_num - log_den).exp();
    }

    total
}

/// Log of the Beta function: ln(B(a,b)) = ln(Gamma(a)) + ln(Gamma(b)) - ln(Gamma(a+b))
fn ln_beta(a: f64, b: f64) -> f64 {
    ln_gamma(a) + ln_gamma(b) - ln_gamma(a + b)
}

/// Lanczos approximation of ln(Gamma(x)) for x > 0.
#[allow(clippy::excessive_precision)]
fn ln_gamma(x: f64) -> f64 {
    // Lanczos coefficients (g=7)
    let coefficients = [
        0.99999999999980993,
        676.5203681218851,
        -1259.1392167224028,
        771.32342877765313,
        -176.61502916214059,
        12.507343278686905,
        -0.13857109526572012,
        9.9843695780195716e-6,
        1.5056327351493116e-7,
    ];

    if x < 0.5 {
        // Reflection formula
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }

    let x = x - 1.0;
    let mut acc = coefficients[0];
    let t = x + 7.5; // g + 0.5

    for (i, &coef) in coefficients.iter().enumerate().skip(1) {
        acc += coef / (x + i as f64);
    }

    0.5 * (2.0 * std::f64::consts::PI).ln() + (t.ln() * (x + 0.5)) - t + acc.ln()
}

// ── Guard Rails ─────────────────────────────────────────────────────

/// Alert emitted when an arm metric drops beyond the guard rail threshold.
#[derive(Debug, Clone)]
pub struct GuardRailAlert {
    pub metric_name: String,
    pub control_value: f64,
    pub variant_value: f64,
    pub drop_pct: f64,
}

/// Checks whether the variant metric has regressed beyond the threshold.
///
/// For higher-is-better metrics: alert if variant < control * (1 - threshold).
/// For lower-is-better metrics: alert if variant > control * (1 + threshold).
/// Default threshold: 0.20 (20%).
pub fn check_guard_rail(
    metric_name: &str,
    control_metric: f64,
    variant_metric: f64,
    lower_is_better: bool,
    threshold: f64,
) -> Option<GuardRailAlert> {
    if control_metric == 0.0 {
        // For lower-is-better metrics, a perfect zero baseline should still alert
        // if variant regresses above zero.
        if lower_is_better && variant_metric > 0.0 {
            return Some(GuardRailAlert {
                metric_name: metric_name.to_string(),
                control_value: control_metric,
                variant_value: variant_metric,
                drop_pct: 100.0,
            });
        }
        return None;
    }

    let triggered = if lower_is_better {
        // Lower is better → variant worse when it's higher by > threshold
        variant_metric > control_metric * (1.0 + threshold)
    } else {
        // Higher is better → variant worse when it's lower by > threshold
        variant_metric < control_metric * (1.0 - threshold)
    };

    if triggered {
        let drop_pct = if lower_is_better {
            // Variant increased (bad) — express as % increase
            (variant_metric - control_metric) / control_metric * 100.0
        } else {
            // Variant decreased (bad) — express as % decrease
            (control_metric - variant_metric) / control_metric * 100.0
        };
        Some(GuardRailAlert {
            metric_name: metric_name.to_string(),
            control_value: control_metric,
            variant_value: variant_metric,
            drop_pct,
        })
    } else {
        None
    }
}

// ── Sample Size Estimator ───────────────────────────────────────────

/// Two-proportion power analysis for sample size estimation.
/// Returns per-arm sample size needed to detect relative MDE at given power/alpha.
fn unavailable_sample_size_estimate() -> SampleSizeEstimate {
    SampleSizeEstimate {
        per_arm: u64::MAX,
        total: u64::MAX,
        estimated_days: None,
        minimum_days: 14,
        effective_days: 14.0,
    }
}

pub fn required_sample_size(
    baseline_rate: f64,
    relative_mde: f64,
    alpha: f64,
    power: f64,
    traffic_split: f64,
) -> SampleSizeEstimate {
    let p1 = baseline_rate;
    let p2 = baseline_rate * (1.0 + relative_mde);
    let delta = (p2 - p1).abs();

    if !p1.is_finite()
        || !p2.is_finite()
        || !relative_mde.is_finite()
        || !alpha.is_finite()
        || !power.is_finite()
        || !traffic_split.is_finite()
        || !(0.0..1.0).contains(&p1)
        || !(0.0..1.0).contains(&p2)
        || relative_mde <= 0.0
        || !(0.0..1.0).contains(&alpha)
        || !(0.0..1.0).contains(&power)
        || !(0.0..1.0).contains(&traffic_split)
        || delta == 0.0
    {
        return unavailable_sample_size_estimate();
    }

    // z-values for alpha/2 upper tail and power
    let z_alpha = z_from_p(1.0 - alpha / 2.0);
    let z_power = z_from_p(power);

    // Pooled proportion
    let p_bar = (p1 + p2) / 2.0;

    // Standard two-proportion formula:
    // n = (z_alpha * sqrt(2*p_bar*(1-p_bar)) + z_power * sqrt(p1*(1-p1) + p2*(1-p2)))^2 / delta^2
    let numerator = z_alpha * (2.0 * p_bar * (1.0 - p_bar)).sqrt()
        + z_power * (p1 * (1.0 - p1) + p2 * (1.0 - p2)).sqrt();
    let raw_per_arm = (numerator.powi(2) / delta.powi(2)).ceil();
    if !raw_per_arm.is_finite() || raw_per_arm <= 0.0 || raw_per_arm >= u64::MAX as f64 {
        return unavailable_sample_size_estimate();
    }
    let per_arm = raw_per_arm as u64;

    // Adjust for traffic split: if split != 0.5, the smaller arm needs more total traffic
    let split_factor = 1.0 / (traffic_split * (1.0 - traffic_split) * 4.0);
    let raw_adjusted_per_arm = (per_arm as f64 * split_factor).ceil();
    if !raw_adjusted_per_arm.is_finite()
        || raw_adjusted_per_arm <= 0.0
        || raw_adjusted_per_arm >= u64::MAX as f64
    {
        return unavailable_sample_size_estimate();
    }
    let adjusted_per_arm = raw_adjusted_per_arm as u64;

    SampleSizeEstimate {
        per_arm: adjusted_per_arm,
        total: adjusted_per_arm.saturating_mul(2),
        estimated_days: None, // Caller must compute from daily traffic
        minimum_days: 14,
        effective_days: 14.0, // Updated by caller
    }
}

/// Inverse normal CDF approximation (Beasley-Springer-Moro).
/// Returns z such that P(Z < z) = p.
fn z_from_p(p: f64) -> f64 {
    // Rational approximation for central region
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }

    // Use symmetry around p=0.5.
    let (p_adj, sign) = if p < 0.5 { (p, -1.0) } else { (1.0 - p, 1.0) };

    let t = (-2.0 * p_adj.ln()).sqrt();

    // Rational approximation (Abramowitz & Stegun 26.2.23)
    let c0 = 2.515517;
    let c1 = 0.802853;
    let c2 = 0.010328;
    let d1 = 1.432788;
    let d2 = 0.189269;
    let d3 = 0.001308;

    let z = t - (c0 + c1 * t + c2 * t * t) / (1.0 + d1 * t + d2 * t * t + d3 * t * t * t);

    sign * z
}

/// Two-tailed p-value for Student's t-distribution with `df` degrees of freedom.
/// Uses the regularized incomplete beta representation.
fn students_t_two_tailed_p(t: f64, df: f64) -> f64 {
    if !df.is_finite() || df <= 0.0 {
        return 1.0;
    }
    let x = df / (df + t * t);
    regularized_incomplete_beta(df / 2.0, 0.5, x)
}

/// Regularized incomplete beta I_x(a, b).
/// Numerical Recipes style continued-fraction implementation.
fn regularized_incomplete_beta(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }

    let bt = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp();

    if x < (a + 1.0) / (a + b + 2.0) {
        (bt * beta_continued_fraction(a, b, x) / a).clamp(0.0, 1.0)
    } else {
        (1.0 - bt * beta_continued_fraction(b, a, 1.0 - x) / b).clamp(0.0, 1.0)
    }
}

/// Evaluate the Lentz continued-fraction expansion for the regularized incomplete beta function I_x(a, b).
///
/// Converges within `MAX_ITERS` iterations to relative tolerance `EPS`. Uses the modified Lentz algorithm with floor `FPMIN` to avoid division by zero.
///
/// # Arguments
///
/// * `a` - First shape parameter (> 0).
/// * `b` - Second shape parameter (> 0).
/// * `x` - Evaluation point in (0, 1).
fn beta_continued_fraction(a: f64, b: f64, x: f64) -> f64 {
    const MAX_ITERS: usize = 200;
    const EPS: f64 = 3.0e-7;
    const FPMIN: f64 = 1.0e-30;

    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;

    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < FPMIN {
        d = FPMIN;
    }
    d = 1.0 / d;
    let mut h = d;

    for m in 1..=MAX_ITERS {
        let m_f = m as f64;
        let m2 = 2.0 * m_f;

        let aa = m_f * (b - m_f) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        h *= d * c;

        let aa = -(a + m_f) * (qab + m_f) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let delta = d * c;
        h *= delta;

        if (delta - 1.0).abs() < EPS {
            break;
        }
    }

    h
}

// ── CUPED Variance Reduction ────────────────────────────────────────

/// Applies CUPED (Controlled-experiment Using Pre-Existing Data) adjustment
/// to per-user experiment metric tuples.
///
/// For each user with a pre-experiment covariate value, adjusts:
///   Y_adj = Y - theta * (X_i - mean(X))
/// where theta = Cov(Y, X) / Var(X).
///
/// Users without covariate data pass through unchanged.
/// Returns original values if fewer than 100 users match or Var(X) == 0.
///
/// Reference: Deng et al. (2013) "Improving the Sensitivity of Online
/// Controlled Experiments by Utilizing Pre-Experiment Data."
pub const CUPED_MIN_MATCHED_USERS: usize = 100;

/// Apply CUPED (Controlled-experiment Using Pre-Existing Data) variance reduction to per-user experiment metric tuples.
///
/// Adjust each matched user's observed rate by subtracting θ·(Xᵢ − X̄), where θ = Cov(Y, X) / Var(X). Users without a covariate entry pass through unchanged.
///
/// Return the original values unmodified when fewer than `CUPED_MIN_MATCHED_USERS` users match or Var(X) ≈ 0.
///
/// # Arguments
///
/// * `experiment_values` - Per-user `(clicks, searches)` tuples.
/// * `user_ids` - User identifier for each tuple, same length as `experiment_values`.
/// * `covariates` - Pre-experiment metric keyed by user ID.
///
/// # Returns
///
/// A new vector of `(adjusted_clicks, searches)` tuples with the same length as the input.
pub fn cuped_adjust(
    experiment_values: &[(f64, f64)],
    user_ids: &[String],
    covariates: &HashMap<String, f64>,
) -> Vec<(f64, f64)> {
    if covariates.is_empty() || experiment_values.len() != user_ids.len() {
        return experiment_values.to_vec();
    }

    // Collect matched (index, rate, covariate) triples
    let matched: Vec<(usize, f64, f64)> = user_ids
        .iter()
        .enumerate()
        .filter_map(|(idx, uid)| {
            let (_, searches) = experiment_values[idx];
            if searches <= 0.0 {
                return None;
            }
            let rate = experiment_values[idx].0 / searches;
            covariates.get(uid).map(|&cov| (idx, rate, cov))
        })
        .collect();

    if matched.len() < CUPED_MIN_MATCHED_USERS {
        return experiment_values.to_vec();
    }

    // Compute mean(X) and mean(Y) over matched users
    let n = matched.len() as f64;
    let mean_x = matched.iter().map(|(_, _, x)| x).sum::<f64>() / n;
    let mean_y = matched.iter().map(|(_, y, _)| y).sum::<f64>() / n;

    // Compute Var(X) and Cov(Y, X)
    let var_x = matched
        .iter()
        .map(|(_, _, x)| (x - mean_x).powi(2))
        .sum::<f64>()
        / (n - 1.0);

    if var_x < 1e-15 {
        return experiment_values.to_vec();
    }

    let cov_yx = matched
        .iter()
        .map(|(_, y, x)| (y - mean_y) * (x - mean_x))
        .sum::<f64>()
        / (n - 1.0);

    let theta = cov_yx / var_x;

    // Apply adjustment: Y_adj = Y - theta * (X_i - mean_X)
    let mut result = experiment_values.to_vec();
    for &(idx, _rate, cov) in &matched {
        let (clicks, searches) = result[idx];
        if searches <= 0.0 {
            continue;
        }
        let rate = clicks / searches;
        let adjusted_rate = rate - theta * (cov - mean_x);
        result[idx] = (adjusted_rate * searches, searches);
    }

    result
}

// ── Interleaving Preference Scoring ─────────────────────────────────

/// Result of interleaving preference analysis across queries.
pub struct PreferenceResult {
    /// ΔAB = (wins_a − wins_b) / (wins_a + wins_b + ties).
    /// Positive → control preferred; negative → variant preferred.
    pub delta_ab: f64,
    pub wins_a: u32,
    pub wins_b: u32,
    pub ties: u32,
    /// Two-sided sign test p-value (binomial at p=0.5, ties excluded).
    pub p_value: f64,
}

/// Compute interleaving preference score from per-query click counts.
///
/// Each entry is `(team_a_clicks, team_b_clicks)` for one query.
/// A query is a "win" for the team with more clicks; equal clicks = tie.
///
/// ΔAB = (wins_A − wins_B) / (wins_A + wins_B + ties)
/// Sign test: two-sided binomial test at p=0.5, ties excluded.
pub fn compute_preference_score(per_query: &[(u32, u32)]) -> PreferenceResult {
    let mut wins_a: u32 = 0;
    let mut wins_b: u32 = 0;
    let mut ties: u32 = 0;

    for &(a, b) in per_query {
        match a.cmp(&b) {
            std::cmp::Ordering::Greater => wins_a += 1,
            std::cmp::Ordering::Less => wins_b += 1,
            std::cmp::Ordering::Equal => ties += 1,
        }
    }

    let total = wins_a + wins_b + ties;
    let delta_ab = if total == 0 {
        0.0
    } else {
        (wins_a as f64 - wins_b as f64) / total as f64
    };

    let p_value = sign_test_p_value(wins_a, wins_b);

    PreferenceResult {
        delta_ab,
        wins_a,
        wins_b,
        ties,
        p_value,
    }
}

/// Two-sided sign test p-value (binomial at p=0.5).
///
/// n = wins_a + wins_b (ties excluded). Uses normal approximation
/// when n > 20; returns 1.0 when n == 0.
fn sign_test_p_value(wins_a: u32, wins_b: u32) -> f64 {
    let n = wins_a + wins_b;
    if n == 0 {
        return 1.0;
    }
    let n_f = n as f64;
    let k = wins_a.min(wins_b) as f64; // smaller of the two

    if n > 20 {
        // Normal approximation: z = (wins_a - n/2) / sqrt(n/4)
        let z = ((wins_a as f64) - n_f / 2.0).abs() / (n_f / 4.0).sqrt();
        2.0 * normal_sf(z)
    } else {
        // Exact two-sided binomial CDF: P(X ≤ k) where X ~ Binomial(n, 0.5)
        // p = 2 * sum_{i=0}^{k} C(n, i) * 0.5^n, capped at 1.0
        let mut cdf = 0.0;
        let mut binom_coeff: f64 = 1.0;
        let p_n = (0.5_f64).powi(n as i32);
        for i in 0..=(k as u32) {
            cdf += binom_coeff * p_n;
            if i < n {
                binom_coeff *= (n - i) as f64 / (i + 1) as f64;
            }
        }
        (2.0 * cdf).min(1.0)
    }
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
