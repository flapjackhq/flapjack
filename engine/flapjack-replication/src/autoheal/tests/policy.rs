use super::*;

// Auto-heal does not reuse CircuitBreaker as its accounting owner. CircuitBreaker is
// a per-peer request gate whose record_success() resets one peer's consecutive
// failures; quorum eviction needs an immutable cluster observation window using
// N = peer_count_at_observation_start + 1 across all peers.
#[test]
fn autoheal_single_failure_holds_with_exact_observations_remaining() {
    let sustained_failure_threshold = 3;
    let observations = vec![failed(CANDIDATE, 1), healthy(OTHER)];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
        decision,
        EvictionDecision::Hold {
            observations_remaining: sustained_failure_threshold - 1
        }
    );
}

#[test]
fn autoheal_threshold_minus_one_holds_with_one_observation_remaining() {
    let sustained_failure_threshold = 4;
    let observations = vec![
        failed(CANDIDATE, sustained_failure_threshold - 1),
        healthy(OTHER),
    ];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
        decision,
        EvictionDecision::Hold {
            observations_remaining: 1
        }
    );
}

#[test]
fn autoheal_sustained_failures_evict_only_when_quorum_remains() {
    let sustained_failure_threshold = 3;
    let observations = vec![
        failed(CANDIDATE, sustained_failure_threshold),
        healthy(OTHER),
        healthy(THIRD),
    ];

    let decision = decide(
        true,
        3,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(decision, expected_evict(CANDIDATE));
}

#[test]
fn autoheal_failures_over_threshold_remain_eligible_when_quorum_remains() {
    let sustained_failure_threshold = 3;
    let observations = vec![
        failed(CANDIDATE, sustained_failure_threshold + 1),
        healthy(OTHER),
    ];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(decision, expected_evict(CANDIDATE));
}

#[test]
fn autoheal_two_of_three_majority_unreachable_refuses_eviction() {
    let sustained_failure_threshold = 3;
    let observations = vec![
        failed(CANDIDATE, sustained_failure_threshold),
        failed(OTHER, sustained_failure_threshold),
    ];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
            decision,
            EvictionDecision::RefuseIndeterminate {
                reason: "2 failed peers constitute a majority of configured peers; local node may be isolated".to_string()
            }
        );
}

#[test]
fn autoheal_transiently_failed_peer_is_unavailable_for_n3_quorum() {
    let sustained_failure_threshold = 3;
    let observations = vec![
        failed(CANDIDATE, sustained_failure_threshold),
        failed(OTHER, 1),
    ];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
            decision,
            EvictionDecision::RefuseIndeterminate {
                reason: "2 failed peers constitute a majority of configured peers; local node may be isolated".to_string()
            }
        );
}

#[test]
fn autoheal_recovered_candidate_resets_stale_observations_to_full_hold_window() {
    let sustained_failure_threshold = 3;
    let observations = vec![healthy(CANDIDATE), healthy(OTHER)];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
        decision,
        EvictionDecision::Hold {
            observations_remaining: sustained_failure_threshold
        }
    );
}

#[test]
fn autoheal_disabled_refuses_single_and_sustained_failure_cases() {
    let sustained_failure_threshold = 3;
    for candidate_observation in [
        failed(CANDIDATE, 1),
        failed(CANDIDATE, sustained_failure_threshold),
    ] {
        let observations = vec![candidate_observation, healthy(OTHER)];

        let decision = decide(
            false,
            2,
            sustained_failure_threshold,
            &observations,
            CANDIDATE,
        );

        assert_eq!(decision, EvictionDecision::RefuseDisabled);
    }
}

#[test]
fn autoheal_n3_denominator_allows_one_failed_peer_to_leave_two_node_quorum() {
    let sustained_failure_threshold = 2;
    let observations = vec![
        failed(CANDIDATE, sustained_failure_threshold),
        healthy(OTHER),
    ];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(decision, expected_evict(CANDIDATE));
}

#[test]
fn autoheal_n4_denominator_refuses_when_eviction_leaves_two_below_required_three() {
    let sustained_failure_threshold = 2;
    let observations = vec![
        failed(CANDIDATE, sustained_failure_threshold),
        failed(OTHER, sustained_failure_threshold),
        healthy(THIRD),
    ];

    let decision = decide(
        true,
        3,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
            decision,
            EvictionDecision::RefuseIndeterminate {
                reason: "2 failed peers constitute a majority of configured peers; local node may be isolated".to_string()
            }
        );
}

