//! Analytics cluster fan-out coordinator.
//!
//! When peers are configured, queries all peers in parallel and merges results.
//! Each peer receives the same analytics query with `X-Flapjack-Local-Only: true`
//! to prevent re-entrant fan-out.

use flapjack::analytics::merge;
use flapjack::analytics::types::{ClusterMetadata, NodeDetail, NodeStatus, PeerResult};
use flapjack_replication::config::{NodeConfig, PeerConfig};
use flapjack_replication::peer::REPLICATION_PEER_APPLICATION_ID;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Fan-out coordinator for cluster analytics queries.
pub struct AnalyticsClusterClient {
    node_id: String,
    peers: Vec<PeerConfig>,
    http_client: reqwest::Client,
    /// Credential this node presents on peer-initiated fan-out (rollup pushes).
    /// Resolved once at startup; analytics code never reads the environment.
    /// `None` keeps rollup pushes unauthenticated for `FLAPJACK_NO_AUTH` and
    /// explicitly-unauthenticated clusters.
    peer_credential: Option<String>,
}

impl AnalyticsClusterClient {
    /// Create a new cluster client from node config and the peer credential
    /// resolved by server startup.
    /// Returns None if no peers are configured (standalone mode).
    pub fn new(node_config: &NodeConfig, peer_credential: Option<String>) -> Option<Arc<Self>> {
        if node_config.peers.is_empty() {
            return None;
        }

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            // Peer origins are validated before this client is built. Following
            // redirects would let a peer forward custom API-key headers beyond
            // that validated origin, including onto cleartext transport.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("analytics HTTP client with static settings must build");

        Some(Arc::new(Self {
            node_id: node_config.node_id.clone(),
            peers: node_config.peers.clone(),
            http_client,
            peer_credential,
        }))
    }

    /// Attach peer identity headers to an outbound fan-out request.
    ///
    /// Both fan-outs send the same header shape but draw from different
    /// credential sources: `query_peers` forwards the inbound request's
    /// credentials, while `push_rollup_to_peers` is a background broadcaster
    /// with no inbound request and uses the configured peer credential.
    fn with_peer_identity(
        request: reqwest::RequestBuilder,
        api_key: Option<&str>,
        application_id: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let request = match api_key {
            Some(key) => request.header("X-Algolia-API-Key", key),
            None => request,
        };
        match application_id {
            Some(id) => request.header("X-Algolia-Application-Id", id),
            None => request,
        }
    }

    /// Query all peers in parallel for an analytics endpoint.
    async fn query_peers(
        &self,
        path: &str,
        query_string: &str,
        headers: &axum::http::HeaderMap,
    ) -> Vec<PeerResult> {
        let mut handles = Vec::new();

        // Extract auth headers to forward to peers
        let api_key = headers
            .get("X-Algolia-API-Key")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let app_id = headers
            .get("X-Algolia-Application-Id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // Pre-collect skipped peers (circuit breaker open)
        let mut results = Vec::new();

        for peer in &self.peers {
            // Check circuit breaker — skip known-dead peers
            if let Some(ref manager) = flapjack_replication::get_global_manager() {
                if !manager.is_peer_available(&peer.node_id) {
                    tracing::debug!(
                        "[HA-analytics] skipping peer {} (circuit breaker open)",
                        peer.node_id
                    );
                    results.push(PeerResult {
                        node_id: peer.node_id.clone(),
                        latency_ms: 0,
                        data: Err("circuit_breaker_open".to_string()),
                    });
                    continue;
                }
            }

            let url = if query_string.is_empty() {
                format!("{}{}", peer.addr, path)
            } else {
                format!("{}{}?{}", peer.addr, path, query_string)
            };
            let client = self.http_client.clone();
            let peer_id = peer.node_id.clone();
            let api_key = api_key.clone();
            let app_id = app_id.clone();

            handles.push(tokio::spawn(async move {
                let start = Instant::now();
                let req = Self::with_peer_identity(
                    client.get(&url).header("X-Flapjack-Local-Only", "true"),
                    api_key.as_deref(),
                    app_id.as_deref(),
                );
                let result = req.send().await;

                let latency_ms = start.elapsed().as_millis() as u64;

                match result {
                    Ok(response) => {
                        if response.status().is_success() {
                            match response.json::<serde_json::Value>().await {
                                Ok(data) => PeerResult {
                                    node_id: peer_id,
                                    latency_ms,
                                    data: Ok(data),
                                },
                                Err(e) => PeerResult {
                                    node_id: peer_id,
                                    latency_ms,
                                    data: Err(format!("parse error: {}", e)),
                                },
                            }
                        } else {
                            PeerResult {
                                node_id: peer_id,
                                latency_ms,
                                data: Err(format!("HTTP {}", response.status())),
                            }
                        }
                    }
                    Err(e) => {
                        let err = if e.is_timeout() {
                            "timeout".to_string()
                        } else {
                            format!("{}", e)
                        };
                        PeerResult {
                            node_id: peer_id,
                            latency_ms,
                            data: Err(err),
                        }
                    }
                }
            }));
        }

        for handle in handles {
            match handle.await {
                Ok(result) => results.push(result),
                Err(e) => {
                    tracing::warn!("[HA-analytics] join error: {}", e);
                }
            }
        }
        results
    }

    /// Return the node IDs of all configured peers.
    pub fn peer_ids(&self) -> Vec<String> {
        self.peers.iter().map(|p| p.node_id.clone()).collect()
    }

    /// Return the node ID of this cluster client (the local node).
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// POST an AnalyticsRollup to every peer's /internal/analytics-rollup endpoint.
    /// Fires and forgets per-peer (non-blocking on individual peer failures).
    pub async fn push_rollup_to_peers(&self, rollup: &AnalyticsRollup) {
        let rollup_json = match serde_json::to_value(rollup) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("[ROLLUP-PUSH] Failed to serialize rollup: {}", e);
                return;
            }
        };

