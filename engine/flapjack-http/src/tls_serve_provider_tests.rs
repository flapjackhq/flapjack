use super::*;

#[test]
fn static_tls_uses_flapjack_ssl_crypto_provider_owner() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let cert_files = super::tls_serve_tests::write_test_cert_files(&temp_dir);
    let paths = super::tls_serve_tests::tls_paths(&cert_files.cert_path, &cert_files.key_path);
    load_tls_config(&paths).expect("static TLS configuration should load");

    let installed = rustls::crypto::CryptoProvider::get_default()
        .expect("loading static TLS must install a default crypto provider");
    let ring = rustls::crypto::ring::default_provider();
    assert_eq!(
        installed.cipher_suites, ring.cipher_suites,
        "static TLS must install ring's exact cipher-suite selection"
    );
    assert_eq!(
        installed
            .kx_groups
            .iter()
            .map(|group| group.name())
            .collect::<Vec<_>>(),
        ring.kx_groups
            .iter()
            .map(|group| group.name())
            .collect::<Vec<_>>(),
        "static TLS must install ring's exact key-exchange group selection"
    );
}