#[test]
fn autoheal_two_node_cluster_single_unreachable_refuses_because_quorum_would_break() {
    let sustained_failure_threshold = 3;
    let observations = vec![failed(CANDIDATE, sustained_failure_threshold)];

    let decision = decide(
        true,
        1,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
        decision,
        EvictionDecision::RefuseWouldBreakQuorum {
            current: 1,
            required: 2
        }
    );
}

#[test]
fn autoheal_candidate_missing_from_observations_refuses_as_indeterminate() {
    let sustained_failure_threshold = 3;
    let observations = vec![healthy(OTHER), healthy(THIRD)];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
        decision,
        EvictionDecision::RefuseIndeterminate {
            reason: "candidate peer node-b has no observation".to_string()
        }
    );
}

#[test]
fn autoheal_candidate_indeterminate_observation_refuses_with_reason() {
    let sustained_failure_threshold = 3;
    let observations = vec![
        indeterminate(
            CANDIDATE,
            "probe timed out before recording success or failure",
        ),
        healthy(OTHER),
    ];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
            decision,
            EvictionDecision::RefuseIndeterminate {
                reason: "candidate peer node-b observation indeterminate: probe timed out before recording success or failure".to_string()
            }
        );
}

#[test]
fn autoheal_duplicate_peer_observations_refuse_as_indeterminate() {
    let sustained_failure_threshold = 3;
    let observations = vec![
        failed(CANDIDATE, sustained_failure_threshold),
        healthy(CANDIDATE),
        healthy(OTHER),
    ];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
        decision,
        EvictionDecision::RefuseIndeterminate {
            reason: "peer node-b has duplicate observations in the auto-heal decision input"
                .to_string()
        }
    );
}

#[test]
fn autoheal_zero_threshold_refuses_as_indeterminate_policy() {
    let sustained_failure_threshold = 0;
    let observations = vec![failed(CANDIDATE, 1), healthy(OTHER)];

    let decision = decide(
        true,
        2,
        sustained_failure_threshold,
        &observations,
        CANDIDATE,
    );

    assert_eq!(
        decision,
        EvictionDecision::RefuseIndeterminate {
            reason: "sustained failure threshold must be greater than zero".to_string()
        }
    );
}

#[test]
fn autoheal_observation_window_fail_recover_fail_resets_stale_count() {
    let sustained_failure_threshold = 3;
    let mut window = AutohealObservationWindow::new(2, [CANDIDATE.to_string(), OTHER.to_string()]);

    window.record_failure(CANDIDATE);
    window.record_success(CANDIDATE);
    window.record_failure(CANDIDATE);
    window.record_success(OTHER);

    let decision = window.decide(true, sustained_failure_threshold, CANDIDATE);

    assert_eq!(
        decision,
        EvictionDecision::Hold {
            observations_remaining: sustained_failure_threshold - 1
        }
    );
}

#[test]
fn autoheal_observation_window_refuses_peer_count_membership_cardinality_mismatch() {
    let sustained_failure_threshold = 3;
    let mut window = AutohealObservationWindow::new(3, [CANDIDATE.to_string(), OTHER.to_string()]);

    window.record_failure(CANDIDATE);
    window.record_success(OTHER);

    let decision = window.decide(true, sustained_failure_threshold, CANDIDATE);

    assert_eq!(
        decision,
        EvictionDecision::RefuseIndeterminate {
            reason: "peer_count_at_observation_start 3 does not match membership snapshot size 2"
                .to_string()
        }
    );
}

#[test]
fn autoheal_observation_window_rejects_duplicate_membership_ids() {
    let sustained_failure_threshold = 3;
    let mut window =
        AutohealObservationWindow::new(2, [CANDIDATE.to_string(), CANDIDATE.to_string()]);

    window.record_failure(CANDIDATE);

    let decision = window.decide(true, sustained_failure_threshold, CANDIDATE);

    assert_eq!(
        decision,
        EvictionDecision::RefuseIndeterminate {
            reason: "observation window membership contains duplicate peer node-b".to_string()
        }
    );
}

#[test]
fn autoheal_observation_window_rejects_observations_outside_membership_snapshot() {
    let sustained_failure_threshold = 3;
    let mut window = AutohealObservationWindow::new(2, [CANDIDATE.to_string(), OTHER.to_string()]);

    window.record_failure("node-x");
    window.record_failure(CANDIDATE);
    window.record_success(OTHER);

    let decision = window.decide(true, sustained_failure_threshold, CANDIDATE);

    assert_eq!(
        decision,
        EvictionDecision::RefuseIndeterminate {
            reason: "peer node-x was observed outside the auto-heal membership snapshot"
                .to_string()
        }
    );
}

