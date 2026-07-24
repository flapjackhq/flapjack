use super::*;
use crate::config::PeerConfig;

fn journal_membership() -> Vec<String> {
    vec![CANDIDATE.to_string(), OTHER.to_string()]
}

fn journal_values(data_dir: &std::path::Path) -> Vec<serde_json::Value> {
    let path = AutohealJournal::path_in_data_dir(data_dir);
    let content = std::fs::read_to_string(path).expect("journal should be readable");
    content
        .lines()
        .map(|line| serde_json::from_str(line).expect("journal line should be valid JSON"))
        .collect()
}

fn peer_config(node_id: &str, addr: &str) -> PeerConfig {
    PeerConfig {
        node_id: node_id.to_string(),
        addr: addr.to_string(),
    }
}

#[test]
fn autoheal_journal_records_hold_and_each_refusal_class_as_exact_decisions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let membership = journal_membership();
    let decisions = [
        EvictionDecision::Hold {
            observations_remaining: 2,
        },
        EvictionDecision::RefuseDisabled,
        EvictionDecision::RefuseWouldBreakQuorum {
            current: 1,
            required: 2,
        },
        EvictionDecision::RefuseIndeterminate {
            reason: "probe returned HTTP 500".to_string(),
        },
    ];

    for decision in decisions {
        journal
            .record_decision(&membership, CANDIDATE, decision)
            .unwrap();
    }

    let values = journal_values(temp_dir.path());
    assert_eq!(values.len(), 4);
    assert_eq!(
        values
            .iter()
            .map(|value| value["decision_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "autoheal-0000000000000001",
            "autoheal-0000000000000002",
            "autoheal-0000000000000003",
            "autoheal-0000000000000004"
        ]
    );
    assert_eq!(
        values
            .iter()
            .map(|value| value["action"]["phase"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["decision_recorded"; 4]
    );
    assert_eq!(values[0]["decision"]["kind"], "hold");
    assert_eq!(values[1]["decision"]["kind"], "refuse_disabled");
    assert_eq!(values[2]["decision"]["kind"], "refuse_would_break_quorum");
    assert_eq!(values[3]["decision"]["kind"], "refuse_indeterminate");
    assert_eq!(
        values[0]["membership_peer_ids"],
        serde_json::json!(membership)
    );
    assert_eq!(values[0]["candidate_peer_id"], CANDIDATE);
}

#[test]
fn autoheal_journal_pins_successful_and_unknown_evictions_until_readmission_succeeds() {
    let temp_dir = tempfile::tempdir().unwrap();
    let membership = journal_membership();
    let candidate = peer_config(CANDIDATE, "http://node-b.example.com:7700");
    let unknown = peer_config(OTHER, "http://node-c.example.com:7700");
    {
        let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 1_900).unwrap();
        journal
            .record_eviction(
                &membership,
                CANDIDATE,
                Some(candidate.clone()),
                expected_evict(CANDIDATE),
                || Ok(()),
            )
            .unwrap();
        journal
            .record_eviction_intent(
                &membership,
                OTHER,
                Some(unknown.clone()),
                expected_evict(OTHER),
            )
            .unwrap();
        for _ in 0..30 {
            journal
                .record_decision(
                    &membership,
                    CANDIDATE,
                    EvictionDecision::Hold {
                        observations_remaining: 2,
                    },
                )
                .unwrap();
        }
    }

    let mut reopened = AutohealJournal::with_max_bytes(temp_dir.path(), 1_900).unwrap();
    let candidates = reopened.unresolved_readmission_candidates().unwrap();

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].peer_config.node_id, CANDIDATE);
    assert_eq!(candidates[0].peer_config.addr, candidate.addr);
    assert_eq!(candidates[0].eviction_outcome, "success");
    assert_eq!(candidates[1].peer_config.node_id, OTHER);
    assert_eq!(candidates[1].peer_config.addr, unknown.addr);
    assert_eq!(candidates[1].eviction_outcome, "outcome_unknown");

    reopened
        .record_readmission(
            &membership,
            &candidate,
            candidates[0].eviction_decision_id.clone(),
            || Ok::<(), String>(()),
        )
        .unwrap();
    let remaining = reopened.unresolved_readmission_candidates().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].peer_config.node_id, OTHER);

    let path = AutohealJournal::path_in_data_dir(temp_dir.path());
    assert!(std::fs::metadata(&path).unwrap().len() <= 1_900);
    let values = journal_values(temp_dir.path());
    assert!(values.iter().any(|value| {
        value["candidate_peer_id"] == OTHER
            && value["candidate_peer_config"]["addr"] == "http://node-c.example.com:7700"
    }));
    assert!(!values.iter().any(|value| {
        value["candidate_peer_id"] == CANDIDATE
            && value["action"]["phase"] == "eviction_outcome"
            && value["action"]["outcome"] == "success"
    }));
}

#[test]
fn autoheal_journal_legacy_eviction_records_are_inspectable_but_not_replayed() {
    let temp_dir = tempfile::tempdir().unwrap();
    let legacy = serde_json::json!({
        "decision_id": "autoheal-0000000000000001",
        "timestamp_millis": 123,
        "membership_peer_ids": [CANDIDATE, OTHER],
        "candidate_peer_id": CANDIDATE,
        "decision": {
            "kind": "evict",
            "node_id": CANDIDATE,
            "reason": "sustained failure threshold reached and quorum remains"
        },
        "action": {
            "phase": "eviction_outcome",
            "outcome": "success",
            "error": null
        }
    });
    std::fs::write(
        AutohealJournal::path_in_data_dir(temp_dir.path()),
        format!("{legacy}\n"),
    )
    .unwrap();

    let journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let events = journal.events().unwrap();
    let candidates = journal.unresolved_readmission_candidates().unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].candidate_peer_id, CANDIDATE);
    assert!(events[0].candidate_peer_config.is_none());
    assert!(candidates.is_empty());
}

