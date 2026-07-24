use super::circuit_breaker::CircuitBreaker;
use super::types::{
    GetOpsQuery, GetOpsResponse, ListTenantsResponse, ReplicateOpsRequest, ReplicateOpsResponse,
};
use std::error::Error;
use std::io::ErrorKind;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerHealthCheck {
    Healthy,
    Unreachable { reason: String },
    Indeterminate { reason: String },
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

    #[cfg(test)]
    pub(crate) fn set_last_success_timestamp_for_test(&self, timestamp_secs: u64) {
        self.last_success.store(timestamp_secs, Ordering::Relaxed);
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
    pub async fn health_check(&self) -> PeerHealthCheck {
        let url = format!("{}/internal/status", self.base_url);

        let response = match self.with_auth(self.http_client.get(&url)).send().await {
            Ok(response) => response,
            Err(error) => {
                self.circuit_breaker.record_failure();
                let reason = format!("Health check failed for {}: {}", self.peer_id, error);
                return if transport_error_proves_unreachable(&error) {
                    PeerHealthCheck::Unreachable { reason }
                } else {
                    PeerHealthCheck::Indeterminate { reason }
                };
            }
        };

        if response.status().is_success() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            self.last_success.store(now, Ordering::Relaxed);
            self.circuit_breaker.record_success();
            PeerHealthCheck::Healthy
        } else {
            self.circuit_breaker.record_failure();
            PeerHealthCheck::Indeterminate {
                reason: format!(
                    "Health check for {} returned {}",
                    self.peer_id,
                    response.status()
                ),
            }
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

fn transport_error_proves_unreachable(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || error_source_has_kind(error, ErrorKind::ConnectionRefused)
        || error_source_text_proves_unreachable(error)
}

fn error_source_has_kind(error: &(dyn Error + 'static), kind: ErrorKind) -> bool {
    let mut current = Some(error);
    while let Some(source) = current {
        if source
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == kind)
        {
            return true;
        }
        current = source.source();
    }
    false
}

fn error_source_text_proves_unreachable(error: &(dyn Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(source) = current {
        let message = source.to_string();
        if message.contains("dns error") || message.contains("failed to lookup address information")
        {
            return true;
        }
        current = source.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit_breaker::CircuitState;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

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

    #[test]
    fn transport_error_text_classifies_dns_failure_as_unreachable() {
        let error = std::io::Error::other(
            "dns error: failed to lookup address information: nodename nor servname provided, or not known",
        );

        assert!(error_source_text_proves_unreachable(&error));
    }

    async fn spawn_status_peer(status: &str) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let response =
            format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            socket.write_all(response.as_bytes()).await.unwrap();
            let _ = socket.shutdown().await;
        });
        (format!("http://{}", addr), handle)
    }

    #[tokio::test]
    async fn health_check_classifies_success_as_healthy() {
        let (base_url, handle) = spawn_status_peer("200 OK").await;
        let peer = PeerClient::new("node-b".to_string(), base_url, None);

        let outcome = peer.health_check().await;
        handle.await.unwrap();

        assert_eq!(outcome, PeerHealthCheck::Healthy);
        assert!(peer.last_success_timestamp() > 0);
        assert_eq!(peer.circuit_breaker().state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn health_check_classifies_connection_refusal_as_unreachable() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let peer = PeerClient::new("node-b".to_string(), format!("http://{}", addr), None);

        let outcome = peer.health_check().await;

        assert!(matches!(
            outcome,
            PeerHealthCheck::Unreachable { reason } if reason.contains("node-b")
        ));
    }

    #[tokio::test]
    async fn health_check_classifies_tls_connect_setup_failure_as_indeterminate() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n")
                .await;
            let _ = socket.shutdown().await;
        });
        let peer = PeerClient::new("node-b".to_string(), format!("https://{}", addr), None);

        let outcome = peer.health_check().await;
        handle.await.unwrap();

        assert!(matches!(
            outcome,
            PeerHealthCheck::Indeterminate { reason } if reason.contains("node-b")
        ));
    }

    #[tokio::test]
    async fn health_check_classifies_http_failure_as_indeterminate() {
        let (base_url, handle) = spawn_status_peer("500 Internal Server Error").await;
        let peer = PeerClient::new("node-b".to_string(), base_url, None);

        let outcome = peer.health_check().await;
        handle.await.unwrap();

        assert_eq!(
            outcome,
            PeerHealthCheck::Indeterminate {
                reason: "Health check for node-b returned 500 Internal Server Error".to_string()
            }
        );
    }
}
