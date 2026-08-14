//! IP-allowlist security sources with persistent JSON-backed store, cached CIDR matcher, and Axum middleware for request filtering.
use crate::error_response::json_error;
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use flapjack::error::FlapjackError;
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct SecuritySourceEntry {
    pub source: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone)]
pub struct SecuritySourcesStore {
    file_path: PathBuf,
}

impl SecuritySourcesStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            file_path: data_dir.join("security_sources.json"),
        }
    }

    pub fn list(&self) -> Result<Vec<SecuritySourceEntry>, FlapjackError> {
        if !self.file_path.exists() {
            return Ok(Vec::new());
        }

        let bytes = std::fs::read(&self.file_path)?;
        if bytes.is_empty() {
            return Ok(Vec::new());
        }

        let entries: Vec<SecuritySourceEntry> = serde_json::from_slice(&bytes)?;
        Ok(entries)
    }

    pub fn replace(&self, entries: Vec<SecuritySourceEntry>) -> Result<(), FlapjackError> {
        let mut normalized = Vec::with_capacity(entries.len());
        for entry in entries {
            upsert_entry(&mut normalized, normalize_entry(entry)?);
        }
        self.save(&normalized)
    }

    pub fn append(&self, entry: SecuritySourceEntry) -> Result<(), FlapjackError> {
        let mut existing = self.list()?;
        upsert_entry(&mut existing, normalize_entry(entry)?);
        self.save(&existing)
    }

    pub fn delete(&self, source: &str) -> Result<(), FlapjackError> {
        let normalized_source = normalize_cidr(source)?;
        let mut existing = self.list()?;
        existing.retain(|entry| entry.source != normalized_source);
        self.save(&existing)
    }

    fn save(&self, entries: &[SecuritySourceEntry]) -> Result<(), FlapjackError> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let data = serde_json::to_vec_pretty(entries)?;
        flapjack::index::atomic_write_file(&self.file_path, &data)?;
        Ok(())
    }

    fn file_modified_time(&self) -> Result<Option<SystemTime>, FlapjackError> {
        if !self.file_path.exists() {
            return Ok(None);
        }
        let metadata = std::fs::metadata(&self.file_path)?;
        Ok(metadata.modified().ok())
    }
}

#[derive(Default)]
struct AllowlistCache {
    file_modified: Option<SystemTime>,
    networks: Vec<IpNet>,
}

/// Cached source-allowlist matcher used by request middleware.
///
/// Tracks file mtime to avoid reparsing unchanged allowlists. Always checks
/// the filesystem on each call (stat is cheap) to ensure security-critical
/// changes are visible immediately.
#[derive(Clone)]
pub struct SecuritySourcesMatcher {
    store: SecuritySourcesStore,
    cache: Arc<RwLock<AllowlistCache>>,
}