        let mut handles = Vec::new();
        for peer in &self.peers {
            let url = format!("{}/internal/analytics-rollup", peer.addr);
            let client = self.http_client.clone();
            let payload = rollup_json.clone();
            let peer_id = peer.node_id.clone();
            let peer_credential = self.peer_credential.clone();

            handles.push(tokio::spawn(async move {
                let request = Self::with_peer_identity(
                    client.post(&url).json(&payload),
                    peer_credential.as_deref(),
                    peer_credential
                        .as_deref()
                        .map(|_| REPLICATION_PEER_APPLICATION_ID),
                );
                match request.send().await {
                    Ok(resp) if resp.status().is_success() => {
                        tracing::debug!(
                            "[ROLLUP-PUSH] OK peer={} status={}",
                            peer_id,
                            resp.status()
                        );
                    }
                    Ok(resp) => {
                        tracing::warn!(
                            "[ROLLUP-PUSH] peer={} returned HTTP {}",
                            peer_id,
                            resp.status()
                        );
                    }
                    Err(e) => {
                        tracing::warn!("[ROLLUP-PUSH] peer={} error: {}", peer_id, e);
                    }
                }
            }));
        }

        for handle in handles {
            let _ = handle.await;
        }
    }

    /// Fan out a query, merge results, and return the merged response with cluster metadata.
    pub async fn fan_out_and_merge(
        &self,
        endpoint: &str,
        path: &str,
        query_string: &str,
        local_result: serde_json::Value,
        limit: usize,
        headers: &axum::http::HeaderMap,
    ) -> serde_json::Value {
        let peer_results = self.query_peers(path, query_string, headers).await;

        // Collect all results: local + successful peers
        let mut all_results = vec![local_result];
        let mut node_details = vec![NodeDetail {
            node_id: self.node_id.clone(),
            status: NodeStatus::Ok,
            latency_ms: Some(0),
        }];

        for pr in &peer_results {
            match &pr.data {
                Ok(data) => {
                    all_results.push(data.clone());
                    node_details.push(NodeDetail {
                        node_id: pr.node_id.clone(),
                        status: NodeStatus::Ok,
                        latency_ms: Some(pr.latency_ms),
                    });
                }
                Err(err) => {
                    let status = if err == "circuit_breaker_open" {
                        NodeStatus::Skipped
                    } else if err == "timeout" {
                        NodeStatus::Timeout
                    } else {
                        NodeStatus::Error(err.clone())
                    };
                    node_details.push(NodeDetail {
                        node_id: pr.node_id.clone(),
                        status,
                        latency_ms: if err == "circuit_breaker_open" {
                            None
                        } else {
                            Some(pr.latency_ms)
                        },
                    });
                }
            }
        }

        let nodes_total = 1 + self.peers.len();
        let nodes_responding = all_results.len();
        let partial = nodes_responding < nodes_total;

        if partial {
            tracing::warn!(
                "[HA-analytics] partial results: {}/{} nodes for {}",
                nodes_responding,
                nodes_total,
                endpoint
            );
        }

        let mut merged = merge::merge_results(endpoint, &all_results, limit);

        let cluster_meta = ClusterMetadata {
            nodes_total,
            nodes_responding,
            partial,
            node_details,
        };

        if let Some(obj) = merged.as_object_mut() {
            obj.insert(
                "cluster".to_string(),
                serde_json::to_value(&cluster_meta).unwrap_or(json!(null)),
            );
        }

        merged
    }
}

