//! Per-tenant disk usage calculator providing a recursive, symlink-safe directory size function used by metrics and internal storage endpoints.

use std::io::{self, ErrorKind};
use std::path::Path;

/// Return whether two directory paths are the same or one contains the other.
///
/// The lexical check covers missing paths. Canonical paths additionally catch
/// configured aliases such as `..` once both directories exist.
pub(crate) fn directory_paths_overlap(left: &Path, right: &Path) -> bool {
    if left.starts_with(right) || right.starts_with(left) {
        return true;
    }

    let (Ok(left), Ok(right)) = (left.canonicalize(), right.canonicalize()) else {
        return false;
    };
    left.starts_with(&right) || right.starts_with(&left)
}

/// Recursively sum the sizes of all regular files under `path`.
///
/// Symlinks are skipped (not followed) to avoid double-counting and loops.
/// Returns `Ok(0)` for an empty directory.
pub fn dir_size_bytes(path: &Path) -> io::Result<u64> {
    let mut total: u64 = 0;
    let root_metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Ok(0);
    }
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let ft = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            total += dir_size_bytes(&entry.path())?;
        } else if ft.is_file() {
            match entry.metadata() {
                Ok(metadata) => total += metadata.len(),
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn dir_size_bytes_known_files() {
        let tmp = TempDir::new().unwrap();
        // Create two files with known sizes
        let mut f1 = std::fs::File::create(tmp.path().join("a.txt")).unwrap();
        f1.write_all(&[0u8; 100]).unwrap();
        let mut f2 = std::fs::File::create(tmp.path().join("b.txt")).unwrap();
        f2.write_all(&[0u8; 200]).unwrap();

        let size = dir_size_bytes(tmp.path()).unwrap();
        assert_eq!(size, 300);
    }

    #[test]
    fn dir_size_bytes_empty_directory() {
        let tmp = TempDir::new().unwrap();
        let size = dir_size_bytes(tmp.path()).unwrap();
        assert_eq!(size, 0);
    }

    #[test]
    fn dir_size_bytes_nested_directories() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut f1 = std::fs::File::create(tmp.path().join("top.txt")).unwrap();
        f1.write_all(&[0u8; 50]).unwrap();
        let mut f2 = std::fs::File::create(sub.join("nested.txt")).unwrap();
        f2.write_all(&[0u8; 75]).unwrap();

        let size = dir_size_bytes(tmp.path()).unwrap();
        assert_eq!(size, 125);
    }

    #[test]
    fn dir_size_bytes_nonexistent_path() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("does_not_exist");
        // Non-directory path returns 0
        let size = dir_size_bytes(&missing).unwrap();
        assert_eq!(size, 0);
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_bytes_skips_symlinks() {
        let tmp = TempDir::new().unwrap();
        let mut f1 = std::fs::File::create(tmp.path().join("real.txt")).unwrap();
        f1.write_all(&[0u8; 100]).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("real.txt"), tmp.path().join("link.txt"))
            .unwrap();

        let size = dir_size_bytes(tmp.path()).unwrap();
        // Only the real file should be counted, not the symlink
        assert_eq!(size, 100);
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_bytes_skips_symlink_root() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("external.bin"), [0_u8; 123]).unwrap();
        let link = tmp.path().join("linked-root");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert_eq!(dir_size_bytes(&link).unwrap(), 0);
    }

    #[tokio::test]
    async fn tenant_storage_bytes_nonexistent_tenant() {
        let tmp = TempDir::new().unwrap();
        let manager = crate::IndexManager::new(tmp.path());
        assert_eq!(manager.tenant_storage_bytes("no_such_tenant"), 0);
    }

    #[tokio::test]
    async fn tenant_storage_bytes_includes_index_and_analytics_files() {
        let tmp = TempDir::new().unwrap();
        let manager = crate::IndexManager::new(tmp.path());
        let analytics = crate::analytics::AnalyticsConfig {
            enabled: true,
            data_dir: tmp.path().join("analytics"),
            flush_interval_secs: 60,
            flush_size: 10_000,
            retention_days: crate::analytics::config::DEFAULT_ANALYTICS_RETENTION_DAYS,
        };
        manager.set_analytics_config(analytics.clone());

        let index_dir = tmp.path().join("products");
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(index_dir.join("segment"), [0_u8; 100]).unwrap();

        let events_dir = analytics.events_dir("products");
        std::fs::create_dir_all(&events_dir).unwrap();
        std::fs::write(events_dir.join("events.parquet"), [0_u8; 200]).unwrap();

        assert_eq!(manager.tenant_storage_bytes("products"), 300);
    }

    #[tokio::test]
    async fn tenant_storage_bytes_does_not_double_count_lexically_aliased_nested_analytics() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().join("data");
        let manager = crate::IndexManager::new(&base);

        let alias_component = base.join("alias");
        std::fs::create_dir_all(&alias_component).unwrap();
        let analytics = crate::analytics::AnalyticsConfig {
            enabled: true,
            data_dir: alias_component.join("..").join("products/analytics"),
            flush_interval_secs: 60,
            flush_size: 10_000,
            retention_days: crate::analytics::config::DEFAULT_ANALYTICS_RETENTION_DAYS,
        };
        manager.set_analytics_config(analytics.clone());

        let index_dir = base.join("products");
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(index_dir.join("segment"), [0_u8; 100]).unwrap();
        let events_dir = analytics.events_dir("products");
        std::fs::create_dir_all(&events_dir).unwrap();
        std::fs::write(events_dir.join("events.parquet"), [0_u8; 200]).unwrap();

        assert_eq!(manager.tenant_storage_bytes("products"), 300);
    }

    #[tokio::test]
    async fn tenant_storage_bytes_does_not_expand_to_analytics_ancestor() {
        let tmp = TempDir::new().unwrap();
        let analytics_root = tmp.path().join("analytics");
        let base = analytics_root.join("products/indexes");
        let manager = crate::IndexManager::new(&base);
        let analytics = crate::analytics::AnalyticsConfig {
            enabled: true,
            data_dir: analytics_root,
            flush_interval_secs: 60,
            flush_size: 10_000,
            retention_days: crate::analytics::config::DEFAULT_ANALYTICS_RETENTION_DAYS,
        };
        manager.set_analytics_config(analytics.clone());

        let index_dir = base.join("products");
        std::fs::create_dir_all(&index_dir).unwrap();
        std::fs::write(index_dir.join("segment"), [0_u8; 100]).unwrap();
        let other_tenant = base.join("orders");
        std::fs::create_dir_all(&other_tenant).unwrap();
        std::fs::write(other_tenant.join("segment"), [0_u8; 400]).unwrap();
        let events_dir = analytics.events_dir("products");
        std::fs::create_dir_all(&events_dir).unwrap();
        std::fs::write(events_dir.join("events.parquet"), [0_u8; 200]).unwrap();

        assert_eq!(manager.tenant_storage_bytes("products"), 100);
    }

    /// Verify that `all_tenant_storage` returns an entry with non-zero byte count for every loaded tenant.
    #[tokio::test]
    async fn all_tenant_storage_returns_entries_for_loaded_tenants() {
        let tmp = TempDir::new().unwrap();
        let manager = crate::IndexManager::new(tmp.path());
        manager.create_tenant("alpha").unwrap();
        manager.create_tenant("beta").unwrap();

        let storage = manager.all_tenant_storage();
        let ids: Vec<&str> = storage.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"alpha"), "should contain alpha");
        assert!(ids.contains(&"beta"), "should contain beta");
        assert_eq!(storage.len(), 2);

        // Each tenant should have some bytes (tantivy creates meta files on create)
        for (tid, bytes) in &storage {
            assert!(*bytes > 0, "tenant {} should have non-zero storage", tid);
        }
    }
}