#[test]
fn autoheal_disabled_refuses_every_input_class_before_other_preconditions() {
    let sustained_failure_threshold = 3;
    let cases = [
        (
            "healthy candidate",
            2,
            sustained_failure_threshold,
            vec![healthy(CANDIDATE), healthy(OTHER)],
        ),
        (
            "missing candidate",
            2,
            sustained_failure_threshold,
            vec![healthy(OTHER), healthy(THIRD)],
        ),
        (
            "indeterminate candidate",
            2,
            sustained_failure_threshold,
            vec![indeterminate(CANDIDATE, "probe timeout"), healthy(OTHER)],
        ),
        (
            "zero threshold policy",
            2,
            0,
            vec![failed(CANDIDATE, 1), healthy(OTHER)],
        ),
        (
            "quorum-breaking input",
            3,
            sustained_failure_threshold,
            vec![
                failed(CANDIDATE, sustained_failure_threshold),
                failed(OTHER, sustained_failure_threshold),
                healthy(THIRD),
            ],
        ),
    ];

    for (case_name, peer_count_at_observation_start, threshold, observations) in cases {
        let decision = decide(
            false,
            peer_count_at_observation_start,
            threshold,
            &observations,
            CANDIDATE,
        );

        assert_eq!(
            decision,
            EvictionDecision::RefuseDisabled,
            "{case_name} must refuse before evaluating any other input class"
        );
    }
}

#[test]
fn autoheal_cycle_success_resets_candidate_failures() {
    let mut orchestrator = AutohealCycle::new(true, 2, [CANDIDATE.to_string(), OTHER.to_string()]);

    assert_eq!(
        orchestrator.record_probe_result(CANDIDATE, ProbeOutcome::Unreachable),
        EvictionDecision::Hold {
            observations_remaining: 1
        }
    );
    assert_eq!(
        orchestrator.record_probe_result(CANDIDATE, ProbeOutcome::Healthy),
        EvictionDecision::Hold {
            observations_remaining: 2
        }
    );
    assert_eq!(
        orchestrator.record_probe_result(CANDIDATE, ProbeOutcome::Unreachable),
        EvictionDecision::Hold {
            observations_remaining: 1
        }
    );
}

#[test]
fn autoheal_cycle_sustained_failures_produce_one_eviction_action() {
    let mut orchestrator = AutohealCycle::new(true, 2, [CANDIDATE.to_string(), OTHER.to_string()]);

    assert_eq!(
        orchestrator.record_probe_result(CANDIDATE, ProbeOutcome::Unreachable),
        EvictionDecision::Hold {
            observations_remaining: 1
        }
    );
    assert_eq!(
        orchestrator.record_probe_result(CANDIDATE, ProbeOutcome::Unreachable),
        expected_evict(CANDIDATE)
    );
    orchestrator.record_eviction_succeeded(CANDIDATE);
    assert_eq!(
        orchestrator.record_probe_result(CANDIDATE, ProbeOutcome::Unreachable),
        EvictionDecision::Hold {
            observations_remaining: 0
        }
    );
}

#[test]
fn autoheal_cycle_quorum_breaking_and_indeterminate_inputs_produce_no_action() {
    let mut quorum_breaking = AutohealCycle::new(true, 1, [CANDIDATE.to_string()]);
    quorum_breaking.record_probe_result(CANDIDATE, ProbeOutcome::Unreachable);
    assert_eq!(
        quorum_breaking.record_probe_result(CANDIDATE, ProbeOutcome::Unreachable),
        EvictionDecision::RefuseWouldBreakQuorum {
            current: 1,
            required: 2
        }
    );

    let mut indeterminate = AutohealCycle::new(true, 2, [CANDIDATE.to_string(), OTHER.to_string()]);
    indeterminate.record_probe_result(CANDIDATE, ProbeOutcome::Unreachable);
    assert_eq!(
        indeterminate.record_probe_result(
            CANDIDATE,
            ProbeOutcome::Indeterminate {
                reason: "HTTP 500".to_string(),
            },
        ),
        EvictionDecision::RefuseIndeterminate {
            reason: "candidate peer node-b observation indeterminate: HTTP 500".to_string()
        }
    );
}

#[test]
fn autoheal_cycle_membership_change_replaces_active_observation_window() {
    let mut orchestrator = AutohealCycle::new(true, 2, [CANDIDATE.to_string(), OTHER.to_string()]);
    orchestrator.record_probe_result(CANDIDATE, ProbeOutcome::Unreachable);

    orchestrator.replace_membership([CANDIDATE.to_string(), THIRD.to_string()]);

    assert_eq!(
        orchestrator.record_probe_result(CANDIDATE, ProbeOutcome::Unreachable),
        EvictionDecision::Hold {
            observations_remaining: 1
        }
    );
}
