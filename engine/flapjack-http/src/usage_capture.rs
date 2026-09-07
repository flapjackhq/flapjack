//! Stub summary for engine/flapjack-http/src/usage_capture.rs.
use crate::usage_persistence::CapturedUsageGauges;
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

/// Capture current gauge values from one whole cached generation.
///
/// Missing values remain `None`; explicit zero remains `Some(0)`. This path
/// never loads an index or scans storage.
pub(crate) fn capture_live_gauges(
    metrics_state: Option<&crate::handlers::metrics::MetricsState>,
) -> HashMap<String, CapturedUsageGauges> {
    capture_requested_live_gauges(metrics_state, UsageGaugeSelection::both(), None)
}

/// Capture only the requested gauge dimensions, optionally for one index.
pub(crate) fn capture_requested_live_gauges(
    metrics_state: Option<&crate::handlers::metrics::MetricsState>,
    selection: UsageGaugeSelection,
    index_filter: Option<&str>,
) -> HashMap<String, CapturedUsageGauges> {
    let snapshot = metrics_state
        .map(|metrics_state| metrics_state.index_gauge_snapshot())
        .unwrap_or_default();
    capture_requested_from_sources(
        selection,
        |captured| capture_document_counts(&snapshot, index_filter, captured),
        |captured| capture_storage_bytes(&snapshot, index_filter, captured),
    )
}

/// TODO: Document capture_requested_from_sources.
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

/// TODO: Document capture_document_counts.
fn capture_document_counts(
    snapshot: &crate::handlers::metrics::IndexGaugeSnapshot,
    index_filter: Option<&str>,
    captured: &mut HashMap<String, CapturedUsageGauges>,
) {
    match index_filter {
        Some(index_name) => {
            if let Some(value) = snapshot
                .get(index_name)
                .and_then(|gauges| gauges.documents_count)
            {
                captured
                    .entry(index_name.to_string())
                    .or_default()
                    .documents_count = Some(value);
            }
        }
        None => {
            for (index_name, gauges) in snapshot {
                if let Some(value) = gauges.documents_count {
                    captured
                        .entry(index_name.clone())
                        .or_default()
                        .documents_count = Some(value);
                }
            }
        }
    }
}

/// TODO: Document capture_storage_bytes.
fn capture_storage_bytes(
    snapshot: &crate::handlers::metrics::IndexGaugeSnapshot,
    index_filter: Option<&str>,
    captured: &mut HashMap<String, CapturedUsageGauges>,
) {
    match index_filter {
        Some(index_name) => {
            if let Some(value) = snapshot
                .get(index_name)
                .and_then(|gauges| gauges.storage_bytes)
            {
                captured
                    .entry(index_name.to_string())
                    .or_default()
                    .storage_bytes = Some(value);
            }
        }
        None => {
            for (index_name, gauges) in snapshot {
                if let Some(value) = gauges.storage_bytes {
                    captured
                        .entry(index_name.clone())
                        .or_default()
                        .storage_bytes = Some(value);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// TODO: Document storage_only_capture_does_not_access_document_source.
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

    /// TODO: Document documents_only_capture_does_not_access_storage_source.
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
    #[test]
    fn single_index_capture_matches_full_snapshot_filter() {
        let metrics_state = crate::handlers::metrics::MetricsState::new();
        metrics_state.replace_index_gauges(std::collections::BTreeMap::from([
            (
                "products".to_string(),
                crate::handlers::metrics::IndexGaugeValues {
                    documents_count: Some(3),
                    storage_bytes: Some(12_345),
                },
            ),
            (
                "empty".to_string(),
                crate::handlers::metrics::IndexGaugeValues {
                    documents_count: Some(0),
                    storage_bytes: Some(0),
                },
            ),
            (
                "docs_only".to_string(),
                crate::handlers::metrics::IndexGaugeValues {
                    documents_count: Some(0),
                    storage_bytes: None,
                },
            ),
            (
                "storage_only".to_string(),
                crate::handlers::metrics::IndexGaugeValues {
                    documents_count: None,
                    storage_bytes: Some(4_096),
                },
            ),
        ]));

        let full = capture_live_gauges(Some(&metrics_state));

        for index in ["products", "empty", "docs_only", "storage_only", "unknown"] {
            let single = capture_requested_live_gauges(
                Some(&metrics_state),
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
                Some(&metrics_state),
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

        // Absent metrics state yields no request-time recovery or scan.
        let no_storage =
            capture_requested_live_gauges(None, UsageGaugeSelection::both(), Some("products"))
                .get("products")
                .copied()
                .unwrap_or_default();
        assert_eq!(no_storage.documents_count, None);
        assert_eq!(no_storage.storage_bytes, None);
    }
}