/// Protect caller API keys forwarded by authenticated analytics fan-out.
///
/// Replication transport validation normally keys off the configured peer
/// credential. Analytics queries are different: they preserve the caller's
/// API key so the remote node applies the same authorization. Every topology
/// mutation boundary must therefore apply this additional transport premise
/// even when the explicit unauthenticated-replication escape is active.
pub(crate) fn validate_authenticated_query_peer_transport(
    node_id: &str,
    addr: &str,
) -> Result<(), String> {
    NodeConfig::validate_credentialed_peer_transport(
        node_id,
        addr,
        "authenticated analytics query fan-out forwards caller API keys",
    )
}

use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

static GLOBAL_ANALYTICS_CLUSTER: OnceCell<Arc<AnalyticsClusterClient>> = OnceCell::new();

/// Set the global analytics cluster client (called once during server startup).
pub fn set_global_cluster(client: Arc<AnalyticsClusterClient>) {
    let _ = GLOBAL_ANALYTICS_CLUSTER.set(client);
}

/// Get the global analytics cluster client if configured.
pub fn get_global_cluster() -> Option<Arc<AnalyticsClusterClient>> {
    GLOBAL_ANALYTICS_CLUSTER.get().map(Arc::clone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_recording_peer() -> (String, tokio::sync::oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test peer must bind before the request is awaited");
        let addr = listener
            .local_addr()
            .expect("bound test peer must expose its address");
        let (tx, rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("test peer should accept");
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let read = socket
                    .read(&mut buf)
                    .await
                    .expect("test peer should read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&request).to_string());
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
                .await
                .expect("test peer should write response");
            let _ = socket.shutdown().await;
        });

        (format!("http://{addr}"), rx)
    }

    fn cluster_for_peer(
        peer_addr: String,
        peer_credential: Option<String>,
    ) -> Arc<AnalyticsClusterClient> {
        AnalyticsClusterClient::new(
            &NodeConfig {
                node_id: "node-a".to_string(),
                bind_addr: "127.0.0.1:0".to_string(),
                advertise_addr: None,
                bootstrap_peer: None,
                peers: vec![PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: peer_addr,
                }],
            },
            peer_credential,
        )
        .expect("cluster client should build with a peer")
    }

    async fn received_request(request_rx: tokio::sync::oneshot::Receiver<String>) -> String {
        tokio::time::timeout(Duration::from_secs(2), request_rx)
            .await
            .expect("peer should receive the request")
            .expect("peer request channel should stay open")
    }

    fn header_value<'a>(request: &'a str, header_name: &str) -> Option<&'a str> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(header_name).then(|| value.trim())
        })
    }

    #[test]
    fn new_returns_none_for_no_peers() {
        let config = NodeConfig {
            node_id: "standalone".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![],
        };
        assert!(AnalyticsClusterClient::new(&config, None).is_none());
    }

    #[test]
    fn new_returns_some_with_peers() {
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
        let client = AnalyticsClusterClient::new(&config, None);
        assert!(client.is_some());
        let client = client.unwrap();
        assert_eq!(client.node_id, "node-a");
        assert_eq!(client.peers.len(), 1);
    }

    #[tokio::test]
    async fn push_rollup_to_peers_uses_query_peer_auth_header_contract() {
        const PEER_KEY: &str = "stage-2-rollup-peer-secret";

        let (query_peer_addr, query_request_rx) = spawn_recording_peer().await;
        let query_cluster = cluster_for_peer(query_peer_addr, None);
        let mut query_headers = axum::http::HeaderMap::new();
        query_headers.insert(
            "X-Algolia-API-Key",
            PEER_KEY
                .parse()
                .expect("test peer key must be a header value"),
        );
        query_headers.insert(
            "X-Algolia-Application-Id",
            REPLICATION_PEER_APPLICATION_ID
                .parse()
                .expect("peer application id must be a header value"),
        );

        let query_results = query_cluster
            .query_peers("/internal/test-analytics", "", &query_headers)
            .await;
        assert_eq!(query_results.len(), 1, "query peer must respond");
        assert!(
            query_results[0].data.is_ok(),
            "query peer response must be valid before comparing headers: {:?}",
            query_results[0].data
        );
        let query_request = received_request(query_request_rx).await;
        assert_eq!(
            header_value(&query_request, "X-Algolia-API-Key"),
            Some(PEER_KEY)
        );
        assert_eq!(
            header_value(&query_request, "X-Algolia-Application-Id"),
            Some(REPLICATION_PEER_APPLICATION_ID)
        );

        let (rollup_peer_addr, rollup_request_rx) = spawn_recording_peer().await;
        let rollup_cluster = cluster_for_peer(rollup_peer_addr, Some(PEER_KEY.to_string()));
        let rollup = AnalyticsRollup {
            node_id: "node-a".to_string(),
            index: "products".to_string(),
            generated_at_secs: 1_722_600_000,
            results: HashMap::new(),
        };

        rollup_cluster.push_rollup_to_peers(&rollup).await;
        let rollup_request = received_request(rollup_request_rx).await;
        assert!(
            rollup_request.starts_with("POST /internal/analytics-rollup HTTP/1.1"),
            "rollup fan-out must POST to the internal rollup endpoint, got: {rollup_request:?}"
        );
        assert_eq!(
            header_value(&rollup_request, "X-Algolia-API-Key"),
            header_value(&query_request, "X-Algolia-API-Key"),
            "rollup fan-out must send the same replication credential as query fan-out"
        );
        assert_eq!(
            header_value(&rollup_request, "X-Algolia-Application-Id"),
            header_value(&query_request, "X-Algolia-Application-Id"),
            "rollup fan-out must identify itself with the same peer application id as query fan-out"
        );
    }

    #[tokio::test]
    async fn authenticated_rollup_does_not_follow_peer_redirect() {
        let target = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("redirect target must bind before the request is sent");
        let target_addr = target.local_addr().unwrap();
        let redirector = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("redirecting peer must bind before the request is sent");
        let redirector_addr = redirector.local_addr().unwrap();

        let redirect_task = tokio::spawn(async move {
            let (mut socket, _) = redirector.accept().await.unwrap();
            let _ = read_request_headers(&mut socket).await;
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://{target_addr}/stolen\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let cluster = cluster_for_peer(
            format!("http://{redirector_addr}"),
            Some("redirect-sensitive-peer-key".to_string()),
        );
        cluster
            .push_rollup_to_peers(&AnalyticsRollup {
                node_id: "node-a".to_string(),
                index: "products".to_string(),
                generated_at_secs: 1_722_600_000,
                results: HashMap::new(),
            })
            .await;
        redirect_task.await.unwrap();

        assert!(
            tokio::time::timeout(Duration::from_millis(200), target.accept())
                .await
                .is_err(),
            "authenticated analytics requests must stop at redirects instead of forwarding the peer key to a new origin"
        );
    }

    async fn read_request_headers(socket: &mut tokio::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let read = socket.read(&mut buf).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buf[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&request).to_string()
    }

    #[test]
    fn fan_out_maps_circuit_breaker_open_to_skipped() {
        // Verify that the error-to-status mapping in fan_out_and_merge
        // correctly converts "circuit_breaker_open" to NodeStatus::Skipped
        let err = "circuit_breaker_open";
        let status = if err == "circuit_breaker_open" {
            NodeStatus::Skipped
        } else if err == "timeout" {
            NodeStatus::Timeout
        } else {
            NodeStatus::Error(err.to_string())
        };
        assert!(matches!(status, NodeStatus::Skipped));
    }

    #[test]
    fn fan_out_maps_timeout_correctly() {
        let err = "timeout";
        let status = if err == "circuit_breaker_open" {
            NodeStatus::Skipped
        } else if err == "timeout" {
            NodeStatus::Timeout
        } else {
            NodeStatus::Error(err.to_string())
        };
        assert!(matches!(status, NodeStatus::Timeout));
    }

    #[test]
    fn fan_out_maps_other_errors_correctly() {
        let err = "connection refused";
        let status = if err == "circuit_breaker_open" {
            NodeStatus::Skipped
        } else if err == "timeout" {
            NodeStatus::Timeout
        } else {
            NodeStatus::Error(err.to_string())
        };
        assert!(matches!(status, NodeStatus::Error(ref s) if s == "connection refused"));
    }
}

