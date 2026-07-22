use chrono::NaiveDate;
use dashmap::DashMap;
use flapjack_http::usage_middleware::TenantUsageCounters;
use flapjack_http::usage_persistence::{CapturedUsageGauges, UsagePersistence};
use serde_json::Value;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

fn fixture_gauges() -> HashMap<String, CapturedUsageGauges> {
    HashMap::from([
        (
            "gauge_probe".to_string(),
            CapturedUsageGauges {
                documents_count: Some(17),
                storage_bytes: Some(123_456),
            },
        ),
        (
            "explicit_zero".to_string(),
            CapturedUsageGauges {
                documents_count: Some(0),
                storage_bytes: Some(0),
            },
        ),
        (
            "legacy_missing".to_string(),
            CapturedUsageGauges {
                documents_count: Some(41),
                storage_bytes: Some(42),
            },
        ),
    ])
}

fn remove_legacy_gauges(snapshot_path: &Path) -> io::Result<()> {
    let snapshot_bytes = std::fs::read(snapshot_path)?;
    let mut snapshot: Value = serde_json::from_slice(&snapshot_bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let legacy_index = snapshot
        .pointer_mut("/indexes/legacy_missing")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "saved snapshot omitted legacy_missing fixture index",
            )
        })?;

    for gauge_name in ["documents_count", "storage_bytes"] {
        if legacy_index.remove(gauge_name).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("saved snapshot omitted {gauge_name} before legacy rewrite"),
            ));
        }
    }

    let rewritten = serde_json::to_vec_pretty(&snapshot).map_err(io::Error::other)?;
    let temporary_path = snapshot_path.with_extension("json.legacy.tmp");
    std::fs::write(&temporary_path, rewritten)?;
    std::fs::rename(temporary_path, snapshot_path)
}

fn parse_arguments() -> Result<(PathBuf, String), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let data_dir = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: usage_gauge_fixture <data_dir> <completed_day_yyyy_mm_dd>")?;
    let date_argument = arguments
        .next()
        .ok_or("usage: usage_gauge_fixture <data_dir> <completed_day_yyyy_mm_dd>")?;
    if arguments.next().is_some() {
        return Err("usage: usage_gauge_fixture <data_dir> <completed_day_yyyy_mm_dd>".into());
    }
    let completed_day = NaiveDate::parse_from_str(
        date_argument
            .to_str()
            .ok_or("completed-day date must be valid UTF-8")?,
        "%Y-%m-%d",
    )?;
    Ok((data_dir, completed_day.format("%Y-%m-%d").to_string()))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (data_dir, completed_day) = parse_arguments()?;
    let persistence = UsagePersistence::new(&data_dir)?;
    let counters: DashMap<String, TenantUsageCounters> = DashMap::new();

    persistence.save_snapshot_with_gauges(&completed_day, &counters, &fixture_gauges())?;

    // Only the legacy-shape case mutates JSON. Normal fixture serialization
    // remains owned by UsagePersistence above, including its atomic write.
    let snapshot_path = data_dir
        .join("_usage")
        .join(format!("{completed_day}.json"));
    remove_legacy_gauges(&snapshot_path)?;
    Ok(())
}
