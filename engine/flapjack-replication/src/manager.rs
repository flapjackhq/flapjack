use super::circuit_breaker::CircuitState;
use super::config::{NodeConfig, PeerConfig};
use super::peer::PeerClient;
use super::types::{
    GetOpsQuery, GetOpsResponse, ListTenantsResponse, PeerHealthStatus, ReplicateOpsRequest,
};
use dashmap::DashMap;
use flapjack::index::oplog::OpLogEntry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};
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

/// Point-in-time receipt for a successful runtime peer membership removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovePeerReceipt {
    pub node_id: String,
    pub peers_total: usize,
}

/// Orchestrates replication to all peers and tracks their acknowledgment status
pub struct ReplicationManager {
    node_id: String,
    bind_addr: String,
    advertise_addr: Option<String>,
    data_dir: PathBuf,
    admin_key: Option<String>,
    peers: RwLock<Vec<Arc<PeerClient>>>,
    /// Tracks delivery status for each configured peer and tenant
    /// Outer map: tenant_id -> inner map
    /// Inner map: peer_id -> last delivery cursor/error
    peer_cursors: Arc<DashMap<String, DashMap<String, PeerCursor>>>,
    /// Handle to the background health probe task (if running)
    health_probe_handle: Mutex<Option<JoinHandle<()>>>,
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
    pub fn new(node_config: NodeConfig, admin_key: Option<String>, data_dir: PathBuf) -> Arc<Self> {
        let peers: Vec<Arc<PeerClient>> = node_config
            .peers
            .iter()
            .map(|peer_config| {
                Arc::new(PeerClient::new(
                    peer_config.node_id.clone(),
                    peer_config.addr.clone(),
                    admin_key.clone(),
                ))
            })
            .collect();

        Arc::new(Self {
            node_id: node_config.node_id,
            bind_addr: node_config.bind_addr,
            advertise_addr: node_config.advertise_addr,
            data_dir,
            admin_key,
            peers: RwLock::new(peers),
            peer_cursors: Arc::new(DashMap::new()),
            health_probe_handle: Mutex::new(None),
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

    fn peer_snapshot(&self) -> Vec<Arc<PeerClient>> {
        self.peers
            .read()
            .expect("replication peer lock poisoned")
            .clone()
    }

    pub fn peer_count(&self) -> usize {
        self.peer_snapshot().len()
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
        self.peer_snapshot()
            .iter()
            .filter(|p| p.is_available())
            .map(|p| p.peer_id().to_string())
            .collect()
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
            self.admin_key.clone(),
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
                    self.admin_key.clone(),
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

    /// Replicate operations to all available peers (fire-and-forget).
    /// Skips peers with tripped circuit breakers.
    pub async fn replicate_ops(self: &Arc<Self>, tenant_id: &str, ops: Vec<OpLogEntry>) {
        if ops.is_empty() {
            return;
        }

        let tenant_id = tenant_id.to_string();

        let peers = self.peer_snapshot();
        for peer in peers {
            let peer_id = peer.peer_id().to_string();
            let tenant_id = tenant_id.clone();
            let ops = ops.clone();
            let manager = Arc::clone(self);

            // Fire-and-forget: spawn task and don't await
            tokio::spawn(async move {
                let _ = manager
                    .replicate_to_single_peer(peer, tenant_id, peer_id, ops)
                    .await;
            });
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
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
    /// TODO: Document ReplicationManager.catch_up_from_peer_with_metadata_internal.
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
        let mut merged_ops: HashMap<(String, u64), OpLogEntry> = HashMap::new();
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
                        let key = (op.node_id.clone(), op.seq);
                        if let Some(existing) = merged_ops.get(&key) {
                            if existing.timestamp_ms != op.timestamp_ms
                                || existing.op_type != op.op_type
                                || existing.tenant_id != op.tenant_id
                                || existing.payload != op.payload
                            {
                                if require_all_peers {
                                    return Err(format!(
                                        "peer {} returned conflicting payload for op ({}, {}) while strict catch-up was requested",
                                        peer.peer_id(),
                                        key.0,
                                        key.1
                                    ));
                                }
                                tracing::warn!(
                                    "[REPL {}] conflicting payload for key ({}, {}) across peers; keeping first seen op",
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

    /// Start background health probing of all peers at the given interval.
    /// Replaces any previously running probe loop so there is at most one active task.
    pub fn start_health_probe(self: &Arc<Self>, interval_secs: u64) {
        self.stop_health_probe();
        let manager = Arc::clone(self);
        let handle = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
            // Skip the first immediate tick
            interval.tick().await;

            loop {
                interval.tick().await;

                let peers = manager.peer_snapshot();
                for peer in peers {
                    match peer.health_check().await {
                        Ok(()) => {
                            tracing::debug!("[HEALTH] peer {} is healthy", peer.peer_id());
                        }
                        Err(e) => {
                            tracing::warn!("[HEALTH] peer {} probe failed: {}", peer.peer_id(), e);
                        }
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
}

#[cfg(test)]
mod tests {
    use super::super::config::{NodeConfig, PeerConfig};
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Barrier;

    struct TestReplicationManager {
        _data_dir: TempDir,
        manager: Arc<ReplicationManager>,
    }

    impl std::ops::Deref for TestReplicationManager {
        type Target = Arc<ReplicationManager>;

        fn deref(&self) -> &Self::Target {
            &self.manager
        }
    }

    fn new_test_manager(config: NodeConfig, admin_key: Option<String>) -> TestReplicationManager {
        let data_dir = tempfile::tempdir().unwrap();
        let manager = ReplicationManager::new(config, admin_key, data_dir.path().to_path_buf());
        TestReplicationManager {
            _data_dir: data_dir,
            manager,
        }
    }

    fn new_test_manager_in(
        data_dir: &Path,
        config: NodeConfig,
        admin_key: Option<String>,
    ) -> Arc<ReplicationManager> {
        ReplicationManager::new(config, admin_key, data_dir.to_path_buf())
    }

    fn write_node_config_fixture(data_dir: &Path, peers: Vec<PeerConfig>) {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers,
        };
        let node_json = std::fs::File::create(data_dir.join("node.json"))
            .expect("node.json fixture should be writable");
        serde_json::to_writer_pretty(node_json, &config)
            .expect("node.json fixture should serialize");
    }

    fn reloaded_peer_tuples(data_dir: &Path) -> Vec<(String, String)> {
        let mut peers = NodeConfig::load_or_default(data_dir)
            .peers
            .into_iter()
            .map(|peer| (peer.node_id, peer.addr))
            .collect::<Vec<_>>();
        peers.sort();
        peers
    }

    #[test]
    fn replace_peers_persists_exact_membership_before_installing_clients() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_node_config_fixture(
            temp_dir.path(),
            vec![PeerConfig {
                node_id: "old-peer".to_string(),
                addr: "http://old-peer.example.com:7700".to_string(),
            }],
        );
        let manager = new_test_manager_in(
            temp_dir.path(),
            NodeConfig::load_or_default(temp_dir.path()),
            None,
        );

        manager
            .replace_peers(vec![
                PeerConfig {
                    node_id: "node-c".to_string(),
                    addr: "http://node-c.example.com:7700".to_string(),
                },
                PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: "https://node-b.example.com:7700".to_string(),
                },
            ])
            .expect("full membership replacement should succeed");

        assert_eq!(manager.peer_count(), 2);
        assert_eq!(
            reloaded_peer_tuples(temp_dir.path()),
            vec![
                (
                    "node-b".to_string(),
                    "https://node-b.example.com:7700".to_string()
                ),
                (
                    "node-c".to_string(),
                    "http://node-c.example.com:7700".to_string()
                ),
            ]
        );
        assert!(!manager.is_peer_available("old-peer"));
    }

    #[test]
    fn replace_peers_preserves_memory_when_persistence_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "old-peer".to_string(),
                addr: "http://old-peer.example.com:7700".to_string(),
            }],
        };
        std::fs::create_dir(temp_dir.path().join("node.json")).unwrap();
        let manager = new_test_manager_in(temp_dir.path(), config, None);

        let error = manager
            .replace_peers(vec![PeerConfig {
                node_id: "new-peer".to_string(),
                addr: "http://new-peer.example.com:7700".to_string(),
            }])
            .expect_err("persistence failure should reject replacement");

        assert!(error.contains("failed to read"));
        assert_eq!(manager.peer_count(), 1);
        assert!(manager.is_peer_available("old-peer"));
        assert!(!manager.is_peer_available("new-peer"));
    }

    async fn spawn_single_response_peer(
        response: GetOpsResponse,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = serde_json::to_string(&response).unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );

        let handle = tokio::spawn(async move {
            if let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(tokio::time::Duration::from_secs(3), listener.accept()).await
            {
                let mut request_buf = [0u8; 2048];
                let _ = socket.read(&mut request_buf).await;
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
                let _ = socket.shutdown().await;
            }
        });

        (format!("http://{}", addr), handle)
    }

    async fn spawn_single_tenant_list_peer(
        response: ListTenantsResponse,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = serde_json::to_string(&response).unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );

        let handle = tokio::spawn(async move {
            if let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(tokio::time::Duration::from_secs(3), listener.accept()).await
            {
                let mut request_buf = [0u8; 2048];
                let _ = socket.read(&mut request_buf).await;
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
                let _ = socket.shutdown().await;
            }
        });

        (format!("http://{}", addr), handle)
    }

    async fn spawn_replicate_peer(
        acked_seq: u64,
        expected_requests: usize,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = serde_json::to_string(&crate::types::ReplicateOpsResponse {
            tenant_id: "tenant-red".to_string(),
            acked_seq,
        })
        .unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );

        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for _ in 0..expected_requests {
                let (mut socket, _) =
                    tokio::time::timeout(tokio::time::Duration::from_secs(3), listener.accept())
                        .await
                        .expect("replicate peer should receive request")
                        .expect("replicate peer accept should succeed");
                let mut request_buf = [0u8; 4096];
                let bytes_read = socket.read(&mut request_buf).await.unwrap();
                requests.push(String::from_utf8_lossy(&request_buf[..bytes_read]).to_string());
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
                let _ = socket.shutdown().await;
            }
            requests
        });

        (format!("http://{}", addr), handle)
    }

    async fn spawn_barrier_replicate_peer(
        acked_seq: u64,
        accepted_barrier: Arc<Barrier>,
        release_barrier: Arc<Barrier>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = serde_json::to_string(&crate::types::ReplicateOpsResponse {
            tenant_id: "tenant-red".to_string(),
            acked_seq,
        })
        .unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );

        let handle = tokio::spawn(async move {
            let (mut socket, _) =
                tokio::time::timeout(tokio::time::Duration::from_secs(3), listener.accept())
                    .await
                    .expect("blocking peer should receive initial request")
                    .expect("blocking peer accept should succeed");
            let mut request_buf = [0u8; 4096];
            let _ = socket.read(&mut request_buf).await;
            accepted_barrier.wait().await;
            release_barrier.wait().await;
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
            let _ = socket.shutdown().await;
        });

        (format!("http://{}", addr), handle)
    }

