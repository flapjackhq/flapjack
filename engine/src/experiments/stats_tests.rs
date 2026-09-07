use super::*;

// ── Normal SF ───────────────────────────────────────────────────

#[test]
fn normal_sf_at_z196_is_approximately_0025() {
    let sf = normal_sf(1.96);
    assert!((sf - 0.025).abs() < 0.0005, "sf={}", sf);
}

#[test]
fn normal_sf_at_z258_is_approximately_0005() {
    let sf = normal_sf(2.576);
    assert!((sf - 0.005).abs() < 0.0005, "sf={}", sf);
}

#[test]
fn normal_sf_at_z0_is_0_5() {
    let sf = normal_sf(0.0);
    assert!((sf - 0.5).abs() < 0.001, "sf={}", sf);
}

#[test]
fn normal_sf_at_z329_gives_p_value_0001() {
    let sf = normal_sf(3.291);
    assert!((sf - 0.0005).abs() < 0.0001, "sf={}", sf);
}

// ── Delta method z-test ─────────────────────────────────────────

/// Verify that the delta-method z-test detects significance and positive relative improvement for a large CTR difference (10% vs 14%) at n = 5000 per arm.
#[test]
fn delta_method_returns_significant_for_large_effect() {
    let control: Vec<(f64, f64)> = (0..5000)
        .map(|i| {
            let searches = 5.0;
            let clicks = if i < 500 { 1.0 } else { 0.0 };
            (clicks, searches)
        })
        .collect();
    let variant: Vec<(f64, f64)> = (0..5000)
        .map(|i| {
            let searches = 5.0;
            let clicks = if i < 700 { 1.0 } else { 0.0 };
            (clicks, searches)
        })
        .collect();
    let result = delta_method_z_test(&control, &variant);
    assert!(result.significant, "p={}", result.p_value);
    assert!(result.relative_improvement > 0.0);
}

#[test]
fn delta_method_returns_not_significant_for_tiny_effect_small_n() {
    let control: Vec<(f64, f64)> = (0..100)
        .map(|i| (if i < 12 { 1.0 } else { 0.0 }, 5.0))
        .collect();
    let variant: Vec<(f64, f64)> = (0..100)
        .map(|i| (if i < 13 { 1.0 } else { 0.0 }, 5.0))
        .collect();
    let result = delta_method_z_test(&control, &variant);
    assert!(
        !result.significant,
        "p={} should not be significant",
        result.p_value
    );
}

#[test]
fn delta_method_winner_is_none_when_not_significant() {
    let control: Vec<(f64, f64)> = (0..50)
        .map(|i| (if i < 6 { 1.0 } else { 0.0 }, 5.0))
        .collect();
    let variant: Vec<(f64, f64)> = (0..50)
        .map(|i| (if i < 7 { 1.0 } else { 0.0 }, 5.0))
        .collect();
    let result = delta_method_z_test(&control, &variant);
    assert!(
        !result.significant,
        "expected non-significant result for this tiny effect, got p={}",
        result.p_value
    );
    assert!(result.winner.is_none());
}

#[test]
fn delta_method_winner_is_variant_when_variant_wins() {
    let control: Vec<(f64, f64)> = (0..10000)
        .map(|i| (if i < 1000 { 1.0 } else { 0.0 }, 10.0))
        .collect();
    let variant: Vec<(f64, f64)> = (0..10000)
        .map(|i| (if i < 1500 { 1.0 } else { 0.0 }, 10.0))
        .collect();
    let result = delta_method_z_test(&control, &variant);
    assert!(result.significant);
    assert_eq!(result.winner, Some("variant".to_string()));
}

// ── Welch's T-Test ──────────────────────────────────────────────

