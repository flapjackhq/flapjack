use super::*;

#[test]
fn bootstrap_peer_env_is_normalized_as_a_safe_origin() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    std::env::remove_var("FLAPJACK_PEERS");
    std::env::set_var(
        "FLAPJACK_BOOTSTRAP_PEER",
        "https://bootstrap.example.com:443///",
    );

    let config = NodeConfig::load_or_default(temp_dir.path());

    std::env::remove_var("FLAPJACK_BOOTSTRAP_PEER");
    assert_eq!(
        config.bootstrap_peer.as_deref(),
        Some("https://bootstrap.example.com")
    );
    assert!(config.peers.is_empty());
}

#[test]
fn bootstrap_configuration_has_replication_intent() {
    let config = NodeConfig {
        node_id: "joiner-a".to_string(),
        bind_addr: "0.0.0.0:7700".to_string(),
        advertise_addr: Some("http://joiner-a.example.com:7700".to_string()),
        peers: Vec::new(),
        bootstrap_peer: Some("http://bootstrap.example.com:7700".to_string()),
    };

    assert!(config.has_replication_intent());
}

#[test]
fn advertised_seed_configuration_has_replication_intent() {
    let config = NodeConfig {
        node_id: "seed-a".to_string(),
        bind_addr: "0.0.0.0:7700".to_string(),
        advertise_addr: Some("http://seed-a.example.com:7700".to_string()),
        peers: Vec::new(),
        bootstrap_peer: None,
    };

    assert!(config.has_replication_intent());
}

#[test]
fn bootstrap_peer_env_rejects_non_origin_and_unsafe_urls() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    std::env::remove_var("FLAPJACK_PEERS");

    for candidate in [
        "http://127.0.0.1:7700",
        "http://169.254.169.254:7700",
        "ftp://bootstrap.example.com:7700",
        "http://bootstrap.example.com:7700/internal/status",
        "0.0.0.0:7700",
    ] {
        std::env::set_var("FLAPJACK_BOOTSTRAP_PEER", candidate);
        let config = NodeConfig::load_or_default(temp_dir.path());
        assert_eq!(
            config.bootstrap_peer, None,
            "unsafe bootstrap candidate should be rejected: {candidate}"
        );
    }

    std::env::remove_var("FLAPJACK_BOOTSTRAP_PEER");
}

#[test]
fn advertise_addr_is_optional_in_existing_node_json() {
    let temp_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        temp_dir.path().join("node.json"),
        r#"{
            "node_id": "node-a",
            "bind_addr": "0.0.0.0:7700",
            "peers": []
        }"#,
    )
    .unwrap();

    let config = NodeConfig::load_or_default(temp_dir.path());

    assert_eq!(config.advertise_addr, None);
}

#[test]
fn advertise_addr_env_is_normalized_by_peer_origin_owner() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    std::env::remove_var("FLAPJACK_PEERS");
    std::env::remove_var("FLAPJACK_BOOTSTRAP_PEER");
    std::env::set_var(
        "FLAPJACK_ADVERTISE_ADDR",
        "http://node-a.example.com:7700///",
    );

    let config = NodeConfig::load_or_default(temp_dir.path());

    std::env::remove_var("FLAPJACK_ADVERTISE_ADDR");
    assert_eq!(
        config.advertise_addr.as_deref(),
        Some("http://node-a.example.com:7700")
    );
    assert!(config.has_replication_intent());
}

#[test]
fn advertise_addr_env_rejects_wildcard_listen_only_and_unsafe_values() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    std::env::remove_var("FLAPJACK_PEERS");
    std::env::remove_var("FLAPJACK_BOOTSTRAP_PEER");

    for candidate in [
        "0.0.0.0:7700",
        "http://0.0.0.0:7700",
        "127.0.0.1:7700",
        "http://127.0.0.1:7700",
        "http://node-a.example.com:7700/internal/status",
    ] {
        std::env::set_var("FLAPJACK_ADVERTISE_ADDR", candidate);
        let config = NodeConfig::load_or_default(temp_dir.path());
        assert_eq!(
            config.advertise_addr, None,
            "unsafe advertised origin should be rejected: {candidate}"
        );
    }

    std::env::remove_var("FLAPJACK_ADVERTISE_ADDR");
}

#[test]
fn node_json_takes_precedence_over_all_topology_environment() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        temp_dir.path().join("node.json"),
        r#"{
            "node_id": "json-node",
            "bind_addr": "0.0.0.0:7700",
            "advertise_addr": "https://json-node.example.com:7700",
            "peers": [{"node_id":"json-peer","addr":"http://json-peer.example.com:7700"}]
        }"#,
    )
    .unwrap();
    std::env::set_var(
        "FLAPJACK_PEERS",
        "env-peer=http://env-peer.example.com:7700",
    );
    std::env::set_var(
        "FLAPJACK_BOOTSTRAP_PEER",
        "http://bootstrap.example.com:7700",
    );
    std::env::set_var(
        "FLAPJACK_ADVERTISE_ADDR",
        "http://env-node.example.com:7700",
    );

    let config = NodeConfig::load_or_default(temp_dir.path());

    std::env::remove_var("FLAPJACK_PEERS");
    std::env::remove_var("FLAPJACK_BOOTSTRAP_PEER");
    std::env::remove_var("FLAPJACK_ADVERTISE_ADDR");
    assert_eq!(config.peers.len(), 1);
    assert_eq!(config.peers[0].node_id, "json-peer");
    assert_eq!(
        config.advertise_addr.as_deref(),
        Some("https://json-node.example.com:7700")
    );
    assert_eq!(config.bootstrap_peer, None);
}

#[test]
fn static_peer_topology_takes_precedence_over_bootstrap_peer() {
    let _guard = ENV_MUTEX.lock().unwrap();
    let temp_dir = tempfile::tempdir().unwrap();
    std::env::set_var(
        "FLAPJACK_PEERS",
        "static-peer=http://static-peer.example.com:7700",
    );
    std::env::set_var(
        "FLAPJACK_BOOTSTRAP_PEER",
        "http://bootstrap.example.com:7700",
    );

    let config = NodeConfig::load_or_default(temp_dir.path());

    std::env::remove_var("FLAPJACK_PEERS");
    std::env::remove_var("FLAPJACK_BOOTSTRAP_PEER");
    assert_eq!(config.peers.len(), 1);
    assert_eq!(config.peers[0].node_id, "static-peer");
    assert_eq!(config.bootstrap_peer, None);
}
