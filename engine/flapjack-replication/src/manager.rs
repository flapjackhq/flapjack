use super::circuit_breaker::CircuitState;
use super::config::NodeConfig;
use super::peer::PeerClient;
use super::types::{
    GetOpsQuery, GetOpsResponse, ListTenantsResponse, PeerHealthStatus, ReplicateOpsRequest,
};
use dashmap::DashMap;
use flapjack::index::oplog::OpLogEntry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::Mutex;
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

/// Orchestrates replication to all peers and tracks their acknowledgment status
pub struct ReplicationManager {
    node_config: NodeConfig,
    peers: Vec<Arc<PeerClient>>,
    /// Tracks delivery status for each configured peer and tenant
    /// Outer map: tenant_id -> inner map
    /// Inner map: peer_id -> last delivery cursor/error
    peer_cursors: Arc<DashMap<String, DashMap<String, PeerCursor>>>,
    /// Handle to the background health probe task (if running)
    health_probe_handle: Mutex<Option<JoinHandle<()>>>,
}

impl ReplicationManager {
    /// Initialize a ReplicationManager from the given configuration, creating PeerClient instances for each configured peer. Peer acknowledgment cursors start empty, and the background health probe is not running until explicitly started via `start_health_probe`.
    ///
    /// # Arguments
    ///
    /// * `node_config` - Configuration containing this node's identity and the list of peer addresses to replicate to.
    ///
    /// # Returns
    ///
    /// An Arc-wrapped ReplicationManager ready for use in multi-threaded contexts.
    pub fn new(node_config: NodeConfig, admin_key: Option<String>) -> Arc<Self> {
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
            node_config,
            peers,
            peer_cursors: Arc::new(DashMap::new()),
            health_probe_handle: Mutex::new(None),
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_config.node_id
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Check if a specific peer is available (circuit breaker not tripped).
    pub fn is_peer_available(&self, node_id: &str) -> bool {
        self.peers
            .iter()
            .find(|p| p.peer_id() == node_id)
            .map(|p| p.is_available())
            .unwrap_or(false)
    }

    /// Get list of available peer node IDs (circuit breaker closed or half-open).
    pub fn available_peers(&self) -> Vec<String> {
        self.peers
            .iter()
            .filter(|p| p.is_available())
            .map(|p| p.peer_id().to_string())
            .collect()
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
        peer: Arc<PeerClient>,
        peer_cursors: Arc<DashMap<String, DashMap<String, PeerCursor>>>,
        tenant_id: String,
        peer_id: String,
        ops: Vec<OpLogEntry>,
    ) -> Result<u64, String> {
        if !peer.is_available() {
            let previous_ack = Self::existing_acked_seq(&peer_cursors, &tenant_id, &peer_id);
            Self::set_peer_cursor(
                &peer_cursors,
                &tenant_id,
                &peer_id,
                PeerCursor::failed("circuit breaker open".to_string(), previous_ack),
            );
            tracing::debug!(
                "[REPL {}] skipping peer {} (circuit breaker open)",
                tenant_id,
                peer_id
            );
            return Err("circuit breaker open".to_string());
        }

        match Self::replicate_to_peer_with_retry(&peer, &tenant_id, ops).await {
            Ok(acked_seq) => {
                Self::set_peer_cursor(
                    &peer_cursors,
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
                let previous_ack = Self::existing_acked_seq(&peer_cursors, &tenant_id, &peer_id);
                Self::set_peer_cursor(
                    &peer_cursors,
                    &tenant_id,
                    &peer_id,
                    PeerCursor::failed(error.clone(), previous_ack),
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
    pub async fn replicate_ops(&self, tenant_id: &str, ops: Vec<OpLogEntry>) {
        if ops.is_empty() {
            return;
        }

        let tenant_id = tenant_id.to_string();

        for peer in &self.peers {
            let peer_id = peer.peer_id().to_string();
            let peer = Arc::clone(peer);
            let tenant_id = tenant_id.clone();
            let ops = ops.clone();
            let peer_cursors = Arc::clone(&self.peer_cursors);
            let peer_id = peer_id.clone();

            // Fire-and-forget: spawn task and don't await
            tokio::spawn(async move {
                let _ = Self::replicate_to_single_peer(peer, peer_cursors, tenant_id, peer_id, ops)
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
            .peers
            .iter()
            .find(|peer| peer.peer_id() == peer_id)
            .cloned()
            .ok_or_else(|| format!("Unknown peer '{}'", peer_id))?;

        Self::replicate_to_single_peer(
            peer,
            Arc::clone(&self.peer_cursors),
            tenant_id.to_string(),
            peer_id.to_string(),
            ops,
        )
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
        if self.peers.is_empty() {
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
        for peer in &self.peers {
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
        if self.peers.is_empty() {
            return Ok(Vec::new());
        }

        let mut tenants = BTreeSet::new();
        let mut any_success = false;
        let mut last_error = String::from("All peers have tripped circuit breakers");
        for peer in &self.peers {
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
                    tenants.extend(peer_tenants);
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
        if self.peers.is_empty() {
            return Err("No peers available for snapshot restore".to_string());
        }

        let mut last_error = String::from("All peers have tripped circuit breakers");
        for peer in self.peers.iter().filter(|p| p.is_available()) {
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

        self.node_config
            .peers
            .iter()
            .zip(self.peers.iter())
            .map(|(cfg, client)| {
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
                    peer_id: cfg.node_id.clone(),
                    addr: cfg.addr.clone(),
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

                for peer in &manager.peers {
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

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

    #[test]
    fn test_manager_creation() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            }],
        };

        let manager = ReplicationManager::new(config, None);

        assert_eq!(manager.node_id(), "node-a");
        assert_eq!(manager.peer_count(), 1);
    }

    #[test]
    fn test_manager_no_peers() {
        let config = NodeConfig {
            node_id: "standalone".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            peers: vec![],
        };

        let manager = ReplicationManager::new(config, None);

        assert_eq!(manager.node_id(), "standalone");
        assert_eq!(manager.peer_count(), 0);
    }

    /// Verify that all configured peers are initially available and `is_peer_available()` returns false for unknown peers.
    #[test]
    fn test_all_peers_available_initially() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
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

        let manager = ReplicationManager::new(config, None);
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
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            }],
        };

        let manager = ReplicationManager::new(config, None);
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
            peers: vec![],
        };

        let manager = ReplicationManager::new(config, None);
        assert!(manager.peer_statuses().is_empty());
    }
    #[tokio::test]
    async fn test_health_probe_handle_starts_and_stops() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            }],
        };
        let manager = ReplicationManager::new(config, None);

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

        let manager = ReplicationManager::new(config, None);
        let available = manager.available_peers();
        assert!(available.contains(&"node-b".to_string()));
        assert!(available.contains(&"node-c".to_string()));
    }

    #[test]
    fn test_get_peer_cursors_empty_initially() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            }],
        };

        let manager = ReplicationManager::new(config, None);
        assert!(manager.get_peer_cursors("some-tenant").is_none());
    }

