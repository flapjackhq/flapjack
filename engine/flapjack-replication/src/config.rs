//! Stub summary for engine/flapjack-replication/src/config.rs.
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::Path;

const NODE_CONFIG_FILE: &str = "node.json";
const REPLICATION_API_KEY_ENV: &str = "FLAPJACK_REPLICATION_API_KEY";
/// Escape hatch that re-permits cleartext peer origins while a peer credential
/// is configured. See [`NodeConfig::retain_transport_safe_peers`].
const ALLOW_CLEARTEXT_REPLICATION_PEERS_ENV: &str = "FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS";

#[cfg(test)]
static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
struct EnvVarRestoreGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl EnvVarRestoreGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }

    fn remove(name: &'static str) -> Self {
        let previous = std::env::var_os(name);
        std::env::remove_var(name);
        Self { name, previous }
    }
}

#[cfg(test)]
impl Drop for EnvVarRestoreGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

#[cfg(test)]
#[derive(Clone)]
struct TestWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl TestWriter {
    fn new() -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(Vec::new())))
    }

    fn output(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[cfg(test)]
impl std::io::Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TestWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(test)]
fn capture_config_logs<T>(action: impl FnOnce() -> T) -> (T, String) {
    use tracing_subscriber::layer::SubscriberExt;

    let writer = TestWriter::new();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(writer.clone()));
    let result = tracing::subscriber::with_default(subscriber, action);
    (result, writer.output())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub node_id: String,
    pub bind_addr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertise_addr: Option<String>,
    pub peers: Vec<PeerConfig>,
    #[serde(skip)]
    pub bootstrap_peer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerConfig {
    pub node_id: String,
    pub addr: String, // e.g., "http://10.0.1.2:7700" or "http://node-b:7700"
}

impl NodeConfig {
    /// Load node configuration for non-server callers, filtering and logging
    /// peer transports that violate the credential policy.
    pub fn load_or_default(data_dir: &Path) -> Self {
        let (config, loaded_from_file, transport_errors) =
            Self::load_with_transport_validation(data_dir);
        for error in transport_errors {
            tracing::error!("{error}");
        }
        Self::log_loaded_source(&config, loaded_from_file);
        config
    }

    /// Load node configuration for server startup, failing rather than silently
    /// downgrading to standalone mode when transport policy rejects topology.
    pub fn load_for_server_startup(data_dir: &Path) -> Result<Self, String> {
        let (config, loaded_from_file, transport_errors) =
            Self::load_with_transport_validation(data_dir);
        if !transport_errors.is_empty() {
            return Err(transport_errors.join("; "));
        }
        Self::log_loaded_source(&config, loaded_from_file);
        Ok(config)
    }

    fn load_with_transport_validation(data_dir: &Path) -> (Self, bool, Vec<String>) {
        let node_json = data_dir.join(NODE_CONFIG_FILE);
        let from_file = Self::load_from_file(&node_json);
        let loaded_from_file = from_file.is_some();
        let mut config = from_file.unwrap_or_else(Self::from_env);

        // Peer transport policy is applied once here, over every outbound peer
        // origin, so `node.json`, `FLAPJACK_PEERS`, and `FLAPJACK_BOOTSTRAP_PEER`
        // obey exactly one rule.
        let (peers, mut transport_errors) = Self::retain_transport_safe_peers(config.peers);
        let (bootstrap_peer, bootstrap_error) =
            Self::retain_transport_safe_bootstrap_peer(config.bootstrap_peer);
        config.peers = peers;
        config.bootstrap_peer = bootstrap_peer;
        transport_errors.extend(bootstrap_error);
        (config, loaded_from_file, transport_errors)
    }

    fn log_loaded_source(config: &Self, loaded_from_file: bool) {
        if loaded_from_file {
            tracing::info!(
                "Loaded node config: node_id={}, peers={}",
                config.node_id,
                config.peers.len()
            );
        } else {
            Self::log_default_source(config);
        }
    }