#[test]
fn welch_t_test_significant_for_large_effect() {
    let control: Vec<f64> = (0..1000)
        .map(|i| if i < 120 { 10.0 } else { 0.0 })
        .collect();
    let variant: Vec<f64> = (0..1000)
        .map(|i| if i < 180 { 10.0 } else { 0.0 })
        .collect();
    let result = welch_t_test(&control, &variant);
    assert!(result.significant, "p={}", result.p_value);
    assert!(result.relative_improvement > 0.0);
}

#[test]
fn welch_t_test_not_significant_for_tiny_effect() {
    let control: Vec<f64> = (0..50).map(|i| if i < 6 { 1.0 } else { 0.0 }).collect();
    let variant: Vec<f64> = (0..50).map(|i| if i < 7 { 1.0 } else { 0.0 }).collect();
    let result = welch_t_test(&control, &variant);
    assert!(!result.significant, "p={}", result.p_value);
}

#[test]
fn welch_t_test_small_df_uses_t_distribution_not_normal() {
    // n=2 per arm with strong apparent mean difference.
    // Normal approximation would produce p < 0.05 here, but with df≈2
    // the proper Student's t two-tailed p-value is not significant.
    let control = vec![0.0, 1.0];
    let variant = vec![2.0, 3.0];
    let result = welch_t_test(&control, &variant);
    assert!(
        !result.significant,
        "small-sample Welch should not be significant here, got p={}",
        result.p_value
    );
}

#[test]
fn welch_t_test_requires_two_samples_per_arm() {
    // With only one sample in control, variance and df are undefined.
    // The test should return a neutral non-significant result.
    let control = vec![0.0];
    let variant = vec![2.0, 3.0, 4.0];
    let result = welch_t_test(&control, &variant);
    assert!(
        !result.significant,
        "Welch test must not report significance with n<2 in an arm, got p={}",
        result.p_value
    );
    assert!(result.winner.is_none());
}

// ── SRM Detection ───────────────────────────────────────────────

#[test]
fn srm_not_detected_for_perfect_50_50() {
    assert!(!check_sample_ratio_mismatch(5000, 5000, 0.5));
}

#[test]
fn srm_detected_for_45_55_split_at_large_n() {
    assert!(check_sample_ratio_mismatch(45000, 55000, 0.5));
}

#[test]
fn srm_not_detected_for_slight_noise_at_small_n() {
    assert!(!check_sample_ratio_mismatch(490, 510, 0.5));
}

#[test]
fn srm_threshold_is_p_001_not_p_005() {
    // 4900/5100 at N=10000: chi2 = 4.0 → should NOT trigger at p=0.01 (threshold 6.635)
    assert!(!check_sample_ratio_mismatch(4900, 5100, 0.5));
    // 4600/5400 at N=10000: chi2 = 64.0 → SHOULD trigger at p=0.01
    assert!(check_sample_ratio_mismatch(4600, 5400, 0.5));
}

// ── Winsorization ───────────────────────────────────────────────

#[test]
fn winsorize_caps_values_above_threshold() {
    let mut values = vec![1.0, 2.0, 3.0, 4.0, 100.0];
    winsorize(&mut values, 10.0);
    assert_eq!(values[4], 10.0);
    assert_eq!(values[0], 1.0);
}

#[test]
fn winsorize_leaves_values_below_cap_unchanged() {
    let mut values = vec![1.0, 2.0, 3.0];
    winsorize(&mut values, 100.0);
    assert_eq!(values, vec![1.0, 2.0, 3.0]);
}

#[test]
fn winsorize_empty_vec_is_noop() {
    let mut values: Vec<f64> = vec![];
    winsorize(&mut values, 10.0);
    assert!(values.is_empty());
}

// ── Outlier Detection ───────────────────────────────────────────

#[test]
fn outlier_detection_excludes_extreme_bot_users() {
    let mut counts = HashMap::new();
    for i in 0..1000 {
        counts.insert(format!("user-{}", i), 10u64);
    }
    counts.insert("bot-user".to_string(), 100_000u64);

    let outliers = detect_outlier_users(&counts);
    assert!(outliers.contains("bot-user"));
    assert!(!outliers.contains("user-0"));
}

