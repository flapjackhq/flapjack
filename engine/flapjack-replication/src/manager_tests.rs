use super::super::autoheal::{
    AutohealCycle, AutohealJournal, EvictionDecision, ProbeOutcome,
    DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD,
};
use super::super::config::{NodeConfig, PeerConfig};
use super::*;
use std::path::Path;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Barrier};

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

fn new_test_manager(config: NodeConfig, peer_credential: Option<String>) -> TestReplicationManager {
    let data_dir = tempfile::tempdir().unwrap();
    let manager = ReplicationManager::new(config, peer_credential, data_dir.path().to_path_buf());
    TestReplicationManager {
        _data_dir: data_dir,
        manager,
    }
}

fn new_test_manager_in(
    data_dir: &Path,
    config: NodeConfig,
    peer_credential: Option<String>,
) -> Arc<ReplicationManager> {
    ReplicationManager::new(config, peer_credential, data_dir.to_path_buf())
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
    serde_json::to_writer_pretty(node_json, &config).expect("node.json fixture should serialize");
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

fn autoheal_manager_config(peers: Vec<PeerConfig>) -> NodeConfig {
    NodeConfig {
        node_id: "node-a".to_string(),
        bind_addr: "0.0.0.0:7700".to_string(),
        advertise_addr: None,
        bootstrap_peer: None,
        peers,
    }
}

fn peer_config(node_id: &str) -> PeerConfig {
    PeerConfig {
        node_id: node_id.to_string(),
        addr: format!("http://{node_id}:7700"),
    }
}

fn read_autoheal_events(data_dir: &Path) -> Vec<serde_json::Value> {
    let content = std::fs::read_to_string(AutohealJournal::path_in_data_dir(data_dir))
        .expect("auto-heal journal should be readable");
    content
        .lines()
        .map(|line| serde_json::from_str(line).expect("journal line should be valid JSON"))
        .collect()
}

fn autoheal_outcomes(outcomes: &[(&str, ProbeOutcome)]) -> BTreeMap<String, ProbeOutcome> {
    outcomes
        .iter()
        .map(|(peer_id, outcome)| ((*peer_id).to_string(), outcome.clone()))
        .collect()
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

/// TODO: Document spawn_single_response_peer.
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

/// TODO: Document spawn_single_tenant_list_peer.
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

/// TODO: Document spawn_replicate_peer.
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

async fn spawn_observed_status_peer() -> (String, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_seen_tx, request_seen_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request_buf = [0u8; 1024];
        let _ = socket.read(&mut request_buf).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let _ = socket.shutdown().await;
        let _ = request_seen_tx.send(());
    });
    (format!("http://{}", addr), request_seen_rx)
}

async fn spawn_barrier_status_peer(
    accepted_barrier: Arc<Barrier>,
    release_barrier: Arc<Barrier>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request_buf = [0u8; 1024];
        let _ = socket.read(&mut request_buf).await;
        accepted_barrier.wait().await;
        release_barrier.wait().await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let _ = socket.shutdown().await;
    });
    format!("http://{}", addr)
}

/// TODO: Document spawn_barrier_replicate_peer.
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

#[tokio::test]
async fn replication_pass_does_not_finish_before_every_spawned_peer_task() {
    let accepted = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (peer_url, peer_handle) =
        spawn_barrier_replicate_peer(1, Arc::clone(&accepted), Arc::clone(&release)).await;
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
    let replication_manager = Arc::clone(&manager.manager);
    let pass = tokio::spawn(async move {
        replication_manager
            .replicate_ops("tenant-red", vec![mutable_peer_test_op(1)])
            .await;
    });

    accepted.wait().await;
    assert!(
        !pass.is_finished(),
        "the complete replication pass must own every spawned peer task until delivery settles"
    );
    release.wait().await;
    pass.await.unwrap();
    peer_handle.await.unwrap();
}

/// TODO: Document wait_for_acked_seq.
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

/// TODO: Document add_peer_returns_receipt_from_mutation_snapshot.
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

/// TODO: Document remove_peer_returns_receipt_from_mutation_snapshot.
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

#[test]
fn autoheal_disabled_probe_pass_records_refusal_without_membership_mutation() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(temp_dir.path(), vec![peer_config("node-b")]);
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let mut cycle = AutohealCycle::new(
        false,
        DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD,
        vec!["node-b".to_string()],
    );

    let removed = manager
        .run_autoheal_probe_pass_for_test(
            &mut journal,
            &mut cycle,
            autoheal_outcomes(&[("node-b", ProbeOutcome::Unreachable)]),
        )
        .expect("disabled auto-heal pass should record a refusal");

    assert_eq!(removed, None);
    assert_eq!(
        reloaded_peer_tuples(temp_dir.path()),
        vec![("node-b".to_string(), "http://node-b:7700".to_string())]
    );
    let events = read_autoheal_events(temp_dir.path());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["decision"]["kind"], "refuse_disabled");
    assert_eq!(events[0]["action"]["phase"], "decision_recorded");
}