impl SecuritySourcesMatcher {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            store: SecuritySourcesStore::new(data_dir),
            cache: Arc::new(RwLock::new(AllowlistCache::default())),
        }
    }

    pub fn allows_ip(&self, ip: IpAddr) -> Result<bool, FlapjackError> {
        let networks = self.current_networks()?;
        if networks.is_empty() {
            return Ok(true);
        }
        Ok(networks.iter().any(|cidr| cidr.contains(&ip)))
    }

    /// Reload and cache the parsed CIDR networks from disk when the backing file's mtime has changed.
    ///
    /// Uses a double-checked locking pattern: a read lock checks the cached mtime, and only acquires a write lock when the file has been modified. Returns an empty `Vec` (allow-all) when the file does not exist.
    ///
    /// # Returns
    ///
    /// The current set of parsed `IpNet` entries, or an error if the file contains an unparseable CIDR.
    fn current_networks(&self) -> Result<Vec<IpNet>, FlapjackError> {
        let file_modified = self.store.file_modified_time()?;

        // Fast path: read lock, return cached if file mtime unchanged.
        {
            let cache = self.cache.read().expect("allowlist cache lock poisoned");
            if cache.file_modified == file_modified {
                return Ok(cache.networks.clone());
            }
        }

        // Slow path: write lock, reload from disk.
        let mut cache = self.cache.write().expect("allowlist cache lock poisoned");

        // Double-check after acquiring write lock (another thread may have reloaded).
        if cache.file_modified == file_modified {
            return Ok(cache.networks.clone());
        }

        let entries = self.store.list()?;
        let networks = entries
            .iter()
            .map(|entry| {
                entry.source.parse::<IpNet>().map_err(|_| {
                    FlapjackError::Io(format!(
                        "Invalid CIDR persisted in security sources store: {}",
                        entry.source
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        cache.networks = networks.clone();
        cache.file_modified = file_modified;
        Ok(networks)
    }
}

/// Extract client IP for security-sources allowlist matching.
///
/// Keep this path aligned with global client-IP extraction so allowlist,
/// rate-limit, and geo/analytics resolve proxy chains the same way.
fn extract_allowlist_client_ip(request: &Request) -> IpAddr {
    crate::middleware::extract_client_ip(request)
}

/// Axum middleware that rejects requests whose client IP is not in the security-sources allowlist.
///
/// Extracts the client IP via the shared proxy-aware resolver, then delegates to `SecuritySourcesMatcher::allows_ip`. Returns 403 Forbidden with a JSON body when the IP is not allowed, or forwards the request otherwise.
///
/// # Arguments
///
/// * `request` - The inbound HTTP request.
/// * `next` - The remaining middleware/handler chain.
/// * `matcher` - Shared allowlist matcher instance.
pub async fn enforce_security_sources(
    request: Request,
    next: Next,
    matcher: &Arc<SecuritySourcesMatcher>,
) -> Response {
    let client_ip = extract_allowlist_client_ip(&request);
    match matcher.allows_ip(client_ip) {
        Ok(true) => next.run(request).await,
        Ok(false) => json_error(StatusCode::FORBIDDEN, "Forbidden"),
        Err(err) => err.into_response(),
    }
}

pub fn normalize_cidr(raw: &str) -> Result<String, FlapjackError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(FlapjackError::InvalidQuery(
            "source CIDR is required".to_string(),
        ));
    }

    let parsed: IpNet = trimmed
        .parse()
        .map_err(|_| FlapjackError::InvalidQuery(format!("Invalid source CIDR: {}", raw)))?;
    Ok(parsed.trunc().to_string())
}

fn normalize_entry(entry: SecuritySourceEntry) -> Result<SecuritySourceEntry, FlapjackError> {
    Ok(SecuritySourceEntry {
        source: normalize_cidr(&entry.source)?,
        description: entry.description,
    })
}

fn upsert_entry(entries: &mut Vec<SecuritySourceEntry>, candidate: SecuritySourceEntry) {
    if let Some(existing) = entries
        .iter_mut()
        .find(|entry| entry.source == candidate.source)
    {
        existing.description = candidate.description;
        return;
    }
    entries.push(candidate);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_cidr_rejects_invalid_values() {
        let err = normalize_cidr("not-a-cidr").expect_err("invalid CIDR must fail");
        assert!(
            matches!(err, FlapjackError::InvalidQuery(_)),
            "expected InvalidQuery, got {err:?}"
        );
    }

    #[test]
    fn normalize_cidr_canonicalizes_valid_values() {
        let normalized = normalize_cidr("10.0.0.42/24").expect("CIDR should parse");
        assert_eq!(normalized, "10.0.0.0/24");
    }

    /// Verify that appending the same CIDR twice updates the description without creating a duplicate entry.
    #[test]
    fn append_is_idempotent_for_same_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecuritySourcesStore::new(dir.path());

        store
            .append(SecuritySourceEntry {
                source: "10.1.0.0/16".to_string(),
                description: "corp".to_string(),
            })
            .unwrap();
        store
            .append(SecuritySourceEntry {
                source: "10.1.0.0/16".to_string(),
                description: "corp-updated".to_string(),
            })
            .unwrap();

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "10.1.0.0/16");
        assert_eq!(entries[0].description, "corp-updated");
    }

    /// Verify that `replace` normalizes overlapping CIDRs into a single entry, keeping the last-seen description.
    #[test]
    fn replace_dedupes_payload_and_keeps_latest_description() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecuritySourcesStore::new(dir.path());

        store
            .replace(vec![
                SecuritySourceEntry {
                    source: "192.168.0.0/24".to_string(),
                    description: "hq".to_string(),
                },
                SecuritySourceEntry {
                    source: "192.168.0.1/24".to_string(),
                    description: "hq-updated".to_string(),
                },
            ])
            .unwrap();

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "192.168.0.0/24");
        assert_eq!(entries[0].description, "hq-updated");
    }

    /// Verify that deleting a source not present in the store succeeds without altering existing entries.
    #[test]
    fn delete_is_noop_when_source_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecuritySourcesStore::new(dir.path());

        store
            .replace(vec![SecuritySourceEntry {
                source: "172.16.0.0/16".to_string(),
                description: "vpn".to_string(),
            }])
            .unwrap();

        store.delete("203.0.113.0/24").unwrap();

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "172.16.0.0/16");
    }

    /// Verify that entries written by one `SecuritySourcesStore` instance are readable by a new instance pointed at the same directory.
    #[test]
    fn persisted_entries_survive_store_recreation() {
        let dir = tempfile::tempdir().unwrap();

        let first = SecuritySourcesStore::new(dir.path());
        first
            .append(SecuritySourceEntry {
                source: "10.9.0.0/16".to_string(),
                description: "temp".to_string(),
            })
            .unwrap();

        let second = SecuritySourcesStore::new(dir.path());
        let entries = second.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "10.9.0.0/16");
    }

    /// Verify that the atomic write-then-rename leaves no `.tmp` file on disk after completion.
    #[test]
    fn atomic_write_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecuritySourcesStore::new(dir.path());

        store
            .replace(vec![SecuritySourceEntry {
                source: "10.0.0.0/24".to_string(),
                description: "office".to_string(),
            }])
            .unwrap();

        assert!(
            !dir.path().join("security_sources.json.tmp").exists(),
            "temp file should be cleaned up after atomic write"
        );
    }

    #[test]
    fn matcher_allows_all_when_allowlist_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let matcher = SecuritySourcesMatcher::new(dir.path());

        let allowed = matcher
            .allows_ip("203.0.113.7".parse().unwrap())
            .expect("empty allowlist check");
        assert!(allowed, "empty allowlist must allow all");
    }

    /// Verify that `SecuritySourcesMatcher` picks up on-disk allowlist changes without requiring a new instance.
    #[test]
    fn matcher_refreshes_after_disk_update_without_restart() {
        let dir = tempfile::tempdir().unwrap();
        let matcher = SecuritySourcesMatcher::new(dir.path());
        let store = SecuritySourcesStore::new(dir.path());

        store
            .replace(vec![SecuritySourceEntry {
                source: "10.0.0.0/8".to_string(),
                description: "corp".to_string(),
            }])
            .unwrap();
        assert!(
            matcher.allows_ip("10.2.3.4".parse().unwrap()).unwrap(),
            "first allowlist should allow 10.0.0.0/8"
        );
        assert!(
            !matcher.allows_ip("203.0.113.7".parse().unwrap()).unwrap(),
            "first allowlist should reject external IP"
        );

        store
            .replace(vec![SecuritySourceEntry {
                source: "203.0.113.0/24".to_string(),
                description: "external".to_string(),
            }])
            .unwrap();

        assert!(
            matcher.allows_ip("203.0.113.7".parse().unwrap()).unwrap(),
            "matcher must observe updated allowlist without restart"
        );
        assert!(
            !matcher.allows_ip("10.2.3.4".parse().unwrap()).unwrap(),
            "old network should no longer match after update"
        );
    }

    /// Verify that client-IP extraction picks the rightmost untrusted hop from `X-Forwarded-For` when the direct peer is a trusted proxy.
    #[test]
    fn allowlist_ip_uses_rightmost_untrusted_xff_with_trusted_peer() {
        use axum::http::Request as HttpRequest;
        use std::net::SocketAddr;
        use std::sync::Arc;

        let mut req = HttpRequest::builder()
            .uri("/1/indexes")
            .header("x-forwarded-for", "203.0.113.9, 10.2.2.2")
            .header("x-real-ip", "10.2.2.2")
            .body(axum::body::Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo::<SocketAddr>(
                "127.0.0.1:7700".parse().unwrap(),
            ));
        req.extensions_mut().insert(Arc::new(
            crate::middleware::TrustedProxyMatcher::from_csv("127.0.0.0/8")
                .expect("trusted proxy CIDR"),
        ));
        let ip = extract_allowlist_client_ip(&req);
        assert_eq!(ip, "10.2.2.2".parse::<IpAddr>().unwrap());
    }

    /// Verify that client-IP extraction falls back to `X-Real-IP` when no `X-Forwarded-For` header is present and the peer is trusted.
    #[test]
    fn allowlist_ip_falls_back_to_x_real_ip_with_trusted_peer() {
        use axum::http::Request as HttpRequest;
        use std::net::SocketAddr;
        use std::sync::Arc;

        let mut req = HttpRequest::builder()
            .uri("/1/indexes")
            .header("x-real-ip", "192.168.1.5")
            .body(axum::body::Body::empty())
            .unwrap();
        req.extensions_mut()
            .insert(axum::extract::ConnectInfo::<SocketAddr>(
                "127.0.0.1:7700".parse().unwrap(),
            ));
        req.extensions_mut().insert(Arc::new(
            crate::middleware::TrustedProxyMatcher::from_csv("127.0.0.0/8")
                .expect("trusted proxy CIDR"),
        ));
        let ip = extract_allowlist_client_ip(&req);
        assert_eq!(ip, "192.168.1.5".parse::<IpAddr>().unwrap());
    }
}