#[test]
fn outlier_detection_requires_both_sd_and_min_count() {
    let mut counts = HashMap::new();
    for i in 0..1000 {
        counts.insert(format!("user-{}", i), 10u64);
    }
    // High relative to mean but below min count of 100
    counts.insert("slightly-high".to_string(), 50u64);
    let outliers = detect_outlier_users(&counts);
    assert!(!outliers.contains("slightly-high"));
}

// ── Bayesian Beta-Binomial ──────────────────────────────────────

#[test]
fn bayesian_prob_returns_near_0_5_for_equal_arms() {
    let prob = beta_binomial_prob_b_greater_a(100, 1000, 100, 1000);
    assert!((prob - 0.5).abs() < 0.05, "prob={}", prob);
}

#[test]
fn bayesian_prob_returns_high_when_b_clearly_better() {
    let prob = beta_binomial_prob_b_greater_a(100, 1000, 200, 1000);
    assert!(prob > 0.99, "prob={}", prob);
}

#[test]
fn bayesian_prob_returns_low_when_a_clearly_better() {
    let prob = beta_binomial_prob_b_greater_a(200, 1000, 100, 1000);
    assert!(prob < 0.01, "prob={}", prob);
}

#[test]
fn bayesian_prob_is_between_0_and_1() {
    let prob = beta_binomial_prob_b_greater_a(50, 500, 60, 500);
    assert!((0.0..=1.0).contains(&prob));
}

// ── Sample Size Estimator ───────────────────────────────────────

#[test]
fn sample_size_baseline_0_12_mde_0_05_power_80_alpha_05() {
    let est = required_sample_size(0.12, 0.05, 0.05, 0.80, 0.5);
    assert_eq!(est.per_arm, 47_045);
    assert_eq!(est.total, 94_090);
}

#[test]
fn sample_size_invalid_probability_inputs_fail_closed() {
    for (baseline, mde, alpha, power, split) in [
        (1.0, 0.05, 0.05, 0.80, 0.5),
        (0.99, 0.05, 0.05, 0.80, 0.5),
        (f64::NAN, 0.05, 0.05, 0.80, 0.5),
        (0.12, f64::INFINITY, 0.05, 0.80, 0.5),
        (0.12, 0.05, 0.0, 0.80, 0.5),
        (0.12, 0.05, 0.05, 1.0, 0.5),
        (0.12, 0.05, 0.05, 0.80, 0.0),
    ] {
        let estimate = required_sample_size(baseline, mde, alpha, power, split);
        assert_eq!(estimate.per_arm, u64::MAX);
        assert_eq!(estimate.total, u64::MAX);
    }
}

#[test]
fn sample_size_larger_mde_needs_fewer_samples() {
    let est_small = required_sample_size(0.12, 0.05, 0.05, 0.80, 0.5);
    let est_large = required_sample_size(0.12, 0.10, 0.05, 0.80, 0.5);
    assert!(est_large.per_arm < est_small.per_arm);
}

#[test]
fn sample_size_higher_power_requires_more_samples() {
    let est_80 = required_sample_size(0.12, 0.05, 0.05, 0.80, 0.5);
    let est_90 = required_sample_size(0.12, 0.05, 0.05, 0.90, 0.5);
    assert!(
        est_90.per_arm > est_80.per_arm,
        "higher power should require more samples"
    );
}

#[test]
fn z_from_p_upper_tail_quantile_is_positive() {
    let z = z_from_p(0.8);
    assert!(z > 0.0, "z(0.8) should be positive, got {}", z);
}

#[test]
fn beta_binomial_invalid_clicks_do_not_panic() {
    let prob = beta_binomial_prob_b_greater_a(11, 10, 5, 10);
    assert!(
        prob.is_finite() && (0.0..=1.0).contains(&prob),
        "invalid inputs should produce a bounded fallback probability, got {}",
        prob
    );
}

// ── StatGate ────────────────────────────────────────────────────