    #[tokio::test]
    async fn test_replicate_ops_empty_ops_is_noop() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b:7700".to_string(),
            }],
        };

        let manager = ReplicationManager::new(config, None);
        // Empty ops should return immediately without spawning tasks
        manager.replicate_ops("test-tenant", vec![]).await;
        // No panic = success
    }

    #[tokio::test]
    async fn test_catch_up_from_peer_no_peers_returns_error() {
        let config = NodeConfig {
            node_id: "standalone".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            peers: vec![],
        };

        let manager = ReplicationManager::new(config, None);
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

        let manager = ReplicationManager::new(
            NodeConfig {
                node_id: "node-b".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
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
        let manager = ReplicationManager::new(
            NodeConfig {
                node_id: "node-b".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
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

        let manager = ReplicationManager::new(
            NodeConfig {
                node_id: "node-c".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
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
        let manager = ReplicationManager::new(
            NodeConfig {
                node_id: "node-c".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
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

        let manager = ReplicationManager::new(
            NodeConfig {
                node_id: "node-b".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
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

    /// Regresses C1 ownership gap locally: both configured unreachable peers
    /// must still be represented after retry exhaustion.
    #[tokio::test]
    async fn test_replicate_ops_tracks_unreachable_peers_after_retry_exhaustion() {
        let config = NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
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

        let manager = ReplicationManager::new(config, None);
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
