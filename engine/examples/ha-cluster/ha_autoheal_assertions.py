#!/usr/bin/env python3
"""Assertion helpers for the HA auto-heal shell contract test."""

import json
import os
import sys


NODE_C = "node-c"


def fail(message):
    raise SystemExit(message)


def load_json_from_stdin(kind):
    raw = sys.stdin.read()
    try:
        return json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f"malformed {kind} JSON: {error}")


def require_node_c_lifecycle(status):
    matches = [
        peer for peer in status.get("autoheal_peers", [])
        if peer.get("peer_id") == NODE_C
    ]
    if len(matches) != 1:
        fail(f"expected one node-c autoheal lifecycle entry, got {len(matches)}")
    return matches[0]


def require_peer_membership(status, expected_peer_ids, reason):
    peer_ids = {peer.get("peer_id") for peer in status.get("peers", [])}
    expected_peer_ids = set(expected_peer_ids)
    if peer_ids != expected_peer_ids:
        fail(
            f"{reason}, expected peers={sorted(expected_peer_ids)!r}, got peers={sorted(peer_ids)!r}"
        )


def assert_majority_refusal_status():
    status = load_json_from_stdin("cluster status")
    reason = os.environ["MAJORITY_REASON"]
    if status.get("autoheal_enabled") is not True:
        fail(f"autoheal_enabled expected true, got {status.get('autoheal_enabled')!r}")
    if status.get("peers_total") != 2:
        fail(f"peers_total expected 2, got {status.get('peers_total')!r}")

    require_peer_membership(
        status,
        {"node-b", NODE_C},
        "node-b and node-c must remain configured",
    )

    autoheal_peers = {
        peer.get("peer_id"): peer
        for peer in status.get("autoheal_peers", [])
        if peer.get("peer_id") in {"node-b", NODE_C}
    }
    if set(autoheal_peers) != {"node-b", NODE_C}:
        fail(f"missing autoheal lifecycle entries for node-b/node-c: {sorted(autoheal_peers)!r}")

    for peer_id in ("node-b", NODE_C):
        peer = autoheal_peers[peer_id]
        decision = peer.get("decision") or {}
        action = peer.get("action") or {}
        if decision.get("kind") != "refuse_indeterminate":
            fail(f"{peer_id} decision.kind expected refuse_indeterminate, got {decision.get('kind')!r}")
        if decision.get("reason") != reason:
            fail(f"{peer_id} decision.reason expected {reason!r}, got {decision.get('reason')!r}")
        if action.get("phase") != "decision_recorded":
            fail(f"{peer_id} action.phase expected decision_recorded, got {action.get('phase')!r}")
        if action.get("outcome") != "not_required":
            fail(f"{peer_id} action.outcome expected not_required, got {action.get('outcome')!r}")
        if peer.get("observation_count", -1) < 3:
            fail(f"{peer_id} observation_count expected >=3, got {peer.get('observation_count')!r}")

    print(json.dumps({
        "decision_kind": "refuse_indeterminate",
        "decision_reason": reason,
        "node_b_observation_count": autoheal_peers["node-b"]["observation_count"],
        "node_c_observation_count": autoheal_peers[NODE_C]["observation_count"],
        "peers_total": status["peers_total"],
    }, sort_keys=True))


def assert_eviction_status():
    status = load_json_from_stdin("cluster status")
    reason = os.environ["EVICT_REASON"]
    if status.get("autoheal_enabled") is not True:
        fail(f"autoheal_enabled expected true, got {status.get('autoheal_enabled')!r}")
    if status.get("peers_total") != 1:
        fail(f"survivor peers_total expected 1 after eviction, got {status.get('peers_total')!r}")

    local_node = status.get("node_id")
    if local_node not in {"node-a", "node-b"}:
        fail(f"eviction status expected node-a/node-b survivor, got {local_node!r}")
    survivor_peer = "node-b" if local_node == "node-a" else "node-a"
    require_peer_membership(
        status,
        {survivor_peer},
        "eviction must preserve only the surviving peer membership",
    )

    peer = require_node_c_lifecycle(status)
    decision = peer.get("decision") or {}
    action = peer.get("action") or {}
    if decision.get("kind") != "evict":
        fail(f"node-c decision.kind expected evict, got {decision.get('kind')!r}")
    if decision.get("node_id") != NODE_C:
        fail(f"node-c decision.node_id expected node-c, got {decision.get('node_id')!r}")
    if decision.get("reason") != reason:
        fail(f"node-c decision.reason expected {reason!r}, got {decision.get('reason')!r}")
    if action.get("phase") != "eviction_outcome":
        fail(f"node-c action.phase expected eviction_outcome, got {action.get('phase')!r}")
    if action.get("outcome") != "success":
        fail(f"node-c action.outcome expected success, got {action.get('outcome')!r}")
    if peer.get("observation_count") != 0:
        fail(f"node-c observation_count expected 0, got {peer.get('observation_count')!r}")

    print(json.dumps({
        "decision_kind": "evict",
        "decision_node_id": NODE_C,
        "observation_count": peer["observation_count"],
        "peers_total": status["peers_total"],
    }, sort_keys=True))