#[test]
fn autoheal_sustained_unreachability_removes_exact_candidate_and_persists_membership() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(
        temp_dir.path(),
        vec![peer_config("node-b"), peer_config("node-c")],
    );
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let mut cycle = AutohealCycle::new(
        true,
        DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD,
        vec!["node-b".to_string(), "node-c".to_string()],
    );

    for _ in 0..DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD - 1 {
        assert_eq!(
            manager
                .run_autoheal_probe_pass_for_test(
                    &mut journal,
                    &mut cycle,
                    autoheal_outcomes(&[
                        ("node-b", ProbeOutcome::Unreachable),
                        ("node-c", ProbeOutcome::Healthy),
                    ]),
                )
                .unwrap(),
            None
        );
    }
    let removed = manager
        .run_autoheal_probe_pass_for_test(
            &mut journal,
            &mut cycle,
            autoheal_outcomes(&[
                ("node-b", ProbeOutcome::Unreachable),
                ("node-c", ProbeOutcome::Healthy),
            ]),
        )
        .expect("eligible auto-heal pass should remove one peer");

    assert_eq!(removed, Some("node-b".to_string()));
    assert_eq!(manager.available_peers(), vec!["node-c".to_string()]);
    assert_eq!(
        reloaded_peer_tuples(temp_dir.path()),
        vec![("node-c".to_string(), "http://node-c:7700".to_string())]
    );
    let events = read_autoheal_events(temp_dir.path());
    assert_eq!(events.last().unwrap()["action"]["outcome"], "success");
    assert!(events
        .iter()
        .all(|event| event["candidate_peer_id"] != "node-c"));
}

#[tokio::test]
async fn autoheal_lifecycle_projection_tracks_cycle_counts_and_recorded_actions() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(
        temp_dir.path(),
        vec![peer_config("node-b"), peer_config("node-c")],
    );
    let manager = new_test_manager_in(
        temp_dir.path(),
        autoheal_manager_config(vec![peer_config("node-b"), peer_config("node-c")]),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let mut cycle = AutohealCycle::new(true, 2, vec!["node-b".to_string(), "node-c".to_string()]);

    manager.start_health_probe(60, true);
    manager.stop_health_probe();
    let before_observation = manager.autoheal_lifecycle_projection();
    assert!(before_observation.autoheal_enabled);
    assert_eq!(before_observation.peers.len(), 2);
    assert_eq!(
        before_observation
            .peers
            .iter()
            .map(|peer| (peer.peer_id.as_str(), peer.observation_count))
            .collect::<Vec<_>>(),
        vec![("node-b", 0), ("node-c", 0)]
    );

    manager
        .run_autoheal_probe_pass_for_test(
            &mut journal,
            &mut cycle,
            autoheal_outcomes(&[
                ("node-b", ProbeOutcome::Unreachable),
                ("node-c", ProbeOutcome::Healthy),
            ]),
        )
        .unwrap();
    let hold = manager.autoheal_lifecycle_projection();
    let node_b = hold
        .peers
        .iter()
        .find(|peer| peer.peer_id == "node-b")
        .expect("node-b lifecycle should be projected");
    assert_eq!(node_b.observation_count, 1);
    assert!(matches!(
        node_b.last_decision,
        Some(EvictionDecision::Hold {
            observations_remaining: 1
        })
    ));
    assert_eq!(
        node_b.last_action.as_ref().unwrap().phase,
        "decision_recorded"
    );
    assert_eq!(node_b.last_action.as_ref().unwrap().outcome, "not_required");

    manager
        .run_autoheal_probe_pass_for_test(
            &mut journal,
            &mut cycle,
            autoheal_outcomes(&[
                ("node-b", ProbeOutcome::Unreachable),
                ("node-c", ProbeOutcome::Healthy),
            ]),
        )
        .unwrap();
    let evicted = manager.autoheal_lifecycle_projection();
    let node_b = evicted
        .peers
        .iter()
        .find(|peer| peer.peer_id == "node-b")
        .expect("evicted node-b lifecycle should remain projected");
    assert_eq!(node_b.observation_count, 0);
    assert!(matches!(
        node_b.last_decision,
        Some(EvictionDecision::Evict { .. })
    ));
    assert_eq!(
        node_b.last_action.as_ref().unwrap().phase,
        "eviction_outcome"
    );
    assert_eq!(node_b.last_action.as_ref().unwrap().outcome, "success");

    manager
        .record_autoheal_readmission_for_test(
            &mut journal,
            &["node-c".to_string()],
            &peer_config("node-b"),
            node_b.eviction_decision_id.clone().unwrap(),
        )
        .unwrap();
    let readmitted = manager.autoheal_lifecycle_projection();
    let node_b = readmitted
        .peers
        .iter()
        .find(|peer| peer.peer_id == "node-b")
        .expect("readmitted node-b lifecycle should remain projected");
    assert_eq!(node_b.observation_count, 0);
    assert_eq!(
        node_b.last_action.as_ref().unwrap().phase,
        "readmission_outcome"
    );
    assert_eq!(node_b.last_action.as_ref().unwrap().outcome, "success");
}