#[test]
fn stat_gate_ready_when_both_conditions_met() {
    let gate = StatGate::new(60000, 60000, 50000, 15.0, 14);
    assert!(gate.minimum_n_reached);
    assert!(gate.minimum_days_reached);
    assert!(gate.ready_to_read);
}

#[test]
fn stat_gate_not_ready_when_n_insufficient() {
    let gate = StatGate::new(30000, 60000, 50000, 15.0, 14);
    assert!(!gate.minimum_n_reached);
    assert!(gate.minimum_days_reached);
    assert!(!gate.ready_to_read);
}

#[test]
fn stat_gate_not_ready_when_days_insufficient() {
    let gate = StatGate::new(60000, 60000, 50000, 10.0, 14);
    assert!(gate.minimum_n_reached);
    assert!(!gate.minimum_days_reached);
    assert!(!gate.ready_to_read);
}

// ── Guard Rails ─────────────────────────────────────────────────

#[test]
fn guard_rail_triggers_when_variant_drops_20_pct() {
    // variant CTR = 0.08, control CTR = 0.12 → 33% drop → triggered
    let alert = check_guard_rail("CTR", 0.12, 0.08, false, 0.20);
    assert!(alert.is_some(), "expected guard rail to trigger");
    let alert = alert.unwrap();
    assert_eq!(alert.metric_name, "CTR");
    assert!(
        (alert.drop_pct - 33.33).abs() < 1.0,
        "drop_pct={}",
        alert.drop_pct
    );
}

#[test]
fn guard_rail_does_not_trigger_at_15_pct_drop() {
    // variant CTR = 0.102, control CTR = 0.12 → 15% drop → NOT triggered at 20% threshold
    let alert = check_guard_rail("CTR", 0.12, 0.102, false, 0.20);
    assert!(
        alert.is_none(),
        "15% drop should not trigger 20% guard rail"
    );
}

#[test]
fn guard_rail_does_not_trigger_for_lower_is_better_improvement() {
    // variant zero_result_rate = 0.05, control = 0.10 → variant improved → NOT triggered
    let alert = check_guard_rail("zero_result_rate", 0.10, 0.05, true, 0.20);
    assert!(
        alert.is_none(),
        "improvement on lower-is-better should not trigger"
    );
}

#[test]
fn guard_rail_triggers_for_lower_is_better_regression() {
    // variant zero_result_rate = 0.15, control = 0.10 → variant 50% worse → triggered
    let alert = check_guard_rail("zero_result_rate", 0.10, 0.15, true, 0.20);
    assert!(
        alert.is_some(),
        "regression on lower-is-better should trigger"
    );
    let alert = alert.unwrap();
    assert!(
        (alert.drop_pct - 50.0).abs() < 1.0,
        "drop_pct={}",
        alert.drop_pct
    );
}

#[test]
fn guard_rail_triggers_for_lower_is_better_regression_from_zero_control() {
    // control at 0.0 is ideal for lower-is-better metrics; any positive variant value regresses.
    let alert = check_guard_rail("zero_result_rate", 0.0, 0.02, true, 0.20);
    assert!(
        alert.is_some(),
        "regression from a zero baseline should still trigger guard rail"
    );
    let alert = alert.unwrap();
    assert!(
        (alert.drop_pct - 100.0).abs() < 1.0,
        "drop_pct={}",
        alert.drop_pct
    );
}

// ── CUPED Variance Reduction ────────────────────────────────────

