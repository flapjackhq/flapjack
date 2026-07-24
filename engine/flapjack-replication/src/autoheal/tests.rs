use super::*;

const CANDIDATE: &str = "node-b";
const OTHER: &str = "node-c";
const THIRD: &str = "node-d";

fn healthy(node_id: &str) -> PeerObservation {
    PeerObservation {
        node_id: node_id.to_string(),
        state: PeerObservationState::Healthy,
    }
}

fn failed(node_id: &str, consecutive_failures: u32) -> PeerObservation {
    PeerObservation {
        node_id: node_id.to_string(),
        state: PeerObservationState::Failed {
            consecutive_failures,
        },
    }
}

fn indeterminate(node_id: &str, reason: &str) -> PeerObservation {
    PeerObservation {
        node_id: node_id.to_string(),
        state: PeerObservationState::Indeterminate {
            reason: reason.to_string(),
        },
    }
}

fn expected_evict(node_id: &str) -> EvictionDecision {
    EvictionDecision::Evict {
        node_id: node_id.to_string(),
        reason: "sustained failure threshold reached and quorum remains".to_string(),
    }
}

mod journal;
mod policy;