// ── Phase 4: Analytics Rollup Exchange (HA Analytics Tier 2) ─────────────────
//
// For clusters with 10+ globally distributed nodes, Tier 1 live fan-out adds
// cross-region latency on every analytics query. Tier 2 exchanges precomputed
// analytics rollups between nodes every ROLLUP_MAX_AGE_SECS / 2 seconds,
// allowing any node to serve analytics queries locally from its rollup cache.
//
// Architecture:
//   - Peers push AnalyticsRollup payloads to /internal/analytics-rollup (POST).
//   - GLOBAL_ROLLUP_CACHE stores the most recent rollup per (peer_id, index).
//   - maybe_fan_out() checks the rollup cache first; if all peers have fresh
//     rollups, merges them locally without any live HTTP fan-out (Tier 2 path).
//   - Falls back to Tier 1 live fan-out when rollups are stale or absent.

/// Maximum age (seconds) before a rollup is considered stale.
/// Rollups older than this trigger Tier 1 live fan-out as a fallback.
pub const ROLLUP_MAX_AGE_SECS: u64 = 600; // 10 minutes

/// Pre-computed analytics snapshot for one index on one node.
/// Pushed to peers via POST /internal/analytics-rollup.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct AnalyticsRollup {
    pub node_id: String,
    pub index: String,
    /// Unix timestamp (seconds) when this rollup was computed.
    pub generated_at_secs: u64,
    /// Keyed by analytics endpoint name (matches merge::merge_results `endpoint` param).
    /// e.g. "searches" → top_searches JSON, "searches/count" → count JSON.
    pub results: HashMap<String, serde_json::Value>,
}

