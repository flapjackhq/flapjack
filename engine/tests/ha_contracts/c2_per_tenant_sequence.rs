//! Stub summary for c2_per_tenant_sequence.rs.
// Contract C2: Per-Tenant Sequence Continuity
//
// Decision 0003 §C2: "Each tenant's oplog maintains a monotonically increasing
// sequence number. append_batch assigns contiguous sequences atomically;
// read_since returns all entries after a given sequence."
//
// Primary owner seam: engine/src/index/oplog.rs
//   - read_committed_seq (line 42)
//   - write_committed_seq (line 52)
//   - append_batch (line 209)
//   - read_since (line 260)
//
// Verdict at HEAD: already upheld.

use flapjack::types::Document;
use flapjack::IndexManager;
use tempfile::TempDir;

/// TODO: Document sequence_numbers_are_monotonically_increasing.
#[tokio::test]
async fn sequence_numbers_are_monotonically_increasing() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test").unwrap();
    // Hand-calc: 5 individual add_documents_sync calls → 5 oplog entries, seq 1..=5
    for i in 1..=5 {
        let doc =
            Document::from_json(&serde_json::json!({"_id": i.to_string(), "value": i})).unwrap();
        manager.add_documents_sync("test", vec![doc]).await.unwrap();
    }

    let ops = manager
        .get_or_create_oplog("test")
        .unwrap()
        .read_since(0)
        .unwrap();
    // Hand-calc: 5 writes → 5 oplog entries
    assert_eq!(
        ops.len(),
        5,
        "5 writes must produce exactly 5 oplog entries"
    );

    let mut prev_seq = 0u64;
    for (idx, op) in ops.iter().enumerate() {
        assert!(
            op.seq > prev_seq,
            "seq {} at position {} must exceed previous seq {}",
            op.seq,
            idx,
            prev_seq
        );
        prev_seq = op.seq;
    }
}

/// TODO: Document read_since_returns_only_entries_after_boundary.
#[tokio::test]
async fn read_since_returns_only_entries_after_boundary() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test").unwrap();
    // Hand-calc: 10 writes → 10 oplog entries with seq 1..=10
    for i in 1..=10 {
        let doc =
            Document::from_json(&serde_json::json!({"_id": i.to_string(), "value": i})).unwrap();
        manager.add_documents_sync("test", vec![doc]).await.unwrap();
    }

    let oplog = manager.get_or_create_oplog("test").unwrap();
    let all_ops = oplog.read_since(0).unwrap();
    assert_eq!(all_ops.len(), 10, "10 writes must produce 10 oplog entries");

    // Hand-calc: read_since(seq_of_5th_entry) → entries 6..=10 → 5 entries
    let fifth_seq = all_ops[4].seq;
    let ops_after = oplog.read_since(fifth_seq).unwrap();

    assert_eq!(
        ops_after.len(),
        5,
        // Hand-calc: 10 total entries, reading after the 5th → 5 remaining
        "read_since(seq={}) must return exactly 5 entries, got {}",
        fifth_seq,
        ops_after.len()
    );
    for op in &ops_after {
        assert!(
            op.seq > fifth_seq,
            "read_since must only return entries with seq > boundary; got seq {} <= {}",
            op.seq,
            fifth_seq
        );
    }
}

/// TODO: Document committed_seq_persists_across_manager_instances.
#[tokio::test]
async fn committed_seq_persists_across_manager_instances() {
    let temp_dir = TempDir::new().unwrap();
    let base = temp_dir.path().to_path_buf();

    // Phase 1: write 3 docs, advance committed_seq
    {
        let manager = IndexManager::new(&base);
        manager.create_tenant("persist_test").unwrap();
        for i in 1..=3 {
            let doc = Document::from_json(&serde_json::json!({"_id": i.to_string(), "value": i}))
                .unwrap();
            manager
                .add_documents_sync("persist_test", vec![doc])
                .await
                .unwrap();
        }
        let oplog = manager.get_or_create_oplog("persist_test").unwrap();
        let ops = oplog.read_since(0).unwrap();
        // Hand-calc: 3 writes → 3 entries, last seq is ops[2].seq
        assert_eq!(ops.len(), 3);
        let final_seq = ops[2].seq;

        let tenant_path = base.join("persist_test");
        flapjack::index::oplog::write_committed_seq(&tenant_path, final_seq).unwrap();

        manager.graceful_shutdown().await;
    }

    // Phase 2: reopen and verify committed_seq survived
    {
        let tenant_path = base.join("persist_test");
        let committed = flapjack::index::oplog::read_committed_seq(&tenant_path);
        // Hand-calc: we wrote committed_seq = seq of 3rd entry (non-zero)
        assert!(
            committed > 0,
            "committed_seq must persist across restarts; got 0"
        );

        let manager = IndexManager::new(&base);
        let oplog = manager.get_or_create_oplog("persist_test").unwrap();
        let ops_after_committed = oplog.read_since(committed).unwrap();
        // Hand-calc: committed_seq = last entry → 0 entries after it
        assert_eq!(
            ops_after_committed.len(),
            0,
            "no ops should exist after the committed boundary"
        );
    }
}