#[test]
fn autoheal_recovery_clears_stale_failures_before_threshold() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(
        temp_dir.path(),
        vec![peer_config("node-b"), peer_config("node-c")],
    );
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let mut cycle = AutohealCycle::new(
        true,
        DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD,
        vec!["node-b".to_string(), "node-c".to_string()],
    );

    for outcome in [
        ProbeOutcome::Unreachable,
        ProbeOutcome::Unreachable,
        ProbeOutcome::Healthy,
        ProbeOutcome::Unreachable,
        ProbeOutcome::Unreachable,
    ] {
        assert_eq!(
            manager
                .run_autoheal_probe_pass_for_test(
                    &mut journal,
                    &mut cycle,
                    autoheal_outcomes(&[("node-b", outcome), ("node-c", ProbeOutcome::Healthy),]),
                )
                .unwrap(),
            None
        );
    }

    assert_eq!(
        manager.available_peers(),
        vec!["node-b".to_string(), "node-c".to_string()]
    );
    assert_eq!(
        reloaded_peer_tuples(temp_dir.path()),
        vec![
            ("node-b".to_string(), "http://node-b:7700".to_string()),
            ("node-c".to_string(), "http://node-c:7700".to_string()),
        ]
    );
}

#[test]
fn autoheal_indeterminate_probe_results_do_not_accumulate_toward_eviction() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(temp_dir.path(), vec![peer_config("node-b")]);
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let mut cycle = AutohealCycle::new(
        true,
        DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD,
        vec!["node-b".to_string()],
    );

    for _ in 0..5 {
        assert_eq!(
            manager
                .run_autoheal_probe_pass_for_test(
                    &mut journal,
                    &mut cycle,
                    autoheal_outcomes(&[(
                        "node-b",
                        ProbeOutcome::Indeterminate {
                            reason: "HTTP 500".to_string(),
                        },
                    )]),
                )
                .unwrap(),
            None
        );
    }

    assert_eq!(manager.available_peers(), vec!["node-b".to_string()]);
    let events = read_autoheal_events(temp_dir.path());
    assert!(events
        .iter()
        .all(|event| event["decision"]["kind"] == "refuse_indeterminate"));
}

#[test]
fn autoheal_two_of_three_peer_loss_records_indeterminate_without_removing_peers() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(
        temp_dir.path(),
        vec![peer_config("node-b"), peer_config("node-c")],
    );
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let mut cycle = AutohealCycle::new(
        true,
        DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD,
        vec!["node-b".to_string(), "node-c".to_string()],
    );

    for _ in 0..DEFAULT_AUTOHEAL_SUSTAINED_FAILURE_THRESHOLD {
        assert_eq!(
            manager
                .run_autoheal_probe_pass_for_test(
                    &mut journal,
                    &mut cycle,
                    autoheal_outcomes(&[
                        ("node-b", ProbeOutcome::Unreachable),
                        ("node-c", ProbeOutcome::Unreachable),
                    ]),
                )
                .unwrap(),
            None
        );
    }

    assert_eq!(
        manager.available_peers(),
        vec!["node-b".to_string(), "node-c".to_string()]
    );
    let events = read_autoheal_events(temp_dir.path());
    assert!(events.iter().any(|event| {
        event["decision"]["kind"] == "refuse_indeterminate"
            && event["decision"]["reason"]
                .as_str()
                .unwrap()
                .contains("local node may be isolated")
    }));
}

#[test]
fn autoheal_conditional_removal_refuses_stale_membership_snapshot() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(
        temp_dir.path(),
        vec![peer_config("node-b"), peer_config("node-c")],
    );
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();

    let removed = manager
        .record_autoheal_eviction_for_test(
            &mut journal,
            &["node-b".to_string(), "node-d".to_string()],
            "node-b",
            EvictionDecision::Evict {
                node_id: "node-b".to_string(),
                reason: "test".to_string(),
            },
        )
        .expect("stale snapshot should be recorded as a refusal");

    assert_eq!(removed, None);
    assert_eq!(
        manager.available_peers(),
        vec!["node-b".to_string(), "node-c".to_string()]
    );
    assert_eq!(
        read_autoheal_events(temp_dir.path())[0]["decision"]["kind"],
        "refuse_indeterminate"
    );
}

