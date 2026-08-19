//! Stub summary for analytics_retention_probe.rs.
use flapjack::analytics::manifest::RollupManifest;
use flapjack::analytics::retention::cleanup_old_partitions;
use std::path::{Path, PathBuf};

const PROBE_INDEX_NAME: &str = "_rf2_retention_probe";
const PROBE_PARTITION: &str = "date=2000-01-01";
const PROBE_RETENTION_DAYS: u32 = 1;

fn fail(message: impl AsRef<str>) -> ! {
    eprintln!("FAIL: {}", message.as_ref());
    std::process::exit(1);
}

/// TODO: Document ensure_probe_layout.
fn ensure_probe_layout(analytics_dir: &Path) -> Result<PathBuf, String> {
    let probe_index_dir = analytics_dir.join(PROBE_INDEX_NAME);
    let probe_partition_dir = probe_index_dir.join("searches").join(PROBE_PARTITION);
    std::fs::create_dir_all(&probe_partition_dir)
        .map_err(|e| format!("create probe partition failed: {e}"))?;
    let marker_path = probe_partition_dir.join("probe_marker.txt");
    std::fs::write(&marker_path, b"retention-probe")
        .map_err(|e| format!("write probe marker failed: {e}"))?;

    let manifest_path = probe_index_dir.join("rollups").join("manifest.json");
    let manifest = RollupManifest::new(PROBE_INDEX_NAME);
    manifest
        .save(&manifest_path)
        .map_err(|e| format!("save probe manifest failed: {e}"))?;
    Ok(probe_partition_dir)
}

fn cleanup_probe_index(analytics_dir: &Path) {
    let _ = std::fs::remove_dir_all(analytics_dir.join(PROBE_INDEX_NAME));
}

/// TODO: Document main.
fn main() {
    let analytics_dir = match std::env::args().nth(1) {
        Some(value) => PathBuf::from(value),
        None => fail("usage: analytics_retention_probe <analytics_dir>"),
    };

    if !analytics_dir.exists() {
        fail(format!(
            "analytics dir does not exist: {}",
            analytics_dir.display()
        ));
    }

    let probe_partition_dir = match ensure_probe_layout(&analytics_dir) {
        Ok(path) => path,
        Err(error) => fail(error),
    };

    if let Err(error) = cleanup_old_partitions(&analytics_dir, PROBE_RETENTION_DAYS) {
        cleanup_probe_index(&analytics_dir);
        fail(format!("retention cleanup failed: {error}"));
    }

    let probe_preserved = probe_partition_dir.exists();
    println!("probe_partition_path={}", probe_partition_dir.display());
    println!(
        "retention_gate_verdict={}",
        if probe_preserved { "pass" } else { "fail" }
    );

    cleanup_probe_index(&analytics_dir);

    if !probe_preserved {
        fail("retention removed uncertified probe partition");
    }
}
