use std::path::PathBuf;
use std::sync::Arc;

use crate::startup::StartupPersistenceMode;

/// Resolved SSL material configuration for the background SSL tasks.
///
/// Renewal and observation have different prerequisites: issuing new material
/// needs the ACME manager, while observing material already on disk needs only
/// its configured directory.
pub(crate) struct ConfiguredSslMaterial {
    pub(crate) manager: Option<Arc<flapjack::SslManager>>,
    pub(crate) material_dir: PathBuf,
    /// Exact validated config retained when a release fence forbids ACME
    /// account/certificate effects during startup. The background owner
    /// initializes it only after the persisted fence is released.
    pub(crate) deferred_config: Option<flapjack::SslConfig>,
}

/// Resolves SSL material configuration and, when reachable, the ACME manager.
#[cfg(test)]
pub(crate) async fn initialize_ssl_material() -> Option<ConfiguredSslMaterial> {
    initialize_ssl_material_for_mode(StartupPersistenceMode::Ordinary).await
}

pub(crate) async fn initialize_ssl_material_for_mode(
    persistence_mode: StartupPersistenceMode,
) -> Option<ConfiguredSslMaterial> {
    let ssl_config = match flapjack::SslConfig::from_env() {
        Ok(ssl_config) => ssl_config,
        Err(error) => return initialize_observer_only_ssl_material(error.to_string()),
    };
    log_ssl_management_enabled(&ssl_config);
    let material_dir = ssl_config.material_dir.clone();
    if persistence_mode == StartupPersistenceMode::FenceActive {
        tracing::info!(
            material_dir = %material_dir.display(),
            "[SSL] Release fence active; deferring ACME initialization"
        );
        return Some(ConfiguredSslMaterial {
            manager: None,
            material_dir,
            deferred_config: Some(ssl_config),
        });
    }
    let manager = match flapjack::SslManager::new(ssl_config).await {
        Ok(manager) => {
            flapjack_ssl::set_global_manager(Arc::clone(&manager));
            Some(manager)
        }
        Err(error) => {
            tracing::error!(
                "[SSL] Failed to initialize SSL manager; auto-renewal is unavailable but \
                 published material at {} is still observed: {}",
                material_dir.display(),
                error
            );
            None
        }
    };
    Some(ConfiguredSslMaterial {
        manager,
        material_dir,
        deferred_config: None,
    })
}

fn initialize_observer_only_ssl_material(error: String) -> Option<ConfiguredSslMaterial> {
    let material_config = match flapjack_ssl::SslMaterialConfig::explicit_from_env() {
        Some(material_config) => material_config,
        None => {
            tracing::info!("[SSL] SSL management disabled: {}", error);
            return None;
        }
    };
    tracing::info!(
        material_dir = %material_config.material_dir.display(),
        "[SSL] Auto-renewal disabled but managed certificate material will be observed: {}",
        error
    );
    Some(ConfiguredSslMaterial {
        manager: None,
        material_dir: material_config.material_dir,
        deferred_config: None,
    })
}

fn log_ssl_management_enabled(config: &flapjack::SslConfig) {
    tracing::info!(
        acme_identifier = %config.acme_identifier,
        "[SSL] SSL management enabled"
    );
}

#[cfg(test)]
mod tests {
    use super::{
        initialize_ssl_material, initialize_ssl_material_for_mode, log_ssl_management_enabled,
    };
    use crate::startup::StartupPersistenceMode;
    use crate::test_helpers::{EnvVarRestoreGuard, SharedLogBuffer, ENV_MUTEX};
    use serde_json::Value;
    use tracing_subscriber::layer::SubscriberExt;

    fn remove_ssl_env() -> Vec<EnvVarRestoreGuard> {
        vec![
            EnvVarRestoreGuard::remove("FLAPJACK_SSL_EMAIL"),
            EnvVarRestoreGuard::remove("FLAPJACK_PUBLIC_IP"),
            EnvVarRestoreGuard::remove("FLAPJACK_SSL_DOMAIN"),
            EnvVarRestoreGuard::remove("FLAPJACK_ACME_DIRECTORY"),
            EnvVarRestoreGuard::remove("FLAPJACK_ACME_MATERIAL_DIR"),
            EnvVarRestoreGuard::remove("FLAPJACK_ACME_ROOT_CA_PEM"),
        ]
    }

    #[tokio::test]
    async fn configured_material_dir_is_observed_when_renewal_email_is_missing() {
        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _clean_env = remove_ssl_env();
        let material_dir = tempfile::TempDir::new().unwrap();
        let _material_dir = EnvVarRestoreGuard::set(
            "FLAPJACK_ACME_MATERIAL_DIR",
            material_dir.path().to_str().unwrap(),
        );
        let _domain = EnvVarRestoreGuard::set("FLAPJACK_SSL_DOMAIN", "rotation.example.test");

        let configured = initialize_ssl_material()
            .await
            .expect("explicit managed material dir must configure observation without renewal");

        assert!(configured.manager.is_none());
        assert_eq!(configured.material_dir, material_dir.path());
    }