#[test]
fn autoheal_conditional_removal_preserves_memory_and_cursors_when_persistence_fails() {
    let temp_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp_dir.path().join("node.json")).unwrap();
    let manager = new_test_manager_in(
        temp_dir.path(),
        autoheal_manager_config(vec![peer_config("node-b"), peer_config("node-c")]),
        None,
    );
    ReplicationManager::set_peer_cursor(
        &manager.peer_cursors,
        "tenant-red",
        "node-b",
        PeerCursor::acknowledged(7),
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();

    let error = manager
        .record_autoheal_eviction_for_test(
            &mut journal,
            &["node-b".to_string(), "node-c".to_string()],
            "node-b",
            EvictionDecision::Evict {
                node_id: "node-b".to_string(),
                reason: "test".to_string(),
            },
        )
        .expect_err("persistence failure should surface");

    assert!(error.contains("failed to read"));
    assert_eq!(
        manager.available_peers(),
        vec!["node-b".to_string(), "node-c".to_string()]
    );
    assert!(manager
        .get_peer_cursors("tenant-red")
        .unwrap()
        .contains_key("node-b"));
    let events = read_autoheal_events(temp_dir.path());
    assert_eq!(events[0]["action"]["phase"], "eviction_intent");
    assert_eq!(events[1]["action"]["outcome"], "failure");
}

#[test]
fn autoheal_failed_eviction_can_retry_with_fresh_evidence() {
    let temp_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp_dir.path().join("node.json")).unwrap();
    let peers = vec![peer_config("node-b"), peer_config("node-c")];
    let manager = new_test_manager_in(
        temp_dir.path(),
        autoheal_manager_config(peers.clone()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let mut cycle = AutohealCycle::new(true, 1, vec!["node-b".to_string(), "node-c".to_string()]);
    let failed_candidate_outcomes = autoheal_outcomes(&[
        ("node-b", ProbeOutcome::Unreachable),
        ("node-c", ProbeOutcome::Healthy),
    ]);

    let error = manager
        .run_autoheal_probe_pass_for_test(
            &mut journal,
            &mut cycle,
            failed_candidate_outcomes.clone(),
        )
        .expect_err("node.json directory should fail the first eviction attempt");
    assert!(error.contains("failed to read"));
    assert_eq!(
        manager.available_peers(),
        vec!["node-b".to_string(), "node-c".to_string()]
    );

    std::fs::remove_dir(temp_dir.path().join("node.json")).unwrap();
    write_node_config_fixture(temp_dir.path(), peers);
    let removed = manager
        .run_autoheal_probe_pass_for_test(&mut journal, &mut cycle, failed_candidate_outcomes)
        .expect("fresh evidence should retry the failed eviction");

    assert_eq!(removed, Some("node-b".to_string()));
    assert_eq!(manager.available_peers(), vec!["node-c".to_string()]);
    assert_eq!(
        reloaded_peer_tuples(temp_dir.path()),
        vec![("node-c".to_string(), "http://node-c:7700".to_string())]
    );
    let eviction_outcomes = read_autoheal_events(temp_dir.path())
        .into_iter()
        .filter(|event| event["action"]["phase"] == "eviction_outcome")
        .map(|event| event["action"]["outcome"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(eviction_outcomes, vec!["failure", "success"]);
}

#[test]
fn autoheal_conditional_removal_cleans_delivery_cursors_through_remove_owner() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(
        temp_dir.path(),
        vec![peer_config("node-b"), peer_config("node-c")],
    );
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
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
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();

    let removed = manager
        .record_autoheal_eviction_for_test(
            &mut journal,
            &["node-b".to_string(), "node-c".to_string()],
            "node-b",
            EvictionDecision::Evict {
                node_id: "node-b".to_string(),
                reason: "test".to_string(),
            },
        )
        .unwrap();

    assert_eq!(removed, Some("node-b".to_string()));
    let cursors = manager.get_peer_cursors("tenant-red").unwrap();
    assert!(!cursors.contains_key("node-b"));
    assert!(cursors.contains_key("node-c"));
}

#[test]
fn autoheal_conditional_removal_already_removed_candidate_is_not_success() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(temp_dir.path(), vec![peer_config("node-c")]);
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();

    let removed = manager
        .record_autoheal_eviction_for_test(
            &mut journal,
            &["node-c".to_string()],
            "node-b",
            EvictionDecision::Evict {
                node_id: "node-b".to_string(),
                reason: "test".to_string(),
            },
        )
        .expect("already removed candidate should be recorded as a refusal");

    assert_eq!(removed, None);
    assert_eq!(manager.available_peers(), vec!["node-c".to_string()]);
    assert_eq!(
        read_autoheal_events(temp_dir.path())[0]["decision"]["kind"],
        "refuse_indeterminate"
    );
}

#[test]
fn autoheal_eviction_intent_captures_candidate_peer_config_before_removal() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(
        temp_dir.path(),
        vec![
            PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b.example.com:7700".to_string(),
            },
            peer_config("node-c"),
        ],
    );
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();

    let removed = manager
        .record_autoheal_eviction_for_test(
            &mut journal,
            &["node-b".to_string(), "node-c".to_string()],
            "node-b",
            EvictionDecision::Evict {
                node_id: "node-b".to_string(),
                reason: "test".to_string(),
            },
        )
        .expect("eviction should succeed");

    assert_eq!(removed, Some("node-b".to_string()));
    let events = read_autoheal_events(temp_dir.path());
    assert_eq!(events[0]["action"]["phase"], "eviction_intent");
    assert_eq!(events[0]["candidate_peer_config"]["node_id"], "node-b");
    assert_eq!(
        events[0]["candidate_peer_config"]["addr"],
        "http://node-b.example.com:7700"
    );
    assert_eq!(manager.available_peers(), vec!["node-c".to_string()]);
}