    /// Build a standalone node configuration from the peer topology environment.
    fn from_env() -> Self {
        let peers = Self::parse_env_peers();
        Self {
            node_id: Self::default_node_id(),
            bind_addr: Self::default_bind_addr(),
            advertise_addr: Self::parse_optional_peer_origin_env("FLAPJACK_ADVERTISE_ADDR"),
            bootstrap_peer: if peers.is_empty() {
                Self::parse_optional_peer_origin_env("FLAPJACK_BOOTSTRAP_PEER")
            } else {
                None
            },
            peers,
        }
    }

    /// Separate accepted peer origins from transport-policy errors while this
    /// node has a peer credential to send.
    ///
    /// Replication requests and background analytics rollup pushes carry
    /// `FLAPJACK_REPLICATION_API_KEY`, so an `http://` peer origin puts that
    /// secret on the wire in plaintext and the peer is refused by default.
    /// Analytics queries instead forward any caller-supplied key and are
    /// checked by the HTTP layer at its topology boundaries, including when
    /// server authentication is disabled.
    /// `FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1` re-permits it and warns
    /// loudly every time; it is default-off and intended only for temporary
    /// rolling upgrades or a private, operator-controlled transport.
    ///
    fn retain_transport_safe_peers(peers: Vec<PeerConfig>) -> (Vec<PeerConfig>, Vec<String>) {
        let mut accepted = Vec::with_capacity(peers.len());
        let mut errors = Vec::new();
        for peer in peers {
            match Self::validate_peer_transport(&peer.node_id, &peer.addr) {
                Ok(()) => accepted.push(peer),
                Err(message) => errors.push(message),
            }
        }
        (accepted, errors)
    }

    fn retain_transport_safe_bootstrap_peer(
        bootstrap_peer: Option<String>,
    ) -> (Option<String>, Option<String>) {
        let Some(bootstrap_peer) = bootstrap_peer else {
            return (None, None);
        };
        match Self::validate_peer_transport("bootstrap", &bootstrap_peer) {
            Ok(()) => (Some(bootstrap_peer), None),
            Err(message) => (None, Some(message)),
        }
    }

    /// True when a real (non-blank) peer credential is configured, matching the
    /// trim-and-reject-empty normalization server startup applies.
    fn has_replication_credential() -> bool {
        std::env::var(REPLICATION_API_KEY_ENV).is_ok_and(|key| !key.trim().is_empty())
    }

    fn is_cleartext_peer_addr(addr: &str) -> bool {
        reqwest::Url::parse(addr).is_ok_and(|url| url.scheme() == "http")
    }

    /// Enforce the cleartext transport policy for one normalized peer address.
    ///
    /// This is shared by startup topology loading and runtime peer mutation so
    /// both paths obey the same transport rule and escape hatch.
    pub fn validate_peer_transport(node_id: &str, addr: &str) -> Result<(), String> {
        if !Self::has_replication_credential() {
            return Ok(());
        }

        Self::validate_credentialed_peer_transport(
            node_id,
            addr,
            &format!("{REPLICATION_API_KEY_ENV} is set"),
        )
    }

    /// Enforce the peer transport policy when a caller knows it will attach a
    /// credential other than `FLAPJACK_REPLICATION_API_KEY`.
    pub fn validate_credentialed_peer_transport(
        node_id: &str,
        addr: &str,
        credential_context: &str,
    ) -> Result<(), String> {
        if !Self::is_cleartext_peer_addr(addr) {
            return Ok(());
        }

        if matches!(
            std::env::var(ALLOW_CLEARTEXT_REPLICATION_PEERS_ENV).as_deref(),
            Ok("1")
        ) {
            tracing::warn!(
                "WARNING: replication peer {} at {} uses cleartext http:// while {}, and is \
                 permitted only because \
                 {ALLOW_CLEARTEXT_REPLICATION_PEERS_ENV}=1; the peer credential is sent in \
                 plaintext. Move the peer to https:// and unset \
                 {ALLOW_CLEARTEXT_REPLICATION_PEERS_ENV}.",
                node_id,
                addr,
                credential_context
            );
            return Ok(());
        }

        Err(format!(
            "Refusing replication peer {node_id} at {addr}: {credential_context} and the peer \
             origin is cleartext http://, which would send the peer credential in \
             plaintext. Move the peer to https://, or set \
             {ALLOW_CLEARTEXT_REPLICATION_PEERS_ENV}=1 to keep the cleartext peer."
        ))
    }