    #[tokio::test]
    async fn configured_material_dir_is_observed_when_renewal_identifier_is_invalid() {
        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _clean_env = remove_ssl_env();
        let material_dir = tempfile::TempDir::new().unwrap();
        let _material_dir = EnvVarRestoreGuard::set(
            "FLAPJACK_ACME_MATERIAL_DIR",
            material_dir.path().to_str().unwrap(),
        );
        let _email = EnvVarRestoreGuard::set("FLAPJACK_SSL_EMAIL", "rotation@example.test");
        let _domain = EnvVarRestoreGuard::set("FLAPJACK_SSL_DOMAIN", "localhost");

        let configured = initialize_ssl_material()
            .await
            .expect("explicit managed material dir must survive invalid renewal identity");

        assert!(configured.manager.is_none());
        assert_eq!(configured.material_dir, material_dir.path());
    }

    #[tokio::test]
    async fn configured_material_dir_is_observed_when_acme_directory_is_invalid() {
        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _clean_env = remove_ssl_env();
        let material_dir = tempfile::TempDir::new().unwrap();
        let _material_dir = EnvVarRestoreGuard::set(
            "FLAPJACK_ACME_MATERIAL_DIR",
            material_dir.path().to_str().unwrap(),
        );
        let _email = EnvVarRestoreGuard::set("FLAPJACK_SSL_EMAIL", "rotation@example.test");
        let _domain = EnvVarRestoreGuard::set("FLAPJACK_SSL_DOMAIN", "rotation.example.test");
        let _directory = EnvVarRestoreGuard::set("FLAPJACK_ACME_DIRECTORY", "http://invalid.test");

        let configured = initialize_ssl_material()
            .await
            .expect("explicit managed material dir must survive invalid renewal directory");

        assert!(configured.manager.is_none());
        assert_eq!(configured.material_dir, material_dir.path());
    }

    #[tokio::test]
    async fn missing_ssl_config_does_not_default_into_material_observation() {
        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _clean_env = remove_ssl_env();

        assert!(
            initialize_ssl_material().await.is_none(),
            "observer-only mode requires an explicit managed material directory"
        );
    }

    #[tokio::test]
    async fn active_fence_observes_existing_ssl_material_without_acme_initialization() {
        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _clean_env = remove_ssl_env();
        let material_dir = tempfile::TempDir::new().unwrap();
        let fullchain = material_dir.path().join("fullchain.pem");
        let private_key = material_dir.path().join("privkey.pem");
        std::fs::write(&fullchain, b"predecessor-certificate").unwrap();
        std::fs::write(&private_key, b"predecessor-private-key").unwrap();
        let before = (
            std::fs::read(&fullchain).unwrap(),
            std::fs::read(&private_key).unwrap(),
        );
        let _material_dir = EnvVarRestoreGuard::set(
            "FLAPJACK_ACME_MATERIAL_DIR",
            material_dir.path().to_str().unwrap(),
        );
        let _email = EnvVarRestoreGuard::set("FLAPJACK_SSL_EMAIL", "rotation@example.test");
        let _domain = EnvVarRestoreGuard::set("FLAPJACK_SSL_DOMAIN", "rotation.example.test");
        let _directory =
            EnvVarRestoreGuard::set("FLAPJACK_ACME_DIRECTORY", "https://127.0.0.1:1/directory");

        let configured = initialize_ssl_material_for_mode(StartupPersistenceMode::FenceActive)
            .await
            .expect("fenced startup should retain material observation");

        assert!(
            configured.manager.is_none(),
            "ACME initialization must be deferred"
        );
        assert!(configured.deferred_config.is_some());
        assert_eq!(
            (
                std::fs::read(&fullchain).unwrap(),
                std::fs::read(&private_key).unwrap()
            ),
            before,
            "fenced startup must preserve exact certificate material hashes"
        );

        let retried = initialize_ssl_material_for_mode(StartupPersistenceMode::FenceActive)
            .await
            .expect("a retried fenced startup should remain observer-only");
        assert!(retried.manager.is_none());
        assert!(retried.deferred_config.is_some());
        assert_eq!(
            (
                std::fs::read(&fullchain).unwrap(),
                std::fs::read(&private_key).unwrap()
            ),
            before,
            "a retried fenced startup must remain byte-exact"
        );
    }

    #[test]
    fn ssl_startup_log_does_not_label_dns_identifier_as_ip() {
        let config = flapjack::SslConfig {
            public_ip: None,
            acme_identifier: "search.example.com".to_string(),
            email: "test@example.com".to_string(),
            acme_directory: "https://acme.example.test/directory".to_string(),
            material_dir: std::path::PathBuf::from("data/ssl/acme"),
            root_ca_pem: None,
            check_interval_secs: 86_400,
            renew_days_threshold: 3,
        };
        let logs = SharedLogBuffer::default();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(logs.clone()),
        );

        tracing::subscriber::with_default(subscriber, || {
            log_ssl_management_enabled(&config);
        });

        let output = logs.contents();
        assert!(
            !output.contains("enabled for IP"),
            "domain startup log must not label the ACME identifier as an IP: {output}"
        );
        let parsed: Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(
            parsed
                .pointer("/fields/acme_identifier")
                .and_then(Value::as_str),
            Some("search.example.com")
        );
    }
}
