//! Durable per-index metadata.
//!
//! Persisted as `index_meta.json` inside each index directory.
//! Tracks `created_at` (RFC3339) and `last_build_time_s` (seconds for last build).
//! Can be read without loading the full Tantivy index.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub(crate) const METADATA_FILE: &str = "index_meta.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMetadata {
    /// RFC3339 timestamp of when the index was first created.
    pub created_at: String,
    /// Duration in seconds of the last index build/commit. 0 if never built.
    #[serde(default)]
    pub last_build_time_s: u64,
}

impl Default for IndexMetadata {
    fn default() -> Self {
        Self::new()
    }
}

impl IndexMetadata {
    /// Create new metadata with the current timestamp and zero build time.
    pub fn new() -> Self {
        Self {
            created_at: chrono::Utc::now().to_rfc3339(),
            last_build_time_s: 0,
        }
    }

    /// Load metadata from the index directory. Returns `None` if the file doesn't exist.
    pub fn load(index_dir: &Path) -> Result<Option<Self>> {
        let path = index_dir.join(METADATA_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path)?;
        let meta: Self = serde_json::from_str(&data).map_err(|e| {
            crate::error::FlapjackError::InvalidQuery(format!("Bad index_meta.json: {}", e))
        })?;
        Ok(Some(meta))
    }

    /// Save metadata to the index directory (atomic write via temp file + rename).
    pub fn save(&self, index_dir: &Path) -> Result<()> {
        let path = index_dir.join(METADATA_FILE);
        let tmp = index_dir.join(".index_meta.json.tmp");
        let data = serde_json::to_string_pretty(self).map_err(|e| {
            crate::error::FlapjackError::InvalidQuery(format!("Serialize error: {}", e))
        })?;
        std::fs::write(&tmp, &data)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Load metadata or create + persist new metadata if none exists.
    pub fn load_or_create(index_dir: &Path) -> Result<Self> {
        if let Some(meta) = Self::load(index_dir)? {
            return Ok(meta);
        }
        let meta = Self::new();
        meta.save(index_dir)?;
        Ok(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_load_nonexistent_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = IndexMetadata::load(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let meta = IndexMetadata {
            created_at: "2026-02-25T12:00:00+00:00".to_string(),
            last_build_time_s: 42,
        };
        meta.save(dir.path()).unwrap();

        let loaded = IndexMetadata::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.created_at, "2026-02-25T12:00:00+00:00");
        assert_eq!(loaded.last_build_time_s, 42);
    }

    #[test]
    fn test_load_or_create_creates_new() {
        let dir = TempDir::new().unwrap();
        let meta = IndexMetadata::load_or_create(dir.path()).unwrap();
        assert!(!meta.created_at.is_empty());
        assert_eq!(meta.last_build_time_s, 0);

        // File should exist now
        assert!(dir.path().join(METADATA_FILE).exists());
    }

    #[test]
    fn test_load_or_create_loads_existing() {
        let dir = TempDir::new().unwrap();
        let original = IndexMetadata {
            created_at: "2025-01-01T00:00:00+00:00".to_string(),
            last_build_time_s: 10,
        };
        original.save(dir.path()).unwrap();

        let loaded = IndexMetadata::load_or_create(dir.path()).unwrap();
        assert_eq!(loaded.created_at, "2025-01-01T00:00:00+00:00");
        assert_eq!(loaded.last_build_time_s, 10);
    }

    #[test]
    fn test_default_last_build_time_s_when_missing() {
        let dir = TempDir::new().unwrap();
        // Write JSON without last_build_time_s field
        std::fs::write(
            dir.path().join(METADATA_FILE),
            r#"{"created_at": "2025-01-01T00:00:00+00:00"}"#,
        )
        .unwrap();

        let loaded = IndexMetadata::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.last_build_time_s, 0);
    }
}