/// Verify that CUPED adjustment reduces per-user rate variance when pre-experiment covariates are strongly correlated with the experiment metric.
#[test]
fn cuped_adjustment_reduces_variance() {
    // Construct correlated pre/post data where CUPED should help.
    // 100 users, pre-experiment metric strongly correlated with experiment metric.
    let user_ids: Vec<String> = (0..100).map(|i| format!("user_{i}")).collect();
    // Experiment values: (clicks, searches) — per-user rate tuples
    let experiment_values: Vec<(f64, f64)> = (0..100)
        .map(|i| {
            let base = (i as f64) * 0.01; // 0.00 to 0.99
            (base * 10.0, 10.0) // rate = base
        })
        .collect();
    // Covariates: strongly correlated pre-experiment metric
    let covariates: HashMap<String, f64> = (0..100)
        .map(|i| {
            let pre_val = (i as f64) * 0.01 + 0.02; // slightly offset but correlated
            (format!("user_{i}"), pre_val)
        })
        .collect();

    let adjusted = cuped_adjust(&experiment_values, &user_ids, &covariates);

    // Compute variance of original rates vs adjusted rates
    let original_rates: Vec<f64> = experiment_values.iter().map(|(c, s)| c / s).collect();
    let orig_mean = original_rates.iter().sum::<f64>() / original_rates.len() as f64;
    let orig_var = original_rates
        .iter()
        .map(|r| (r - orig_mean).powi(2))
        .sum::<f64>()
        / (original_rates.len() - 1) as f64;

    let adj_rates: Vec<f64> = adjusted.iter().map(|(c, s)| c / s).collect();
    let adj_mean = adj_rates.iter().sum::<f64>() / adj_rates.len() as f64;
    let adj_var = adj_rates
        .iter()
        .map(|r| (r - adj_mean).powi(2))
        .sum::<f64>()
        / (adj_rates.len() - 1) as f64;

    assert!(
        adj_var < orig_var,
        "CUPED should reduce variance: original={orig_var:.6}, adjusted={adj_var:.6}"
    );
}

/// Verify that CUPED returns the original values unchanged when all covariates are identical (Var(X) = 0).
#[test]
fn cuped_adjustment_zero_covariance_returns_original() {
    // Uncorrelated data: pre-experiment metric is random noise, not correlated
    let user_ids: Vec<String> = (0..100).map(|i| format!("user_{i}")).collect();
    let experiment_values: Vec<(f64, f64)> = (0..100).map(|i| ((i as f64 % 5.0), 10.0)).collect();
    // Covariates all identical → Var(X) == 0 → theta undefined → return original
    let covariates: HashMap<String, f64> = (0..100).map(|i| (format!("user_{i}"), 0.5)).collect();

    let adjusted = cuped_adjust(&experiment_values, &user_ids, &covariates);

    // Should be identical to original since Var(X) == 0
    for (orig, adj) in experiment_values.iter().zip(adjusted.iter()) {
        assert!(
            (orig.0 - adj.0).abs() < 1e-10 && (orig.1 - adj.1).abs() < 1e-10,
            "values should be unchanged when Var(X)=0"
        );
    }
}

#[test]
fn cuped_adjustment_empty_covariate_returns_original() {
    let user_ids: Vec<String> = (0..50).map(|i| format!("user_{i}")).collect();
    let experiment_values: Vec<(f64, f64)> = (0..50).map(|i| ((i as f64 % 3.0), 10.0)).collect();
    let covariates: HashMap<String, f64> = HashMap::new();

    let adjusted = cuped_adjust(&experiment_values, &user_ids, &covariates);

    for (orig, adj) in experiment_values.iter().zip(adjusted.iter()) {
        assert!(
            (orig.0 - adj.0).abs() < 1e-10 && (orig.1 - adj.1).abs() < 1e-10,
            "values should be unchanged when no covariates"
        );
    }
}