    fn mutable_peer_test_op(seq: u64) -> OpLogEntry {
        OpLogEntry {
            seq,
            timestamp_ms: seq,
            node_id: "node-a".to_string(),
            tenant_id: "tenant-red".to_string(),
            op_type: "upsert".to_string(),
            payload: serde_json::json!({
                "objectID": format!("doc-{seq}"),
                "body": {"_id": format!("doc-{seq}"), "name": format!("Doc {seq}")}
            }),
        }
    }

    async fn wait_for_acked_seq(
        manager: &ReplicationManager,
        tenant_id: &str,
        peer_id: &str,
        expected_seq: u64,
    ) {
        tokio::time::timeout(tokio::time::Duration::from_secs(3), async {
            loop {
                if manager
                    .get_peer_cursors(tenant_id)
                    .and_then(|tenant| tenant.get(peer_id).and_then(|cursor| cursor.last_acked_seq))
                    == Some(expected_seq)
                {
                    break;
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("peer cursor should reach expected acked sequence");
    }

    #[test]
    fn test_manager_creation() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            }],
        };

        let manager = new_test_manager(config, None);

        assert_eq!(manager.node_id(), "node-a");
        assert_eq!(manager.peer_count(), 1);
    }

    #[test]
    fn test_manager_no_peers() {
        let config = NodeConfig {
            node_id: "standalone".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![],
        };

        let manager = new_test_manager(config, None);

        assert_eq!(manager.node_id(), "standalone");
        assert_eq!(manager.peer_count(), 0);
    }

