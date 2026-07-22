use super::circuit_breaker::CircuitBreaker;
use super::types::{
    GetOpsQuery, GetOpsResponse, ListTenantsResponse, ReplicateOpsRequest, ReplicateOpsResponse,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Default: trip after 3 consecutive failures, probe again after 30 seconds
const DEFAULT_FAILURE_THRESHOLD: u32 = 3;
const DEFAULT_RECOVERY_TIMEOUT_SECS: u64 = 30;

/// HTTP client wrapper for communicating with a single peer node
pub struct PeerClient {
    peer_id: String,
    base_url: String,
    http_client: reqwest::Client,
    admin_key: Option<String>,
    last_success: Arc<AtomicU64>, // Unix timestamp in seconds
    circuit_breaker: CircuitBreaker,
}

impl PeerClient {
    /// Build a peer client with a 5-second HTTP timeout and a fresh circuit breaker.
    pub fn new(peer_id: String, base_url: String, admin_key: Option<String>) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            peer_id,
            base_url,
            http_client,
            admin_key,
            last_success: Arc::new(AtomicU64::new(0)),
            circuit_breaker: CircuitBreaker::new(
                DEFAULT_FAILURE_THRESHOLD,
                DEFAULT_RECOVERY_TIMEOUT_SECS,
            ),
        }
    }

    /// Attach auth headers to a request builder when an admin key is configured.
    /// Uses the existing `x-algolia-api-key` / `x-algolia-application-id` headers
    /// that the auth middleware already understands — no new header introduced.
    fn with_auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.admin_key {
            Some(key) => builder
                .header("x-algolia-api-key", key)
                .header("x-algolia-application-id", "flapjack-replication"),
            None => builder,
        }
    }

    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn last_success_timestamp(&self) -> u64 {
        self.last_success.load(Ordering::Relaxed)
    }

    /// Check if this peer's circuit breaker allows requests.
    pub fn is_available(&self) -> bool {
        self.circuit_breaker.allow_request()
    }

    /// Access the circuit breaker (for health probing to call record_success/failure).
    pub fn circuit_breaker(&self) -> &CircuitBreaker {
        &self.circuit_breaker
    }

    /// Replicate operations to this peer
    pub async fn replicate_ops(
        &self,
        req: ReplicateOpsRequest,
    ) -> Result<ReplicateOpsResponse, String> {
        let url = format!("{}/internal/replicate", self.base_url);

        let response = self
            .with_auth(self.http_client.post(&url).json(&req))
            .send()
            .await
            .map_err(|e| {
                self.circuit_breaker.record_failure();
                format!("Failed to send request to {}: {}", self.peer_id, e)
            })?;

        if !response.status().is_success() {
            self.circuit_breaker.record_failure();
            return Err(format!(
                "Peer {} returned error: {}",
                self.peer_id,
                response.status()
            ));
        }

        let resp: ReplicateOpsResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response from {}: {}", self.peer_id, e))?;

        // Update last success timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_success.store(now, Ordering::Relaxed);
        self.circuit_breaker.record_success();

        Ok(resp)
    }

    /// Fetch operations from this peer for catch-up
    pub async fn get_ops(&self, query: GetOpsQuery) -> Result<GetOpsResponse, String> {
        let url = format!(
            "{}/internal/ops?tenant_id={}&since_seq={}",
            self.base_url, query.tenant_id, query.since_seq
        );

        let response = self
            .with_auth(self.http_client.get(&url))
            .send()
            .await
            .map_err(|e| {
                self.circuit_breaker.record_failure();
                format!("Failed to fetch ops from {}: {}", self.peer_id, e)
            })?;

        if !response.status().is_success() {
            self.circuit_breaker.record_failure();
            return Err(format!(
                "Peer {} returned error: {}",
                self.peer_id,
                response.status()
            ));
        }

        let resp: GetOpsResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse ops from {}: {}", self.peer_id, e))?;

        // Update last success timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_success.store(now, Ordering::Relaxed);
        self.circuit_breaker.record_success();

        Ok(resp)
    }

    /// Download a full tenant snapshot from this peer.
    pub async fn get_snapshot(&self, tenant_id: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}/internal/snapshot/{}", self.base_url, tenant_id);

        let response = self
            .with_auth(self.http_client.get(&url))
            .send()
            .await
            .map_err(|e| {
                self.circuit_breaker.record_failure();
                format!("Failed to fetch snapshot from {}: {}", self.peer_id, e)
            })?;

        if !response.status().is_success() {
            self.circuit_breaker.record_failure();
            return Err(format!(
                "Peer {} returned error: {}",
                self.peer_id,
                response.status()
            ));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| format!("Failed to read snapshot body from {}: {}", self.peer_id, e))?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_success.store(now, Ordering::Relaxed);
        self.circuit_breaker.record_success();

        Ok(bytes.to_vec())
    }

    /// Ping this peer's status endpoint (for active health probing).
    /// Returns Ok(()) on success, Err on failure. Updates circuit breaker.
    pub async fn health_check(&self) -> Result<(), String> {
        let url = format!("{}/internal/status", self.base_url);

        let response = self
            .with_auth(self.http_client.get(&url))
            .send()
            .await
            .map_err(|e| {
                self.circuit_breaker.record_failure();
                format!("Health check failed for {}: {}", self.peer_id, e)
            })?;

        if response.status().is_success() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            self.last_success.store(now, Ordering::Relaxed);
            self.circuit_breaker.record_success();
            Ok(())
        } else {
            self.circuit_breaker.record_failure();
            Err(format!(
                "Health check for {} returned {}",
                self.peer_id,
                response.status()
            ))
        }
    }

    /// Fetch the list of visible tenant IDs from this peer.
    pub async fn list_tenants(&self) -> Result<ListTenantsResponse, String> {
        let url = format!("{}/internal/tenants", self.base_url);

        let response = self
            .with_auth(self.http_client.get(&url))
            .send()
            .await
            .map_err(|e| {
                self.circuit_breaker.record_failure();
                format!("Failed to fetch tenants from {}: {}", self.peer_id, e)
            })?;

        if !response.status().is_success() {
            self.circuit_breaker.record_failure();
            return Err(format!(
                "Peer {} returned error: {}",
                self.peer_id,
                response.status()
            ));
        }

        let resp: ListTenantsResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse tenants from {}: {}", self.peer_id, e))?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_success.store(now, Ordering::Relaxed);
        self.circuit_breaker.record_success();

        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit_breaker::CircuitState;

    #[test]
    fn test_peer_client_creation() {
        let peer = PeerClient::new(
            "test-peer".to_string(),
            "http://localhost:7700".to_string(),
            None,
        );

        assert_eq!(peer.peer_id(), "test-peer");
        assert_eq!(peer.last_success_timestamp(), 0);
        assert!(peer.admin_key.is_none());
    }

    #[test]
    fn test_peer_client_creation_with_admin_key() {
        let peer = PeerClient::new(
            "test-peer".to_string(),
            "http://localhost:7700".to_string(),
            Some("my-secret-key".to_string()),
        );

        assert_eq!(peer.peer_id(), "test-peer");
        assert_eq!(peer.admin_key.as_deref(), Some("my-secret-key"));
    }

    #[test]
    fn test_new_peer_is_available() {
        let peer = PeerClient::new(
            "test-peer".to_string(),
            "http://localhost:7700".to_string(),
            None,
        );
        assert!(peer.is_available());
        assert_eq!(peer.circuit_breaker().state(), CircuitState::Closed);
    }
}
