use super::{
    bootstrap_join_with_client, build_bootstrap_http_client, merge_bootstrap_membership,
    resolve_advertised_origin,
};
use crate::startup::{acquire_data_dir_process_lock, ServerConfig};
use crate::test_helpers::{EnvVarRestoreGuard, ENV_MUTEX};
use flapjack::index::oplog::OpLogEntry;
use flapjack_replication::config::{NodeConfig, PeerConfig};
use flapjack_replication::manager::ReplicationManager;
use flapjack_replication::peer::REPLICATION_PEER_APPLICATION_ID;
use serde_json::Value;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn bootstrap_node_config(bootstrap_peer: String) -> NodeConfig {
    NodeConfig {
        node_id: "joiner-a".to_string(),
        bind_addr: "0.0.0.0:7700".to_string(),
        advertise_addr: Some("http://joiner-a.example.com:7700".to_string()),
        peers: Vec::new(),
        bootstrap_peer: Some(bootstrap_peer),
    }
}

fn server_config_for_data_dir(
    data_dir: &tempfile::TempDir,
    replication_api_key_env: Option<String>,
) -> ServerConfig {
    ServerConfig {
        env_mode: "development".to_string(),
        no_auth: false,
        disable_dashboard: false,
        allow_no_auth_public_bind: false,
        admin_key_env: Some("admin-secret".to_string()),
        replication_api_key_env,
        data_dir: data_dir.path().display().to_string(),
        bind_addr: "127.0.0.1:0".to_string(),
        tls_paths: None,
        node_config: NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "127.0.0.1:0".to_string(),
            advertise_addr: None,
            peers: Vec::new(),
            bootstrap_peer: None,
        },
        _data_dir_lock: acquire_data_dir_process_lock(data_dir.path()).unwrap(),
    }
}

fn node_config_with_peer(peer_url: String) -> NodeConfig {
    NodeConfig {
        node_id: "node-a".to_string(),
        bind_addr: "127.0.0.1:0".to_string(),
        advertise_addr: None,
        peers: vec![PeerConfig {
            node_id: "node-b".to_string(),
            addr: peer_url,
        }],
        bootstrap_peer: None,
    }
}

fn bootstrap_test_op(seq: u64) -> OpLogEntry {
    OpLogEntry {
        seq,
        timestamp_ms: seq,
        node_id: "node-a".to_string(),
        tenant_id: "tenant-red".to_string(),
        op_type: "upsert".to_string(),
        payload: serde_json::json!({
            "objectID": format!("doc-{seq}"),
            "body": {"_id": format!("doc-{seq}")}
        }),
    }
}

async fn read_http_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0u8; 2048];
        let read =
            tokio::time::timeout(tokio::time::Duration::from_secs(3), socket.read(&mut chunk))
                .await
                .expect("fake bootstrap request read should not time out")
                .expect("fake bootstrap request read should succeed");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let header_text = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = header_text
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if bytes.len() >= header_end + 4 + content_length {
            break;
        }
    }
    String::from_utf8(bytes).expect("reqwest should emit UTF-8 test requests")
}

async fn spawn_replicate_peer() -> (String, tokio::task::JoinHandle<String>) {
    let bind_result = TcpListener::bind("127.0.0.1:0").await;
    assert!(
        bind_result.is_ok(),
        "fake replication listener must bind before the request is awaited"
    );
    let listener = bind_result.unwrap();
    let listener_addr = listener.local_addr().unwrap();
    let body = serde_json::json!({
        "tenant_id": "tenant-red",
        "acked_seq": 1
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let handle = tokio::spawn(async move {
        let (mut socket, _) =
            tokio::time::timeout(tokio::time::Duration::from_secs(3), listener.accept())
                .await
                .expect("fake replication peer should receive expected request")
                .expect("fake replication peer accept should succeed");
        let request = read_http_request(&mut socket).await;
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
        request
    });
    (format!("http://{}", listener_addr), handle)
}

async fn spawn_fake_bootstrap(
    responses: Vec<(u16, String)>,
) -> (
    String,
    reqwest::Client,
    tokio::task::JoinHandle<Vec<String>>,
) {
    let bind_result = TcpListener::bind("127.0.0.1:0").await;
    assert!(
        bind_result.is_ok(),
        "fake bootstrap listener must bind before the request is awaited"
    );
    let listener = bind_result.unwrap();
    let listener_addr = listener.local_addr().unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .resolve(
            "bootstrap.test",
            SocketAddr::from(([127, 0, 0, 1], listener_addr.port())),
        )
        .build()
        .unwrap();
    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut socket, _) =
                tokio::time::timeout(tokio::time::Duration::from_secs(3), listener.accept())
                    .await
                    .expect("fake bootstrap should receive expected request")
                    .expect("fake bootstrap accept should succeed");
            requests.push(read_http_request(&mut socket).await);
            let response = format!(
                "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
        requests
    });
    (
        format!("http://bootstrap.test:{}", listener_addr.port()),
        client,
        handle,
    )
}

