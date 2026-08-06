//! Pure, HTTP-agnostic snapshot byte operations shared by the snapshot HTTP
//! handlers and the background snapshot tasks.
//!
//! Nothing in this module depends on `axum` or on `handlers::snapshot`, so
//! background tasks can reuse the exact export read path without pulling in
//! response types. Callers are responsible for quiescing the tenant first and
//! for running these synchronous gzip/tar operations inside `spawn_blocking` so
//! they never block the async worker pool.

use flapjack::error::FlapjackError;
use flapjack::index::snapshot::export_to_bytes;
use std::path::Path;

const SNAPSHOT_EXPORT_MAX_ATTEMPTS: usize = 3;

/// Export a tenant's on-disk index directory to compressed snapshot bytes,
/// retrying transient IO churn.
///
/// Records the `snapshot_export_read` writer-lifecycle checkpoint so tests can
/// prove the tenant was merge-quiesced before the read. The caller MUST have
/// quiesced the tenant through [`flapjack::IndexManager::quiesce_tenant`] first
/// and MUST run this inside `spawn_blocking` — the gzip/tar work is synchronous
/// and CPU-bound.
pub(crate) fn export_snapshot_bytes(
    index_path: &Path,
    tenant_id: &str,
) -> Result<Vec<u8>, FlapjackError> {
    #[cfg(any(debug_assertions, test))]
    flapjack::index::write_queue::record_writer_lifecycle_publication_checkpoint(
        tenant_id,
        "snapshot_export_read",
    );
    #[cfg(not(any(debug_assertions, test)))]
    let _ = tenant_id;
    export_with_retry(|| export_to_bytes(index_path))
}

fn should_retry_export_error(error: &FlapjackError) -> bool {
    matches!(error, FlapjackError::Io(_))
}

/// Retry a snapshot export a bounded number of times, but only for transient
/// IO errors (file churn during a concurrent merge/GC). Non-IO errors are
/// returned immediately because they are not transient.
pub(crate) fn export_with_retry(
    mut export_once: impl FnMut() -> Result<Vec<u8>, FlapjackError>,
) -> Result<Vec<u8>, FlapjackError> {
    let mut attempt = 1usize;
    loop {
        match export_once() {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                if attempt >= SNAPSHOT_EXPORT_MAX_ATTEMPTS || !should_retry_export_error(&error) {
                    return Err(error);
                }
                tracing::warn!(
                    attempt,
                    max_attempts = SNAPSHOT_EXPORT_MAX_ATTEMPTS,
                    error = %error,
                    "Transient snapshot export failed; retrying"
                );
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::export_with_retry;
    use flapjack::FlapjackError;

    #[test]
    fn export_with_retry_retries_transient_io_errors() {
        let mut attempts = 0usize;
        let bytes = export_with_retry(|| {
            attempts += 1;
            if attempts < 3 {
                Err(FlapjackError::Io("transient".to_string()))
            } else {
                Ok(vec![1, 2, 3])
            }
        })
        .expect("third attempt should succeed");
        assert_eq!(bytes, vec![1, 2, 3]);
        assert_eq!(attempts, 3, "must retry transient IO errors");
    }

    #[test]
    fn export_with_retry_does_not_retry_non_io_errors() {
        let mut attempts = 0usize;
        let error = export_with_retry(|| {
            attempts += 1;
            Err(FlapjackError::Config("not transient".to_string()))
        })
        .expect_err("non-IO errors should fail immediately");
        assert!(matches!(error, FlapjackError::Config(_)));
        assert_eq!(
            attempts, 1,
            "non-IO errors should not be retried because they are not transient file-churn failures"
        );
    }
}