/// Verify that positively correlated covariates produce a positive θ, increasing rates for low-covariate users and decreasing rates for high-covariate users.
#[test]
fn cuped_theta_sign_is_correct() {
    // Positive covariance: higher pre-metric → higher post-metric → theta > 0
    let user_ids: Vec<String> = (0..100).map(|i| format!("user_{i}")).collect();
    let experiment_values: Vec<(f64, f64)> = (0..100).map(|i| ((i as f64) * 0.1, 10.0)).collect();
    let covariates: HashMap<String, f64> = (0..100)
        .map(|i| (format!("user_{i}"), (i as f64) * 0.1))
        .collect();

    let adjusted = cuped_adjust(&experiment_values, &user_ids, &covariates);

    // For positively correlated data, CUPED subtracts theta*(X_i - mean_X).
    // Users with high X_i should have their rate decreased; users with low X_i increased.
    // Check user_0 (low covariate) gets increased rate and user_99 (high covariate) gets decreased rate.
    let orig_rate_0 = experiment_values[0].0 / experiment_values[0].1;
    let adj_rate_0 = adjusted[0].0 / adjusted[0].1;
    let orig_rate_99 = experiment_values[99].0 / experiment_values[99].1;
    let adj_rate_99 = adjusted[99].0 / adjusted[99].1;

    assert!(
        adj_rate_0 > orig_rate_0,
        "low-covariate user should get rate increase: orig={orig_rate_0}, adj={adj_rate_0}"
    );
    assert!(
        adj_rate_99 < orig_rate_99,
        "high-covariate user should get rate decrease: orig={orig_rate_99}, adj={adj_rate_99}"
    );
}

/// Verify that CUPED adjusts only users with covariate data and passes unmatched users through unchanged when coverage exceeds CUPED_MIN_MATCHED_USERS.
#[test]
fn cuped_adjustment_partial_coverage() {
    // 200 users, but only first 120 have pre-experiment data (above MIN_MATCHED_USERS=100)
    let user_ids: Vec<String> = (0..200).map(|i| format!("user_{i}")).collect();
    let experiment_values: Vec<(f64, f64)> = (0..200).map(|i| ((i as f64) * 0.1, 10.0)).collect();
    // Only first 120 users have covariates (meets the 100-user minimum)
    let covariates: HashMap<String, f64> = (0..120)
        .map(|i| (format!("user_{i}"), (i as f64) * 0.1))
        .collect();

    let adjusted = cuped_adjust(&experiment_values, &user_ids, &covariates);

    assert_eq!(adjusted.len(), experiment_values.len());

    // Users 120-199 have no covariate → should be unchanged
    for i in 120..200 {
        assert!(
            (experiment_values[i].0 - adjusted[i].0).abs() < 1e-10,
            "unmatched user {i} should be unchanged"
        );
    }

    // Users 0-119 have covariates → should be adjusted (not identical to original)
    let mut any_changed = false;
    for i in 0..120 {
        if (experiment_values[i].0 - adjusted[i].0).abs() > 1e-10 {
            any_changed = true;
            break;
        }
    }
    assert!(any_changed, "matched users should have adjusted values");
}

// ── Interleaving preference scoring tests ───────────────────────────

/// Verify that variant (Team B) winning more queries produces a negative ΔAB and correct win/tie tallies.
#[test]
fn interleaving_preference_score_variant_wins() {
    // Variant (Team B) wins more queries → negative ΔAB
    let per_query = vec![
        (1, 3), // query 0: A=1, B=3 → B wins
        (0, 2), // query 1: A=0, B=2 → B wins
        (2, 3), // query 2: A=2, B=3 → B wins
        (1, 0), // query 3: A=1, B=0 → A wins
    ];
    let result = compute_preference_score(&per_query);
    assert!(
        result.delta_ab < 0.0,
        "variant preferred → negative ΔAB, got {}",
        result.delta_ab
    );
    assert_eq!(result.wins_a, 1);
    assert_eq!(result.wins_b, 3);
    assert_eq!(result.ties, 0);
}

/// Verify that control (Team A) winning more queries produces a positive ΔAB.
#[test]
fn interleaving_preference_score_control_wins() {
    // Control (Team A) wins more queries → positive ΔAB
    let per_query = vec![
        (3, 1), // A wins
        (2, 0), // A wins
        (1, 2), // B wins
    ];
    let result = compute_preference_score(&per_query);
    assert!(
        result.delta_ab > 0.0,
        "control preferred → positive ΔAB, got {}",
        result.delta_ab
    );
    assert_eq!(result.wins_a, 2);
    assert_eq!(result.wins_b, 1);
    assert_eq!(result.ties, 0);
}