#[tokio::test]
async fn serve_startup_uses_configured_peer_credential_for_outbound_replication() {
    let data_dir = tempfile::tempdir().unwrap();
    let (peer_url, peer_handle) = spawn_replicate_peer().await;
    let server_config =
        server_config_for_data_dir(&data_dir, Some("replication-secret".to_string()));
    let infrastructure = crate::server::initialize_server_infrastructure(
        &server_config,
        data_dir.path(),
        Some("admin-secret".to_string()),
        node_config_with_peer(peer_url),
    )
    .await
    .unwrap();
    let replication_manager = infrastructure
        .replication_manager
        .expect("peer topology should initialize replication manager");

    replication_manager
        .replicate_ops_to_peer("tenant-red", "node-b", vec![bootstrap_test_op(1)])
        .await
        .unwrap();

    let request = peer_handle.await.unwrap();
    let lower = request.to_ascii_lowercase();
    assert!(lower.contains("x-algolia-api-key: replication-secret"));
    assert!(!lower.contains("x-algolia-api-key: admin-secret"));
    assert!(lower.contains(&format!(
        "x-algolia-application-id: {REPLICATION_PEER_APPLICATION_ID}"
    )));
}

#[tokio::test]
async fn serve_startup_without_peer_key_does_not_send_admin_key_to_peers() {
    let data_dir = tempfile::tempdir().unwrap();
    let (peer_url, peer_handle) = spawn_replicate_peer().await;
    let server_config = server_config_for_data_dir(&data_dir, None);
    let runtime_admin_key = "runtime-admin-secret";
    let infrastructure = crate::server::initialize_server_infrastructure(
        &server_config,
        data_dir.path(),
        Some(runtime_admin_key.to_string()),
        node_config_with_peer(peer_url),
    )
    .await
    .unwrap();
    let replication_manager = infrastructure
        .replication_manager
        .expect("peer topology should initialize replication manager");

    replication_manager
        .replicate_ops_to_peer("tenant-red", "node-b", vec![bootstrap_test_op(1)])
        .await
        .unwrap();

    let request = peer_handle.await.unwrap();
    let lower = request.to_ascii_lowercase();
    assert!(
        !lower.contains("x-algolia-api-key:"),
        "explicit unauthenticated mode must not substitute the admin key: {request}"
    );
    assert!(
        !lower.contains("x-algolia-application-id:"),
        "an unauthenticated peer request must not claim peer identity: {request}"
    );
}