#[test]
fn autoheal_readmission_reuses_add_peer_idempotence_and_conflict_rules() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(
        temp_dir.path(),
        vec![PeerConfig {
            node_id: "node-b".to_string(),
            addr: "http://node-b.example.com:7700".to_string(),
        }],
    );
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let candidate = PeerConfig {
        node_id: "node-b".to_string(),
        addr: "http://node-b.example.com:7700".to_string(),
    };

    let receipt = manager
        .record_autoheal_readmission_for_test(
            &mut journal,
            &["node-b".to_string()],
            &candidate,
            "autoheal-0000000000000007".to_string(),
        )
        .expect("same id and address readmission should be idempotent");

    assert_eq!(
        receipt,
        AddPeerReceipt {
            node_id: "node-b".to_string(),
            addr: "http://node-b.example.com:7700".to_string(),
            peers_total: 1,
        }
    );
    let conflict = manager
        .record_autoheal_readmission_for_test(
            &mut journal,
            &["node-b".to_string()],
            &PeerConfig {
                node_id: "node-b".to_string(),
                addr: "http://node-b-new.example.com:7700".to_string(),
            },
            "autoheal-0000000000000008".to_string(),
        )
        .expect_err("changed address should still use AddPeerError::Conflict");
    assert!(matches!(conflict, AddPeerError::Conflict(_)));
    assert_eq!(manager.peer_count(), 1);
}

#[test]
fn autoheal_readmission_journal_intent_failure_performs_no_add() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(temp_dir.path(), vec![]);
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let journal_path = AutohealJournal::path_in_data_dir(temp_dir.path());
    std::fs::remove_file(&journal_path).unwrap();
    std::fs::create_dir(&journal_path).unwrap();

    let error = manager
        .record_autoheal_readmission_for_test(
            &mut journal,
            &[],
            &peer_config("node-b"),
            "autoheal-0000000000000007".to_string(),
        )
        .expect_err("journal intent failure should abort before add_peer");

    assert!(
        matches!(error, AddPeerError::Persistence(message) if message.contains("failed to open"))
    );
    assert_eq!(manager.peer_count(), 0);
    assert!(reloaded_peer_tuples(temp_dir.path()).is_empty());
}

#[test]
fn autoheal_readmission_persistence_failure_preserves_memory_and_node_json_bytes() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(temp_dir.path(), vec![]);
    let node_json_path = temp_dir.path().join("node.json");
    let original_node_json = std::fs::read(&node_json_path).unwrap();
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let original_mode = std::fs::metadata(temp_dir.path()).unwrap().permissions();
    let mut read_only = original_mode.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(temp_dir.path(), read_only).unwrap();

    let result = manager.record_autoheal_readmission_for_test(
        &mut journal,
        &[],
        &peer_config("node-b"),
        "autoheal-0000000000000007".to_string(),
    );

    std::fs::set_permissions(temp_dir.path(), original_mode).unwrap();
    let error = result.expect_err("node.json persistence should fail");
    assert!(matches!(error, AddPeerError::Persistence(_)));
    assert_eq!(manager.peer_count(), 0);
    assert_eq!(std::fs::read(&node_json_path).unwrap(), original_node_json);
}