    /// Persist runtime membership through the canonical `{data_dir}/node.json` owner.
    pub fn persist_peers(&self, data_dir: &Path, peers: Vec<PeerConfig>) -> Result<(), String> {
        let node_json = data_dir.join(NODE_CONFIG_FILE);
        let document = self.peer_document_with_replacement(&node_json, peers)?;
        Self::replace_node_json_atomically(&node_json, &document)
    }

    pub fn has_replication_intent(&self) -> bool {
        !self.peers.is_empty() || self.bootstrap_peer.is_some() || self.advertise_addr.is_some()
    }

    /// Read `{data_dir}/node.json` if it exists, returning `None` on read or parse errors.
    fn load_from_file(node_json: &Path) -> Option<Self> {
        if !node_json.exists() {
            return None;
        }

        let content = match std::fs::read_to_string(node_json) {
            Ok(content) => content,
            Err(error) => {
                tracing::error!("Failed to read node.json: {}, using defaults", error);
                return None;
            }
        };

        match serde_json::from_str::<NodeConfig>(&content) {
            Ok(mut config) => {
                config.advertise_addr = config
                    .advertise_addr
                    .as_deref()
                    .and_then(Self::normalize_peer_addr);
                config.peers = config
                    .peers
                    .into_iter()
                    .filter_map(|peer| {
                        Self::normalize_peer_addr(&peer.addr).map(|normalized_addr| PeerConfig {
                            node_id: peer.node_id.trim().to_string(),
                            addr: normalized_addr,
                        })
                    })
                    .filter(|peer| !peer.node_id.is_empty())
                    .collect();
                Some(config)
            }
            Err(error) => {
                tracing::error!("Failed to parse node.json: {}, using defaults", error);
                None
            }
        }
    }

    fn peer_document_with_replacement(
        &self,
        node_json: &Path,
        peers: Vec<PeerConfig>,
    ) -> Result<serde_json::Value, String> {
        if !node_json.exists() {
            let mut config = self.clone();
            config.peers = peers;
            return serde_json::to_value(config).map_err(|error| {
                format!("failed to serialize node config for node.json: {error}")
            });
        }

        let peers_value = serde_json::to_value(peers)
            .map_err(|error| format!("failed to serialize replication peers: {error}"))?;
        Self::existing_peer_document_with_replacement(node_json, peers_value)
    }

    fn existing_peer_document_with_replacement(
        node_json: &Path,
        peers_value: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let content = std::fs::read_to_string(node_json)
            .map_err(|error| format!("failed to read {}: {error}", node_json.display()))?;
        let mut document: serde_json::Value = serde_json::from_str(&content)
            .map_err(|error| format!("failed to parse {}: {error}", node_json.display()))?;
        let Some(object) = document.as_object_mut() else {
            return Err(format!(
                "{} must contain a JSON object",
                node_json.display()
            ));
        };
        object.insert("peers".to_string(), peers_value);
        Ok(document)
    }