#[test]
fn advertised_origin_prefers_config_and_rejects_unsafe_bind_fallback() {
    let explicit = bootstrap_node_config("http://bootstrap.example.com:7700".to_string());
    assert_eq!(
        resolve_advertised_origin(&explicit).unwrap(),
        "http://joiner-a.example.com:7700"
    );

    let mut safe_fallback = explicit.clone();
    safe_fallback.advertise_addr = None;
    safe_fallback.bind_addr = "10.0.0.8:7700".to_string();
    assert_eq!(
        resolve_advertised_origin(&safe_fallback).unwrap(),
        "http://10.0.0.8:7700"
    );

    safe_fallback.bind_addr = "0.0.0.0:7700".to_string();
    assert!(resolve_advertised_origin(&safe_fallback)
        .unwrap_err()
        .contains("FLAPJACK_ADVERTISE_ADDR"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Process-global env guard must span bootstrap.
async fn bootstrap_join_posts_identity_merges_status_and_persists_membership() {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let _allow_cleartext =
        EnvVarRestoreGuard::set("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS", "1");
    let status = serde_json::json!({
        "node_id": "bootstrap-a",
        "replication_enabled": true,
        "peers_total": 2,
        "peers_healthy": 0,
        "peers": [
            {
                "peer_id": "joiner-a",
                "addr": "http://joiner-a.example.com:7700",
                "status": "never_contacted",
                "last_success_secs_ago": null
            },
            {
                "peer_id": "node-c",
                "addr": "https://node-c.example.com:7700",
                "status": "healthy",
                "last_success_secs_ago": 1
            },
            {
                "peer_id": "node-c",
                "addr": "https://node-c.example.com:7700",
                "status": "healthy",
                "last_success_secs_ago": 1
            }
        ]
    });
    let legacy_status: crate::handlers::internal::ClusterStatusResponse =
        serde_json::from_value(status.clone()).unwrap();
    let crate::handlers::internal::ClusterStatusResponse::Ha(legacy_status) = legacy_status else {
        panic!("legacy HA cluster-status payload should deserialize to HA branch");
    };
    assert!(!legacy_status.autoheal_enabled);
    assert!(
        legacy_status.autoheal_peers.is_empty(),
        "legacy cluster-status payloads must not synthesize lifecycle membership"
    );
    let (bootstrap_peer, client, server) = spawn_fake_bootstrap(vec![
        (200, serde_json::json!({"ok": true}).to_string()),
        (200, status.to_string()),
    ])
    .await;
    let data_dir = tempfile::tempdir().unwrap();
    let mut config = bootstrap_node_config(bootstrap_peer.clone());
    let manager = ReplicationManager::new(
        config.clone(),
        Some("admin-secret".to_string()),
        data_dir.path().to_path_buf(),
    );

    bootstrap_join_with_client(&client, &mut config, &manager, Some("admin-secret"))
        .await
        .unwrap();

    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("POST /internal/cluster/peers HTTP/1.1"));
    assert!(requests[1].starts_with("GET /internal/cluster/status HTTP/1.1"));
    for request in &requests {
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("x-algolia-api-key: admin-secret"));
        assert!(lower.contains(&format!(
            "x-algolia-application-id: {REPLICATION_PEER_APPLICATION_ID}"
        )));
    }
    let request_body = requests[0].split("\r\n\r\n").nth(1).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(request_body).unwrap(),
        serde_json::json!({
            "node_id": "joiner-a",
            "addr": "http://joiner-a.example.com:7700"
        })
    );
    assert_eq!(manager.peer_count(), 2);
    assert_eq!(
        config
            .peers
            .iter()
            .map(|peer| (peer.node_id.as_str(), peer.addr.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("bootstrap-a", bootstrap_peer.as_str()),
            ("node-c", "https://node-c.example.com:7700")
        ]
    );
    let persisted: Value =
        serde_json::from_slice(&std::fs::read(data_dir.path().join("node.json")).unwrap()).unwrap();
    assert_eq!(
        persisted,
        serde_json::json!({
            "node_id": "joiner-a",
            "bind_addr": "0.0.0.0:7700",
            "advertise_addr": "http://joiner-a.example.com:7700",
            "peers": serde_json::to_value(&config.peers).unwrap()
        })
    );
}

#[allow(clippy::await_holding_lock)] // Process-global env guard must span bootstrap.
async fn bootstrap_error_from_responses(responses: Vec<(u16, String)>) -> String {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let _allow_cleartext =
        EnvVarRestoreGuard::set("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS", "1");
    let (bootstrap_peer, client, server) = spawn_fake_bootstrap(responses).await;
    let data_dir = tempfile::tempdir().unwrap();
    let mut config = bootstrap_node_config(bootstrap_peer);
    let manager = ReplicationManager::new(
        config.clone(),
        Some("admin-secret".to_string()),
        data_dir.path().to_path_buf(),
    );
    let error = bootstrap_join_with_client(&client, &mut config, &manager, Some("admin-secret"))
        .await
        .expect_err("bootstrap fixture should fail");
    let _ = server.await.unwrap();
    error
}

#[tokio::test]
async fn bootstrap_join_requires_admin_key() {
    let (bootstrap_peer, client, server) =
        spawn_fake_bootstrap(vec![(200, serde_json::json!({"ok": true}).to_string())]).await;
    let data_dir = tempfile::tempdir().unwrap();
    let mut config = bootstrap_node_config(bootstrap_peer);
    let manager = ReplicationManager::new(config.clone(), None, data_dir.path().to_path_buf());

    let error = bootstrap_join_with_client(&client, &mut config, &manager, None)
        .await
        .expect_err("bootstrap join without admin auth must fail");

    assert!(error.contains("admin API key"));
    server.abort();
}

#[tokio::test]
async fn bootstrap_admin_auth_does_not_follow_peer_redirect() {
    let redirect_target = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("redirect target must bind before bootstrap starts");
    let target_addr = redirect_target.local_addr().unwrap();
    let redirector = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bootstrap redirector must bind before bootstrap starts");
    let redirector_addr = redirector.local_addr().unwrap();
    let target_task = tokio::spawn(async move {
        let accepted = tokio::time::timeout(
            tokio::time::Duration::from_millis(500),
            redirect_target.accept(),
        )
        .await;
        let Ok(Ok((mut socket, _))) = accepted else {
            return None;
        };
        let request = read_http_request(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 409 Redirected\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        Some(request)
    });
    let redirect_task = tokio::spawn(async move {
        let (mut socket, _) = redirector.accept().await.unwrap();
        let _ = read_http_request(&mut socket).await;
        let response = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://{target_addr}/stolen\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let data_dir = tempfile::tempdir().unwrap();
    let mut config = bootstrap_node_config(format!("http://{redirector_addr}"));
    let manager = ReplicationManager::new(config.clone(), None, data_dir.path().to_path_buf());
    let client = build_bootstrap_http_client().unwrap();
    let error = bootstrap_join_with_client(
        &client,
        &mut config,
        &manager,
        Some("redirect-sensitive-admin-key"),
    )
    .await
    .expect_err("bootstrap redirect must be rejected");
    assert!(error.contains("307"), "unexpected bootstrap error: {error}");
    redirect_task.await.unwrap();

    assert!(
        target_task.await.unwrap().is_none(),
        "bootstrap must not forward the admin key to a redirect target"
    );
}

#[tokio::test]
async fn bootstrap_join_fails_loudly_for_rejected_add_and_invalid_status() {
    let rejected = bootstrap_error_from_responses(vec![(
        409,
        serde_json::json!({"message": "duplicate"}).to_string(),
    )])
    .await;
    assert!(rejected.contains("409"));

    let invalid = bootstrap_error_from_responses(vec![
        (200, "{}".to_string()),
        (200, "not-json".to_string()),
    ])
    .await;
    assert!(invalid.contains("invalid cluster status"));
}

#[tokio::test]
async fn bootstrap_join_fails_loudly_for_unreachable_or_self_only_peer() {
    let data_dir = tempfile::tempdir().unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .resolve("bootstrap.test", SocketAddr::from(([127, 0, 0, 1], 9)))
        .build()
        .unwrap();
    let mut config = bootstrap_node_config("http://bootstrap.test".to_string());
    let manager = ReplicationManager::new(
        config.clone(),
        Some("admin-secret".to_string()),
        data_dir.path().to_path_buf(),
    );
    let unreachable =
        bootstrap_join_with_client(&client, &mut config, &manager, Some("admin-secret"))
            .await
            .unwrap_err();
    assert!(unreachable.contains("bootstrap peer"));

    let self_only = bootstrap_error_from_responses(vec![
        (200, "{}".to_string()),
        (
            200,
            serde_json::json!({
                "node_id": "joiner-a",
                "replication_enabled": true,
                "peers": [{
                    "peer_id": "joiner-a",
                    "addr": "http://joiner-a.example.com:7700",
                    "status": "never_contacted",
                    "last_success_secs_ago": null
                }]
            })
            .to_string(),
        ),
    ])
    .await;
    assert!(self_only.contains("no remote members"));
}

#[tokio::test]
async fn bootstrap_join_rejects_conflicting_addresses_and_blank_ids() {
    let conflict = bootstrap_error_from_responses(vec![
        (200, "{}".to_string()),
        (
            200,
            serde_json::json!({
                "node_id": "bootstrap-a",
                "replication_enabled": true,
                "peers": [{
                    "peer_id": "bootstrap-a",
                    "addr": "http://different-bootstrap.example.com:7700",
                    "status": "healthy",
                    "last_success_secs_ago": 0
                }]
            })
            .to_string(),
        ),
    ])
    .await;

    assert!(conflict.contains("conflicting addresses"));

    let blank_id = bootstrap_error_from_responses(vec![
        (200, "{}".to_string()),
        (
            200,
            serde_json::json!({
                "node_id": "bootstrap-a",
                "replication_enabled": true,
                "peers": [{
                    "peer_id": "  ",
                    "addr": "http://node-c.example.com:7700",
                    "status": "healthy",
                    "last_success_secs_ago": 0
                }]
            })
            .to_string(),
        ),
    ])
    .await;

    assert!(blank_id.contains("blank node_id"));
}

#[test]
fn bootstrap_membership_rejects_cleartext_peer_without_replication_key() {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");
    let _allow_cleartext = EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS");
    let config = bootstrap_node_config("https://bootstrap.example.com:7700".to_string());
    let status = serde_json::from_value(serde_json::json!({
        "node_id": "bootstrap-a",
        "replication_enabled": true,
        "peers": [{
            "peer_id": "node-c",
            "addr": "http://node-c.example.com:7700",
            "status": "healthy",
            "last_success_secs_ago": 0
        }]
    }))
    .expect("cluster status fixture should deserialize");

    let error = merge_bootstrap_membership(&config, "https://bootstrap.example.com:7700", status)
        .expect_err("authenticated analytics must reject learned cleartext membership");

    assert!(
        error.contains("node-c"),
        "error must name the peer: {error}"
    );
    assert!(
        error.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1"),
        "error must name the explicit escape hatch: {error}"
    );
    assert!(
        error.contains("analytics") && error.contains("caller API keys"),
        "error must identify the credential-bearing analytics path: {error}"
    );
}

#[test]
fn replication_initialization_distinguishes_standalone_and_bootstrap_intent() {
    let data_dir = tempfile::tempdir().unwrap();
    let standalone = NodeConfig {
        node_id: "standalone-a".to_string(),
        bind_addr: "0.0.0.0:7700".to_string(),
        advertise_addr: None,
        peers: Vec::new(),
        bootstrap_peer: None,
    };
    assert!(super::initialize_replication(&standalone, None, data_dir.path()).is_none());

    let advertised_seed = NodeConfig {
        node_id: "seed-a".to_string(),
        bind_addr: "0.0.0.0:7700".to_string(),
        advertise_addr: Some("http://seed-a.example.com:7700".to_string()),
        peers: Vec::new(),
        bootstrap_peer: None,
    };
    let manager = super::initialize_replication(&advertised_seed, None, data_dir.path())
        .expect("advertised seed should initialize an empty replication manager");
    assert_eq!(manager.peer_count(), 0);
    assert_eq!(manager.node_id(), "seed-a");

    let bootstrap = bootstrap_node_config("http://bootstrap.example.com:7700".to_string());
    let manager = super::initialize_replication(&bootstrap, None, data_dir.path())
        .expect("bootstrap intent should initialize an empty replication manager");
    assert_eq!(manager.peer_count(), 0);
    assert_eq!(manager.node_id(), "joiner-a");
}
