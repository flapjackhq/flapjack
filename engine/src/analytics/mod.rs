//! Search analytics engine powered by DataFusion + Parquet.
//!
//! Tracks search events automatically and click/conversion events via the Insights API.
//! Data is stored in Parquet files with Hive-style date partitioning and queried
//! using DataFusion SQL for efficient analytics aggregation.

pub mod aggregation;
pub mod collector;
pub mod config;
pub mod hll;
pub mod manifest;
pub mod merge;
mod mutation;
pub mod query;
pub mod retention;
pub mod schema;
pub mod seed;
pub mod types;
pub mod writer;

pub use collector::{AnalyticsCollector, DebugEvent};
pub use config::AnalyticsConfig;
pub use query::{AnalyticsQueryEngine, AnalyticsQueryParams};

use once_cell::sync::OnceCell;
use std::sync::Arc;

static GLOBAL_COLLECTOR: OnceCell<Arc<AnalyticsCollector>> = OnceCell::new();
const HOUR_MS: i64 = 3_600_000;

/// Resolve the hourly rollup window width, honoring the test-only override when set.
///
/// Production callers keep the canonical one-hour window because the override is
/// absent unless a test or loadtest harness sets it explicitly.
pub fn resolved_hourly_rollup_window_ms() -> i64 {
    std::env::var("FLAPJACK_ROLLUP_WINDOW_OVERRIDE_MS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|window_ms| *window_ms > 0)
        .unwrap_or(HOUR_MS)
}

/// Initialize the global analytics collector. Call once at startup.
pub fn init_global_collector(collector: Arc<AnalyticsCollector>) {
    let _ = GLOBAL_COLLECTOR.set(collector);
}

/// Get the global analytics collector, if initialized.
pub fn get_global_collector() -> Option<&'static Arc<AnalyticsCollector>> {
    GLOBAL_COLLECTOR.get()
}