#[tokio::test]
async fn autoheal_unknown_eviction_readmission_retry_closes_candidate_once() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(temp_dir.path(), vec![]);
    let (peer_url, mut request_seen) = spawn_observed_status_peer().await;
    {
        let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
        journal
            .record_eviction_intent(
                &["node-b".to_string()],
                "node-b",
                Some(PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: peer_url.clone(),
                }),
                EvictionDecision::Evict {
                    node_id: "node-b".to_string(),
                    reason: "test".to_string(),
                },
            )
            .unwrap();
    }
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();

    let receipts = manager
        .readmit_healthy_autoheal_candidates_for_test(&mut journal, &BTreeMap::new())
        .await
        .expect("healthy candidate should be readmitted");

    tokio::time::timeout(tokio::time::Duration::from_secs(1), &mut request_seen)
        .await
        .expect("candidate health should be probed")
        .expect("candidate status peer should observe one request");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].node_id, "node-b");
    assert_eq!(manager.peer_count(), 1);
    assert!(journal
        .unresolved_readmission_candidates()
        .unwrap()
        .is_empty());
    let outcomes = read_autoheal_events(temp_dir.path())
        .into_iter()
        .filter(|event| event["action"]["phase"] == "readmission_outcome")
        .collect::<Vec<_>>();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0]["action"]["outcome"], "success");
}

#[tokio::test]
async fn autoheal_readmission_reuses_active_probe_result_for_existing_member() {
    let temp_dir = tempfile::tempdir().unwrap();
    write_node_config_fixture(
        temp_dir.path(),
        vec![PeerConfig {
            node_id: "node-b".to_string(),
            addr: "http://127.0.0.1:9".to_string(),
        }],
    );
    {
        let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
        journal
            .record_eviction_intent(
                &["node-b".to_string()],
                "node-b",
                Some(PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: "http://127.0.0.1:9".to_string(),
                }),
                EvictionDecision::Evict {
                    node_id: "node-b".to_string(),
                    reason: "test".to_string(),
                },
            )
            .unwrap();
    }
    let manager = new_test_manager_in(
        temp_dir.path(),
        NodeConfig::load_or_default(temp_dir.path()),
        None,
    );
    let mut journal = AutohealJournal::with_max_bytes(temp_dir.path(), 16 * 1024).unwrap();
    let active_outcomes = autoheal_outcomes(&[("node-b", ProbeOutcome::Healthy)]);

    let receipts = manager
        .readmit_healthy_autoheal_candidates_for_test(&mut journal, &active_outcomes)
        .await
        .expect("healthy active result should close the unresolved candidate");

    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].addr, "http://127.0.0.1:9");
    assert!(journal
        .unresolved_readmission_candidates()
        .unwrap()
        .is_empty());
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

#[test]
fn ops_contract_peer_statuses_maps_runtime_wire_tokens() {
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let config = NodeConfig {
        node_id: "node-a".to_string(),
        bind_addr: "0.0.0.0:7700".to_string(),
        advertise_addr: None,
        bootstrap_peer: None,
        peers: vec![
            PeerConfig {
                node_id: "node-healthy".to_string(),
                addr: "http://node-healthy:7700".to_string(),
            },
            PeerConfig {
                node_id: "node-stale".to_string(),
                addr: "http://node-stale:7700".to_string(),
            },
            PeerConfig {
                node_id: "node-unhealthy".to_string(),
                addr: "http://node-unhealthy:7700".to_string(),
            },
            PeerConfig {
                node_id: "node-circuit-open".to_string(),
                addr: "http://node-circuit-open:7700".to_string(),
            },
        ],
    };
    let manager = new_test_manager(config, None);

    let peers = manager.peer_snapshot();
    assert_eq!(peers.len(), 4);
    for peer in peers {
        match peer.peer_id() {
            "node-healthy" => peer.set_last_success_timestamp_for_test(now_secs - 10),
            "node-stale" => peer.set_last_success_timestamp_for_test(now_secs - 120),
            "node-unhealthy" => peer.set_last_success_timestamp_for_test(now_secs - 600),
            "node-circuit-open" => {
                peer.set_last_success_timestamp_for_test(now_secs - 10);
                peer.circuit_breaker().record_failure();
                peer.circuit_breaker().record_failure();
                peer.circuit_breaker().record_failure();
                assert_eq!(peer.circuit_breaker().state(), CircuitState::Open);
            }
            other => panic!("unexpected peer fixture {other}"),
        }
    }

    let statuses = manager
        .peer_statuses()
        .into_iter()
        .map(|status| (status.peer_id.clone(), status))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(statuses.len(), 4);

    let healthy = statuses.get("node-healthy").unwrap();
    assert_eq!(healthy.addr, "http://node-healthy:7700");
    assert_eq!(healthy.status, "healthy");
    assert!(
        healthy.last_success_secs_ago.unwrap() < 60,
        "healthy peers must stay below the 60-second stale threshold"
    );

    let stale = statuses.get("node-stale").unwrap();
    assert_eq!(stale.addr, "http://node-stale:7700");
    assert_eq!(stale.status, "stale");
    assert!(
        (60..300).contains(&stale.last_success_secs_ago.unwrap()),
        "stale peers must stay in the 60-299 second bucket"
    );

    let unhealthy = statuses.get("node-unhealthy").unwrap();
    assert_eq!(unhealthy.addr, "http://node-unhealthy:7700");
    assert_eq!(unhealthy.status, "unhealthy");
    assert!(
        unhealthy.last_success_secs_ago.unwrap() >= 300,
        "unhealthy peers must be at or beyond the 300-second threshold"
    );

    let circuit_open = statuses.get("node-circuit-open").unwrap();
    assert_eq!(circuit_open.addr, "http://node-circuit-open:7700");
    assert_eq!(circuit_open.status, "circuit_open");
    assert!(
        circuit_open.last_success_secs_ago.unwrap() < 60,
        "open circuit must own the status even for an otherwise healthy timestamp"
    );
}