def assert_readmission_status():
    status = load_json_from_stdin("cluster status")
    if status.get("autoheal_enabled") is not True:
        fail(f"autoheal_enabled expected true, got {status.get('autoheal_enabled')!r}")
    if status.get("peers_total") != 2:
        fail(f"survivor peers_total expected 2 after readmission, got {status.get('peers_total')!r}")

    local_node = status.get("node_id")
    if local_node not in {"node-a", "node-b"}:
        fail(f"readmission status expected node-a/node-b survivor, got {local_node!r}")
    survivor_peer = "node-b" if local_node == "node-a" else "node-a"

    require_peer_membership(
        status,
        {survivor_peer, NODE_C},
        "readmission must restore node-c without losing the surviving peer",
    )

    action = require_node_c_lifecycle(status).get("action") or {}
    if action.get("phase") != "readmission_outcome":
        fail(f"node-c action.phase expected readmission_outcome, got {action.get('phase')!r}")
    if action.get("outcome") != "success":
        fail(f"node-c action.outcome expected success, got {action.get('outcome')!r}")

    print(json.dumps({
        "node_c_action_outcome": "success",
        "node_c_action_phase": "readmission_outcome",
        "peers_total": status["peers_total"],
    }, sort_keys=True))


def assert_disabled_status():
    status = load_json_from_stdin("cluster status")
    if status.get("autoheal_enabled") is not False:
        fail(f"autoheal_enabled expected false, got {status.get('autoheal_enabled')!r}")
    if status.get("peers_total") != 2:
        fail(f"disabled survivor peers_total expected 2, got {status.get('peers_total')!r}")

    local_node = status.get("node_id")
    if local_node not in {"node-a", "node-b"}:
        fail(f"disabled status expected node-a/node-b survivor, got {local_node!r}")
    survivor_peer = "node-b" if local_node == "node-a" else "node-a"

    require_peer_membership(
        status,
        {survivor_peer, NODE_C},
        "disabled mode must preserve both configured peers",
    )

    peer = require_node_c_lifecycle(status)
    decision = peer.get("decision") or {}
    action = peer.get("action") or {}
    if decision.get("kind") != "refuse_disabled":
        fail(f"node-c decision.kind expected refuse_disabled, got {decision.get('kind')!r}")
    if action.get("phase") != "decision_recorded":
        fail(f"node-c action.phase expected decision_recorded, got {action.get('phase')!r}")

    print(json.dumps({
        "decision_kind": "refuse_disabled",
        "node_c_action_phase": "decision_recorded",
        "peers_total": status["peers_total"],
    }, sort_keys=True))


def assert_exact_query_result():
    label = os.environ["LABEL"]
    raw = sys.stdin.read()
    try:
        result = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f"{label} malformed query JSON: {error}")

    object_id = os.environ["OBJECT_ID"]
    hits = result.get("hits") or []
    first = hits[0] if hits else {}
    if result.get("nbHits") != 1:
        fail(f"{label} nbHits expected 1, got {result.get('nbHits')!r}")
    if first.get("objectID") != object_id:
        fail(f"{label} first objectID expected {object_id!r}, got {first.get('objectID')!r}")

    print(json.dumps({
        "first_objectID": first["objectID"],
        "nbHits": result["nbHits"],
    }, sort_keys=True))


ASSERTIONS = {
    "majority_refusal": assert_majority_refusal_status,
    "eviction": assert_eviction_status,
    "readmission": assert_readmission_status,
    "disabled": assert_disabled_status,
    "exact_query": assert_exact_query_result,
}


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in ASSERTIONS:
        fail(f"usage: {sys.argv[0]} [{'|'.join(sorted(ASSERTIONS))}]")
    ASSERTIONS[sys.argv[1]]()


if __name__ == "__main__":
    main()
