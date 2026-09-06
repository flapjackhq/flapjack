//! Stub summary for engine/flapjack-replication/src/manager.rs.
use super::autoheal::{
    AutohealActionRecord, AutohealCycle, AutohealJournal, AutohealJournalEvent, EvictionDecision,
    ProbeOutcome, DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD,
};
use super::circuit_breaker::CircuitState;
use super::config::{NodeConfig, PeerConfig};
use super::peer::{PeerClient, PeerHealthCheck};
use super::types::{
    GetOpsQuery, GetOpsResponse, ListTenantsResponse, PeerHealthStatus, ReplicateOpsRequest,
};
use dashmap::DashMap;
use flapjack::index::oplog::OpLogEntry;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task::JoinHandle;

/// Canonical per-peer delivery status tracked by the replication owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerCursor {
    pub last_acked_seq: Option<u64>,
    pub last_delivery_error: Option<String>,
}

impl PeerCursor {
    fn acknowledged(acked_seq: u64) -> Self {
        Self {
            last_acked_seq: Some(acked_seq),
            last_delivery_error: None,
        }
    }

    fn failed(error: String, last_acked_seq: Option<u64>) -> Self {
        Self {
            last_acked_seq,
            last_delivery_error: Some(error),
        }
    }
}

/// Point-in-time receipt for a successful runtime peer membership add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddPeerReceipt {
    pub node_id: String,
    pub addr: String,
    pub peers_total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddPeerError {
    Conflict(String),
    Persistence(String),
}