/// TODO: Document mutable_peer_replicate_ops_uses_snapshots_while_membership_changes.
#[tokio::test]
async fn mutable_peer_replicate_ops_uses_snapshots_while_membership_changes() {
    let accepted_barrier = Arc::new(Barrier::new(2));
    let release_barrier = Arc::new(Barrier::new(2));
    let (node_b_url, node_b_handle) =
        spawn_barrier_replicate_peer(10, accepted_barrier.clone(), release_barrier.clone()).await;
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
        let first_manager = Arc::clone(&manager.manager);
        let first_replication = tokio::spawn(async move {
            first_manager
                .replicate_ops("tenant-red", vec![mutable_peer_test_op(1)])
                .await;
        });
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
        first_replication.await.unwrap();
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

/// TODO: Document mutable_peer_duplicate_add_rejects_atomically_and_remove_clears_cursors.
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

/// TODO: Document mutable_peer_no_mutation_preserves_status_and_cursor_views.
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

/// TODO: Document mutable_peer_runtime_added_peer_uses_retained_peer_credential.
#[tokio::test]
async fn mutable_peer_runtime_added_peer_uses_retained_peer_credential() {
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
        "runtime-created peer should authenticate replication requests with the retained peer credential; request was:\n{}",
        requests[0]
    );
    wait_for_acked_seq(&manager, "tenant-red", "node-b", 33).await;
}

/// TODO: Document mutable_peer_removed_peer_cursor_does_not_reappear_after_in_flight_completion.
#[tokio::test]
async fn mutable_peer_removed_peer_cursor_does_not_reappear_after_in_flight_completion() {
    let accepted_barrier = Arc::new(Barrier::new(2));
    let release_barrier = Arc::new(Barrier::new(2));
    let (peer_url, peer_handle) =
        spawn_barrier_replicate_peer(44, accepted_barrier.clone(), release_barrier.clone()).await;
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
        let in_flight_manager = Arc::clone(&manager.manager);
        let in_flight = tokio::spawn(async move {
            in_flight_manager
                .replicate_ops("tenant-red", vec![mutable_peer_test_op(1)])
                .await;
        });
        accepted_barrier.wait().await;

        assert!(manager
            .remove_peer("node-b")
            .expect("remove should succeed")
            .is_some());

        release_barrier.wait().await;
        in_flight.await.unwrap();
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

/// The public liveness projection follows the manager-owned task handle.
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
    assert!(!manager.health_probe_is_running());
    manager.start_health_probe(1, false);
    assert!(manager.health_probe_handle.lock().unwrap().is_some());
    assert!(manager.health_probe_is_running());

    assert!(manager.stop_health_probe());
    assert!(!manager.stop_health_probe());
    assert!(manager.health_probe_handle.lock().unwrap().is_none());
    assert!(!manager.health_probe_is_running());
}

#[tokio::test]
async fn health_probe_supervisor_keeps_probing_when_autoheal_journal_startup_fails() {
    let temp_dir = tempfile::tempdir().unwrap();
    let invalid_data_dir = temp_dir.path().join("not-a-directory");
    std::fs::write(&invalid_data_dir, b"not a directory").unwrap();
    let (peer_url, mut request_seen) = spawn_observed_status_peer().await;
    let manager = new_test_manager_in(
        &invalid_data_dir,
        NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: peer_url,
            }],
        },
        None,
    );

    manager.start_health_probe_with_interval(tokio::time::Duration::from_millis(1), false);
    tokio::time::timeout(tokio::time::Duration::from_secs(1), &mut request_seen)
        .await
        .expect("health probe should still contact peers when auto-heal journal setup fails")
        .expect("status peer should report the observed request");

    tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
        loop {
            if manager.peer_statuses()[0].last_success_secs_ago.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("successful health probe should still update peer health status");
    assert!(manager.stop_health_probe());
}

#[tokio::test]
async fn autoheal_pass_does_not_probe_when_mutation_admission_is_refused() {
    let (peer_url, request_seen) = spawn_observed_status_peer().await;
    let manager = new_test_manager(
        NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: peer_url,
            }],
        },
        None,
    );

    manager.start_health_probe_with_interval_and_admission(
        tokio::time::Duration::from_millis(1),
        false,
        || async { None::<()> },
    );
    assert!(
        tokio::time::timeout(tokio::time::Duration::from_millis(30), request_seen)
            .await
            .is_err(),
        "a refused release-fence admission must suppress the complete autoheal pass"
    );
    assert!(manager.stop_health_probe());
}