/// Verify that equal win counts produce a ΔAB of zero and ties are counted separately.
#[test]
fn interleaving_preference_score_tie() {
    let per_query = vec![
        (2, 1), // A wins
        (1, 2), // B wins
        (1, 1), // tie
    ];
    let result = compute_preference_score(&per_query);
    assert_eq!(result.wins_a, 1);
    assert_eq!(result.wins_b, 1);
    assert_eq!(result.ties, 1);
    // ΔAB = (1-1)/(1+1+1) = 0
    assert!(
        (result.delta_ab).abs() < 1e-10,
        "equal wins → ΔAB ≈ 0, got {}",
        result.delta_ab
    );
}

/// Verify that a lopsided split (25 vs 5 wins) reaches significance at α = 0.05 using the normal-approximation sign test.
#[test]
fn interleaving_sign_test_significant() {
    // 30 queries, 25 won by B, 5 by A → should be significant
    let mut per_query = Vec::new();
    for _ in 0..25 {
        per_query.push((0, 3)); // B wins
    }
    for _ in 0..5 {
        per_query.push((3, 0)); // A wins
    }
    let result = compute_preference_score(&per_query);
    assert!(
        result.p_value < 0.05,
        "25 vs 5 wins should be significant, p={}",
        result.p_value
    );
}

/// Verify that a near-even split (6 vs 4 wins) at small n does not reach significance at α = 0.05.
#[test]
fn interleaving_sign_test_not_significant() {
    // 10 queries, 6 won by B, 4 by A → should NOT be significant (too few, too balanced)
    let mut per_query = Vec::new();
    for _ in 0..6 {
        per_query.push((0, 3)); // B wins
    }
    for _ in 0..4 {
        per_query.push((3, 0)); // A wins
    }
    let result = compute_preference_score(&per_query);
    assert!(
        result.p_value >= 0.05,
        "6 vs 4 wins should not be significant, p={}",
        result.p_value
    );
}

/// Verify that ties are excluded from the sign test denominator so only non-tied queries determine significance.
#[test]
fn interleaving_sign_test_ignores_ties() {
    // 3 ties + 20 wins by B + 2 wins by A → ties excluded from sign test
    let mut per_query = Vec::new();
    for _ in 0..3 {
        per_query.push((2, 2)); // tie
    }
    for _ in 0..20 {
        per_query.push((0, 3)); // B wins
    }
    for _ in 0..2 {
        per_query.push((3, 0)); // A wins
    }
    let result = compute_preference_score(&per_query);
    assert_eq!(result.ties, 3);
    // Sign test uses only 22 non-tied queries (20 + 2), not 25
    assert!(
        result.p_value < 0.05,
        "20 vs 2 wins should be significant, p={}",
        result.p_value
    );
    assert_eq!(result.wins_a + result.wins_b, 22);
}

#[test]
fn interleaving_preference_score_empty_input() {
    let per_query: Vec<(u32, u32)> = vec![];
    let result = compute_preference_score(&per_query);
    assert_eq!(result.wins_a, 0);
    assert_eq!(result.wins_b, 0);
    assert_eq!(result.ties, 0);
    assert!((result.delta_ab).abs() < 1e-10);
    assert!((result.p_value - 1.0).abs() < 1e-10, "empty → p=1.0");
}

#[test]
fn interleaving_preference_score_all_ties() {
    let per_query = vec![(1, 1), (2, 2), (0, 0)];
    let result = compute_preference_score(&per_query);
    assert_eq!(result.ties, 3);
    assert_eq!(result.wins_a, 0);
    assert_eq!(result.wins_b, 0);
    assert!((result.p_value - 1.0).abs() < 1e-10, "all ties → p=1.0");
}
