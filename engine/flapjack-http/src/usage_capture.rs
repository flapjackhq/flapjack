use crate::usage_persistence::CapturedUsageGauges;
use dashmap::DashMap;
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub(crate) struct UsageGaugeSelection {
    documents_count: bool,
    storage_bytes: bool,
}

impl UsageGaugeSelection {
    pub(crate) fn from_statistics(stats: &[&str]) -> Self {
        Self {
            documents_count: stats.contains(&"documents_count"),
            storage_bytes: stats.contains(&"storage_bytes"),
        }
    }

    const fn both() -> Self {
        Self {
            documents_count: true,
            storage_bytes: true,
        }
    }
}

/// Capture current live gauge values for all loaded indexes.
///
/// Document counts come from loaded tenants with successful `tenant_doc_count`.
/// Storage bytes come from the background metrics cache (`storage_gauges`).
/// Missing storage remains `None`; explicit `0` remains `Some(0)`.
/// No manager-wide storage or directory walk is performed.
pub(crate) fn capture_live_gauges(
    manager: &flapjack::IndexManager,
    storage_gauges: Option<&DashMap<String, u64>>,
) -> HashMap<String, CapturedUsageGauges> {
    capture_requested_live_gauges(manager, storage_gauges, UsageGaugeSelection::both(), None)
}

/// Capture only the requested gauge dimensions, optionally for one index.
pub(crate) fn capture_requested_live_gauges(
    manager: &flapjack::IndexManager,
    storage_gauges: Option<&DashMap<String, u64>>,
    selection: UsageGaugeSelection,
    index_filter: Option<&str>,
) -> HashMap<String, CapturedUsageGauges> {
    capture_requested_from_sources(
        selection,
        |captured| capture_document_counts(manager, index_filter, captured),
        |captured| capture_storage_bytes(storage_gauges, index_filter, captured),
    )
}

fn capture_requested_from_sources<CaptureDocuments, CaptureStorage>(
    selection: UsageGaugeSelection,
    capture_documents: CaptureDocuments,
    capture_storage: CaptureStorage,
) -> HashMap<String, CapturedUsageGauges>
where
    CaptureDocuments: FnOnce(&mut HashMap<String, CapturedUsageGauges>),
    CaptureStorage: FnOnce(&mut HashMap<String, CapturedUsageGauges>),
{
    let mut captured = HashMap::new();
    if selection.documents_count {
        capture_documents(&mut captured);
    }
    if selection.storage_bytes {
        capture_storage(&mut captured);
    }
    captured
}

fn capture_document_counts(
    manager: &flapjack::IndexManager,
    index_filter: Option<&str>,
    captured: &mut HashMap<String, CapturedUsageGauges>,
) {
    match index_filter {
        Some(index_name) => {
            if let Some(value) = manager.tenant_doc_count(index_name) {
                captured
                    .entry(index_name.to_string())
                    .or_default()
                    .documents_count = Some(value);
            }
        }
        None => {
            for index_name in manager.loaded_tenant_ids() {
                if let Some(value) = manager.tenant_doc_count(&index_name) {
                    captured.entry(index_name).or_default().documents_count = Some(value);
                }
            }
        }
    }
}