#[tokio::test]
async fn autoheal_pass_holds_its_admission_permit_until_the_peer_probe_settles() {
    struct DropSignal(Option<oneshot::Sender<()>>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(signal) = self.0.take() {
                let _ = signal.send(());
            }
        }
    }

    let accepted = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let peer_url = spawn_barrier_status_peer(Arc::clone(&accepted), Arc::clone(&release)).await;
    let manager = new_test_manager(
        NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-b".to_string(),
                addr: peer_url,
            }],
        },
        None,
    );
    let (dropped_tx, mut dropped_rx) = oneshot::channel();
    let signal = Arc::new(std::sync::Mutex::new(Some(dropped_tx)));
    manager.start_health_probe_with_interval_and_admission(
        tokio::time::Duration::from_millis(1),
        false,
        move || {
            let signal = Arc::clone(&signal);
            async move { Some(DropSignal(signal.lock().unwrap().take())) }
        },
    );

    accepted.wait().await;
    tokio::select! {
        _ = &mut dropped_rx => panic!("autoheal permit dropped before the in-flight probe settled"),
        _ = tokio::time::sleep(tokio::time::Duration::from_millis(30)) => {}
    }
    release.wait().await;
    tokio::time::timeout(tokio::time::Duration::from_secs(1), &mut dropped_rx)
        .await
        .expect("autoheal permit must drop after the complete pass")
        .expect("drop signal owner must remain live");
    assert!(manager.stop_health_probe());
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

/// TODO: Document test_catch_up_from_peer_merges_ops_from_all_available_peers.
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
            timestamp_ms: 100,
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

/// TODO: Document test_catch_up_from_peer_with_metadata_strict_rejects_partial_peer_success.
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

    let (good_peer_url, good_peer_handle) = spawn_single_response_peer(good_peer_response).await;
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

/// Destination-local sequences from different peers are independent even
/// when both entries preserve the same origin node ID.
#[tokio::test]
async fn strict_catch_up_keeps_peer_local_seq_collisions_with_same_origin() {
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

    let (first_peer_url, first_peer_handle) = spawn_single_response_peer(first_peer_response).await;
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

    let merged = manager
        .catch_up_from_peer_with_metadata_strict("tenant-red", 0)
        .await
        .expect("peer-local sequence collisions must not conflate independent streams");
    let _ = first_peer_handle.await;
    let _ = conflicting_peer_handle.await;

    assert_eq!(merged.ops.len(), 2);
    assert_eq!(merged.ops[0].payload["body"]["title"], "first");
    assert_eq!(merged.ops[1].payload["body"]["title"], "second");
}

#[tokio::test]
async fn strict_catch_up_rejects_conflicting_duplicate_seq_from_one_peer() {
    let peer_response = GetOpsResponse {
        tenant_id: "tenant-red".to_string(),
        ops: vec![
            OpLogEntry {
                seq: 1,
                timestamp_ms: 100,
                node_id: "node-a".to_string(),
                tenant_id: "tenant-red".to_string(),
                op_type: "upsert".to_string(),
                payload: serde_json::json!({"objectID": "a1", "body": {"_id": "a1", "title": "first"}}),
            },
            OpLogEntry {
                seq: 1,
                timestamp_ms: 200,
                node_id: "node-a".to_string(),
                tenant_id: "tenant-red".to_string(),
                op_type: "upsert".to_string(),
                payload: serde_json::json!({"objectID": "a1", "body": {"_id": "a1", "title": "second"}}),
            },
        ],
        current_seq: 1,
        oldest_retained_seq: Some(1),
        node_current_seqs: BTreeMap::from([(String::from("node-a"), 1)]),
    };
    let (peer_url, peer_handle) = spawn_single_response_peer(peer_response).await;
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
        .expect_err("one peer must not claim two payloads for one local sequence");
    let _ = peer_handle.await;

    assert!(
        error.contains("conflicting payload") && error.contains("node-a") && error.contains('1'),
        "strict conflict error should identify the peer-local duplicate: {error}"
    );
}

/// TODO: Document test_discover_tenants_from_peers_strict_rejects_partial_peer_success.
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
        .expect_err("strict tenant discovery must fail when any configured peer is unreachable");
    let _ = handle.await;

    assert!(
        error.contains("peer node-c tenant discovery failed"),
        "strict tenant discovery failure should identify the unreachable peer, got: {}",
        error
    );
}

/// TODO: Document test_discover_tenants_from_peers_skips_invalid_tenant_ids.
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

/// TODO: Document test_discover_tenants_from_peers_strict_rejects_invalid_tenant_ids.
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