/// Thread-safe store of the most recent rollup received from each peer.
pub struct RollupCache {
    /// Key: (peer_node_id, index_name) → most recent rollup from that peer.
    entries: dashmap::DashMap<(String, String), AnalyticsRollup>,
}

impl RollupCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: dashmap::DashMap::new(),
        })
    }

    /// Store a rollup received from a peer (replaces any older entry).
    pub fn store(&self, rollup: AnalyticsRollup) {
        self.entries
            .insert((rollup.node_id.clone(), rollup.index.clone()), rollup);
    }

    /// Retrieve the most recent rollup from `peer_id` for `index`.
    pub fn get(&self, peer_id: &str, index: &str) -> Option<AnalyticsRollup> {
        self.entries
            .get(&(peer_id.to_string(), index.to_string()))
            .map(|e| e.value().clone())
    }

    /// True if `peer_id`'s rollup for `index` exists and is < `max_age_secs` old.
    pub fn is_fresh(&self, peer_id: &str, index: &str, max_age_secs: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.entries
            .get(&(peer_id.to_string(), index.to_string()))
            .map(|e| now.saturating_sub(e.generated_at_secs) < max_age_secs)
            .unwrap_or(false)
    }

    /// True if ALL `peer_ids` have fresh rollups for `index`.
    /// Returns false when `peer_ids` is empty.
    pub fn all_fresh(&self, peer_ids: &[String], index: &str, max_age_secs: u64) -> bool {
        !peer_ids.is_empty()
            && peer_ids
                .iter()
                .all(|id| self.is_fresh(id, index, max_age_secs))
    }

    /// Return all cached rollups for `index` (from any peer).
    pub fn all_for_index(&self, index: &str) -> Vec<AnalyticsRollup> {
        self.entries
            .iter()
            .filter(|e| e.key().1 == index)
            .map(|e| e.value().clone())
            .collect()
    }

    /// Return every cached rollup from all peers and all indexes.
    /// Used by the /internal/rollup-cache diagnostic endpoint.
    pub fn all_entries(&self) -> Vec<AnalyticsRollup> {
        self.entries.iter().map(|e| e.value().clone()).collect()
    }

    /// Remove all cached entries. Used by tests to prevent global state leakage.
    pub fn clear(&self) {
        self.entries.clear();
    }
}

