use crate::error::{FlapjackError, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::net::IpAddr;
use std::path::PathBuf;

const DEFAULT_ACME_DIRECTORY: &str = "https://acme-v02.api.letsencrypt.org/directory";
const DEFAULT_DATA_DIR: &str = "./data";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SslConfig {
    pub public_ip: Option<String>,
    pub acme_identifier: String,
    pub email: String,
    pub acme_directory: String,
    pub material_dir: PathBuf,
    pub root_ca_pem: Option<PathBuf>,
    pub check_interval_secs: u64,
    pub renew_days_threshold: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SslMaterialConfig {
    pub material_dir: PathBuf,
}

impl SslMaterialConfig {
    pub fn from_env() -> Self {
        Self {
            material_dir: Self::explicit_material_dir_from_env()
                .unwrap_or_else(Self::default_material_dir_from_env),
        }
    }

    pub fn explicit_from_env() -> Option<Self> {
        Self::explicit_material_dir_from_env().map(|material_dir| Self { material_dir })
    }

    fn explicit_material_dir_from_env() -> Option<PathBuf> {
        env::var_os("FLAPJACK_ACME_MATERIAL_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }

    fn default_material_dir_from_env() -> PathBuf {
        PathBuf::from(env::var_os("FLAPJACK_DATA_DIR").unwrap_or_else(|| DEFAULT_DATA_DIR.into()))
            .join("ssl")
            .join("acme")
    }
}

impl SslConfig {
    /// Load SSL configuration from environment variables.
    /// Always enabled (opinionated approach).
    ///
    /// Required: FLAPJACK_SSL_EMAIL
    /// Required: FLAPJACK_PUBLIC_IP or FLAPJACK_SSL_DOMAIN
    /// Optional: FLAPJACK_ACME_DIRECTORY (defaults to Let's Encrypt production)
    /// Optional: FLAPJACK_ACME_MATERIAL_DIR (defaults under FLAPJACK_DATA_DIR)
    /// Optional: FLAPJACK_ACME_ROOT_CA_PEM
    pub fn from_env() -> Result<Self> {
        let email = env::var("FLAPJACK_SSL_EMAIL").map_err(|_| {
            FlapjackError::Config("FLAPJACK_SSL_EMAIL is required for SSL auto-renewal".into())
        })?;

        let (acme_identifier, public_ip) = Self::resolve_acme_identifier()?;

        let acme_directory =
            env::var("FLAPJACK_ACME_DIRECTORY").unwrap_or_else(|_| DEFAULT_ACME_DIRECTORY.into());

        // Validate ACME directory URL is HTTPS (security requirement)
        if !acme_directory.starts_with("https://") {
            return Err(FlapjackError::Config(format!(
                "ACME directory must use HTTPS, got: {}",
                acme_directory
            )));
        }

        let material_dir = SslMaterialConfig::from_env().material_dir;
        let root_ca_pem = env::var_os("FLAPJACK_ACME_ROOT_CA_PEM").map(PathBuf::from);

        Ok(Self {
            public_ip,
            acme_identifier,
            email,
            acme_directory,
            material_dir,
            root_ca_pem,
            check_interval_secs: 86400, // 24 hours (opinionated, not configurable)
            renew_days_threshold: 3,    // 3 days (opinionated, not configurable)
        })
    }

    fn resolve_acme_identifier() -> Result<(String, Option<String>)> {
        if let Some(ip) = Self::non_empty_env("FLAPJACK_PUBLIC_IP") {
            ip.parse::<IpAddr>().map_err(|_| {
                FlapjackError::Config(format!("Invalid FLAPJACK_PUBLIC_IP: {}", ip))
            })?;
            return Ok((ip.clone(), Some(ip)));
        }

        if let Some(domain) = Self::non_empty_env("FLAPJACK_SSL_DOMAIN") {
            Self::validate_dns_name(&domain)?;
            return Ok((domain, None));
        }

        Err(FlapjackError::Config(
            "Set FLAPJACK_PUBLIC_IP or FLAPJACK_SSL_DOMAIN for SSL auto-renewal.".into(),
        ))
    }

    fn non_empty_env(key: &str) -> Option<String> {
        env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn validate_dns_name(domain: &str) -> Result<()> {
        let labels: Vec<&str> = domain.split('.').collect();
        let labels_are_valid = labels.len() >= 2
            && domain.len() <= 253
            && labels.iter().all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '-')
                    && label
                        .chars()
                        .next()
                        .is_some_and(|character| character.is_ascii_alphanumeric())
                    && label
                        .chars()
                        .last()
                        .is_some_and(|character| character.is_ascii_alphanumeric())
            });
        if labels_are_valid {
            return Ok(());
        }
        Err(FlapjackError::Config(format!(
            "Invalid FLAPJACK_SSL_DOMAIN: {domain}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = env::var_os(key);
            env::set_var(key, value);
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = env::var_os(key);
            env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    #[test]
    #[serial]
    fn test_config_from_env() {
        let _email = EnvVarGuard::set("FLAPJACK_SSL_EMAIL", "test@example.com");
        let _public_ip = EnvVarGuard::set("FLAPJACK_PUBLIC_IP", "127.0.0.1");
        let _domain = EnvVarGuard::unset("FLAPJACK_SSL_DOMAIN");

        let config = SslConfig::from_env().unwrap();
        assert_eq!(config.email, "test@example.com");
        assert_eq!(config.public_ip.as_deref(), Some("127.0.0.1"));
        assert_eq!(config.acme_identifier, "127.0.0.1");
        assert_eq!(config.check_interval_secs, 86400);
        assert_eq!(config.renew_days_threshold, 3);
    }

    #[test]
    #[serial]
    fn test_config_requires_email() {
        let _email = EnvVarGuard::unset("FLAPJACK_SSL_EMAIL");
        let _public_ip = EnvVarGuard::set("FLAPJACK_PUBLIC_IP", "127.0.0.1");

        let result = SslConfig::from_env();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("FLAPJACK_SSL_EMAIL"));
    }

    #[test]
    #[serial]
    fn domain_identifier_does_not_require_public_ip() {
        let _email = EnvVarGuard::set("FLAPJACK_SSL_EMAIL", "test@example.com");
        let _public_ip = EnvVarGuard::unset("FLAPJACK_PUBLIC_IP");
        let _domain = EnvVarGuard::set("FLAPJACK_SSL_DOMAIN", "search.example.com");

        let config = SslConfig::from_env().expect("domain-only SSL config must load");
        assert_eq!(config.public_ip, None);
        assert_eq!(config.acme_identifier, "search.example.com");
    }

    #[test]
    #[serial]
    fn domain_identifier_is_trimmed_and_validated_at_config_load() {
        let _email = EnvVarGuard::set("FLAPJACK_SSL_EMAIL", "test@example.com");
        let _public_ip = EnvVarGuard::unset("FLAPJACK_PUBLIC_IP");

        for invalid in [
            "https://search.example.com",
            "search example.com",
            "localhost",
            "-search.example.com",
            "search-.example.com",
            "search..example.com",
        ] {
            let _domain = EnvVarGuard::set("FLAPJACK_SSL_DOMAIN", invalid);
            assert!(
                SslConfig::from_env().is_err(),
                "invalid domain must fail configuration: {invalid}"
            );
        }

        let _domain = EnvVarGuard::set("FLAPJACK_SSL_DOMAIN", "  search.example.com  ");
        let config = SslConfig::from_env().expect("a trimmed valid DNS name must load");
        assert_eq!(config.acme_identifier, "search.example.com");
        assert_eq!(config.public_ip, None);
    }

    #[test]
    #[serial]
    fn test_config_rejects_non_https_acme_directory() {
        let _email = EnvVarGuard::set("FLAPJACK_SSL_EMAIL", "test@example.com");
        let _public_ip = EnvVarGuard::set("FLAPJACK_PUBLIC_IP", "127.0.0.1");
        let _acme_directory =
            EnvVarGuard::set("FLAPJACK_ACME_DIRECTORY", "http://example.invalid/acme");

        let result = SslConfig::from_env();
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("ACME directory must use HTTPS"));
    }

    #[test]
    #[serial]
    fn material_directory_defaults_under_flapjack_data_dir() {
        let data_dir = PathBuf::from("/tmp/flapjack-config-material-default");
        let _data_dir = EnvVarGuard::set("FLAPJACK_DATA_DIR", &data_dir);
        let _email = EnvVarGuard::set("FLAPJACK_SSL_EMAIL", "test@example.com");
        let _public_ip = EnvVarGuard::set("FLAPJACK_PUBLIC_IP", "127.0.0.1");
        let _material_dir = EnvVarGuard::unset("FLAPJACK_ACME_MATERIAL_DIR");

        let config = SslConfig::from_env().expect("SSL config must load");
        assert_eq!(config.material_dir, data_dir.join("ssl").join("acme"));
        assert!(!config.material_dir.starts_with("/etc/letsencrypt/live"));
    }

    #[test]
    #[serial]
    fn renewal_config_reuses_canonical_material_config_resolution() {
        let explicit_dir = PathBuf::from("/tmp/flapjack-canonical-explicit-material");
        let _explicit_dir = EnvVarGuard::set("FLAPJACK_ACME_MATERIAL_DIR", &explicit_dir);
        let _data_dir = EnvVarGuard::set("FLAPJACK_DATA_DIR", "/tmp/ignored-data-dir");
        let _email = EnvVarGuard::set("FLAPJACK_SSL_EMAIL", "test@example.com");
        let _public_ip = EnvVarGuard::set("FLAPJACK_PUBLIC_IP", "127.0.0.1");

        let material_config = SslMaterialConfig::from_env();
        let renewal_config = SslConfig::from_env().expect("SSL config must load");

        assert_eq!(material_config.material_dir, explicit_dir);
        assert_eq!(renewal_config.material_dir, material_config.material_dir);
    }

    #[test]
    #[serial]
    fn root_ca_override_is_resolved_from_env() {
        let root_ca = PathBuf::from("/tmp/flapjack-test-root.pem");
        let _root_ca = EnvVarGuard::set("FLAPJACK_ACME_ROOT_CA_PEM", &root_ca);
        let _email = EnvVarGuard::set("FLAPJACK_SSL_EMAIL", "test@example.com");
        let _public_ip = EnvVarGuard::set("FLAPJACK_PUBLIC_IP", "127.0.0.1");

        let config = SslConfig::from_env().expect("SSL config must load");
        assert_eq!(config.root_ca_pem.as_deref(), Some(root_ca.as_path()));
    }

    #[test]
    #[serial]
    fn explicit_material_config_loads_without_renewal_fields() {
        let material_dir = PathBuf::from("/tmp/flapjack-explicit-material");
        let _material_dir = EnvVarGuard::set("FLAPJACK_ACME_MATERIAL_DIR", &material_dir);
        let _email = EnvVarGuard::unset("FLAPJACK_SSL_EMAIL");
        let _public_ip = EnvVarGuard::unset("FLAPJACK_PUBLIC_IP");
        let _domain = EnvVarGuard::unset("FLAPJACK_SSL_DOMAIN");
        let _acme_directory = EnvVarGuard::set("FLAPJACK_ACME_DIRECTORY", "http://invalid.test");

        assert_eq!(
            SslMaterialConfig::explicit_from_env(),
            Some(SslMaterialConfig { material_dir })
        );
    }

    #[test]
    #[serial]
    fn material_config_is_absent_without_explicit_material_dir() {
        let _material_dir = EnvVarGuard::unset("FLAPJACK_ACME_MATERIAL_DIR");

        assert_eq!(SslMaterialConfig::explicit_from_env(), None);
    }

    #[test]
    #[serial]
    fn empty_material_dir_env_is_treated_as_absent() {
        let data_dir = PathBuf::from("/tmp/flapjack-material-dir-empty-fallback");
        let _material_dir = EnvVarGuard::set("FLAPJACK_ACME_MATERIAL_DIR", "");
        let _data_dir = EnvVarGuard::set("FLAPJACK_DATA_DIR", &data_dir);
        let _email = EnvVarGuard::set("FLAPJACK_SSL_EMAIL", "test@example.com");
        let _public_ip = EnvVarGuard::set("FLAPJACK_PUBLIC_IP", "127.0.0.1");

        assert_eq!(SslMaterialConfig::explicit_from_env(), None);

        let renewal_config = SslConfig::from_env().expect("SSL config must fall back to data dir");
        assert_eq!(
            renewal_config.material_dir,
            data_dir.join("ssl").join("acme")
        );
    }
}