#[test]
fn autoheal_journal_records_successful_and_failed_eviction_transactions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let membership = journal_membership();

    journal
        .record_eviction(
            &membership,
            CANDIDATE,
            None,
            expected_evict(CANDIDATE),
            || Ok(()),
        )
        .unwrap();
    let error = journal
        .record_eviction(&membership, OTHER, None, expected_evict(OTHER), || {
            Err("node.json write failed".to_string())
        })
        .unwrap_err();

    assert_eq!(error, "node.json write failed");
    let values = journal_values(temp_dir.path());
    assert_eq!(values.len(), 4);
    assert_eq!(values[0]["decision_id"], values[1]["decision_id"]);
    assert_eq!(values[0]["action"]["phase"], "eviction_intent");
    assert_eq!(values[1]["action"]["phase"], "eviction_outcome");
    assert_eq!(values[1]["action"]["outcome"], "success");
    assert_eq!(values[2]["decision_id"], values[3]["decision_id"]);
    assert_eq!(values[3]["action"]["outcome"], "failure");
    assert_eq!(values[3]["action"]["error"], "node.json write failed");
}

#[test]
fn autoheal_journal_reopen_closes_dangling_intent_as_unknown_without_replaying_action() {
    let temp_dir = tempfile::tempdir().unwrap();
    let membership = journal_membership();
    {
        let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
        journal
            .record_eviction_intent(&membership, CANDIDATE, None, expected_evict(CANDIDATE))
            .unwrap();
    }

    let mut reopened = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let fresh_window = reopened.fresh_observation_window(membership.clone());
    let values = journal_values(temp_dir.path());

    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["decision_id"], values[1]["decision_id"]);
    assert_eq!(values[1]["action"]["phase"], "eviction_recovery");
    assert_eq!(values[1]["action"]["outcome"], "outcome_unknown");
    assert_eq!(
        fresh_window.decide(true, 2, CANDIDATE),
        EvictionDecision::Hold {
            observations_remaining: 2
        }
    );
}

#[test]
fn autoheal_journal_compacts_without_splitting_transactions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 1_800).unwrap();
    let membership = journal_membership();

    for _ in 0..30 {
        journal
            .record_decision(
                &membership,
                CANDIDATE,
                EvictionDecision::Hold {
                    observations_remaining: 2,
                },
            )
            .unwrap();
    }
    journal
        .record_eviction_intent(&membership, CANDIDATE, None, expected_evict(CANDIDATE))
        .unwrap();

    let path = AutohealJournal::path_in_data_dir(temp_dir.path());
    assert!(std::fs::metadata(&path).unwrap().len() <= 1_800);
    let values = journal_values(temp_dir.path());
    assert!(values
        .last()
        .is_some_and(|value| value["action"]["phase"] == "eviction_intent"));
    let mut counts = BTreeMap::new();
    for value in values {
        *counts
            .entry(value["decision_id"].as_str().unwrap().to_string())
            .or_insert(0) += 1;
    }
    assert!(
        counts.values().all(|count| *count == 1),
        "single-line transactions must not be split or duplicated"
    );
}

#[test]
fn autoheal_journal_rejects_single_transaction_larger_than_configured_cap() {
    let temp_dir = tempfile::tempdir().unwrap();
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 128).unwrap();
    let membership = journal_membership();

    let error = journal
        .record_decision(
            &membership,
            CANDIDATE,
            EvictionDecision::RefuseIndeterminate {
                reason: "x".repeat(512),
            },
        )
        .unwrap_err();

    assert!(error.contains("exceeds auto-heal journal max_bytes"));
    let path = AutohealJournal::path_in_data_dir(temp_dir.path());
    assert!(std::fs::metadata(&path).unwrap().len() <= 128);
    assert!(journal_values(temp_dir.path()).is_empty());
}

#[test]
fn autoheal_journal_reopen_truncates_only_malformed_final_fragment() {
    let temp_dir = tempfile::tempdir().unwrap();
    let membership = journal_membership();
    {
        let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
        journal
            .record_decision(
                &membership,
                CANDIDATE,
                EvictionDecision::Hold {
                    observations_remaining: 2,
                },
            )
            .unwrap();
    }
    let path = AutohealJournal::path_in_data_dir(temp_dir.path());
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(br#"{"decision_id":"partial"#)
        .unwrap();

    let _reopened = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let values = journal_values(temp_dir.path());

    assert_eq!(values.len(), 1);
    assert_eq!(values[0]["decision_id"], "autoheal-0000000000000001");
    assert!(!std::fs::read_to_string(path).unwrap().contains("partial"));
}

#[test]
fn autoheal_journal_write_failure_surfaces_before_eviction_action_runs() {
    let temp_dir = tempfile::tempdir().unwrap();
    let membership = journal_membership();
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let path = AutohealJournal::path_in_data_dir(temp_dir.path());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let action_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let action_called_in_closure = action_called.clone();

    let error = journal
        .record_eviction(
            &membership,
            CANDIDATE,
            None,
            expected_evict(CANDIDATE),
            || {
                action_called_in_closure.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap_err();

    assert!(error.contains("failed to open"));
    assert!(
        !action_called.load(std::sync::atomic::Ordering::SeqCst),
        "eviction action must not run before a durable intent exists"
    );
}
