use super::*;
use crate::source_scan;
use chrono::{TimeZone, Utc};
use rcgen::{CertificateParams, KeyPair};
use serial_test::serial;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const TEST_CERTIFICATE_PEM: &str =
    "-----BEGIN CERTIFICATE-----\ndGVzdC1jZXJ0aWZpY2F0ZQ==\n-----END CERTIFICATE-----\n";

thread_local! {
    static MATERIAL_PUBLICATION_FAULT: std::cell::Cell<Option<MaterialPublicationFault>> =
        const { std::cell::Cell::new(None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MaterialPublicationFault {
    CertificateWrite,
    PrivateKeyWrite,
    FileSync,
    GenerationSync,
    ParentSync,
    LinkSwapAfterStaging,
    Retention,
}

impl SslManager {
    pub(super) fn inject_material_publication_fault(fault: MaterialPublicationFault) {
        MATERIAL_PUBLICATION_FAULT.set(Some(fault));
    }

    pub(super) fn clear_material_publication_fault() {
        MATERIAL_PUBLICATION_FAULT.set(None);
    }

    pub(super) fn maybe_fail_publication(fault: MaterialPublicationFault) -> Result<()> {
        if MATERIAL_PUBLICATION_FAULT.get() == Some(fault) {
            return Err(FlapjackError::Ssl(format!(
                "Injected material publication fault: {fault:?}"
            )));
        }
        Ok(())
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the system clock must be after the unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "flapjack_ssl_{label}_{}_{}_{}",
            std::process::id(),
            timestamp,
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("the test directory must be creatable");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn generated_private_key_pem() -> String {
    KeyPair::generate()
        .expect("the test private key must be generated")
        .serialize_pem()
}

fn manager_production_source() -> String {
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/manager.rs"))
        .expect("manager source must be readable");
    source_scan::production_code(&source)
}

fn manager_production_source_with_literals() -> String {
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/manager.rs"))
        .expect("manager source must be readable");
    source_scan::production_code_with_literals(&source)
}

fn compact_code(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn production_function_source(source: &str, function_name: &str) -> String {
    let signature_start = source
        .find(&format!("fn {function_name}"))
        .unwrap_or_else(|| panic!("{function_name} must exist in production source"));
    let opening_brace = signature_start
        + source[signature_start..]
            .find('{')
            .unwrap_or_else(|| panic!("{function_name} must have a body"));
    let mut depth = 0usize;
    for (offset, character) in source[opening_brace..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let function_end = opening_brace + offset + character.len_utf8();
                    return source[signature_start..function_end].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("{function_name} body must close");
}

fn material_files(material_dir: &Path) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    (
        fs::read(material_dir.join("fullchain.pem")).ok(),
        fs::read(material_dir.join("privkey.pem")).ok(),
    )
}

fn temp_artifacts(material_dir: &Path) -> Vec<String> {
    let mut artifacts: Vec<String> = fs::read_dir(material_dir)
        .expect("the material directory must be readable")
        .map(|entry| {
            entry
                .expect("each material directory entry must be readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    artifacts.sort();
    artifacts
}

fn generation_dirs(parent: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = fs::read_dir(parent)
        .expect("the material parent directory must be readable")
        .map(|entry| entry.expect("each entry must be readable"))
        .filter(|entry| {
            entry
                .file_type()
                .map(|file_type| file_type.is_dir())
                .unwrap_or(false)
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".flapjack-acme-"))
        .collect();
    dirs.sort();
    dirs
}

#[cfg(unix)]
fn internal_staging_links(material_dir: &Path) -> Vec<String> {
    let mut links: Vec<String> = fs::read_dir(material_dir)
        .expect("the material directory must be readable")
        .map(|entry| entry.expect("each material directory entry must be readable"))
        .filter(|entry| {
            entry
                .file_type()
                .map(|file_type| file_type.is_symlink())
                .unwrap_or(false)
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(GENERATION_PREFIX) && name.ends_with("-staging"))
        .collect();
    links.sort();
    links
}

struct CwdGuard(PathBuf);

impl CwdGuard {
    fn change_to(dir: &Path) -> Self {
        let previous = std::env::current_dir().expect("the current directory must be readable");
        std::env::set_current_dir(dir).expect("the fixture directory must be enterable");
        Self(previous)
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

fn seed_material_pair(
    material_dir: &Path,
    certificate: &[u8],
    private_key: &[u8],
) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    fs::create_dir(material_dir).expect("material directory must be creatable");
    fs::write(material_dir.join("fullchain.pem"), certificate)
        .expect("certificate fixture must be writable");
    fs::write(material_dir.join("privkey.pem"), private_key)
        .expect("private key fixture must be writable");
    (Some(certificate.to_vec()), Some(private_key.to_vec()))
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

struct MaterialPublicationFaultGuard;

impl MaterialPublicationFaultGuard {
    fn inject(fault: MaterialPublicationFault) -> Self {
        SslManager::inject_material_publication_fault(fault);
        Self
    }
}

impl Drop for MaterialPublicationFaultGuard {
    fn drop(&mut self) {
        SslManager::clear_material_publication_fault();
    }
}

fn manager_with_config(config: SslConfig) -> SslManager {
    SslManager {
        config,
        acme_client: None,
        last_check: Arc::new(RwLock::new(None)),
        last_renewal: Arc::new(RwLock::new(None)),
        renewal_status: Arc::new(RwLock::new(RenewalStatus::default())),
    }
}

fn test_manager(renew_days_threshold: u64) -> SslManager {
    manager_with_config(SslConfig {
        public_ip: Some("127.0.0.1".to_string()),
        acme_identifier: "127.0.0.1".to_string(),
        email: "test@example.com".to_string(),
        acme_directory: "https://acme-staging-v02.api.letsencrypt.org/directory".to_string(),
        material_dir: std::env::temp_dir().join("flapjack-ssl-test-material"),
        root_ca_pem: None,
        check_interval_secs: 60,
        renew_days_threshold,
    })
}

#[tokio::test]
async fn record_certificate_expiry_status_marks_ok() {
    let manager = test_manager(3);

    manager.record_certificate_expiry_status(5).await;

    let status = manager.get_status().await;
    assert_eq!(status.status, "ok");
    assert_eq!(status.error, None);
    assert_eq!(status.cert_expires_in_days, Some(5));
}

#[tokio::test]
async fn renewal_loop_checks_missing_certificate_immediately() {
    let fixture = TestDirectory::new("immediate_renewal_check");
    let manager = Arc::new(manager_with_config(SslConfig {
        public_ip: Some("127.0.0.1".to_string()),
        acme_identifier: "127.0.0.1".to_string(),
        email: "test@example.com".to_string(),
        acme_directory: "https://acme.example.test/directory".to_string(),
        material_dir: fixture.path().join("missing_material"),
        root_ca_pem: None,
        check_interval_secs: 3_600,
        renew_days_threshold: 3,
    }));
    let renewal_task = tokio::spawn(Arc::clone(&manager).start_renewal_loop());

    let observed_failure = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let status = manager.get_status().await;
            if status.status == "failed" {
                return status.error;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    renewal_task.abort();
    let _ = renewal_task.await;

    let error = observed_failure
        .expect("the initial check must run without waiting for the one-hour interval")
        .expect("the missing test ACME client must produce a recorded error");
    assert!(error.contains("ACME client not initialized"));
}

#[tokio::test]
async fn missing_private_key_triggers_immediate_pair_renewal() {
    let fixture = TestDirectory::new("missing_private_key");
    let material_dir = fixture.path().join("material");
    fs::create_dir(&material_dir).expect("material directory must be creatable");
    fs::write(material_dir.join("fullchain.pem"), TEST_CERTIFICATE_PEM)
        .expect("the legacy certificate-only fixture must be writable");
    let manager = manager_with_config(SslConfig {
        public_ip: Some("127.0.0.1".to_string()),
        acme_identifier: "127.0.0.1".to_string(),
        email: "test@example.com".to_string(),
        acme_directory: "https://acme.example.test/directory".to_string(),
        material_dir,
        root_ca_pem: None,
        check_interval_secs: 3_600,
        renew_days_threshold: 3,
    });

    let error = manager
        .check_and_renew()
        .await
        .expect_err("a certificate without its private key must trigger pair renewal");

    assert!(error.to_string().contains("ACME client not initialized"));
}

#[test]
fn certificate_needs_renewal_uses_threshold() {
    let manager = test_manager(3);

    assert!(manager.certificate_needs_renewal(2));
    assert!(!manager.certificate_needs_renewal(3));
}

#[test]
fn publication_persists_certificate_and_parseable_private_key() {
    let fixture = TestDirectory::new("persisted_pair");
    let material_dir = fixture.path().join("material");
    let private_key_pem = generated_private_key_pem();

    SslManager::write_certificate_files_to_dir(
        &material_dir,
        TEST_CERTIFICATE_PEM,
        &private_key_pem,
    )
    .expect("publishing certificate material must succeed");

    let persisted_certificate = fs::read_to_string(material_dir.join("fullchain.pem"))
        .expect("the published certificate must be readable");
    assert_eq!(
        persisted_certificate, TEST_CERTIFICATE_PEM,
        "publication must persist the complete certificate chain"
    );
    let persisted_key = fs::read_to_string(material_dir.join("privkey.pem")).ok();
    assert!(
        persisted_key
            .as_deref()
            .is_some_and(|pem| { !pem.trim().is_empty() && KeyPair::from_pem(pem).is_ok() }),
        "publication must persist a non-empty, parseable PEM private key; got {persisted_key:?}"
    );
}

#[test]
fn renewal_publication_wiring_preserves_issued_key_and_resolved_path() {
    let production_source = manager_production_source();
    let compact = compact_code(&production_source);
    let publication_source =
        production_function_source(&production_source, "publish_issued_certificate");
    let compact_publication = compact_code(&publication_source);
    let writer_source = production_function_source(&production_source, "write_certificate_files");
    let compact_writer = compact_code(&writer_source);
    let writer_source_with_literals = production_function_source(
        &manager_production_source_with_literals(),
        "write_certificate_files",
    );
    let compact_writer_literals = compact_code(&writer_source_with_literals);

    assert!(
        compact.contains("let(cert_pem,key_pem)="),
        "renew_certificate must bind the issued private key as live publication input"
    );
    assert!(
        compact.contains("self.publish_issued_certificate(&cert_pem,&key_pem,Utc::now()).await?"),
        "renew_certificate must pass the issued pair into the completion owner"
    );
    assert!(
        compact_publication.contains("self.write_certificate_files(cert_pem,key_pem)?")
            && compact_publication.contains("certificate_expiry_days_from_pem"),
        "the completion owner must derive status from and publish the same issued certificate pair"
    );
    assert!(
            compact_writer.contains("fnwrite_certificate_files(&self,cert_pem:&str,key_pem:&str)")
                && compact_writer.contains("self.get_cert_path()"),
            "write_certificate_files must publish through the same destination resolved by get_cert_path"
        );
    assert!(
        !compact_writer_literals.contains("/etc/letsencrypt/live"),
        "the shipped writer must not hard-code the /etc/letsencrypt/live destination; \
             resolve it through get_cert_path instead"
    );
    assert!(
        !compact_writer.contains("write_certificate_files_to_dir(&cert_dir,cert_pem,\"\")"),
        "the shipped writer must not substitute an empty private key"
    );
}

#[test]
fn nonunix_publication_never_removes_visible_material_before_commit() {
    let compact = compact_code(&manager_production_source());

    assert!(
        !compact.contains("fs::remove_dir_all(cert_dir)"),
        "an unsupported platform must fail before mutation instead of deleting the visible pair"
    );
}

#[cfg(not(unix))]
#[test]
fn nonunix_directory_sync_does_not_turn_a_completed_rename_into_failure() {
    let fixture = TestDirectory::new("nonunix_directory_sync");

    SslManager::sync_dir(fixture.path())
        .expect("unsupported directory fsync must not report a completed publication as failed");
}

#[tokio::test]
async fn dns_renewal_status_reports_issued_certificate_lifetime() {
    let fixture = TestDirectory::new("dns_renewal_status");
    let renewed_at = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let mut params = CertificateParams::new(vec!["search.example.com".to_string()]).unwrap();
    params.not_before = rcgen::date_time_ymd(2029, 12, 1);
    params.not_after = rcgen::date_time_ymd(2030, 1, 11);
    let key_pair = KeyPair::generate().unwrap();
    let certificate_pem = params.self_signed(&key_pair).unwrap().pem();
    let manager = manager_with_config(SslConfig {
        public_ip: None,
        acme_identifier: "search.example.com".to_string(),
        email: "test@example.com".to_string(),
        acme_directory: "https://acme.example.test/directory".to_string(),
        material_dir: fixture.path().join("material"),
        root_ca_pem: None,
        check_interval_secs: 60,
        renew_days_threshold: 3,
    });

    manager
        .publish_issued_certificate(&certificate_pem, &key_pair.serialize_pem(), renewed_at)
        .await
        .expect("issued DNS certificate publication must succeed");

    let status = manager.get_status().await;
    assert_eq!(status.cert_expires_in_days, Some(10));
    assert_eq!(*manager.last_renewal.read().await, Some(renewed_at));
}

#[cfg(unix)]
#[test]
fn publication_fault_injection_rejects_both_sequential_write_orders() {
    use std::os::unix::fs::PermissionsExt;

    for blocked_half in ["fullchain.pem", "privkey.pem"] {
        let fixture = TestDirectory::new("fault_boundary");
        let material_dir = fixture.path().join("material");
        let old_certificate = b"old certificate generation\n".to_vec();
        let old_private_key = generated_private_key_pem().into_bytes();
        let old_pair = seed_material_pair(&material_dir, &old_certificate, &old_private_key);

        let blocked_path = material_dir.join(blocked_half);
        let mut perms = fs::metadata(&blocked_path)
            .expect("the fault fixture must exist")
            .permissions();
        perms.set_mode(0o400);
        fs::set_permissions(&blocked_path, perms)
            .expect("the fault fixture must be made read-only");
        assert!(
            fs::OpenOptions::new()
                .write(true)
                .open(&blocked_path)
                .is_err(),
            "read-only fault fixture must reject writes to {blocked_half}; \
                 running as root defeats this guard"
        );

        let new_private_key = generated_private_key_pem().into_bytes();
        let new_pair = (
            Some(TEST_CERTIFICATE_PEM.as_bytes().to_vec()),
            Some(new_private_key.clone()),
        );
        let publication_result = SslManager::write_certificate_files_to_dir(
            &material_dir,
            TEST_CERTIFICATE_PEM,
            std::str::from_utf8(&new_private_key).expect("generated key must be utf-8 PEM"),
        );
        let visible_pair = material_files(&material_dir);

        assert!(
            visible_pair == old_pair || visible_pair == new_pair,
            "a faulted publication (blocked {blocked_half}) exposed a mixed old/new pair; \
                 result={publication_result:?}; visible_pair={visible_pair:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn publication_commits_complete_generation_without_mutating_visible_pair() {
    use std::os::unix::fs::symlink;

    let fixture = TestDirectory::new("atomic_pair");
    let material_dir = fixture.path().join("material");
    let old_generation_dir = fixture.path().join("old_generation");
    let old_certificate = b"old certificate generation\n".to_vec();
    let old_private_key = generated_private_key_pem().into_bytes();
    let old_pair = seed_material_pair(&old_generation_dir, &old_certificate, &old_private_key);
    symlink(&old_generation_dir, &material_dir)
        .expect("the visible material path must point at the old generation");

    let new_private_key = generated_private_key_pem().into_bytes();
    let publication_result = SslManager::write_certificate_files_to_dir(
        &material_dir,
        TEST_CERTIFICATE_PEM,
        std::str::from_utf8(&new_private_key).expect("generated key must be utf-8 PEM"),
    );

    let new_pair = (
        Some(TEST_CERTIFICATE_PEM.as_bytes().to_vec()),
        Some(new_private_key),
    );
    assert!(
            material_files(&old_generation_dir) == old_pair,
            "publication must stage a new generation instead of mutating the visible old generation; result={publication_result:?}"
        );
    assert!(
            material_files(&material_dir) == new_pair,
            "one pair-level commit must make the complete new generation visible; result={publication_result:?}"
        );
    assert_eq!(
        temp_artifacts(&material_dir),
        Vec::<String>::new(),
        "failed or interrupted publication must not leave temporary artifacts"
    );
}

#[test]
fn publication_replaces_existing_real_material_directory_as_pair() {
    let fixture = TestDirectory::new("real_republication");
    let material_dir = fixture.path().join("material");
    let old_certificate = b"old certificate generation\n".to_vec();
    let old_private_key = generated_private_key_pem().into_bytes();
    seed_material_pair(&material_dir, &old_certificate, &old_private_key);

    let new_private_key = generated_private_key_pem().into_bytes();
    SslManager::write_certificate_files_to_dir(
        &material_dir,
        TEST_CERTIFICATE_PEM,
        std::str::from_utf8(&new_private_key).expect("generated key must be utf-8 PEM"),
    )
    .expect("publishing over existing material must succeed");

    assert_eq!(
            material_files(&material_dir),
            (
                Some(TEST_CERTIFICATE_PEM.as_bytes().to_vec()),
                Some(new_private_key)
            ),
            "republishing over a real material directory must replace certificate and key as one generation"
        );
    assert_eq!(
        temp_artifacts(&material_dir),
        Vec::<String>::new(),
        "successful republishing must not leave temporary artifacts"
    );
}

#[cfg(unix)]
#[test]
fn published_private_key_is_owner_private() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = TestDirectory::new("private_key_mode");
    let material_dir = fixture.path().join("material");
    let private_key_pem = generated_private_key_pem();
    SslManager::write_certificate_files_to_dir(
        &material_dir,
        TEST_CERTIFICATE_PEM,
        &private_key_pem,
    )
    .expect("publishing certificate material must succeed");

    let key_mode = fs::metadata(material_dir.join("privkey.pem"))
        .ok()
        .map(|metadata| metadata.permissions().mode() & 0o777);
    assert_eq!(
        key_mode,
        Some(0o600),
        "the published private key must have exact owner-only mode bits"
    );
}

#[test]
#[serial]
fn certificate_path_defaults_under_flapjack_data_dir() {
    let fixture = TestDirectory::new("default_material_path");
    let _data_dir = EnvVarGuard::set("FLAPJACK_DATA_DIR", fixture.path());
    let _email = EnvVarGuard::set("FLAPJACK_SSL_EMAIL", "test@example.com");
    let _public_ip = EnvVarGuard::set("FLAPJACK_PUBLIC_IP", "127.0.0.1");
    let _acme_directory = EnvVarGuard::unset("FLAPJACK_ACME_DIRECTORY");

    let manager = manager_with_config(
        SslConfig::from_env().expect("SSL config must load from the test environment"),
    );
    let cert_path = manager.get_cert_path();

    assert_eq!(
        cert_path.file_name().and_then(|name| name.to_str()),
        Some("fullchain.pem")
    );
    assert!(
        cert_path.starts_with(fixture.path()) && !cert_path.starts_with("/etc/letsencrypt/live"),
        "the default certificate path must be operator-local under FLAPJACK_DATA_DIR; got {}",
        cert_path.display()
    );
}

#[path = "manager_publication_tests.rs"]
mod publication_lifecycle;
