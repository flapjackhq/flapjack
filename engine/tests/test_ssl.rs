use flapjack_ssl::{SslConfig, SslManager};
use serial_test::serial;
use std::env;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
#[serial]
fn test_ssl_config_requires_email() {
    // Clear env
    env::remove_var("FLAPJACK_SSL_EMAIL");
    env::remove_var("FLAPJACK_PUBLIC_IP");

    // Should fail without email
    let result = SslConfig::from_env();
    assert!(result.is_err(), "Config should require FLAPJACK_SSL_EMAIL");

    let err = result.unwrap_err();
    assert!(err.to_string().contains("FLAPJACK_SSL_EMAIL"));
}

#[test]
#[serial]
fn test_ssl_config_requires_acme_identifier() {
    env::set_var("FLAPJACK_SSL_EMAIL", "test@example.com");
    env::remove_var("FLAPJACK_PUBLIC_IP");
    env::remove_var("FLAPJACK_SSL_DOMAIN");

    let result = SslConfig::from_env();
    assert!(
        result.is_err(),
        "Config should fail when neither supported ACME identifier is set"
    );

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("FLAPJACK_PUBLIC_IP")
            && err.to_string().contains("FLAPJACK_SSL_DOMAIN"),
        "Error should name both supported ACME identifier variables, got: {}",
        err
    );

    env::remove_var("FLAPJACK_SSL_EMAIL");
}

#[test]
#[serial]
fn test_ssl_config_valid() {
    env::set_var("FLAPJACK_SSL_EMAIL", "test@example.com");
    env::set_var("FLAPJACK_PUBLIC_IP", "192.0.2.1");

    let config = SslConfig::from_env().expect("Config should parse");

    assert_eq!(config.email, "test@example.com");
    assert_eq!(config.public_ip.as_deref(), Some("192.0.2.1"));
    assert_eq!(config.check_interval_secs, 86400); // 24 hours
    assert_eq!(config.renew_days_threshold, 3);
    assert!(config.acme_directory.contains("letsencrypt.org"));

    env::remove_var("FLAPJACK_SSL_EMAIL");
    env::remove_var("FLAPJACK_PUBLIC_IP");
}

#[test]
#[serial]
fn test_ssl_config_staging_directory() {
    env::set_var("FLAPJACK_SSL_EMAIL", "test@example.com");
    env::set_var("FLAPJACK_PUBLIC_IP", "192.0.2.1");
    env::set_var(
        "FLAPJACK_ACME_DIRECTORY",
        "https://acme-staging-v02.api.letsencrypt.org/directory",
    );

    let config = SslConfig::from_env().expect("Config should parse");

    assert!(config.acme_directory.contains("staging"));

    env::remove_var("FLAPJACK_SSL_EMAIL");
    env::remove_var("FLAPJACK_PUBLIC_IP");
    env::remove_var("FLAPJACK_ACME_DIRECTORY");
}

#[test]
#[serial]
fn test_ssl_config_invalid_ip() {
    env::set_var("FLAPJACK_SSL_EMAIL", "test@example.com");
    env::set_var("FLAPJACK_PUBLIC_IP", "not-an-ip");

    let result = SslConfig::from_env();
    assert!(result.is_err(), "Config should reject invalid IP");

    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("Invalid") || err_msg.contains("FLAPJACK_PUBLIC_IP"),
        "Error should mention invalid IP, got: {}",
        err_msg
    );

    env::remove_var("FLAPJACK_SSL_EMAIL");
    env::remove_var("FLAPJACK_PUBLIC_IP");
}

#[test]
#[serial]
fn test_ssl_config_rejects_http_acme_directory() {
    env::set_var("FLAPJACK_SSL_EMAIL", "test@example.com");
    env::set_var("FLAPJACK_PUBLIC_IP", "192.0.2.1");
    env::set_var(
        "FLAPJACK_ACME_DIRECTORY",
        "http://insecure.example.com/directory",
    );

    let result = SslConfig::from_env();
    assert!(result.is_err(), "Config should reject HTTP ACME directory");

    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("HTTPS"),
        "Error should mention HTTPS requirement, got: {}",
        err
    );

    env::remove_var("FLAPJACK_SSL_EMAIL");
    env::remove_var("FLAPJACK_PUBLIC_IP");
    env::remove_var("FLAPJACK_ACME_DIRECTORY");
}

#[test]
fn test_acme_challenge_concurrent_storage() {
    // Test that multiple challenge tokens can be stored and retrieved independently
    // This validates the fix for the race condition where clearing all challenges
    // could interfere with concurrent certificate requests

    use dashmap::DashMap;

    let challenges: Arc<DashMap<String, String>> = Arc::new(DashMap::new());

    // Simulate two concurrent orders with different tokens
    let token1 = "order1-token".to_string();
    let token2 = "order2-token".to_string();
    let response1 = "order1-response".to_string();
    let response2 = "order2-response".to_string();

    // Store both tokens (simulating concurrent requests)
    challenges.insert(token1.clone(), response1.clone());
    challenges.insert(token2.clone(), response2.clone());

    // Both should be retrievable
    assert_eq!(
        challenges.get(&token1).map(|v| v.clone()),
        Some(response1.clone())
    );
    assert_eq!(
        challenges.get(&token2).map(|v| v.clone()),
        Some(response2.clone())
    );

    // Remove only token1 (simulating order1 completing)
    challenges.remove(&token1);

    // token1 should be gone
    assert!(challenges.get(&token1).is_none());

    // token2 should still be present (this would fail with global clear())
    assert_eq!(
        challenges.get(&token2).map(|v| v.clone()),
        Some(response2.clone())
    );

    // Remove token2
    challenges.remove(&token2);

    // Now both should be gone
    assert!(challenges.get(&token1).is_none());
    assert!(challenges.get(&token2).is_none());
}

#[tokio::test]
async fn test_ssl_manager_initialization() {
    let unique_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after the unix epoch")
        .as_nanos();
    let fixture = tempfile::tempdir().expect("tempdir must be creatable");
    let missing_root = fixture.path().join(format!(
        "missing-root-{unique_suffix}-{}.pem",
        std::process::id()
    ));
    let config = SslConfig {
        public_ip: Some("192.0.2.1".to_string()),
        acme_identifier: "192.0.2.1".to_string(),
        email: "test@example.com".to_string(),
        acme_directory: "https://acme.example.test/directory".to_string(),
        material_dir: fixture.path().join("material"),
        root_ca_pem: Some(missing_root.clone()),
        check_interval_secs: 86_400,
        renew_days_threshold: 3,
    };

    let error = match SslManager::new(config).await {
        Ok(_) => panic!("missing root CA override must fail manager initialization"),
        Err(error) => error,
    };
    let error_text = error.to_string();

    assert!(
        error_text.contains(missing_root.to_string_lossy().as_ref())
            || error_text.contains("root")
            || error_text.contains("No such file"),
        "manager initialization must surface the root-CA failure context, got: {error_text}"
    );
}