    fn replace_node_json_atomically(
        node_json: &Path,
        document: &serde_json::Value,
    ) -> Result<(), String> {
        let parent = node_json
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", node_json.display()))?;
        let temp_path = parent.join(format!(
            ".node.json.{}.{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));

        let result = Self::write_node_json_temp(&temp_path, document).and_then(|()| {
            std::fs::rename(&temp_path, node_json).map_err(|error| {
                format!(
                    "failed to replace {} with {}: {error}",
                    node_json.display(),
                    temp_path.display()
                )
            })
        });
        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }

    fn write_node_json_temp(temp_path: &Path, document: &serde_json::Value) -> Result<(), String> {
        use std::io::Write;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temp_path)
            .map_err(|error| format!("failed to create {}: {error}", temp_path.display()))?;
        serde_json::to_writer_pretty(&mut file, document)
            .map_err(|error| format!("failed to serialize {}: {error}", temp_path.display()))?;
        file.write_all(b"\n")
            .map_err(|error| format!("failed to finish {}: {error}", temp_path.display()))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync {}: {error}", temp_path.display()))
    }

    fn default_node_id() -> String {
        std::env::var("FLAPJACK_NODE_ID").unwrap_or_else(|_| {
            hostname::get()
                .ok()
                .and_then(|hostname| hostname.into_string().ok())
                .unwrap_or_else(|| "unknown".to_string())
        })
    }

    fn default_bind_addr() -> String {
        std::env::var("FLAPJACK_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:7700".to_string())
    }

    fn parse_env_peers() -> Vec<PeerConfig> {
        std::env::var("FLAPJACK_PEERS")
            .unwrap_or_default()
            .split(',')
            .filter_map(Self::parse_peer_entry)
            .collect()
    }

    fn parse_optional_peer_origin_env(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .and_then(|value| Self::normalize_peer_addr(&value))
    }

    /// Parse one `FLAPJACK_PEERS` entry in `node_id=addr` form, ignoring malformed values.
    fn parse_peer_entry(peer: &str) -> Option<PeerConfig> {
        if peer.trim().is_empty() {
            return None;
        }

        let mut parts = peer.splitn(2, '=');
        let peer_id = parts.next()?.trim().to_string();
        let addr = parts.next()?.trim();
        if peer_id.is_empty() || addr.is_empty() {
            return None;
        }
        let normalized_addr = Self::normalize_peer_addr(addr)?;

        Some(PeerConfig {
            node_id: peer_id,
            addr: normalized_addr,
        })
    }

    /// Validate and canonicalize a replication peer URL, rejecting local-host
    /// and metadata-style destinations that are unsafe for server-side fan-out.
    pub fn normalize_peer_addr(addr: &str) -> Option<String> {
        let parsed = reqwest::Url::parse(addr).ok()?;
        match parsed.scheme() {
            "http" | "https" => {}
            _ => return None,
        }
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !parsed.path().trim_matches('/').is_empty()
        {
            return None;
        }

        let host = parsed.host_str()?;
        if host.eq_ignore_ascii_case("localhost") {
            return None;
        }
        // Literal-IP hosts are checked directly. Non-IP hosts (real hostnames AND
        // non-canonical numeric forms like `2130706433` that the IP parser rejects
        // but the OS resolver maps to loopback/link-local) are resolved and every
        // resolved address is checked. Operator-configured replication peers may
        // legitimately live on RFC1918/ULA private networks, so only local-host /
        // metadata-style destinations are blocked here.
        if let Some(ip) = Self::parse_literal_peer_ip(host) {
            if Self::is_unsafe_peer_ip(&ip) {
                return None;
            }
        } else if Self::first_blocked_resolved_ip(host, parsed.port_or_known_default()).is_some() {
            return None;
        }

        Some(parsed.origin().ascii_serialization())
    }

    fn parse_literal_peer_ip(host: &str) -> Option<IpAddr> {
        host.parse::<IpAddr>().ok().or_else(|| {
            host.strip_prefix('[')?
                .strip_suffix(']')?
                .parse::<IpAddr>()
                .ok()
        })
    }

    /// Resolve a non-literal peer host and return the first resolved address that
    /// is unsafe for peer fan-out, or None. Resolution failure returns None
    /// (peer addresses commonly use docker/k8s service names that only resolve at
    /// connect time), so config intake must not require live DNS.
    fn first_blocked_resolved_ip(host: &str, port: Option<u16>) -> Option<IpAddr> {
        use std::net::ToSocketAddrs;
        (host, port.unwrap_or(0))
            .to_socket_addrs()
            .ok()?
            .map(|sa| sa.ip())
            .find(Self::is_unsafe_peer_ip)
    }

    /// TODO: Document NodeConfig.is_unsafe_peer_ip.
    fn is_unsafe_peer_ip(ip: &IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_link_local() || v4.is_broadcast() || v4.is_unspecified()
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unicast_link_local()
                    || v6.to_ipv4_mapped().is_some_and(|v4| {
                        v4.is_loopback()
                            || v4.is_link_local()
                            || v4.is_broadcast()
                            || v4.is_unspecified()
                    })
            }
        }
    }

    fn log_default_source(config: &NodeConfig) {
        if config.bootstrap_peer.is_some() {
            tracing::info!(
                "No node.json found, bootstrap join configured: node_id={}",
                config.node_id
            );
        } else if config.peers.is_empty() {
            tracing::info!(
                "No node.json found, running in standalone mode: node_id={}",
                config.node_id
            );
        } else {
            tracing::info!(
                "No node.json found, loaded {} peer(s) from FLAPJACK_PEERS: node_id={}",
                config.peers.len(),
                config.node_id
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_load_or_default_no_file() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();

        // Ensure no env var pollution from other tests
        std::env::remove_var("FLAPJACK_PEERS");
        std::env::remove_var("FLAPJACK_NODE_ID");

        let config = NodeConfig::load_or_default(temp_dir.path());

        // Should use defaults
        assert_eq!(config.peers.len(), 0);
        assert!(!config.node_id.is_empty());
    }

    /// Verify that a well-formed `node.json` file is parsed and its node ID, bind address, and peer list are returned verbatim.
    #[test]
    fn test_load_or_default_valid_file() {
        // node.json here carries a cleartext peer, so a sibling test's credential
        // env can flip the transport policy mid-load unless we serialize.
        let _guard = ENV_MUTEX.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let node_json_path = temp_dir.path().join("node.json");

        let config_str = r#"{
            "node_id": "test-node",
            "bind_addr": "0.0.0.0:7700",
            "peers": [
                {"node_id": "peer-1", "addr": "http://peer1:7700"}
            ]
        }"#;

        let mut file = std::fs::File::create(&node_json_path).unwrap();
        file.write_all(config_str.as_bytes()).unwrap();

        let config = NodeConfig::load_or_default(temp_dir.path());

        assert_eq!(config.node_id, "test-node");
        assert_eq!(config.bind_addr, "0.0.0.0:7700");
        assert_eq!(config.peers.len(), 1);
        assert_eq!(config.peers[0].node_id, "peer-1");
        assert_eq!(config.peers[0].addr, "http://peer1:7700");
    }

    /// Verify that a malformed `node.json` file is gracefully ignored, falling back to standalone defaults with no peers.
    #[test]
    fn test_load_or_default_invalid_json() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let node_json_path = temp_dir.path().join("node.json");

        // Ensure no env var pollution from other tests
        std::env::remove_var("FLAPJACK_PEERS");
        std::env::remove_var("FLAPJACK_NODE_ID");

        let mut file = std::fs::File::create(&node_json_path).unwrap();
        file.write_all(b"invalid json").unwrap();

        let config = NodeConfig::load_or_default(temp_dir.path());

        // Should fall back to defaults
        assert_eq!(config.peers.len(), 0);
    }

    /// Verify that `FLAPJACK_NODE_ID` and `FLAPJACK_PEERS` environment variables are used to construct the config when no `node.json` exists, including correct parsing of multiple comma-separated `id=addr` pairs.
    #[test]
    fn test_load_or_default_flapjack_peers_env_var() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();

        std::env::set_var("FLAPJACK_NODE_ID", "test-node-env");
        std::env::set_var(
            "FLAPJACK_PEERS",
            "node-b=http://node-b:7700,node-c=http://node-c:7701",
        );

        let config = NodeConfig::load_or_default(temp_dir.path());

        std::env::remove_var("FLAPJACK_NODE_ID");
        std::env::remove_var("FLAPJACK_PEERS");

        assert_eq!(config.node_id, "test-node-env");
        assert_eq!(config.peers.len(), 2);
        assert_eq!(config.peers[0].node_id, "node-b");
        assert_eq!(config.peers[0].addr, "http://node-b:7700");
        assert_eq!(config.peers[1].node_id, "node-c");
        assert_eq!(config.peers[1].addr, "http://node-c:7701");
    }

    /// Verify that a single-entry `FLAPJACK_PEERS` value (no trailing comma) is parsed into exactly one peer with the expected node ID and IP-based address.
    #[test]
    fn test_load_or_default_single_peer_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();

        std::env::set_var("FLAPJACK_NODE_ID", "node-a-single");
        std::env::set_var("FLAPJACK_PEERS", "node-b=http://node-b.example.com:7700");

        let config = NodeConfig::load_or_default(temp_dir.path());

        std::env::remove_var("FLAPJACK_NODE_ID");
        std::env::remove_var("FLAPJACK_PEERS");

        assert_eq!(config.peers.len(), 1);
        assert_eq!(config.peers[0].node_id, "node-b");
        assert_eq!(config.peers[0].addr, "http://node-b.example.com:7700");
    }

    #[test]
    fn test_load_or_default_empty_peers_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();

        std::env::set_var("FLAPJACK_PEERS", "");

        let config = NodeConfig::load_or_default(temp_dir.path());

        std::env::remove_var("FLAPJACK_PEERS");

        assert_eq!(config.peers.len(), 0);
    }

    /// Verify that a valid `node.json` file takes precedence over `FLAPJACK_NODE_ID` and `FLAPJACK_PEERS` environment variables set at the same time.
    #[test]
    fn test_node_json_takes_precedence_over_env() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let node_json_path = temp_dir.path().join("node.json");

        let config_str = r#"{
            "node_id": "from-json",
            "bind_addr": "0.0.0.0:7700",
            "peers": [
                {"node_id": "peer-json", "addr": "http://peer-json:7700"}
            ]
        }"#;

        let mut file = std::fs::File::create(&node_json_path).unwrap();
        file.write_all(config_str.as_bytes()).unwrap();

        std::env::set_var("FLAPJACK_NODE_ID", "from-env");
        std::env::set_var("FLAPJACK_PEERS", "peer-env=http://peer-env:7700");

        let config = NodeConfig::load_or_default(temp_dir.path());

        std::env::remove_var("FLAPJACK_NODE_ID");
        std::env::remove_var("FLAPJACK_PEERS");

        // node.json takes precedence
        assert_eq!(config.node_id, "from-json");
        assert_eq!(config.peers.len(), 1);
        assert_eq!(config.peers[0].node_id, "peer-json");
    }

    #[test]
    fn persist_peers_replaces_only_peers_in_existing_node_json() {
        let temp_dir = tempfile::tempdir().unwrap();
        let node_json_path = temp_dir.path().join("node.json");
        let original = serde_json::json!({
            "node_id": "node-a",
            "bind_addr": "0.0.0.0:7700",
            "peers": [
                {"node_id": "node-b", "addr": "http://node-b:7700"}
            ],
            "operator": {
                "role": "primary",
                "limits": {"replicas": 3}
            }
        });
        std::fs::write(
            &node_json_path,
            serde_json::to_vec_pretty(&original).unwrap(),
        )
        .unwrap();

        let config = NodeConfig::load_or_default(temp_dir.path());
        config
            .persist_peers(
                temp_dir.path(),
                vec![PeerConfig {
                    node_id: "node-c".to_string(),
                    addr: "http://node-c:7700".to_string(),
                }],
            )
            .expect("peer persistence should succeed");

        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&node_json_path).unwrap()).unwrap();
        assert_eq!(persisted["node_id"], original["node_id"]);
        assert_eq!(persisted["bind_addr"], original["bind_addr"]);
        assert_eq!(persisted["operator"], original["operator"]);
        assert_eq!(
            persisted["peers"],
            serde_json::json!([
                {"node_id": "node-c", "addr": "http://node-c:7700"}
            ])
        );
    }

    #[test]
    fn persist_peers_creates_loadable_node_json_from_active_config_when_missing() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();

        std::env::set_var("FLAPJACK_NODE_ID", "node-env");
        std::env::set_var("FLAPJACK_BIND_ADDR", "0.0.0.0:9900");
        std::env::set_var("FLAPJACK_PEERS", "node-b=http://node-b:7700");
        let config = NodeConfig::load_or_default(temp_dir.path());
        std::env::remove_var("FLAPJACK_NODE_ID");
        std::env::remove_var("FLAPJACK_BIND_ADDR");
        std::env::remove_var("FLAPJACK_PEERS");

        config
            .persist_peers(
                temp_dir.path(),
                vec![
                    PeerConfig {
                        node_id: "node-b".to_string(),
                        addr: "http://node-b:7700".to_string(),
                    },
                    PeerConfig {
                        node_id: "node-c".to_string(),
                        addr: "http://node-c:7700".to_string(),
                    },
                ],
            )
            .expect("missing node.json should be created");

        let reloaded = NodeConfig::load_or_default(temp_dir.path());
        assert_eq!(reloaded.node_id, "node-env");
        assert_eq!(reloaded.bind_addr, "0.0.0.0:9900");
        assert_eq!(reloaded.peers.len(), 2);
        assert_eq!(reloaded.peers[0].node_id, "node-b");
        assert_eq!(reloaded.peers[0].addr, "http://node-b:7700");
        assert_eq!(reloaded.peers[1].node_id, "node-c");
        assert_eq!(reloaded.peers[1].addr, "http://node-c:7700");
    }

    /// TODO: Document a10_env_peer_parser_rejects_unsafe_or_malformed_peer_addresses.
    #[test]
    fn a10_env_peer_parser_rejects_unsafe_or_malformed_peer_addresses() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();

        std::env::set_var("FLAPJACK_NODE_ID", "node-a-a10");
        // Covers every blocked class. RFC1918 peers remain allowed because
        // replication/analytics fan-out is explicitly configured for internal
        // cluster destinations; only metadata/local-host style targets are unsafe.
        // Classes blocked here:
        //   - link-local metadata, loopback
        //   - non-http scheme
        //   - numeric-form `2130706433` (url crate canonicalizes to 127.0.0.1,
        //     caught by the literal-IP check; asserts canonicalization holds)
        //   - `localhost.` trailing-dot FQDN (kept as a registered name by the url
        //     crate, slips past literal + "localhost" checks; only resolve-and-check
        //     catches it — the hostname-resolution SSRF bypass the fix closes)
        std::env::set_var(
            "FLAPJACK_PEERS",
            "meta=http://169.254.169.254,loop=http://127.0.0.1:7700,priv=http://10.0.0.1:7700,bad=file:///tmp/x,numeric=http://2130706433:7700,lhdot=http://localhost.:7700,ok=https://peer-a.example.com:7700",
        );

        let config = NodeConfig::load_or_default(temp_dir.path());

        std::env::remove_var("FLAPJACK_NODE_ID");
        std::env::remove_var("FLAPJACK_PEERS");

        assert_eq!(config.peers.len(), 2);
        assert_eq!(config.peers[0].node_id, "priv");
        assert_eq!(config.peers[0].addr, "http://10.0.0.1:7700");
        assert_eq!(config.peers[1].node_id, "ok");
        assert_eq!(config.peers[1].addr, "https://peer-a.example.com:7700");
    }

    #[test]
    fn cleartext_peer_with_configured_credential_is_refused_by_default() {
        let (config, output) = {
            let _guard = ENV_MUTEX.lock().unwrap();
            let temp_dir = tempfile::tempdir().unwrap();
            let _peer_key =
                EnvVarRestoreGuard::set("FLAPJACK_REPLICATION_API_KEY", "stage-2-peer-secret");
            let _allow_cleartext =
                EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS");
            let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
            let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");
            let _peers = EnvVarRestoreGuard::set(
                "FLAPJACK_PEERS",
                "cleartext=http://peer-clear.example.com:7700",
            );

            capture_config_logs(|| NodeConfig::load_or_default(temp_dir.path()))
        };

        assert!(
            config.peers.is_empty(),
            "credentialed cleartext peer must be refused by default, got: {:?}",
            config.peers
        );
        assert!(
            output.contains("http://peer-clear.example.com:7700") || output.contains("cleartext"),
            "refusal must name the offending peer, got: {output:?}"
        );
        assert!(
            output.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1"),
            "refusal must name the cleartext escape variable, got: {output:?}"
        );
    }

    #[test]
    fn cleartext_peer_escape_repermits_and_warns() {
        let (config, output) = {
            let _guard = ENV_MUTEX.lock().unwrap();
            let temp_dir = tempfile::tempdir().unwrap();
            let _peer_key =
                EnvVarRestoreGuard::set("FLAPJACK_REPLICATION_API_KEY", "stage-2-peer-secret");
            let _allow_cleartext =
                EnvVarRestoreGuard::set("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS", "1");
            let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
            let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");
            let _peers = EnvVarRestoreGuard::set(
                "FLAPJACK_PEERS",
                "cleartext=http://peer-clear.example.com:7700",
            );

            capture_config_logs(|| NodeConfig::load_or_default(temp_dir.path()))
        };

        assert_eq!(config.peers.len(), 1);
        assert_eq!(config.peers[0].node_id, "cleartext");
        assert_eq!(config.peers[0].addr, "http://peer-clear.example.com:7700");
        assert!(
            output.contains("WARNING") || output.contains("warning"),
            "cleartext escape use must emit a loud warning, got: {output:?}"
        );
        assert!(
            output.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1"),
            "warning must name the cleartext escape variable, got: {output:?}"
        );
        assert!(
            output.contains("http://peer-clear.example.com:7700") || output.contains("cleartext"),
            "warning must name the cleartext peer, got: {output:?}"
        );
    }

    /// TODO: Document a10_node_json_filters_unsafe_peer_addresses.
    #[test]
    fn a10_node_json_filters_unsafe_peer_addresses() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let node_json_path = temp_dir.path().join("node.json");

        let config_str = r#"{
                "node_id": "json-node",
                "bind_addr": "0.0.0.0:7700",
                "peers": [
                    {"node_id": "unsafe-meta", "addr": "http://169.254.169.254"},
                    {"node_id": "unsafe-scheme", "addr": "file:///tmp/peer"},
                    {"node_id": "private-ok", "addr": "http://10.0.0.2:7700"},
                    {"node_id": "safe", "addr": "http://peer-safe.example.com:7700"}
                ]
            }"#;

        let mut file = std::fs::File::create(&node_json_path).unwrap();
        file.write_all(config_str.as_bytes()).unwrap();

        let config = NodeConfig::load_or_default(temp_dir.path());
        assert_eq!(config.node_id, "json-node");
        assert_eq!(config.peers.len(), 2);
        assert_eq!(config.peers[0].node_id, "private-ok");
        assert_eq!(config.peers[0].addr, "http://10.0.0.2:7700");
        assert_eq!(config.peers[1].node_id, "safe");
        assert_eq!(config.peers[1].addr, "http://peer-safe.example.com:7700");
    }

    #[test]
    fn normalize_peer_addr_rejects_unsafe_bracketed_ipv6_literals() {
        assert_eq!(NodeConfig::normalize_peer_addr("http://[::1]:7700"), None);
        assert_eq!(
            NodeConfig::normalize_peer_addr("http://[fe80::1]:7700"),
            None
        );
        assert_eq!(
            NodeConfig::normalize_peer_addr("http://[::ffff:127.0.0.1]:7700"),
            None
        );
        assert_eq!(
            NodeConfig::normalize_peer_addr("http://[fd00::1]:7700"),
            Some("http://[fd00::1]:7700".to_string())
        );
    }

    /// TODO: Document normalize_peer_addr_rejects_non_origin_urls.
    #[test]
    fn normalize_peer_addr_rejects_non_origin_urls() {
        for candidate in [
            "http://user:secret@10.0.0.1:7700",
            "http://10.0.0.1:7700/internal/status",
            "http://10.0.0.1:7700?mode=debug",
            "http://10.0.0.1:7700#frag",
        ] {
            assert_eq!(
                NodeConfig::normalize_peer_addr(candidate),
                None,
                "candidate should be rejected: {candidate}"
            );
        }

        assert_eq!(
            NodeConfig::normalize_peer_addr("http://10.0.0.1:7700///"),
            Some("http://10.0.0.1:7700".to_string())
        );
    }
}

#[cfg(test)]
#[path = "config_bootstrap_tests.rs"]
mod bootstrap_tests;
