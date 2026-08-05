use super::acme::AcmeClient;
use super::config::SslConfig;
use crate::error::{FlapjackError, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

const FULLCHAIN_FILE_NAME: &str = "fullchain.pem";
const PRIVATE_KEY_FILE_NAME: &str = "privkey.pem";
const GENERATION_PREFIX: &str = ".flapjack-acme-";
const CURRENT_LINK_NAME: &str = "current";

#[cfg(test)]
use tests::MaterialPublicationFault;

pub struct SslManager {
    pub config: SslConfig,
    acme_client: Option<Arc<AcmeClient>>,
    last_check: Arc<RwLock<Option<DateTime<Utc>>>>,
    last_renewal: Arc<RwLock<Option<DateTime<Utc>>>>,
    renewal_status: Arc<RwLock<RenewalStatus>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenewalStatus {
    pub enabled: bool,
    pub status: String,
    pub error: Option<String>,
    pub cert_expires_in_days: Option<i64>,
    pub next_check: Option<DateTime<Utc>>,
}

impl Default for RenewalStatus {
    fn default() -> Self {
        Self {
            enabled: true,
            status: "initializing".to_string(),
            error: None,
            cert_expires_in_days: None,
            next_check: None,
        }
    }
}

impl SslManager {
    pub async fn new(config: SslConfig) -> Result<Arc<Self>> {
        tracing::info!(
            "[SSL] Initializing SSL manager for ACME identifier: {}",
            config.acme_identifier
        );

        let acme_client = Arc::new(
            AcmeClient::new(
                &config.email,
                &config.acme_directory,
                config.root_ca_pem.as_deref(),
            )
            .await?,
        );

        Ok(Arc::new(Self {
            config,
            acme_client: Some(acme_client),
            last_check: Arc::new(RwLock::new(None)),
            last_renewal: Arc::new(RwLock::new(None)),
            renewal_status: Arc::new(RwLock::new(RenewalStatus::default())),
        }))
    }

    pub async fn start_renewal_loop(self: Arc<Self>) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(
            self.config.check_interval_secs,
        ));

        loop {
            // Tokio's first tick is immediate, so a fresh node requests missing material at startup.
            interval.tick().await;
            tracing::info!("[SSL] Running certificate expiry check...");

            if let Err(e) = self.check_and_renew().await {
                tracing::error!("[SSL] Renewal check failed: {}", e);
                eprintln!("ALERT: SSL renewal check failed: {}", e);

                let mut status = self.renewal_status.write().await;
                status.status = "failed".to_string();
                status.error = Some(e.to_string());
            }

            let mut status = self.renewal_status.write().await;
            status.next_check =
                Some(Utc::now() + Duration::seconds(self.config.check_interval_secs as i64));
        }
    }

    async fn check_and_renew(&self) -> Result<()> {
        self.record_last_check().await;

        let cert_path = self.get_cert_path();
        if !cert_path.exists() || !self.private_key_is_usable() {
            return self.renew_incomplete_material(&cert_path).await;
        }

        let days_remaining = self.get_cert_expiry_days(&cert_path)?;
        self.record_certificate_expiry_status(days_remaining).await;
        tracing::info!("[SSL] Certificate expires in {} days", days_remaining);

        if self.certificate_needs_renewal(days_remaining) {
            return self.renew_expiring_certificate(days_remaining).await;
        }

        Ok(())
    }

    async fn renew_certificate(&self) -> Result<()> {
        {
            let mut status = self.renewal_status.write().await;
            status.status = "renewing".to_string();
        }

        let acme_client = self
            .acme_client
            .as_ref()
            .ok_or_else(|| FlapjackError::Ssl("ACME client not initialized".to_string()))?;

        tracing::info!("[SSL] Requesting new certificate from Let's Encrypt...");

        let (cert_pem, key_pem) = acme_client
            .request_certificate(&self.config.acme_identifier)
            .await?;

        self.publish_issued_certificate(&cert_pem, &key_pem, Utc::now())
            .await?;

        // L7 serves rotation from engine/flapjack-http/src/tls_serve.rs; this owner only publishes material.

        tracing::info!("[SSL] Certificate renewed successfully!");

        Ok(())
    }
    async fn publish_issued_certificate(
        &self,
        cert_pem: &str,
        key_pem: &str,
        renewed_at: DateTime<Utc>,
    ) -> Result<()> {
        let days_remaining =
            Self::certificate_expiry_days_from_pem(cert_pem.as_bytes(), renewed_at.timestamp())?;
        self.write_certificate_files(cert_pem, key_pem)?;
        *self.last_renewal.write().await = Some(renewed_at);

        let mut status = self.renewal_status.write().await;
        status.status = "ok".to_string();
        status.error = None;
        status.cert_expires_in_days = Some(days_remaining);
        Ok(())
    }

    fn write_certificate_files(&self, cert_pem: &str, key_pem: &str) -> Result<()> {
        let cert_path = self.get_cert_path();
        let cert_dir = cert_path.parent().ok_or_else(|| {
            FlapjackError::Ssl(format!(
                "Certificate path has no parent directory: {}",
                cert_path.display()
            ))
        })?;
        Self::write_certificate_files_to_dir(cert_dir, cert_pem, key_pem)
    }

    fn write_certificate_files_to_dir(
        cert_dir: &Path,
        cert_pem: &str,
        key_pem: &str,
    ) -> Result<()> {
        let parent = Self::material_parent(cert_dir)?;
        fs::create_dir_all(&parent).map_err(|e| {
            FlapjackError::Ssl(format!("Failed to create material parent directory: {}", e))
        })?;

        let previous_generation = Self::live_generation_name(cert_dir);
        let generation_dir = Self::stage_material_generation(&parent, cert_dir, cert_pem, key_pem)?;
        let publish_result = Self::publish_material_generation(cert_dir, &parent, &generation_dir);

        // Keep the prior generation so a reader that resolved it before the swap can open both files.
        // The next successful publication retires it, bounding retention to two complete pairs.
        if let Err(error) =
            Self::retire_superseded_generations(&parent, cert_dir, previous_generation.as_deref())
        {
            tracing::warn!(
                "[SSL] Certificate material retention failed after publication attempt: {}",
                error
            );
        }

        publish_result?;

        tracing::info!("[SSL] Certificate material written to {:?}", cert_dir);
        Ok(())
    }

    fn get_cert_path(&self) -> PathBuf {
        self.config.material_dir.join(FULLCHAIN_FILE_NAME)
    }

    fn private_key_is_usable(&self) -> bool {
        fs::read_to_string(self.config.material_dir.join(PRIVATE_KEY_FILE_NAME))
            .ok()
            .is_some_and(|pem| rcgen::KeyPair::from_pem(&pem).is_ok())
    }

    fn get_cert_expiry_days(&self, cert_path: &Path) -> Result<i64> {
        let cert_pem = fs::read(cert_path)
            .map_err(|e| FlapjackError::Ssl(format!("Failed to read certificate: {}", e)))?;
        Self::certificate_expiry_days_from_pem(&cert_pem, Utc::now().timestamp())
    }

    fn certificate_expiry_days_from_pem(cert_pem: &[u8], now_timestamp: i64) -> Result<i64> {
        use x509_parser::prelude::*;

        let (_, pem) = parse_x509_pem(cert_pem)
            .map_err(|e| FlapjackError::Ssl(format!("Failed to parse certificate PEM: {}", e)))?;

        let cert = pem
            .parse_x509()
            .map_err(|e| FlapjackError::Ssl(format!("Failed to parse X509 certificate: {}", e)))?;

        let seconds_remaining = cert.validity().not_after.timestamp() - now_timestamp;
        Ok(seconds_remaining / 86_400)
    }

    pub fn get_acme_client(&self) -> Option<Arc<AcmeClient>> {
        self.acme_client.clone()
    }

    pub async fn get_status(&self) -> RenewalStatus {
        self.renewal_status.read().await.clone()
    }

    async fn record_last_check(&self) {
        *self.last_check.write().await = Some(Utc::now());
    }

    async fn renew_incomplete_material(&self, cert_path: &PathBuf) -> Result<()> {
        tracing::warn!(path = ?cert_path, "[SSL] Certificate material is incomplete; requesting a new pair");
        self.renew_certificate().await
    }

    async fn record_certificate_expiry_status(&self, days_remaining: i64) {
        let mut status = self.renewal_status.write().await;
        status.cert_expires_in_days = Some(days_remaining);
        status.status = "ok".to_string();
        status.error = None;
    }

    fn certificate_needs_renewal(&self, days_remaining: i64) -> bool {
        days_remaining < self.config.renew_days_threshold as i64
    }

    async fn renew_expiring_certificate(&self, days_remaining: i64) -> Result<()> {
        tracing::warn!(
            "[SSL] Certificate expires in {} days (threshold: {}), renewing...",
            days_remaining,
            self.config.renew_days_threshold
        );
        self.renew_certificate().await
    }

    fn stage_material_generation(
        parent: &Path,
        cert_dir: &Path,
        cert_pem: &str,
        key_pem: &str,
    ) -> Result<PathBuf> {
        let generation_dir =
            Self::create_owner_private_dir(&Self::unique_path(parent, cert_dir, "generation"))?;
        if let Err(error) =
            Self::write_generation_contents(parent, &generation_dir, cert_pem, key_pem)
        {
            return Err(Self::retire_partial_generation(&generation_dir, error));
        }
        Ok(generation_dir)
    }

    fn write_generation_contents(
        parent: &Path,
        generation_dir: &Path,
        cert_pem: &str,
        key_pem: &str,
    ) -> Result<()> {
        Self::write_synced_file(
            &generation_dir.join(FULLCHAIN_FILE_NAME),
            cert_pem.as_bytes(),
            false,
        )?;
        Self::write_synced_file(
            &generation_dir.join(PRIVATE_KEY_FILE_NAME),
            key_pem.as_bytes(),
            true,
        )?;
        Self::sync_dir(generation_dir)?;
        Self::sync_dir(parent)?;
        Ok(())
    }

    fn retire_partial_generation(
        generation_dir: &Path,
        original_error: FlapjackError,
    ) -> FlapjackError {
        match fs::remove_dir_all(generation_dir) {
            Ok(()) => original_error,
            Err(cleanup_error) => FlapjackError::Ssl(format!(
                "{original_error}; failed to retire partial material generation: {cleanup_error}"
            )),
        }
    }

    #[cfg(unix)]
    fn publish_material_generation(
        cert_dir: &Path,
        parent: &Path,
        generation_dir: &Path,
    ) -> Result<()> {
        match fs::symlink_metadata(cert_dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Self::install_generation_link(cert_dir, parent, generation_dir)
            }
            Err(error) => Err(FlapjackError::Ssl(format!(
                "Failed to inspect material path: {}",
                error
            ))),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Self::swap_generation_link(cert_dir, parent, generation_dir)
            }
            Ok(metadata) if metadata.is_dir() => {
                Self::republish_real_directory(cert_dir, generation_dir)
            }
            Ok(_) => Err(FlapjackError::Ssl(format!(
                "Material path exists and is not a directory: {}",
                cert_dir.display()
            ))),
        }
    }

    #[cfg(unix)]
    fn install_generation_link(
        cert_dir: &Path,
        parent: &Path,
        generation_dir: &Path,
    ) -> Result<()> {
        Self::symlink_sibling(generation_dir, cert_dir)?;
        Self::sync_dir(parent)
    }

    #[cfg(unix)]
    fn swap_generation_link(cert_dir: &Path, parent: &Path, generation_dir: &Path) -> Result<()> {
        let staging_link = Self::unique_path(parent, cert_dir, "next");
        Self::symlink_sibling(generation_dir, &staging_link)?;
        Self::sync_dir(parent)?;
        Self::rename_over(&staging_link, cert_dir)?;
        Self::sync_dir(parent)
    }

    #[cfg(unix)]
    fn republish_real_directory(cert_dir: &Path, generation_dir: &Path) -> Result<()> {
        Self::retire_internal_staging_links(cert_dir)?;
        let current_link = cert_dir.join(CURRENT_LINK_NAME);
        if Self::is_symlink(&current_link) {
            Self::validate_managed_real_directory(cert_dir, &current_link)?;
        } else {
            Self::adopt_real_directory_into_generations(cert_dir, &current_link)?;
        }
        Self::place_symlink_atomically(
            cert_dir,
            &current_link,
            &Self::parent_relative_target(generation_dir)?,
        )?;
        Self::sync_dir(cert_dir)
    }

    #[cfg(unix)]
    fn validate_managed_real_directory(cert_dir: &Path, current_link: &Path) -> Result<()> {
        let current_target = fs::read_link(current_link).map_err(|e| {
            FlapjackError::Ssl(format!("Failed to read current material link: {}", e))
        })?;
        let generation_name = current_target.file_name().ok_or_else(|| {
            FlapjackError::Ssl(format!(
                "Managed material current link has no generation name: {}",
                current_link.display()
            ))
        })?;
        if !Self::is_owner_generation_name(cert_dir, generation_name) {
            return Err(FlapjackError::Ssl(format!(
                "Managed material current link points outside this owner namespace: {}",
                current_link.display()
            )));
        }
        for file_name in [FULLCHAIN_FILE_NAME, PRIVATE_KEY_FILE_NAME] {
            Self::repair_or_validate_visible_material_link(cert_dir, file_name)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn repair_or_validate_visible_material_link(cert_dir: &Path, file_name: &str) -> Result<()> {
        let visible = cert_dir.join(file_name);
        let expected = Path::new(CURRENT_LINK_NAME).join(file_name);
        match fs::read_link(&visible) {
            Ok(actual) if actual == expected => return Ok(()),
            Ok(actual) => {
                return Err(FlapjackError::Ssl(format!(
                    "Managed material file points at {}, expected {}",
                    actual.display(),
                    expected.display()
                )))
            }
            Err(_)
                if fs::symlink_metadata(&visible)
                    .map(|metadata| metadata.is_file())
                    .unwrap_or(false) => {}
            Err(error) => {
                return Err(FlapjackError::Ssl(format!(
                    "Managed material file must link through current: {}: {}",
                    visible.display(),
                    error
                )))
            }
        }

        let current_file = cert_dir.join(&expected);
        let visible_content = fs::read(&visible).map_err(|error| {
            FlapjackError::Ssl(format!(
                "Failed to read partially adopted material file {}: {error}",
                visible.display()
            ))
        })?;
        let current_content = fs::read(&current_file).map_err(|error| {
            FlapjackError::Ssl(format!(
                "Failed to read current material file {}: {error}",
                current_file.display()
            ))
        })?;
        if visible_content != current_content {
            return Err(FlapjackError::Ssl(format!(
                "Managed material file differs from recoverable current content: {}",
                visible.display()
            )));
        }
        Self::place_symlink_atomically(cert_dir, &visible, &expected)
    }

    #[cfg(unix)]
    fn adopt_real_directory_into_generations(cert_dir: &Path, current_link: &Path) -> Result<()> {
        let parent = Self::material_parent(cert_dir)?;
        let boot_dir =
            Self::create_owner_private_dir(&Self::unique_path(&parent, cert_dir, "boot"))?;
        for file_name in [FULLCHAIN_FILE_NAME, PRIVATE_KEY_FILE_NAME] {
            let existing = cert_dir.join(file_name);
            if existing.exists() {
                fs::hard_link(&existing, boot_dir.join(file_name)).map_err(|e| {
                    FlapjackError::Ssl(format!("Failed to snapshot existing material: {}", e))
                })?;
            }
        }
        Self::sync_dir(&boot_dir)?;
        Self::sync_dir(&parent)?;

        Self::place_symlink_atomically(
            cert_dir,
            current_link,
            &Self::parent_relative_target(&boot_dir)?,
        )?;
        for file_name in [FULLCHAIN_FILE_NAME, PRIVATE_KEY_FILE_NAME] {
            let visible = cert_dir.join(file_name);
            let through_current = PathBuf::from(CURRENT_LINK_NAME).join(file_name);
            Self::place_symlink_atomically(cert_dir, &visible, &through_current)?;
        }
        Self::sync_dir(cert_dir)
    }

    fn retire_superseded_generations(
        parent: &Path,
        cert_dir: &Path,
        previous_generation: Option<&OsStr>,
    ) -> Result<()> {
        #[cfg(test)]
        Self::maybe_fail_publication(MaterialPublicationFault::Retention)?;
        let live = Self::live_generation_name(cert_dir);
        let entries = fs::read_dir(parent).map_err(|e| {
            FlapjackError::Ssl(format!("Failed to read material parent directory: {}", e))
        })?;
        for entry in entries {
            let entry = entry.map_err(|e| {
                FlapjackError::Ssl(format!("Failed to read material directory entry: {}", e))
            })?;
            let name = entry.file_name();
            if !Self::is_owner_generation_name(cert_dir, &name) {
                continue;
            }
            if live.as_deref() == Some(name.as_os_str()) {
                continue;
            }
            if previous_generation == Some(name.as_os_str()) {
                continue;
            }
            Self::remove_generation_entry(&entry)?;
        }
        Self::sync_dir(parent)
    }

    fn remove_generation_entry(entry: &fs::DirEntry) -> Result<()> {
        let path = entry.path();
        let is_dir = entry
            .file_type()
            .map(|file_type| file_type.is_dir())
            .unwrap_or(false);
        let removal = if is_dir {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        removal.map_err(|e| {
            FlapjackError::Ssl(format!("Failed to retire superseded generation: {}", e))
        })
    }

    fn live_generation_name(cert_dir: &Path) -> Option<OsString> {
        if let Ok(target) = fs::read_link(cert_dir) {
            return target.file_name().map(OsStr::to_os_string);
        }
        if let Ok(target) = fs::read_link(cert_dir.join(CURRENT_LINK_NAME)) {
            return target.file_name().map(OsStr::to_os_string);
        }
        None
    }

    #[cfg(not(unix))]
    fn publish_material_generation(
        cert_dir: &Path,
        parent: &Path,
        generation_dir: &Path,
    ) -> Result<()> {
        match cert_dir.try_exists() {
            Ok(false) => {}
            Ok(true) => {
                return Err(FlapjackError::Ssl(format!(
                    "Atomic replacement of an existing ACME material directory is unsupported \
                 on this platform; leaving the visible pair unchanged: {}",
                    cert_dir.display()
                )))
            }
            Err(error) => {
                return Err(FlapjackError::Ssl(format!(
                    "Failed to inspect material path: {error}"
                )))
            }
        }
        fs::rename(generation_dir, cert_dir).map_err(|e| {
            FlapjackError::Ssl(format!("Failed to publish material directory: {}", e))
        })?;
        Self::sync_dir(parent)
    }

    fn create_owner_private_dir(path: &Path) -> Result<PathBuf> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path).map_err(|e| {
            FlapjackError::Ssl(format!(
                "Failed to create material generation directory: {}",
                e
            ))
        })?;
        Ok(path.to_path_buf())
    }

    #[cfg(unix)]
    fn symlink_sibling(target: &Path, link_path: &Path) -> Result<()> {
        std::os::unix::fs::symlink(Self::file_name_of(target)?, link_path)
            .map_err(|e| FlapjackError::Ssl(format!("Failed to stage material link: {}", e)))
    }

    #[cfg(unix)]
    fn place_symlink_atomically(dir: &Path, final_path: &Path, target: &Path) -> Result<()> {
        let staging = Self::unique_path(dir, final_path, "staging");
        std::os::unix::fs::symlink(target, &staging)
            .map_err(|e| FlapjackError::Ssl(format!("Failed to stage material link: {}", e)))?;
        let publish_result = (|| {
            #[cfg(test)]
            Self::maybe_fail_publication(MaterialPublicationFault::LinkSwapAfterStaging)?;
            Self::sync_dir(dir)?;
            Self::rename_over(&staging, final_path)
        })();
        match publish_result {
            Ok(()) => Ok(()),
            Err(error) => Err(Self::retire_staging_link(&staging, error)),
        }
    }

    #[cfg(unix)]
    fn retire_staging_link(staging: &Path, original_error: FlapjackError) -> FlapjackError {
        match fs::remove_file(staging) {
            Ok(()) => original_error,
            Err(error) if error.kind() == io::ErrorKind::NotFound => original_error,
            Err(error) => FlapjackError::Ssl(format!(
                "{original_error}; failed to retire staged material link: {error}"
            )),
        }
    }

    #[cfg(unix)]
    fn retire_internal_staging_links(cert_dir: &Path) -> Result<()> {
        let mut removed_any = false;
        for entry in fs::read_dir(cert_dir).map_err(|error| {
            FlapjackError::Ssl(format!("Failed to inspect material directory: {error}"))
        })? {
            let entry = entry.map_err(|error| {
                FlapjackError::Ssl(format!(
                    "Failed to inspect material directory entry: {error}"
                ))
            })?;
            let is_stale_owner_link = entry
                .file_type()
                .map(|file_type| file_type.is_symlink())
                .unwrap_or(false)
                && Self::is_internal_staging_link_name(&entry.file_name());
            if is_stale_owner_link {
                fs::remove_file(entry.path()).map_err(|error| {
                    FlapjackError::Ssl(format!("Failed to retire staged material link: {error}"))
                })?;
                removed_any = true;
            }
        }
        if removed_any {
            Self::sync_dir(cert_dir)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn is_internal_staging_link_name(name: &OsStr) -> bool {
        [
            CURRENT_LINK_NAME,
            FULLCHAIN_FILE_NAME,
            PRIVATE_KEY_FILE_NAME,
        ]
        .iter()
        .any(|label| Self::is_owner_artifact_name(Path::new(label), name, &["staging"]))
    }

    #[cfg(unix)]
    fn rename_over(from: &Path, to: &Path) -> Result<()> {
        fs::rename(from, to).map_err(|e| {
            let _ = fs::remove_file(from);
            FlapjackError::Ssl(format!("Failed to publish material link: {}", e))
        })
    }

    #[cfg(unix)]
    fn is_symlink(path: &Path) -> bool {
        fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
    }

    #[cfg(unix)]
    fn parent_relative_target(generation_dir: &Path) -> Result<PathBuf> {
        Ok(Path::new("..").join(Self::file_name_of(generation_dir)?))
    }

    #[cfg(unix)]
    fn file_name_of(path: &Path) -> Result<&OsStr> {
        path.file_name().ok_or_else(|| {
            FlapjackError::Ssl(format!(
                "Material path has no file name: {}",
                path.display()
            ))
        })
    }

    fn write_synced_file(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
        #[cfg(test)]
        {
            if path.file_name() == Some(OsStr::new(FULLCHAIN_FILE_NAME)) {
                Self::maybe_fail_publication(MaterialPublicationFault::CertificateWrite)?;
            }
            if path.file_name() == Some(OsStr::new(PRIVATE_KEY_FILE_NAME)) {
                Self::maybe_fail_publication(MaterialPublicationFault::PrivateKeyWrite)?;
            }
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .map_err(|e| FlapjackError::Ssl(format!("Failed to create material file: {}", e)))?;
        file.write_all(bytes)
            .map_err(|e| FlapjackError::Ssl(format!("Failed to write material file: {}", e)))?;
        #[cfg(test)]
        Self::maybe_fail_publication(MaterialPublicationFault::FileSync)?;
        file.sync_all()
            .map_err(|e| FlapjackError::Ssl(format!("Failed to sync material file: {}", e)))
    }

    fn sync_dir(path: &Path) -> Result<()> {
        #[cfg(test)]
        {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(GENERATION_PREFIX))
            {
                Self::maybe_fail_publication(MaterialPublicationFault::GenerationSync)?;
            } else {
                Self::maybe_fail_publication(MaterialPublicationFault::ParentSync)?;
            }
        }
        #[cfg(unix)]
        {
            File::open(path)
                .and_then(|directory| directory.sync_all())
                .map_err(|e| FlapjackError::Ssl(format!("Failed to sync directory: {}", e)))
        }
        #[cfg(not(unix))]
        {
            // Rust has no portable directory fsync; file contents are synced before publication.
            let _ = path;
            Ok(())
        }
    }

    fn material_parent(cert_dir: &Path) -> Result<PathBuf> {
        cert_dir.parent().map(Path::to_path_buf).ok_or_else(|| {
            FlapjackError::Ssl(format!(
                "Material directory has no parent: {}",
                cert_dir.display()
            ))
        })
    }

    fn unique_path(dir: &Path, labelled_after: &Path, suffix: &str) -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        dir.join(format!(
            "{}{timestamp}-{}-{suffix}",
            Self::owner_generation_prefix(labelled_after),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn owner_generation_prefix(cert_dir: &Path) -> String {
        let label = cert_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("material");
        format!("{GENERATION_PREFIX}{label}-")
    }

    fn is_owner_generation_name(cert_dir: &Path, generation_name: &OsStr) -> bool {
        Self::is_owner_artifact_name(cert_dir, generation_name, &["generation", "boot", "next"])
    }

    fn is_owner_artifact_name(cert_dir: &Path, artifact_name: &OsStr, suffixes: &[&str]) -> bool {
        let name = artifact_name.to_string_lossy();
        let Some(generation_suffix) = name.strip_prefix(&Self::owner_generation_prefix(cert_dir))
        else {
            return false;
        };
        let mut parts = generation_suffix.split('-');
        let (timestamp, sequence, suffix, trailing) =
            (parts.next(), parts.next(), parts.next(), parts.next());
        matches!((timestamp, sequence, suffix, trailing),
            (Some(timestamp), Some(sequence), Some(suffix), None)
                if timestamp.parse::<u128>().is_ok()
                    && sequence.parse::<u64>().is_ok()
                    && suffixes.contains(&suffix))
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