impl std::fmt::Display for AddPeerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(message) | Self::Persistence(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AddPeerError {}

impl From<String> for AddPeerError {
    fn from(message: String) -> Self {
        Self::Persistence(message)
    }
}

/// Point-in-time receipt for a successful runtime peer membership removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovePeerReceipt {
    pub node_id: String,
    pub peers_total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutohealLifecycleProjection {
    pub autoheal_enabled: bool,
    pub peers: Vec<AutohealPeerLifecycle>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutohealPeerLifecycle {
    pub peer_id: String,
    pub addr: Option<String>,
    pub observation_count: u32,
    pub eviction_decision_id: Option<String>,
    pub last_decision: Option<EvictionDecision>,
    pub last_action: Option<AutohealActionRecord>,
}

#[derive(Debug, Clone, Default)]
struct AutohealLifecycleState {
    autoheal_enabled: bool,
    peers: BTreeMap<String, AutohealPeerLifecycle>,
}

impl From<PeerHealthCheck> for ProbeOutcome {
    fn from(value: PeerHealthCheck) -> Self {
        match value {
            PeerHealthCheck::Healthy => Self::Healthy,
            PeerHealthCheck::Unreachable { .. } => Self::Unreachable,
            PeerHealthCheck::Indeterminate { reason } => Self::Indeterminate { reason },
        }
    }
}

/// Orchestrates replication to all peers and tracks their acknowledgment status
pub struct ReplicationManager {
    node_id: String,
    bind_addr: String,
    advertise_addr: Option<String>,
    data_dir: PathBuf,
    peer_credential: Option<String>,
    peers: RwLock<Vec<Arc<PeerClient>>>,
    /// Tracks delivery status for each configured peer and tenant
    /// Outer map: tenant_id -> inner map
    /// Inner map: peer_id -> last delivery cursor/error
    peer_cursors: Arc<DashMap<String, DashMap<String, PeerCursor>>>,
    /// Handle to the background health probe task (if running)
    health_probe_handle: Mutex<Option<JoinHandle<()>>>,
    autoheal_lifecycle: RwLock<AutohealLifecycleState>,
}

impl ReplicationManager {
    fn validate_discovered_tenant_id(peer_id: &str, tenant_id: &str) -> Result<(), String> {
        flapjack::validate_index_name(tenant_id).map_err(|error| {
            format!(
                "peer {} returned invalid tenant id '{}': {}",
                peer_id, tenant_id, error
            )
        })
    }

    /// Initialize a ReplicationManager from the given configuration, creating PeerClient instances for each configured peer. Peer acknowledgment cursors start empty, and the background health probe is not running until explicitly started via `start_health_probe`.
    ///
    /// # Arguments
    ///
    /// * `node_config` - Configuration containing this node's identity and the list of peer addresses to replicate to.
    ///
    /// # Returns
    ///
    /// An Arc-wrapped ReplicationManager ready for use in multi-threaded contexts.
    pub fn new(
        node_config: NodeConfig,
        peer_credential: Option<String>,
        data_dir: PathBuf,
    ) -> Arc<Self> {
        let peers: Vec<Arc<PeerClient>> = node_config
            .peers
            .iter()
            .map(|peer_config| {
                Arc::new(PeerClient::new(
                    peer_config.node_id.clone(),
                    peer_config.addr.clone(),
                    peer_credential.clone(),
                ))
            })
            .collect();

        let autoheal_lifecycle = Self::hydrate_autoheal_lifecycle(&data_dir);

        Arc::new(Self {
            node_id: node_config.node_id,
            bind_addr: node_config.bind_addr,
            advertise_addr: node_config.advertise_addr,
            data_dir,
            peer_credential,
            peers: RwLock::new(peers),
            peer_cursors: Arc::new(DashMap::new()),
            health_probe_handle: Mutex::new(None),
            autoheal_lifecycle: RwLock::new(autoheal_lifecycle),
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    fn active_node_config_with_peers(&self, peers: Vec<PeerConfig>) -> NodeConfig {
        NodeConfig {
            node_id: self.node_id.clone(),
            bind_addr: self.bind_addr.clone(),
            advertise_addr: self.advertise_addr.clone(),
            peers,
            bootstrap_peer: None,
        }
    }

    fn persist_peer_membership(&self, peers: Vec<PeerConfig>) -> Result<(), String> {
        let config = self.active_node_config_with_peers(peers.clone());
        config.persist_peers(&self.data_dir, peers)
    }

    fn peer_configs_from_clients(peers: &[Arc<PeerClient>]) -> Vec<PeerConfig> {
        peers
            .iter()
            .map(|peer| PeerConfig {
                node_id: peer.peer_id().to_string(),
                addr: peer.base_url().to_string(),
            })
            .collect()
    }

    fn sorted_peer_ids_from_clients(peers: &[Arc<PeerClient>]) -> Vec<String> {
        let mut peer_ids = peers
            .iter()
            .map(|peer| peer.peer_id().to_string())
            .collect::<Vec<_>>();
        peer_ids.sort();
        peer_ids
    }

    fn peer_snapshot(&self) -> Vec<Arc<PeerClient>> {
        self.peers
            .read()
            .expect("replication peer lock poisoned")
            .clone()
    }

    pub fn peer_count(&self) -> usize {
        self.peer_snapshot().len()
    }

    fn current_peer_ids_sorted(&self) -> Vec<String> {
        Self::sorted_peer_ids_from_clients(&self.peer_snapshot())
    }

    fn current_peer_configs_sorted(&self) -> Vec<PeerConfig> {
        let mut configs = Self::peer_configs_from_clients(&self.peer_snapshot());
        configs.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        configs
    }

    pub fn autoheal_lifecycle_projection(&self) -> AutohealLifecycleProjection {
        let state = self
            .autoheal_lifecycle
            .read()
            .expect("auto-heal lifecycle lock poisoned");
        let mut peers = state.peers.values().cloned().collect::<Vec<_>>();
        peers.sort_by(|left, right| left.peer_id.cmp(&right.peer_id));
        AutohealLifecycleProjection {
            autoheal_enabled: state.autoheal_enabled,
            peers,
        }
    }

    fn hydrate_autoheal_lifecycle(data_dir: &Path) -> AutohealLifecycleState {
        if !AutohealJournal::path_in_data_dir(data_dir).exists() {
            return AutohealLifecycleState::default();
        }

        let events = match AutohealJournal::read_events_read_only(data_dir) {
            Ok(events) => events,
            Err(error) => {
                tracing::warn!(
                    "[autoheal] lifecycle projection could not read journal at startup: {}",
                    error
                );
                return AutohealLifecycleState::default();
            }
        };
        let mut state = AutohealLifecycleState::default();
        for event in events {
            apply_lifecycle_event(&mut state, &event);
        }
        state
    }

    fn initialize_autoheal_lifecycle(&self, autoheal_enabled: bool) {
        let active_peers = self.current_peer_configs_sorted();
        let mut state = self
            .autoheal_lifecycle
            .write()
            .expect("auto-heal lifecycle lock poisoned");
        state.autoheal_enabled = autoheal_enabled;
        if autoheal_enabled {
            upsert_lifecycle_active_peers(&mut state, active_peers, &BTreeMap::new());
        }
    }

    fn update_autoheal_lifecycle_observations(&self, cycle: &AutohealCycle) {
        let active_peers = self.current_peer_configs_sorted();
        let observation_counts = cycle.observation_counts();
        let mut state = self
            .autoheal_lifecycle
            .write()
            .expect("auto-heal lifecycle lock poisoned");
        upsert_lifecycle_active_peers(&mut state, active_peers, &observation_counts);
    }

    fn record_autoheal_lifecycle_decision(
        &self,
        candidate_peer_id: &str,
        decision: EvictionDecision,
        action: AutohealActionRecord,
    ) {
        let mut state = self
            .autoheal_lifecycle
            .write()
            .expect("auto-heal lifecycle lock poisoned");
        let peer = state
            .peers
            .entry(candidate_peer_id.to_string())
            .or_insert_with(|| empty_lifecycle_peer(candidate_peer_id));
        if matches!(
            action.phase.as_str(),
            "eviction_outcome" | "readmission_outcome"
        ) && action.outcome == "success"
        {
            peer.observation_count = 0;
        }
        peer.last_decision = Some(decision);
        peer.last_action = Some(action);
    }

    fn record_autoheal_lifecycle_action(
        &self,
        eviction_decision_id: String,
        peer_config: &PeerConfig,
        decision: EvictionDecision,
        action: AutohealActionRecord,
    ) {
        let mut state = self
            .autoheal_lifecycle
            .write()
            .expect("auto-heal lifecycle lock poisoned");
        let peer = state
            .peers
            .entry(peer_config.node_id.clone())
            .or_insert_with(|| empty_lifecycle_peer(&peer_config.node_id));
        peer.addr = Some(peer_config.addr.clone());
        peer.eviction_decision_id = Some(eviction_decision_id);
        if matches!(
            action.phase.as_str(),
            "eviction_outcome" | "readmission_outcome"
        ) && action.outcome == "success"
        {
            peer.observation_count = 0;
        }
        peer.last_decision = Some(decision);
        peer.last_action = Some(action);
    }

    /// Check if a specific peer is available (circuit breaker not tripped).
    pub fn is_peer_available(&self, node_id: &str) -> bool {
        self.peer_snapshot()
            .iter()
            .find(|p| p.peer_id() == node_id)
            .map(|p| p.is_available())
            .unwrap_or(false)
    }

    /// Get list of available peer node IDs (circuit breaker closed or half-open).
    pub fn available_peers(&self) -> Vec<String> {
        let mut peer_ids = self
            .peer_snapshot()
            .iter()
            .filter(|p| p.is_available())
            .map(|p| p.peer_id().to_string())
            .collect::<Vec<_>>();
        peer_ids.sort();
        peer_ids
    }

    /// Add a peer and return the post-mutation membership receipt from the same lock snapshot.
    pub fn add_peer(&self, peer_config: PeerConfig) -> Result<AddPeerReceipt, AddPeerError> {
        let mut peers = self.peers.write().expect("replication peer lock poisoned");
        if let Some(existing) = peers
            .iter()
            .find(|existing| existing.peer_id() == peer_config.node_id.as_str())
        {
            if existing.base_url() == peer_config.addr {
                return Ok(AddPeerReceipt {
                    node_id: existing.peer_id().to_string(),
                    addr: existing.base_url().to_string(),
                    peers_total: peers.len(),
                });
            }
            return Err(AddPeerError::Conflict(format!(
                "Peer '{}' already exists with a different address",
                peer_config.node_id
            )));
        }

        let mut persisted_peers = Self::peer_configs_from_clients(&peers);
        persisted_peers.push(peer_config.clone());
        self.persist_peer_membership(persisted_peers)
            .map_err(AddPeerError::Persistence)?;

        let node_id = peer_config.node_id;
        let addr = peer_config.addr;
        let peer = Arc::new(PeerClient::new(
            node_id.clone(),
            addr.clone(),
            self.peer_credential.clone(),
        ));
        peers.push(peer);
        Ok(AddPeerReceipt {
            node_id,
            addr,
            peers_total: peers.len(),
        })
    }

    /// Remove a peer and return the post-mutation membership receipt from the same lock snapshot.
    pub fn remove_peer(&self, node_id: &str) -> Result<Option<RemovePeerReceipt>, String> {
        let mut peers = self.peers.write().expect("replication peer lock poisoned");
        self.remove_peer_locked(&mut peers, node_id)
    }

    fn remove_peer_locked(
        &self,
        peers: &mut Vec<Arc<PeerClient>>,
        node_id: &str,
    ) -> Result<Option<RemovePeerReceipt>, String> {
        let Some(index) = peers.iter().position(|peer| peer.peer_id() == node_id) else {
            return Ok(None);
        };
        let persisted_peers = peers
            .iter()
            .enumerate()
            .filter(|(peer_index, _)| *peer_index != index)
            .map(|(_, peer)| PeerConfig {
                node_id: peer.peer_id().to_string(),
                addr: peer.base_url().to_string(),
            })
            .collect();
        self.persist_peer_membership(persisted_peers)?;

        let removed_peer = peers.remove(index);

        for tenant_cursors in self.peer_cursors.iter() {
            tenant_cursors.value().remove(node_id);
        }
        Ok(Some(RemovePeerReceipt {
            node_id: removed_peer.peer_id().to_string(),
            peers_total: peers.len(),
        }))
    }

    /// Persist and install an authoritative peer membership snapshot.
    pub fn replace_peers(&self, peer_configs: Vec<PeerConfig>) -> Result<(), String> {
        let replacement = peer_configs
            .iter()
            .map(|peer| {
                Arc::new(PeerClient::new(
                    peer.node_id.clone(),
                    peer.addr.clone(),
                    self.peer_credential.clone(),
                ))
            })
            .collect();
        let retained_peer_ids = peer_configs
            .iter()
            .map(|peer| peer.node_id.clone())
            .collect::<BTreeSet<_>>();

        let mut peers = self.peers.write().expect("replication peer lock poisoned");
        self.persist_peer_membership(peer_configs)?;
        *peers = replacement;
        for tenant_cursors in self.peer_cursors.iter() {
            tenant_cursors
                .value()
                .retain(|peer_id, _| retained_peer_ids.contains(peer_id));
        }
        Ok(())
    }

    fn set_peer_cursor(
        peer_cursors: &DashMap<String, DashMap<String, PeerCursor>>,
        tenant_id: &str,
        peer_id: &str,
        cursor: PeerCursor,
    ) {
        let tenant_cursors = peer_cursors.entry(tenant_id.to_string()).or_default();
        tenant_cursors.insert(peer_id.to_string(), cursor);
    }

    fn existing_acked_seq(
        peer_cursors: &DashMap<String, DashMap<String, PeerCursor>>,
        tenant_id: &str,
        peer_id: &str,
    ) -> Option<u64> {
        peer_cursors
            .get(tenant_id)
            .and_then(|tenant| tenant.get(peer_id).and_then(|cursor| cursor.last_acked_seq))
    }

    fn set_failed_peer_cursor(
        peer_cursors: &DashMap<String, DashMap<String, PeerCursor>>,
        tenant_id: &str,
        peer_id: &str,
        error: String,
    ) {
        let previous_ack = Self::existing_acked_seq(peer_cursors, tenant_id, peer_id);
        Self::set_peer_cursor(
            peer_cursors,
            tenant_id,
            peer_id,
            PeerCursor::failed(error, previous_ack),
        );
    }

    fn set_peer_cursor_if_current_member(
        &self,
        peer: &Arc<PeerClient>,
        tenant_id: &str,
        peer_id: &str,
        cursor: PeerCursor,
    ) -> bool {
        let peers = self.peers.read().expect("replication peer lock poisoned");
        if !peers.iter().any(|current| Arc::ptr_eq(current, peer)) {
            return false;
        }
        Self::set_peer_cursor(self.peer_cursors.as_ref(), tenant_id, peer_id, cursor);
        true
    }

    fn set_failed_peer_cursor_if_current_member(
        &self,
        peer: &Arc<PeerClient>,
        tenant_id: &str,
        peer_id: &str,
        error: String,
    ) -> bool {
        let peers = self.peers.read().expect("replication peer lock poisoned");
        if !peers.iter().any(|current| Arc::ptr_eq(current, peer)) {
            return false;
        }
        Self::set_failed_peer_cursor(self.peer_cursors.as_ref(), tenant_id, peer_id, error);
        true
    }

    /// TODO: Document ReplicationManager.replicate_to_peer_with_retry.
    async fn replicate_to_peer_with_retry(
        peer: &Arc<PeerClient>,
        tenant_id: &str,
        ops: Vec<OpLogEntry>,
    ) -> Result<u64, String> {
        let req = ReplicateOpsRequest {
            tenant_id: tenant_id.to_string(),
            ops,
        };
        let result = peer.replicate_ops(req.clone()).await;
        let result = match result {
            Ok(resp) => Ok(resp),
            Err(error) => {
                tracing::warn!(
                    "[REPL {}] peer {} failed (will retry in 2s): {}",
                    tenant_id,
                    peer.peer_id(),
                    error
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                peer.replicate_ops(req).await
            }
        };
        result.map(|resp| resp.acked_seq)
    }

    /// TODO: Document ReplicationManager.replicate_to_single_peer.
    async fn replicate_to_single_peer(
        &self,
        peer: Arc<PeerClient>,
        tenant_id: String,
        peer_id: String,
        ops: Vec<OpLogEntry>,
    ) -> Result<u64, String> {
        if !peer.is_available() {
            let error = "circuit breaker open".to_string();
            let _ = self.set_failed_peer_cursor_if_current_member(
                &peer,
                &tenant_id,
                &peer_id,
                error.clone(),
            );
            tracing::debug!(
                "[REPL {}] skipping peer {} (circuit breaker open)",
                tenant_id,
                peer_id
            );
            return Err(error);
        }

        match Self::replicate_to_peer_with_retry(&peer, &tenant_id, ops).await {
            Ok(acked_seq) => {
                let _ = self.set_peer_cursor_if_current_member(
                    &peer,
                    &tenant_id,
                    &peer_id,
                    PeerCursor::acknowledged(acked_seq),
                );
                tracing::info!(
                    "[REPL {}] peer {} acked seq {}",
                    tenant_id,
                    peer_id,
                    acked_seq
                );
                Ok(acked_seq)
            }
            Err(error) => {
                let _ = self.set_failed_peer_cursor_if_current_member(
                    &peer,
                    &tenant_id,
                    &peer_id,
                    error.clone(),
                );
                tracing::warn!(
                    "[REPL {}] peer {} failed after retry, ops dropped: {}",
                    tenant_id,
                    peer_id,
                    error
                );
                Err(error)
            }
        }
    }

    /// Replicate operations to all available peers, settling every peer task
    /// before the pass returns. The HTTP owner may run this pass in a detached
    /// request-child task, but that task can now retain one mutation permit
    /// until every per-peer delivery has completed.
    pub async fn replicate_ops(self: &Arc<Self>, tenant_id: &str, ops: Vec<OpLogEntry>) {
        if ops.is_empty() {
            return;
        }

        let tenant_id = tenant_id.to_string();

        let peers = self.peer_snapshot();
        let mut peer_tasks = tokio::task::JoinSet::new();
        for peer in peers {
            let peer_id = peer.peer_id().to_string();
            let tenant_id = tenant_id.clone();
            let ops = ops.clone();
            let manager = Arc::clone(self);

            peer_tasks.spawn(async move {
                let _ = manager
                    .replicate_to_single_peer(peer, tenant_id, peer_id, ops)
                    .await;
            });
        }
        while let Some(result) = peer_tasks.join_next().await {
            if let Err(error) = result {
                tracing::error!(error = %error, "replication peer task failed");
            }
        }
    }

    /// Replicate operations to one specific peer and update canonical delivery cursor state.
    pub async fn replicate_ops_to_peer(
        &self,
        tenant_id: &str,
        peer_id: &str,
        ops: Vec<OpLogEntry>,
    ) -> Result<u64, String> {
        if ops.is_empty() {
            return Ok(0);
        }

        let peer = self
            .peer_snapshot()
            .iter()
            .find(|peer| peer.peer_id() == peer_id)
            .cloned()
            .ok_or_else(|| format!("Unknown peer '{}'", peer_id))?;

        self.replicate_to_single_peer(peer, tenant_id.to_string(), peer_id.to_string(), ops)
            .await
    }

    /// Catch up from peers — tries all available peers until one succeeds.
    /// Skips peers with open circuit breakers and moves to the next on failure.
    pub async fn catch_up_from_peer(
        &self,
        tenant_id: &str,
        local_seq: u64,
    ) -> Result<Vec<OpLogEntry>, String> {
        self.catch_up_from_peer_with_metadata(tenant_id, local_seq)
            .await
            .map(|resp| resp.ops)
    }

    /// Catch up from all available peers, merging operations and metadata.
    pub async fn catch_up_from_peer_with_metadata(
        &self,
        tenant_id: &str,
        local_seq: u64,
    ) -> Result<GetOpsResponse, String> {
        self.catch_up_from_peer_with_metadata_internal(tenant_id, local_seq, false)
            .await
    }

    /// Strict catch-up used by pre-serve bootstrap. Every configured peer must
    /// answer successfully so the node never starts from partial replication state.
    pub async fn catch_up_from_peer_with_metadata_strict(
        &self,
        tenant_id: &str,
        local_seq: u64,
    ) -> Result<GetOpsResponse, String> {
        self.catch_up_from_peer_with_metadata_internal(tenant_id, local_seq, true)
            .await
    }

    /// TODO: Document ReplicationManager.validate_catch_up_response.
    fn validate_catch_up_response(
        peer_id: &str,
        requested_tenant_id: &str,
        response: &GetOpsResponse,
    ) -> Result<(), String> {
        if response.tenant_id != requested_tenant_id {
            return Err(format!(
                "peer {} returned catch-up payload for tenant '{}' while '{}' was requested",
                peer_id, response.tenant_id, requested_tenant_id
            ));
        }
        if let Some(foreign_op) = response
            .ops
            .iter()
            .find(|op| op.tenant_id != requested_tenant_id)
        {
            return Err(format!(
                "peer {} returned op seq {} for tenant '{}' while '{}' was requested",
                peer_id, foreign_op.seq, foreign_op.tenant_id, requested_tenant_id
            ));
        }
        Ok(())
    }
    /// Merge catch-up responses from available peers, optionally failing fast in strict mode.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    #[allow(clippy::cognitive_complexity)] // Merge semantics must branch on per-peer availability, strict mode, and dedup conflicts in one owner path.
    async fn catch_up_from_peer_with_metadata_internal(
        &self,
        tenant_id: &str,
        local_seq: u64,
        require_all_peers: bool,
    ) -> Result<GetOpsResponse, String> {
        let peers = self.peer_snapshot();
        if peers.is_empty() {
            return Err("No peers available for catch-up".to_string());
        }

        let query = GetOpsQuery {
            tenant_id: tenant_id.to_string(),
            since_seq: local_seq,
        };

        let mut last_error = String::from("All peers have tripped circuit breakers");
        let mut any_success = false;
        let mut merged_current_seq = 0_u64;
        let mut merged_oldest_retained_seq: Option<u64> = None;
        let mut merged_node_current_seqs = BTreeMap::new();
        // Peer-local sequence domains are independent. Ordered keys provide a
        // deterministic final tie-break when retained origin tuples are equal.
        let mut merged_ops: BTreeMap<(String, u64), OpLogEntry> = BTreeMap::new();
        for peer in peers {
            if !peer.is_available() {
                let error = format!("peer {} unavailable (circuit breaker open)", peer.peer_id());
                if require_all_peers {
                    return Err(error);
                }
                last_error = error;
                continue;
            }

            match peer.get_ops(query.clone()).await {
                Ok(resp) => {
                    if let Err(error) =
                        Self::validate_catch_up_response(peer.peer_id(), tenant_id, &resp)
                    {
                        if require_all_peers {
                            return Err(error);
                        }
                        tracing::warn!(
                            "[REPL {}] invalid catch-up response from peer {}: {}",
                            tenant_id,
                            peer.peer_id(),
                            error
                        );
                        last_error = error;
                        continue;
                    }
                    any_success = true;
                    merged_current_seq = merged_current_seq.max(resp.current_seq);
                    merged_oldest_retained_seq =
                        match (merged_oldest_retained_seq, resp.oldest_retained_seq) {
                            (Some(existing), Some(incoming)) => Some(existing.min(incoming)),
                            (None, Some(incoming)) => Some(incoming),
                            (existing, None) => existing,
                        };
                    if resp.node_current_seqs.is_empty() {
                        merged_node_current_seqs
                            .insert(peer.peer_id().to_string(), resp.current_seq);
                    } else {
                        for (node_id, node_seq) in resp.node_current_seqs {
                            merged_node_current_seqs
                                .entry(node_id)
                                .and_modify(|existing| *existing = (*existing).max(node_seq))
                                .or_insert(node_seq);
                        }
                    }

                    for op in resp.ops {
                        let key = (peer.peer_id().to_string(), op.seq);
                        if let Some(existing) = merged_ops.get(&key) {
                            if existing.timestamp_ms != op.timestamp_ms
                                || existing.op_type != op.op_type
                                || existing.tenant_id != op.tenant_id
                                || existing.payload != op.payload
                            {
                                if require_all_peers {
                                    return Err(format!(
                                        "peer {} returned conflicting payloads for local seq {} while strict catch-up was requested",
                                        key.0, key.1
                                    ));
                                }
                                tracing::warn!(
                                    "[REPL {}] peer {} returned conflicting payloads for local seq {}; keeping first seen op",
                                    tenant_id,
                                    key.0,
                                    key.1
                                );
                            }
                            continue;
                        }
                        merged_ops.insert(key, op);
                    }

                    tracing::info!(
                        "[REPL {}] merged catch-up from peer {}: local_seq={}, peer_seq={}",
                        tenant_id,
                        peer.peer_id(),
                        local_seq,
                        resp.current_seq
                    );
                }
                Err(e) => {
                    if require_all_peers {
                        return Err(format!(
                            "peer {} failed catch-up for tenant '{}': {}",
                            peer.peer_id(),
                            tenant_id,
                            e
                        ));
                    }
                    tracing::warn!(
                        "[REPL {}] catch-up from peer {} failed, continuing merge: {}",
                        tenant_id,
                        peer.peer_id(),
                        e
                    );
                    last_error = e;
                }
            }
        }

        if !any_success {
            return Err(last_error);
        }

        let mut merged_ops: Vec<OpLogEntry> = merged_ops.into_values().collect();
        merged_ops.sort_by(|left, right| {
            left.seq
                .cmp(&right.seq)
                .then_with(|| left.node_id.cmp(&right.node_id))
                .then_with(|| left.timestamp_ms.cmp(&right.timestamp_ms))
        });

        Ok(GetOpsResponse {
            tenant_id: tenant_id.to_string(),
            ops: merged_ops,
            current_seq: merged_current_seq,
            oldest_retained_seq: merged_oldest_retained_seq,
            node_current_seqs: merged_node_current_seqs,
        })
    }

    /// Discover visible tenant IDs from currently available peers.
    pub async fn discover_tenants_from_peers(&self) -> Vec<String> {
        self.discover_tenants_from_peers_internal(false)
            .await
            .unwrap_or_default()
    }

    /// Discover visible tenant IDs from peers, requiring every configured peer
    /// to answer successfully.
    pub async fn discover_tenants_from_peers_strict(&self) -> Result<Vec<String>, String> {
        self.discover_tenants_from_peers_internal(true).await
    }

    /// Merge unique tenant IDs from available peers and, in strict mode, fail on
    /// the first unavailable or erroring peer instead of silently returning a
    /// partial tenant set.
    async fn discover_tenants_from_peers_internal(
        &self,
        require_all_peers: bool,
    ) -> Result<Vec<String>, String> {
        let peers = self.peer_snapshot();
        if peers.is_empty() {
            return Ok(Vec::new());
        }

        let mut tenants = BTreeSet::new();
        let mut any_success = false;
        let mut last_error = String::from("All peers have tripped circuit breakers");
        for peer in peers {
            if !peer.is_available() {
                let error = format!("peer {} unavailable (circuit breaker open)", peer.peer_id());
                if require_all_peers {
                    return Err(error);
                }
                last_error = error;
                continue;
            }

            match peer.list_tenants().await {
                Ok(ListTenantsResponse {
                    tenants: peer_tenants,
                }) => {
                    any_success = true;
                    for tenant_id in peer_tenants {
                        if let Err(error) =
                            Self::validate_discovered_tenant_id(peer.peer_id(), &tenant_id)
                        {
                            if require_all_peers {
                                return Err(error);
                            }
                            tracing::warn!(
                                    "[REPL] tenant discovery from peer {} returned invalid tenant id '{}': {}",
                                    peer.peer_id(),
                                    tenant_id,
                                    error
                                );
                            last_error = error;
                            continue;
                        }
                        tenants.insert(tenant_id);
                    }
                }
                Err(error) => {
                    if require_all_peers {
                        return Err(format!(
                            "peer {} tenant discovery failed: {}",
                            peer.peer_id(),
                            error
                        ));
                    }
                    tracing::debug!(
                        "[REPL] tenant discovery from peer {} failed: {}",
                        peer.peer_id(),
                        error
                    );
                    last_error = error;
                }
            }
        }

        if require_all_peers && !any_success {
            return Err(last_error);
        }

        Ok(tenants.into_iter().collect())
    }

    /// Download a full tenant snapshot from peers, trying available peers in order.
    pub async fn download_snapshot_from_peer(&self, tenant_id: &str) -> Result<Vec<u8>, String> {
        let peers = self.peer_snapshot();
        if peers.is_empty() {
            return Err("No peers available for snapshot restore".to_string());
        }

        let mut last_error = String::from("All peers have tripped circuit breakers");
        for peer in peers.iter().filter(|p| p.is_available()) {
            match peer.get_snapshot(tenant_id).await {
                Ok(bytes) => {
                    tracing::info!(
                        "[REPL {}] downloaded snapshot from peer {} ({} bytes)",
                        tenant_id,
                        peer.peer_id(),
                        bytes.len()
                    );
                    return Ok(bytes);
                }
                Err(error) => {
                    tracing::warn!(
                        "[REPL {}] snapshot download from peer {} failed, trying next: {}",
                        tenant_id,
                        peer.peer_id(),
                        error
                    );
                    last_error = error;
                }
            }
        }

        Err(last_error)
    }

    /// Get peer acknowledgment status for a tenant
    pub fn get_peer_cursors(&self, tenant_id: &str) -> Option<DashMap<String, PeerCursor>> {
        self.peer_cursors.get(tenant_id).map(|entry| entry.clone())
    }

    /// Return health status of all configured peers based on last_success timestamps
    /// and circuit breaker state.
    pub fn peer_statuses(&self) -> Vec<PeerHealthStatus> {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.peer_snapshot()
            .into_iter()
            .map(|client| {
                let last_ts = client.last_success_timestamp();
                let cb_state = client.circuit_breaker().state();

                let (secs_ago, status) = if last_ts == 0 {
                    (None, "never_contacted".to_string())
                } else {
                    let ago = now_secs.saturating_sub(last_ts);
                    let s = match cb_state {
                        CircuitState::Open => "circuit_open",
                        _ if ago < 60 => "healthy",
                        _ if ago < 300 => "stale",
                        _ => "unhealthy",
                    };
                    (Some(ago), s.to_string())
                };
                PeerHealthStatus {
                    peer_id: client.peer_id().to_string(),
                    addr: client.base_url().to_string(),
                    last_success_secs_ago: secs_ago,
                    status,
                }
            })
            .collect()
    }

    async fn run_health_probe_pass(
        &self,
        journal: &mut AutohealJournal,
        cycle: &mut AutohealCycle,
    ) -> Result<(Option<String>, BTreeMap<String, ProbeOutcome>), String> {
        let (snapshot_peer_ids, outcomes) = self.collect_health_probe_outcomes().await;
        let removed =
            self.apply_autoheal_probe_pass(journal, cycle, snapshot_peer_ids, outcomes.clone())?;
        Ok((removed, outcomes))
    }

    async fn collect_health_probe_outcomes(&self) -> (Vec<String>, BTreeMap<String, ProbeOutcome>) {
        let peers = self.peer_snapshot();
        let snapshot_peer_ids = Self::sorted_peer_ids_from_clients(&peers);
        let mut outcomes = BTreeMap::new();

        for peer in peers {
            let peer_id = peer.peer_id().to_string();
            let health = peer.health_check().await;
            log_peer_health_result(&peer_id, &health);
            outcomes.insert(peer_id, ProbeOutcome::from(health));
        }

        (snapshot_peer_ids, outcomes)
    }

    fn apply_autoheal_probe_pass(
        &self,
        journal: &mut AutohealJournal,
        cycle: &mut AutohealCycle,
        snapshot_peer_ids: Vec<String>,
        outcomes: BTreeMap<String, ProbeOutcome>,
    ) -> Result<Option<String>, String> {
        if cycle.member_peer_ids() != snapshot_peer_ids.as_slice() {
            cycle.replace_membership(snapshot_peer_ids.clone());
        }

        let decisions = cycle.record_probe_pass(&outcomes);
        self.update_autoheal_lifecycle_observations(cycle);

        for (candidate_peer_id, decision) in decisions {
            match decision {
                EvictionDecision::Evict { .. } => {
                    if let Some(removed_peer_id) = self.record_autoheal_eviction(
                        journal,
                        &snapshot_peer_ids,
                        &candidate_peer_id,
                        decision,
                    )? {
                        cycle.record_eviction_succeeded(&removed_peer_id);
                        return Ok(Some(removed_peer_id));
                    }
                }
                decision => {
                    journal.record_decision(
                        &snapshot_peer_ids,
                        &candidate_peer_id,
                        decision.clone(),
                    )?;
                    self.record_autoheal_lifecycle_decision(
                        &candidate_peer_id,
                        decision,
                        autoheal_action("decision_recorded", "not_required", None),
                    );
                }
            }
        }

        Ok(None)
    }

    fn record_autoheal_eviction(
        &self,
        journal: &mut AutohealJournal,
        membership_peer_ids: &[String],
        candidate_peer_id: &str,
        decision: EvictionDecision,
    ) -> Result<Option<String>, String> {
        let mut peers = self.peers.write().expect("replication peer lock poisoned");
        let current_peer_ids = Self::sorted_peer_ids_from_clients(&peers);
        let expected_peer_ids = sorted_peer_ids(membership_peer_ids);

        if !expected_peer_ids
            .iter()
            .any(|peer_id| peer_id == candidate_peer_id)
        {
            journal.record_decision(
                &expected_peer_ids,
                candidate_peer_id,
                EvictionDecision::RefuseIndeterminate {
                    reason: format!(
                        "candidate peer {candidate_peer_id} is no longer in the auto-heal membership snapshot"
                    ),
                },
            )?;
            return Ok(None);
        }

        if current_peer_ids != expected_peer_ids {
            journal.record_decision(
                &expected_peer_ids,
                candidate_peer_id,
                EvictionDecision::RefuseIndeterminate {
                    reason: format!(
                        "membership changed before auto-heal eviction: expected {:?}, current {:?}",
                        expected_peer_ids, current_peer_ids
                    ),
                },
            )?;
            return Ok(None);
        }

        let candidate = peers
            .iter()
            .find(|peer| peer.peer_id() == candidate_peer_id)
            .map(|peer| PeerConfig {
                node_id: peer.peer_id().to_string(),
                addr: peer.base_url().to_string(),
            })
            .ok_or_else(|| {
                format!("candidate peer {candidate_peer_id} is no longer in active membership")
            })?;
        let candidate_peer_id = candidate.node_id.clone();
        let decision_id = match journal.record_eviction(
            &expected_peer_ids,
            &candidate_peer_id,
            Some(candidate.clone()),
            decision.clone(),
            || {
                self.remove_peer_locked(&mut peers, &candidate_peer_id)?
                    .map(|_| ())
                    .ok_or_else(|| {
                        format!("candidate peer {candidate_peer_id} was already removed")
                    })
            },
        ) {
            Ok(decision_id) => decision_id,
            Err(error) => {
                self.record_autoheal_lifecycle_action(
                    String::new(),
                    &candidate,
                    decision,
                    autoheal_action("eviction_outcome", "failure", Some(error.clone())),
                );
                return Err(error);
            }
        };
        self.record_autoheal_lifecycle_action(
            decision_id,
            &candidate,
            decision,
            autoheal_action("eviction_outcome", "success", None),
        );
        Ok(Some(candidate_peer_id))
    }

    async fn readmit_healthy_autoheal_candidates(
        &self,
        journal: &mut AutohealJournal,
        active_outcomes: &BTreeMap<String, ProbeOutcome>,
    ) -> Result<Vec<AddPeerReceipt>, AddPeerError> {
        let candidates = journal
            .unresolved_readmission_candidates()
            .map_err(AddPeerError::Persistence)?;
        let mut receipts = Vec::new();

        for candidate in candidates {
            if !matches!(
                self.health_check_readmission_candidate(&candidate.peer_config, active_outcomes)
                    .await,
                PeerHealthCheck::Healthy
            ) {
                continue;
            }

            let membership_peer_ids = self.current_peer_ids_sorted();
            let receipt = self.record_autoheal_readmission(
                journal,
                &membership_peer_ids,
                &candidate.peer_config,
                candidate.eviction_decision_id,
            )?;
            receipts.push(receipt);
        }

        Ok(receipts)
    }

    async fn health_check_readmission_candidate(
        &self,
        peer_config: &PeerConfig,
        active_outcomes: &BTreeMap<String, ProbeOutcome>,
    ) -> PeerHealthCheck {
        if let Some(outcome) = active_outcomes.get(&peer_config.node_id) {
            return match outcome {
                ProbeOutcome::Healthy => PeerHealthCheck::Healthy,
                ProbeOutcome::Unreachable => PeerHealthCheck::Unreachable {
                    reason: format!(
                        "active peer {} was unreachable in this health-probe pass",
                        peer_config.node_id
                    ),
                },
                ProbeOutcome::Indeterminate { reason } => PeerHealthCheck::Indeterminate {
                    reason: reason.clone(),
                },
            };
        }

        let active_peer = self
            .peer_snapshot()
            .into_iter()
            .find(|peer| peer.peer_id() == peer_config.node_id.as_str());
        let peer = active_peer.unwrap_or_else(|| {
            Arc::new(PeerClient::new(
                peer_config.node_id.clone(),
                peer_config.addr.clone(),
                self.peer_credential.clone(),
            ))
        });
        let health = peer.health_check().await;
        log_peer_health_result(peer.peer_id(), &health);
        health
    }

    fn record_autoheal_readmission(
        &self,
        journal: &mut AutohealJournal,
        membership_peer_ids: &[String],
        peer_config: &PeerConfig,
        eviction_decision_id: String,
    ) -> Result<AddPeerReceipt, AddPeerError> {
        let decision = EvictionDecision::Evict {
            node_id: peer_config.node_id.clone(),
            reason: "readmission retry for prior auto-heal eviction".to_string(),
        };
        let result = journal.record_readmission(
            membership_peer_ids,
            peer_config,
            eviction_decision_id.clone(),
            || self.add_peer(peer_config.clone()),
        );
        match result {
            Ok(receipt) => {
                self.record_autoheal_lifecycle_action(
                    eviction_decision_id,
                    peer_config,
                    decision,
                    autoheal_action("readmission_outcome", "success", None),
                );
                Ok(receipt)
            }
            Err(error) => {
                self.record_autoheal_lifecycle_action(
                    eviction_decision_id,
                    peer_config,
                    decision,
                    autoheal_action("readmission_outcome", "failure", Some(error.to_string())),
                );
                Err(error)
            }
        }
    }

    #[cfg(test)]
    fn run_autoheal_probe_pass_for_test(
        &self,
        journal: &mut AutohealJournal,
        cycle: &mut AutohealCycle,
        outcomes: BTreeMap<String, ProbeOutcome>,
    ) -> Result<Option<String>, String> {
        self.apply_autoheal_probe_pass(journal, cycle, self.current_peer_ids_sorted(), outcomes)
    }

    #[cfg(test)]
    fn record_autoheal_eviction_for_test(
        &self,
        journal: &mut AutohealJournal,
        membership_peer_ids: &[String],
        candidate_peer_id: &str,
        decision: EvictionDecision,
    ) -> Result<Option<String>, String> {
        self.record_autoheal_eviction(journal, membership_peer_ids, candidate_peer_id, decision)
    }

    #[cfg(test)]
    fn record_autoheal_readmission_for_test(
        &self,
        journal: &mut AutohealJournal,
        membership_peer_ids: &[String],
        peer_config: &PeerConfig,
        eviction_decision_id: String,
    ) -> Result<AddPeerReceipt, AddPeerError> {
        self.record_autoheal_readmission(
            journal,
            membership_peer_ids,
            peer_config,
            eviction_decision_id,
        )
    }

    #[cfg(test)]
    async fn readmit_healthy_autoheal_candidates_for_test(
        &self,
        journal: &mut AutohealJournal,
        active_outcomes: &BTreeMap<String, ProbeOutcome>,
    ) -> Result<Vec<AddPeerReceipt>, AddPeerError> {
        self.readmit_healthy_autoheal_candidates(journal, active_outcomes)
            .await
    }

    /// Start background health probing of all peers at the given interval.
    /// Replaces any previously running probe loop so there is at most one active task.
    pub fn start_health_probe(self: &Arc<Self>, interval_secs: u64, autoheal_enabled: bool) {
        self.start_health_probe_with_interval(Duration::from_secs(interval_secs), autoheal_enabled);
    }

    /// Start the owned autoheal loop with one caller-supplied admission permit
    /// retained across each complete probe, eviction, and readmission pass.
    pub fn start_health_probe_with_admission<Admit, AdmitFuture, Permit>(
        self: &Arc<Self>,
        interval_secs: u64,
        autoheal_enabled: bool,
        admit: Admit,
    ) where
        Admit: Fn() -> AdmitFuture + Send + Sync + 'static,
        AdmitFuture: Future<Output = Option<Permit>> + Send + 'static,
        Permit: Send + 'static,
    {
        self.start_health_probe_with_interval_and_admission(
            Duration::from_secs(interval_secs),
            autoheal_enabled,
            admit,
        );
    }

    fn start_health_probe_with_interval(
        self: &Arc<Self>,
        interval_duration: Duration,
        autoheal_enabled: bool,
    ) {
        self.start_health_probe_with_interval_and_admission(
            interval_duration,
            autoheal_enabled,
            || async { Some(()) },
        );
    }

    fn start_health_probe_with_interval_and_admission<Admit, AdmitFuture, Permit>(
        self: &Arc<Self>,
        interval_duration: Duration,
        autoheal_enabled: bool,
        admit: Admit,
    ) where
        Admit: Fn() -> AdmitFuture + Send + Sync + 'static,
        AdmitFuture: Future<Output = Option<Permit>> + Send + 'static,
        Permit: Send + 'static,
    {
        self.stop_health_probe();
        self.initialize_autoheal_lifecycle(autoheal_enabled);
        let manager = Arc::clone(self);
        let handle = tokio::spawn(async move {
            let mut journal = None;
            let mut cycle = AutohealCycle::new(
                autoheal_enabled,
                DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD,
                manager.current_peer_ids_sorted(),
            );
            let mut interval = tokio::time::interval(interval_duration);
            // Skip the first immediate tick
            interval.tick().await;

            loop {
                interval.tick().await;

                let Some(_mutation_permit) = admit().await else {
                    continue;
                };

                if journal.is_none() {
                    journal = match AutohealJournal::new(&manager.data_dir) {
                        Ok(journal) => Some(journal),
                        Err(error) => {
                            tracing::error!(
                                "[autoheal] journal unavailable after mutation admission; continuing health probes without auto-heal recording: {}",
                                error
                            );
                            None
                        }
                    };
                }

                match journal.as_mut() {
                    Some(journal) => {
                        let active_outcomes = match manager
                            .run_health_probe_pass(journal, &mut cycle)
                            .await
                        {
                            Ok((Some(node_id), active_outcomes)) => {
                                tracing::warn!("[autoheal] evicted peer {}", node_id);
                                active_outcomes
                            }
                            Ok((None, active_outcomes)) => active_outcomes,
                            Err(error) => {
                                tracing::error!("[autoheal] health probe pass failed: {}", error);
                                BTreeMap::new()
                            }
                        };
                        match manager
                            .readmit_healthy_autoheal_candidates(journal, &active_outcomes)
                            .await
                        {
                            Ok(receipts) => {
                                for receipt in receipts {
                                    tracing::info!(
                                        "[autoheal] readmitted peer {} at {}",
                                        receipt.node_id,
                                        receipt.addr
                                    );
                                }
                            }
                            Err(error) => {
                                tracing::error!(
                                    "[autoheal] readmission candidate pass failed: {}",
                                    error
                                );
                            }
                        }
                    }
                    None => {
                        manager.collect_health_probe_outcomes().await;
                    }
                }
            }
        });
        let mut slot = self.health_probe_handle.lock().unwrap();
        *slot = Some(handle);
    }

    /// Stop a running background health probe task, if any.
    pub fn stop_health_probe(&self) -> bool {
        match self.health_probe_handle.lock().unwrap().take() {
            Some(handle) => {
                handle.abort();
                true
            }
            None => false,
        }
    }

    /// Whether the owned replication health-probe task is still live.
    pub fn health_probe_is_running(&self) -> bool {
        self.health_probe_handle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }
}

fn sorted_peer_ids(peer_ids: &[String]) -> Vec<String> {
    let mut sorted = peer_ids.to_vec();
    sorted.sort();
    sorted
}

fn upsert_lifecycle_active_peers(
    state: &mut AutohealLifecycleState,
    active_peers: Vec<PeerConfig>,
    observation_counts: &BTreeMap<String, u32>,
) {
    for peer_config in active_peers {
        let observation_count = observation_counts
            .get(&peer_config.node_id)
            .copied()
            .unwrap_or(0);
        let peer = state
            .peers
            .entry(peer_config.node_id.clone())
            .or_insert_with(|| empty_lifecycle_peer(&peer_config.node_id));
        peer.addr = Some(peer_config.addr);
        peer.observation_count = observation_count;
    }
}

fn apply_lifecycle_event(state: &mut AutohealLifecycleState, event: &AutohealJournalEvent) {
    let peer = state
        .peers
        .entry(event.candidate_peer_id.clone())
        .or_insert_with(|| empty_lifecycle_peer(&event.candidate_peer_id));
    if let Some(peer_config) = &event.candidate_peer_config {
        peer.addr = Some(peer_config.addr.clone());
    }
    if matches!(event.decision, EvictionDecision::Evict { .. }) {
        peer.eviction_decision_id = Some(event.decision_id.clone());
    }
    peer.observation_count = 0;
    peer.last_decision = Some(event.decision.clone());
    peer.last_action = Some(event.action.clone());
}

fn empty_lifecycle_peer(peer_id: &str) -> AutohealPeerLifecycle {
    AutohealPeerLifecycle {
        peer_id: peer_id.to_string(),
        addr: None,
        observation_count: 0,
        eviction_decision_id: None,
        last_decision: None,
        last_action: None,
    }
}

fn autoheal_action(phase: &str, outcome: &str, error: Option<String>) -> AutohealActionRecord {
    AutohealActionRecord {
        phase: phase.to_string(),
        outcome: outcome.to_string(),
        error,
    }
}

fn log_peer_health_result(peer_id: &str, health: &PeerHealthCheck) {
    match health {
        PeerHealthCheck::Healthy => {
            tracing::debug!("[HEALTH] peer {} is healthy", peer_id);
        }
        PeerHealthCheck::Unreachable { reason } => {
            tracing::warn!("[HEALTH] peer {} probe unreachable: {}", peer_id, reason);
        }
        PeerHealthCheck::Indeterminate { reason } => {
            tracing::warn!("[HEALTH] peer {} probe indeterminate: {}", peer_id, reason);
        }
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