    #[test]
    fn add_peer_returns_receipt_from_mutation_snapshot() {
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "0.0.0.0:7700".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![],
            },
            None,
        );

        let receipt = manager
            .add_peer(PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            })
            .expect("runtime add should succeed");

        assert_eq!(receipt.node_id, "node-b");
        assert_eq!(receipt.addr, "http://node-b:7700");
        assert_eq!(receipt.peers_total, 1);
        assert_eq!(manager.peer_count(), 1);
    }

    #[test]
    fn add_peer_persists_membership_to_node_json_for_restart() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_node_config_fixture(temp_dir.path(), vec![]);
        let manager = new_test_manager_in(
            temp_dir.path(),
            NodeConfig::load_or_default(temp_dir.path()),
            None,
        );

        manager
            .add_peer(PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            })
            .expect("runtime add should succeed");

        assert_eq!(
            reloaded_peer_tuples(temp_dir.path()),
            vec![("node-b".to_string(), "http://node-b:7700".to_string())]
        );
    }

    #[test]
    fn remove_peer_returns_receipt_from_mutation_snapshot() {
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "0.0.0.0:7700".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![
                    PeerConfig {
                        node_id: "node-b".to_string(),
                        addr: "http://node-b:7700".to_string(),
                    },
                    PeerConfig {
                        node_id: "node-c".to_string(),
                        addr: "http://node-c:7700".to_string(),
                    },
                ],
            },
            None,
        );

        let receipt = manager
            .remove_peer("node-b")
            .expect("runtime remove should succeed")
            .expect("known peer should return a removal receipt");

        assert_eq!(receipt.node_id, "node-b");
        assert_eq!(receipt.peers_total, 1);
        assert_eq!(manager.peer_count(), 1);
        assert_eq!(
            manager
                .remove_peer("node-missing")
                .expect("unknown peer is not an error"),
            None
        );
    }

    #[test]
    fn remove_peer_persists_membership_to_node_json_for_restart() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_node_config_fixture(
            temp_dir.path(),
            vec![
                PeerConfig {
                    node_id: "node-c".to_string(),
                    addr: "http://node-c:7700".to_string(),
                },
                PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: "http://node-b:7700".to_string(),
                },
            ],
        );
        let manager = new_test_manager_in(
            temp_dir.path(),
            NodeConfig::load_or_default(temp_dir.path()),
            None,
        );

        manager
            .remove_peer("node-b")
            .expect("runtime remove should succeed")
            .expect("known peer should be removed");

        let persisted_peers = reloaded_peer_tuples(temp_dir.path());
        assert_eq!(
            persisted_peers,
            vec![("node-c".to_string(), "http://node-c:7700".to_string())]
        );
        assert!(persisted_peers
            .iter()
            .all(|(node_id, _)| node_id != "node-b"));
    }

    #[test]
    fn fresh_manager_reloads_runtime_membership_from_node_json() {
        let temp_dir = tempfile::tempdir().unwrap();
        write_node_config_fixture(temp_dir.path(), vec![]);
        let manager = new_test_manager_in(
            temp_dir.path(),
            NodeConfig::load_or_default(temp_dir.path()),
            None,
        );

        manager
            .add_peer(PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            })
            .expect("runtime add should succeed");

        let restarted = new_test_manager_in(
            temp_dir.path(),
            NodeConfig::load_or_default(temp_dir.path()),
            None,
        );
        assert_eq!(restarted.peer_count(), 1);
        assert_eq!(restarted.available_peers(), vec!["node-b".to_string()]);
    }

    #[test]
    fn add_peer_returns_error_and_preserves_memory_when_persistence_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        let missing_data_dir = temp_dir.path().join("missing-data-dir");
        let manager = new_test_manager_in(
            &missing_data_dir,
            NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "0.0.0.0:7700".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![],
            },
            None,
        );

        let error = manager
            .add_peer(PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            })
            .expect_err("missing data dir should fail persistence");

        assert!(matches!(
            error,
            AddPeerError::Persistence(message) if message.contains("failed to create")
        ));
        assert_eq!(manager.peer_count(), 0);
        assert!(manager.available_peers().is_empty());
    }

    #[test]
    fn remove_peer_returns_error_and_preserves_memory_and_cursors_when_persistence_fails() {
        let temp_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp_dir.path().join("node.json"))
            .expect("node.json directory fixture should be creatable");
        let manager = new_test_manager_in(
            temp_dir.path(),
            NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "0.0.0.0:7700".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![
                    PeerConfig {
                        node_id: "node-b".to_string(),
                        addr: "http://node-b:7700".to_string(),
                    },
                    PeerConfig {
                        node_id: "node-c".to_string(),
                        addr: "http://node-c:7700".to_string(),
                    },
                ],
            },
            None,
        );
        ReplicationManager::set_peer_cursor(
            &manager.peer_cursors,
            "tenant-red",
            "node-b",
            PeerCursor::acknowledged(7),
        );
        ReplicationManager::set_peer_cursor(
            &manager.peer_cursors,
            "tenant-red",
            "node-c",
            PeerCursor::acknowledged(8),
        );

        let error = manager
            .remove_peer("node-b")
            .expect_err("node.json directory should fail persistence");

        assert!(
            error.contains("failed to read"),
            "persistence error should identify node.json read failure, got: {error}"
        );
        assert_eq!(manager.peer_count(), 2);
        assert_eq!(
            manager.available_peers(),
            vec!["node-b".to_string(), "node-c".to_string()]
        );
        let cursors = manager
            .get_peer_cursors("tenant-red")
            .expect("tenant-red cursors should remain");
        assert_eq!(
            cursors
                .get("node-b")
                .and_then(|cursor| cursor.last_acked_seq),
            Some(7)
        );
        assert_eq!(
            cursors
                .get("node-c")
                .and_then(|cursor| cursor.last_acked_seq),
            Some(8)
        );
    }

    /// Verify that all configured peers are initially available and `is_peer_available()` returns false for unknown peers.
    #[test]
    fn test_all_peers_available_initially() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![
                PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: "http://node-b:7700".to_string(),
                },
                PeerConfig {
                    node_id: "node-c".to_string(),
                    addr: "http://node-c:7700".to_string(),
                },
            ],
        };

        let manager = new_test_manager(config, None);
        assert!(manager.is_peer_available("node-b"));
        assert!(manager.is_peer_available("node-c"));
        assert!(!manager.is_peer_available("node-d")); // unknown peer
        assert_eq!(manager.available_peers().len(), 2);
    }

    /// Verify that peer health statuses report 'never_contacted' with no timestamp before any peer has been successfully contacted.
    #[test]
    fn test_peer_statuses_initially_never_contacted() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            }],
        };

        let manager = new_test_manager(config, None);
        let statuses = manager.peer_statuses();

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].peer_id, "node-b");
        assert_eq!(statuses[0].addr, "http://node-b:7700");
        assert_eq!(statuses[0].status, "never_contacted");
        assert!(statuses[0].last_success_secs_ago.is_none());
    }

    #[test]
    fn test_peer_statuses_no_peers_returns_empty() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![],
        };

        let manager = new_test_manager(config, None);
        assert!(manager.peer_statuses().is_empty());
    }

    #[tokio::test]
    async fn mutable_peer_replicate_ops_uses_snapshots_while_membership_changes() {
        let accepted_barrier = Arc::new(Barrier::new(2));
        let release_barrier = Arc::new(Barrier::new(2));
        let (node_b_url, node_b_handle) =
            spawn_barrier_replicate_peer(10, accepted_barrier.clone(), release_barrier.clone())
                .await;
        let (node_c_url, node_c_handle) = spawn_replicate_peer(20, 2).await;

        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: node_b_url,
                }],
            },
            None,
        );

        tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
            manager
                .replicate_ops("tenant-red", vec![mutable_peer_test_op(1)])
                .await;
            accepted_barrier.wait().await;

            assert!(manager
                .remove_peer("node-b")
                .expect("remove should succeed")
                .is_some());
            manager
                .add_peer(PeerConfig {
                    node_id: "node-c".to_string(),
                    addr: node_c_url,
                })
                .expect("add should succeed while another replication is in flight");

            manager
                .replicate_ops("tenant-red", vec![mutable_peer_test_op(2)])
                .await;
            manager
                .replicate_ops("tenant-red", vec![mutable_peer_test_op(3)])
                .await;

            release_barrier.wait().await;
            let _ = node_b_handle.await;
            let node_c_requests = node_c_handle.await.expect("node-c handler should finish");

            assert_eq!(node_c_requests.len(), 2);
            assert_eq!(manager.peer_count(), 1);
            assert_eq!(manager.available_peers(), vec!["node-c".to_string()]);
            assert!(!manager.is_peer_available("node-b"));
            assert!(manager.is_peer_available("node-c"));
        })
        .await
        .expect("membership mutation must not deadlock in-flight replication");
    }

    #[test]
    fn mutable_peer_duplicate_add_rejects_atomically_and_remove_clears_cursors() {
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![
                    PeerConfig {
                        node_id: "node-b".to_string(),
                        addr: "http://node-b:7700".to_string(),
                    },
                    PeerConfig {
                        node_id: "node-c".to_string(),
                        addr: "http://node-c:7700".to_string(),
                    },
                ],
            },
            None,
        );
        let initial_statuses: Vec<_> = manager
            .peer_statuses()
            .into_iter()
            .map(|status| {
                (
                    status.peer_id,
                    status.addr,
                    status.status,
                    status.last_success_secs_ago,
                )
            })
            .collect();

        let idempotent = manager
            .add_peer(PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            })
            .expect("re-registering the same peer identity and address should be idempotent");
        assert_eq!(idempotent.node_id, "node-b");
        assert_eq!(idempotent.addr, "http://node-b:7700");
        assert_eq!(idempotent.peers_total, 2);

        let duplicate = manager.add_peer(PeerConfig {
            node_id: "node-b".to_string(),
            addr: "http://node-b-new:7700".to_string(),
        });

        assert!(matches!(duplicate, Err(AddPeerError::Conflict(_))));
        assert_eq!(manager.peer_count(), 2);
        let duplicate_statuses: Vec<_> = manager
            .peer_statuses()
            .into_iter()
            .map(|status| {
                (
                    status.peer_id,
                    status.addr,
                    status.status,
                    status.last_success_secs_ago,
                )
            })
            .collect();
        assert_eq!(duplicate_statuses, initial_statuses);

        ReplicationManager::set_peer_cursor(
            &manager.peer_cursors,
            "tenant-red",
            "node-b",
            PeerCursor::acknowledged(7),
        );
        ReplicationManager::set_peer_cursor(
            &manager.peer_cursors,
            "tenant-red",
            "node-c",
            PeerCursor::acknowledged(8),
        );
        ReplicationManager::set_peer_cursor(
            &manager.peer_cursors,
            "tenant-blue",
            "node-b",
            PeerCursor::acknowledged(9),
        );
        ReplicationManager::set_peer_cursor(
            &manager.peer_cursors,
            "tenant-blue",
            "node-c",
            PeerCursor::acknowledged(10),
        );

        assert!(manager
            .remove_peer("node-b")
            .expect("remove should succeed")
            .is_some());
        assert_eq!(manager.peer_count(), 1);
        assert!(manager
            .remove_peer("node-missing")
            .expect("unknown peer is not an error")
            .is_none());

        let red = manager
            .get_peer_cursors("tenant-red")
            .expect("tenant-red cursors should remain");
        assert!(!red.contains_key("node-b"));
        assert_eq!(
            red.get("node-c").and_then(|cursor| cursor.last_acked_seq),
            Some(8)
        );

        let blue = manager
            .get_peer_cursors("tenant-blue")
            .expect("tenant-blue cursors should remain");
        assert!(!blue.contains_key("node-b"));
        assert_eq!(
            blue.get("node-c").and_then(|cursor| cursor.last_acked_seq),
            Some(10)
        );
    }

    #[tokio::test]
    async fn mutable_peer_no_mutation_preserves_status_and_cursor_views() {
        let (node_b_url, node_b_handle) = spawn_replicate_peer(11, 1).await;
        let (node_c_url, node_c_handle) = spawn_replicate_peer(22, 1).await;
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![
                    PeerConfig {
                        node_id: "node-b".to_string(),
                        addr: node_b_url.clone(),
                    },
                    PeerConfig {
                        node_id: "node-c".to_string(),
                        addr: node_c_url.clone(),
                    },
                ],
            },
            None,
        );

        assert_eq!(manager.peer_count(), 2);
        assert_eq!(
            manager.available_peers(),
            vec!["node-b".to_string(), "node-c".to_string()]
        );

        manager
            .replicate_ops("tenant-red", vec![mutable_peer_test_op(1)])
            .await;
        let _ = node_b_handle.await;
        let _ = node_c_handle.await;
        wait_for_acked_seq(&manager, "tenant-red", "node-b", 11).await;
        wait_for_acked_seq(&manager, "tenant-red", "node-c", 22).await;

        let statuses = manager.peer_statuses();
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].peer_id, "node-b");
        assert_eq!(statuses[0].addr, node_b_url);
        assert_eq!(statuses[0].status, "healthy");
        assert!(statuses[0].last_success_secs_ago.is_some());
        assert_eq!(statuses[1].peer_id, "node-c");
        assert_eq!(statuses[1].addr, node_c_url);
        assert_eq!(statuses[1].status, "healthy");
        assert!(statuses[1].last_success_secs_ago.is_some());

        let cursors = manager
            .get_peer_cursors("tenant-red")
            .expect("replication should create tenant cursors");
        assert_eq!(
            cursors
                .get("node-b")
                .and_then(|cursor| cursor.last_acked_seq),
            Some(11)
        );
        assert_eq!(
            cursors
                .get("node-c")
                .and_then(|cursor| cursor.last_acked_seq),
            Some(22)
        );
    }

    #[tokio::test]
    async fn mutable_peer_runtime_added_peer_uses_retained_admin_key() {
        let (peer_url, peer_handle) = spawn_replicate_peer(33, 1).await;
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![],
            },
            Some("replication-secret".to_string()),
        );

        manager
            .add_peer(PeerConfig {
                node_id: "node-b".to_string(),
                addr: peer_url,
            })
            .expect("runtime add should succeed");
        manager
            .replicate_ops("tenant-red", vec![mutable_peer_test_op(1)])
            .await;

        let requests = peer_handle.await.expect("peer handler should finish");
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].contains("x-algolia-api-key: replication-secret"),
            "runtime-created peer should authenticate replication requests with the retained admin key; request was:\n{}",
            requests[0]
        );
        wait_for_acked_seq(&manager, "tenant-red", "node-b", 33).await;
    }

    #[tokio::test]
    async fn mutable_peer_removed_peer_cursor_does_not_reappear_after_in_flight_completion() {
        let accepted_barrier = Arc::new(Barrier::new(2));
        let release_barrier = Arc::new(Barrier::new(2));
        let (peer_url, peer_handle) =
            spawn_barrier_replicate_peer(44, accepted_barrier.clone(), release_barrier.clone())
                .await;
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: peer_url,
                }],
            },
            None,
        );

        tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
            manager
                .replicate_ops("tenant-red", vec![mutable_peer_test_op(1)])
                .await;
            accepted_barrier.wait().await;

            assert!(manager
                .remove_peer("node-b")
                .expect("remove should succeed")
                .is_some());

            release_barrier.wait().await;
            let _ = peer_handle.await;
            assert!(
                tokio::time::timeout(tokio::time::Duration::from_millis(250), async {
                    loop {
                        if manager
                            .get_peer_cursors("tenant-red")
                            .as_ref()
                            .is_some_and(|tenant| tenant.contains_key("node-b"))
                        {
                            break;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .is_err(),
                "removed peer cursor must stay absent after its in-flight replication finishes"
            );
        })
        .await
        .expect("removed peer cursor regression must finish without deadlocking");
    }

    #[tokio::test]
    async fn test_health_probe_handle_starts_and_stops() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            }],
        };
        let manager = new_test_manager(config, None);

        assert!(manager.health_probe_handle.lock().unwrap().is_none());
        manager.start_health_probe(1);
        assert!(manager.health_probe_handle.lock().unwrap().is_some());

        assert!(manager.stop_health_probe());
        assert!(!manager.stop_health_probe());
        assert!(manager.health_probe_handle.lock().unwrap().is_none());
    }

    /// Verify that `available_peers()` returns a list containing all configured peer node IDs.
    #[test]
    fn test_available_peers_returns_names() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![
                PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: "http://node-b:7700".to_string(),
                },
                PeerConfig {
                    node_id: "node-c".to_string(),
                    addr: "http://node-c:7700".to_string(),
                },
            ],
        };

        let manager = new_test_manager(config, None);
        let available = manager.available_peers();
        assert!(available.contains(&"node-b".to_string()));
        assert!(available.contains(&"node-c".to_string()));
    }

    #[test]
    fn test_get_peer_cursors_empty_initially() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            }],
        };

        let manager = new_test_manager(config, None);
        assert!(manager.get_peer_cursors("some-tenant").is_none());
    }

    #[tokio::test]
    async fn test_replicate_ops_empty_ops_is_noop() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            }],
        };

        let manager = new_test_manager(config, None);
        // Empty ops should return immediately without spawning tasks
        manager.replicate_ops("test-tenant", vec![]).await;
        // No panic = success
    }

    #[tokio::test]
    async fn test_catch_up_from_peer_no_peers_returns_error() {
        let config = NodeConfig {
            node_id: "standalone".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![],
        };

        let manager = new_test_manager(config, None);
        let result = manager.catch_up_from_peer("test-tenant", 0).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No peers available"));
    }

    #[tokio::test]
    async fn test_catch_up_from_peer_merges_ops_from_all_available_peers() {
        let peer_a_response = GetOpsResponse {
            tenant_id: "tenant-red".to_string(),
            ops: vec![OpLogEntry {
                seq: 1,
                timestamp_ms: 100,
                node_id: "node-a".to_string(),
                tenant_id: "tenant-red".to_string(),
                op_type: "upsert".to_string(),
                payload: serde_json::json!({"objectID": "a1", "body": {"_id": "a1", "title": "A"}}),
            }],
            current_seq: 1,
            oldest_retained_seq: Some(1),
            node_current_seqs: BTreeMap::from([(String::from("node-a"), 1)]),
        };
        let peer_c_response = GetOpsResponse {
            tenant_id: "tenant-red".to_string(),
            ops: vec![OpLogEntry {
                seq: 1,
                timestamp_ms: 200,
                node_id: "node-c".to_string(),
                tenant_id: "tenant-red".to_string(),
                op_type: "upsert".to_string(),
                payload: serde_json::json!({"objectID": "c1", "body": {"_id": "c1", "title": "C"}}),
            }],
            current_seq: 1,
            oldest_retained_seq: Some(1),
            node_current_seqs: BTreeMap::from([(String::from("node-c"), 1)]),
        };

        let (peer_a_url, peer_a_handle) = spawn_single_response_peer(peer_a_response).await;
        let (peer_c_url, peer_c_handle) = spawn_single_response_peer(peer_c_response).await;

        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-b".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![
                    PeerConfig {
                        node_id: "node-a".to_string(),
                        addr: peer_a_url,
                    },
                    PeerConfig {
                        node_id: "node-c".to_string(),
                        addr: peer_c_url,
                    },
                ],
            },
            None,
        );

        let merged = manager
            .catch_up_from_peer_with_metadata("tenant-red", 0)
            .await
            .expect("at least one available peer should answer");

        let _ = peer_a_handle.await;
        let _ = peer_c_handle.await;

        assert_eq!(merged.ops.len(), 2);
        assert_eq!(merged.node_current_seqs.get("node-a"), Some(&1));
        assert_eq!(merged.node_current_seqs.get("node-c"), Some(&1));
        assert!(merged
            .ops
            .iter()
            .any(|entry| entry.node_id == "node-a" && entry.seq == 1));
        assert!(merged
            .ops
            .iter()
            .any(|entry| entry.node_id == "node-c" && entry.seq == 1));
    }

    #[tokio::test]
    async fn test_catch_up_from_peer_with_metadata_strict_rejects_partial_peer_success() {
        let peer_a_response = GetOpsResponse {
            tenant_id: "tenant-red".to_string(),
            ops: vec![OpLogEntry {
                seq: 1,
                timestamp_ms: 100,
                node_id: "node-a".to_string(),
                tenant_id: "tenant-red".to_string(),
                op_type: "upsert".to_string(),
                payload: serde_json::json!({"objectID": "a1", "body": {"_id": "a1", "title": "A"}}),
            }],
            current_seq: 1,
            oldest_retained_seq: Some(1),
            node_current_seqs: BTreeMap::from([(String::from("node-a"), 1)]),
        };

        let (peer_a_url, peer_a_handle) = spawn_single_response_peer(peer_a_response).await;
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-b".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![
                    PeerConfig {
                        node_id: "node-a".to_string(),
                        addr: peer_a_url,
                    },
                    PeerConfig {
                        node_id: "node-c".to_string(),
                        addr: "http://127.0.0.1:1".to_string(),
                    },
                ],
            },
            None,
        );

        let error = manager
            .catch_up_from_peer_with_metadata_strict("tenant-red", 0)
            .await
            .expect_err("strict catch-up must fail when any configured peer is unreachable");
        let _ = peer_a_handle.await;

        assert!(
            error.contains("peer node-c failed catch-up"),
            "strict failure should identify the unreachable peer, got: {}",
            error
        );
    }

    /// Peer responses must match the requested tenant exactly. A foreign tenant
    /// payload must be rejected instead of being merged into the requested
    /// tenant's catch-up batch.
    #[tokio::test]
    async fn test_catch_up_from_peer_skips_peer_returning_foreign_tenant_ops() {
        let good_peer_response = GetOpsResponse {
            tenant_id: "tenant-red".to_string(),
            ops: vec![OpLogEntry {
                seq: 1,
                timestamp_ms: 100,
                node_id: "node-a".to_string(),
                tenant_id: "tenant-red".to_string(),
                op_type: "upsert".to_string(),
                payload: serde_json::json!({"objectID": "a1", "body": {"_id": "a1", "title": "A"}}),
            }],
            current_seq: 1,
            oldest_retained_seq: Some(1),
            node_current_seqs: BTreeMap::from([(String::from("node-a"), 1)]),
        };
        let foreign_peer_response = GetOpsResponse {
            tenant_id: "tenant-red".to_string(),
            ops: vec![OpLogEntry {
                seq: 9,
                timestamp_ms: 200,
                node_id: "node-b".to_string(),
                tenant_id: "tenant-blue".to_string(),
                op_type: "upsert".to_string(),
                payload: serde_json::json!({"objectID": "b9", "body": {"_id": "b9", "title": "B"}}),
            }],
            current_seq: 9,
            oldest_retained_seq: Some(9),
            node_current_seqs: BTreeMap::from([(String::from("node-b"), 9)]),
        };

        let (good_peer_url, good_peer_handle) =
            spawn_single_response_peer(good_peer_response).await;
        let (foreign_peer_url, foreign_peer_handle) =
            spawn_single_response_peer(foreign_peer_response).await;

        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-c".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![
                    PeerConfig {
                        node_id: "node-a".to_string(),
                        addr: good_peer_url,
                    },
                    PeerConfig {
                        node_id: "node-b".to_string(),
                        addr: foreign_peer_url,
                    },
                ],
            },
            None,
        );

        let merged = manager
            .catch_up_from_peer_with_metadata("tenant-red", 0)
            .await
            .expect("the valid peer response should still succeed");

        let _ = good_peer_handle.await;
        let _ = foreign_peer_handle.await;

        assert_eq!(merged.ops.len(), 1);
        assert_eq!(merged.ops[0].tenant_id, "tenant-red");
        assert_eq!(merged.ops[0].node_id, "node-a");
        assert_eq!(merged.node_current_seqs.get("node-a"), Some(&1));
        assert!(
            !merged.node_current_seqs.contains_key("node-b"),
            "foreign-tenant peer metadata must not be merged"
        );
    }

    /// Strict catch-up must fail closed when a peer answers the request with the
    /// wrong tenant altogether.
    #[tokio::test]
    async fn test_catch_up_from_peer_with_metadata_strict_rejects_wrong_tenant_response() {
        let wrong_tenant_response = GetOpsResponse {
            tenant_id: "tenant-blue".to_string(),
            ops: vec![OpLogEntry {
                seq: 1,
                timestamp_ms: 100,
                node_id: "node-a".to_string(),
                tenant_id: "tenant-blue".to_string(),
                op_type: "upsert".to_string(),
                payload: serde_json::json!({"objectID": "b1", "body": {"_id": "b1", "title": "B"}}),
            }],
            current_seq: 1,
            oldest_retained_seq: Some(1),
            node_current_seqs: BTreeMap::from([(String::from("node-a"), 1)]),
        };

        let (peer_url, peer_handle) = spawn_single_response_peer(wrong_tenant_response).await;
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-c".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![PeerConfig {
                    node_id: "node-a".to_string(),
                    addr: peer_url,
                }],
            },
            None,
        );

        let error = manager
            .catch_up_from_peer_with_metadata_strict("tenant-red", 0)
            .await
            .expect_err("strict catch-up must reject a peer response for a different tenant");
        let _ = peer_handle.await;

        assert!(
            error.contains("tenant-blue") && error.contains("tenant-red"),
            "strict failure should identify both the returned and requested tenant, got: {}",
            error
        );
    }

    #[tokio::test]
    async fn test_catch_up_from_peer_with_metadata_strict_rejects_conflicting_duplicate_ops() {
        let first_peer_response = GetOpsResponse {
            tenant_id: "tenant-red".to_string(),
            ops: vec![OpLogEntry {
                seq: 1,
                timestamp_ms: 100,
                node_id: "node-a".to_string(),
                tenant_id: "tenant-red".to_string(),
                op_type: "upsert".to_string(),
                payload: serde_json::json!({"objectID": "a1", "body": {"_id": "a1", "title": "first"}}),
            }],
            current_seq: 1,
            oldest_retained_seq: Some(1),
            node_current_seqs: BTreeMap::from([(String::from("node-a"), 1)]),
        };
        let conflicting_peer_response = GetOpsResponse {
            tenant_id: "tenant-red".to_string(),
            ops: vec![OpLogEntry {
                seq: 1,
                timestamp_ms: 200,
                node_id: "node-a".to_string(),
                tenant_id: "tenant-red".to_string(),
                op_type: "upsert".to_string(),
                payload: serde_json::json!({"objectID": "a1", "body": {"_id": "a1", "title": "second"}}),
            }],
            current_seq: 1,
            oldest_retained_seq: Some(1),
            node_current_seqs: BTreeMap::from([(String::from("node-a"), 1)]),
        };

        let (first_peer_url, first_peer_handle) =
            spawn_single_response_peer(first_peer_response).await;
        let (conflicting_peer_url, conflicting_peer_handle) =
            spawn_single_response_peer(conflicting_peer_response).await;
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-c".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![
                    PeerConfig {
                        node_id: "node-a".to_string(),
                        addr: first_peer_url,
                    },
                    PeerConfig {
                        node_id: "node-b".to_string(),
                        addr: conflicting_peer_url,
                    },
                ],
            },
            None,
        );

        let error = manager
            .catch_up_from_peer_with_metadata_strict("tenant-red", 0)
            .await
            .expect_err("strict catch-up must fail closed on conflicting peer payloads");
        let _ = first_peer_handle.await;
        let _ = conflicting_peer_handle.await;

        assert!(
            error.contains("conflicting payload") && error.contains("(node-a, 1)"),
            "strict conflict error should identify the duplicate op key, got: {}",
            error
        );
    }

    #[tokio::test]
    async fn test_discover_tenants_from_peers_strict_rejects_partial_peer_success() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = serde_json::to_string(&ListTenantsResponse {
            tenants: vec!["tenant-red".to_string()],
        })
        .unwrap();
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let handle = tokio::spawn(async move {
            if let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(tokio::time::Duration::from_secs(3), listener.accept()).await
            {
                let mut request_buf = [0u8; 2048];
                let _ = socket.read(&mut request_buf).await;
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
                let _ = socket.shutdown().await;
            }
        });

        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-b".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![
                    PeerConfig {
                        node_id: "node-a".to_string(),
                        addr: format!("http://{}", addr),
                    },
                    PeerConfig {
                        node_id: "node-c".to_string(),
                        addr: "http://127.0.0.1:1".to_string(),
                    },
                ],
            },
            None,
        );

        let error = manager
            .discover_tenants_from_peers_strict()
            .await
            .expect_err(
                "strict tenant discovery must fail when any configured peer is unreachable",
            );
        let _ = handle.await;

        assert!(
            error.contains("peer node-c tenant discovery failed"),
            "strict tenant discovery failure should identify the unreachable peer, got: {}",
            error
        );
    }

    #[tokio::test]
    async fn test_discover_tenants_from_peers_skips_invalid_tenant_ids() {
        let (peer_url, peer_handle) = spawn_single_tenant_list_peer(ListTenantsResponse {
            tenants: vec!["tenant-red".to_string(), "../escape".to_string()],
        })
        .await;
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-b".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![PeerConfig {
                    node_id: "node-a".to_string(),
                    addr: peer_url,
                }],
            },
            None,
        );

        let tenants = manager.discover_tenants_from_peers().await;
        let _ = peer_handle.await;

        assert_eq!(tenants, vec!["tenant-red".to_string()]);
    }

    #[tokio::test]
    async fn test_discover_tenants_from_peers_strict_rejects_invalid_tenant_ids() {
        let (peer_url, peer_handle) = spawn_single_tenant_list_peer(ListTenantsResponse {
            tenants: vec!["../escape".to_string()],
        })
        .await;
        let manager = new_test_manager(
            NodeConfig {
                node_id: "node-b".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![PeerConfig {
                    node_id: "node-a".to_string(),
                    addr: peer_url,
                }],
            },
            None,
        );

        let error = manager
            .discover_tenants_from_peers_strict()
            .await
            .expect_err("strict tenant discovery must fail on invalid peer tenant ids");
        let _ = peer_handle.await;

        assert!(
            error.contains("invalid tenant id '../escape'"),
            "strict tenant discovery failure should identify the invalid tenant id, got: {}",
            error
        );
    }

    /// Regresses C1 ownership gap locally: both configured unreachable peers
    /// must still be represented after retry exhaustion.
    #[tokio::test]
    async fn test_replicate_ops_tracks_unreachable_peers_after_retry_exhaustion() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![
                PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: "http://127.0.0.1:1".to_string(),
                },
                PeerConfig {
                    node_id: "node-c".to_string(),
                    addr: "http://127.0.0.1:2".to_string(),
                },
            ],
        };

        let manager = new_test_manager(config, None);
        let op = OpLogEntry {
            seq: 1,
            timestamp_ms: 1,
            node_id: "node-a".to_string(),
            tenant_id: "tenant-red".to_string(),
            op_type: "upsert".to_string(),
            payload: serde_json::json!({"objectID": "doc-1", "body": {"_id": "doc-1", "name": "Alpha"}}),
        };

        manager.replicate_ops("tenant-red", vec![op]).await;
        tokio::time::sleep(tokio::time::Duration::from_millis(2300)).await;

        let tracked_peers = manager
            .get_peer_cursors("tenant-red")
            .expect("tenant cursor map should exist after retry exhaustion");
        assert_eq!(tracked_peers.len(), 2);
        assert!(tracked_peers.contains_key("node-b"));
        assert!(tracked_peers.contains_key("node-c"));
        assert!(tracked_peers
            .iter()
            .all(|entry| entry.value().last_acked_seq.is_none()));
        assert!(tracked_peers
            .iter()
            .all(|entry| entry.value().last_delivery_error.is_some()));
    }
}
