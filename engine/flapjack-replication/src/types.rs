use flapjack::index::oplog::OpLogEntry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Opt-in release transport headers layered over the existing replication
/// endpoints. Legacy peer traffic omits the contract header and keeps its
/// existing JSON wire format; release orchestration must bind every response
/// to one tenant and one exact source sequence interval.
pub const RELEASE_TRANSFER_CONTRACT_HEADER: &str = "x-flapjack-release-transfer";
pub const RELEASE_TRANSFER_CONTRACT_V1: &str = "one-uid-contiguous-v1";
pub const RELEASE_TRANSFER_TENANT_HEADER: &str = "x-flapjack-release-transfer-tenant";
pub const RELEASE_TRANSFER_AFTER_SEQ_HEADER: &str = "x-flapjack-release-transfer-after-seq";
pub const RELEASE_TRANSFER_THROUGH_SEQ_HEADER: &str = "x-flapjack-release-transfer-through-seq";
pub const RELEASE_TRANSFER_STATUS_HEADER: &str = "x-flapjack-release-transfer-status";
pub const RELEASE_TRANSFER_STATUS_CONTIGUOUS: &str = "contiguous";
pub const RELEASE_TRANSFER_STATUS_RESNAPSHOT_REQUIRED: &str = "resnapshot_required";
pub const RELEASE_TRANSFER_STATUS_ACKNOWLEDGED: &str = "acknowledged";
pub const RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER: &str =
    "x-flapjack-release-transfer-snapshot-sha256";
pub const RELEASE_TRANSFER_TRANSACTION_HEADER: &str = "x-flapjack-release-transfer-transaction";
pub const RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER: &str =
    "x-flapjack-release-transfer-payload-sha256";

/// Request to replicate operations to a peer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicateOpsRequest {
    pub tenant_id: String,
    pub ops: Vec<OpLogEntry>,
}

/// Response from replicating operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicateOpsResponse {
    pub tenant_id: String,
    pub acked_seq: u64, // Highest sequence number successfully applied
}

/// Query parameters for fetching operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetOpsQuery {
    pub tenant_id: String,
    pub since_seq: u64, // Fetch ops with seq > since_seq
}

/// Response containing operations for catch-up
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetOpsResponse {
    pub tenant_id: String,
    pub ops: Vec<OpLogEntry>,
    pub current_seq: u64, // Latest sequence number on this node
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_retained_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub node_current_seqs: BTreeMap<String, u64>,
}

/// Response containing tenant IDs available on a peer node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListTenantsResponse {
    pub tenants: Vec<String>,
}

/// Basic replication status for monitoring
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationStatus {
    pub node_id: String,
    pub replication_enabled: bool,
    pub peer_count: usize,
}

/// Health status of a single peer, derived from last_success tracking and circuit breaker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerHealthStatus {
    pub peer_id: String,
    pub addr: String,
    /// Seconds since last successful replication. None = never contacted.
    pub last_success_secs_ago: Option<u64>,
    /// "healthy" (<60s), "stale" (60-300s), "unhealthy" (>300s),
    /// "circuit_open" (circuit breaker tripped), "never_contacted"
    pub status: String,
}