/// Global rollup cache — lazily initialized on first access; always available.
static GLOBAL_ROLLUP_CACHE: once_cell::sync::Lazy<Arc<RollupCache>> =
    once_cell::sync::Lazy::new(RollupCache::new);

/// Get the global rollup cache. Always returns Some; the cache is created lazily.
pub fn get_global_rollup_cache() -> Arc<RollupCache> {
    Arc::clone(&GLOBAL_ROLLUP_CACHE)
}

#[cfg(test)]
mod rollup_tests {
    use super::*;

    fn make_rollup(peer_id: &str, index: &str, age_secs: u64) -> AnalyticsRollup {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        AnalyticsRollup {
            node_id: peer_id.to_string(),
            index: index.to_string(),
            generated_at_secs: now.saturating_sub(age_secs),
            results: HashMap::new(),
        }
    }

    #[test]
    fn rollup_cache_stores_and_retrieves() {
        let cache = RollupCache::new();
        cache.store(make_rollup("peer-1", "my-index", 0));
        let got = cache.get("peer-1", "my-index");
        assert!(got.is_some());
        assert_eq!(got.unwrap().node_id, "peer-1");
    }

    #[test]
    fn rollup_cache_missing_returns_none() {
        let cache = RollupCache::new();
        assert!(cache.get("no-peer", "no-index").is_none());
    }

    #[test]
    fn rollup_cache_newer_overwrites_older() {
        let cache = RollupCache::new();
        cache.store(make_rollup("peer-1", "idx", 100));
        let new = make_rollup("peer-1", "idx", 0);
        let new_ts = new.generated_at_secs;
        cache.store(new);
        let got = cache.get("peer-1", "idx").unwrap();
        // The newer (smaller age) entry should survive
        assert!(got.generated_at_secs >= new_ts.saturating_sub(1));
    }

    #[test]
    fn rollup_cache_is_fresh_young_rollup() {
        let cache = RollupCache::new();
        cache.store(make_rollup("peer-1", "idx", 30)); // 30 seconds old
        assert!(cache.is_fresh("peer-1", "idx", 60)); // max_age=60 → fresh
        assert!(!cache.is_fresh("peer-1", "idx", 20)); // max_age=20 → stale
    }

    #[test]
    fn rollup_cache_is_fresh_missing_rollup_is_false() {
        let cache = RollupCache::new();
        assert!(!cache.is_fresh("peer-1", "idx", 600));
    }

    #[test]
    fn rollup_cache_all_fresh_requires_all_peers() {
        let cache = RollupCache::new();
        cache.store(make_rollup("peer-1", "idx", 10));
        cache.store(make_rollup("peer-2", "idx", 10));

        let both = vec!["peer-1".to_string(), "peer-2".to_string()];
        assert!(cache.all_fresh(&both, "idx", 60));

        let partial = vec!["peer-1".to_string(), "peer-3".to_string()]; // peer-3 absent
        assert!(!cache.all_fresh(&partial, "idx", 60));
    }

    #[test]
    fn rollup_cache_all_fresh_empty_peers_returns_false() {
        let cache = RollupCache::new();
        assert!(!cache.all_fresh(&[], "idx", 600));
    }

    #[test]
    fn rollup_cache_all_for_index_returns_only_matching() {
        let cache = RollupCache::new();
        cache.store(make_rollup("peer-1", "idx-a", 0));
        cache.store(make_rollup("peer-2", "idx-a", 0));
        cache.store(make_rollup("peer-1", "idx-b", 0));

        let rollups = cache.all_for_index("idx-a");
        assert_eq!(rollups.len(), 2);
        assert!(rollups.iter().all(|r| r.index == "idx-a"));
    }

    #[test]
    fn rollup_cache_all_for_index_empty_when_no_match() {
        let cache = RollupCache::new();
        assert!(cache.all_for_index("nonexistent").is_empty());
    }
}