fn capture_storage_bytes(
    storage_gauges: Option<&DashMap<String, u64>>,
    index_filter: Option<&str>,
    captured: &mut HashMap<String, CapturedUsageGauges>,
) {
    match index_filter {
        Some(index_name) => {
            let value = storage_gauges
                .and_then(|gauges| gauges.get(index_name).map(|entry| *entry.value()));
            if let Some(value) = value {
                captured
                    .entry(index_name.to_string())
                    .or_default()
                    .storage_bytes = Some(value);
            }
        }
        None => {
            if let Some(gauges) = storage_gauges {
                for entry in gauges.iter() {
                    captured
                        .entry(entry.key().clone())
                        .or_default()
                        .storage_bytes = Some(*entry.value());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn storage_only_capture_does_not_access_document_source() {
        let document_source_accessed = Cell::new(false);
        let storage_source_accessed = Cell::new(false);

        let captured = capture_requested_from_sources(
            UsageGaugeSelection::from_statistics(&["storage_bytes"]),
            |_| document_source_accessed.set(true),
            |captured| {
                storage_source_accessed.set(true);
                captured.insert(
                    "products".to_string(),
                    CapturedUsageGauges {
                        documents_count: None,
                        storage_bytes: Some(12_345),
                    },
                );
                captured.insert(
                    "empty".to_string(),
                    CapturedUsageGauges {
                        documents_count: None,
                        storage_bytes: Some(0),
                    },
                );
            },
        );

        assert!(!document_source_accessed.get());
        assert!(storage_source_accessed.get());
        assert_eq!(captured["products"].storage_bytes, Some(12_345));
        assert_eq!(captured.get("empty").unwrap().storage_bytes, Some(0));
    }

    #[test]
    fn documents_only_capture_does_not_access_storage_source() {
        let document_source_accessed = Cell::new(false);
        let storage_source_accessed = Cell::new(false);

        let captured = capture_requested_from_sources(
            UsageGaugeSelection::from_statistics(&["documents_count"]),
            |captured| {
                document_source_accessed.set(true);
                captured.insert(
                    "products".to_string(),
                    CapturedUsageGauges {
                        documents_count: Some(3),
                        storage_bytes: None,
                    },
                );
                captured.insert(
                    "empty".to_string(),
                    CapturedUsageGauges {
                        documents_count: Some(0),
                        storage_bytes: None,
                    },
                );
            },
            |_| storage_source_accessed.set(true),
        );

        assert!(document_source_accessed.get());
        assert!(!storage_source_accessed.get());
        assert_eq!(captured["products"].documents_count, Some(3));
        assert_eq!(captured.get("empty").unwrap().documents_count, Some(0));
    }

    /// The single-index fast path must produce the exact same gauge values as
    /// filtering the full-tenant capture to that index, across loaded/unloaded,
    /// present/absent storage, and explicit-zero cases.
    #[tokio::test]
    async fn single_index_capture_matches_full_walk() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let manager = &state.manager;
        let gauges = state.metrics_state.as_ref().unwrap().storage_gauges.clone();
        gauges.clear();

        // "products": 3 live docs, 12_345 storage bytes.
        manager.create_tenant("products").unwrap();
        manager
            .add_documents_sync(
                "products",
                (0..3u64)
                    .map(|i| flapjack::types::Document {
                        id: format!("products_{i}"),
                        fields: std::collections::HashMap::new(),
                    })
                    .collect(),
            )
            .await
            .unwrap();
        gauges.insert("products".to_string(), 12_345);

        // "empty": 0 live docs, explicit 0 storage bytes.
        manager.create_tenant("empty").unwrap();
        gauges.insert("empty".to_string(), 0);

        // "docs_only": loaded (0 docs), no storage gauge.
        manager.create_tenant("docs_only").unwrap();

        // "storage_only": not loaded, storage gauge present.
        gauges.insert("storage_only".to_string(), 4_096);

        let full = capture_live_gauges(manager, Some(&gauges));

        for index in ["products", "empty", "docs_only", "storage_only", "unknown"] {
            let single = capture_requested_live_gauges(
                manager,
                Some(&gauges),
                UsageGaugeSelection::both(),
                Some(index),
            );
            let single = single.get(index).copied().unwrap_or_default();
            let expected = full.get(index).copied().unwrap_or_default();
            assert_eq!(
                single, expected,
                "single-index capture for {index} must match filtered full-walk capture",
            );
        }

        // Known-answer anchors so the equality above cannot pass on both sides
        // being wrong in the same way.
        let capture_index = |index_name| {
            capture_requested_live_gauges(
                manager,
                Some(&gauges),
                UsageGaugeSelection::both(),
                Some(index_name),
            )
            .get(index_name)
            .copied()
            .unwrap_or_default()
        };

        let products = capture_index("products");
        assert_eq!(products.documents_count, Some(3));
        assert_eq!(products.storage_bytes, Some(12_345));

        let empty = capture_index("empty");
        assert_eq!(empty.documents_count, Some(0));
        assert_eq!(empty.storage_bytes, Some(0));

        let docs_only = capture_index("docs_only");
        assert_eq!(docs_only.documents_count, Some(0));
        assert_eq!(docs_only.storage_bytes, None);

        let storage_only = capture_index("storage_only");
        assert_eq!(storage_only.documents_count, None);
        assert_eq!(storage_only.storage_bytes, Some(4_096));

        let unknown = capture_index("unknown");
        assert_eq!(unknown, CapturedUsageGauges::default());

        // Absent metrics state yields no storage, matching the full-walk contract.
        let no_storage = capture_requested_live_gauges(
            manager,
            None,
            UsageGaugeSelection::both(),
            Some("products"),
        )
        .get("products")
        .copied()
        .unwrap_or_default();
        assert_eq!(no_storage.documents_count, Some(3));
        assert_eq!(no_storage.storage_bytes, None);
    }
}
