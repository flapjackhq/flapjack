use super::tls_serve_rotation_tests::{
    publish_real_dir_generation, publish_symlink_generation, startup_resolver_under_material,
    test_resolver,
};
use super::tls_serve_tests::{write_named_test_cert_files, TestCertFiles};
use super::*;
use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn bootstrap_pair_remains_authorized_when_renewal_publishes_first() {
    let temp_dir = TempDir::new().unwrap();
    let material_dir = temp_dir.path().join("material");
    let startup_cert = write_named_test_cert_files(&temp_dir, "startup_cert");
    let published_cert = write_named_test_cert_files(&temp_dir, "published_cert");
    let resolver = startup_resolver_under_material(&material_dir, &startup_cert);

    publish_real_dir_generation(&material_dir, "generation_b", &published_cert);

    resolver
        .validate_material_observer_source(&material_dir)
        .expect("managed bootstrap paths must remain authorized after renewal publishes");
}

#[cfg(unix)]
#[test]
fn startup_pair_symlinked_outside_material_directory_is_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let source_cert = write_named_test_cert_files(&temp_dir, "outside_cert");
    let material_dir = temp_dir.path().join("material");
    std::fs::create_dir_all(&material_dir).unwrap();
    let escaped_dir = material_dir.join("outside");
    std::os::unix::fs::symlink(temp_dir.path(), &escaped_dir).unwrap();
    let resolver = test_resolver(&TestCertFiles {
        cert_path: escaped_dir.join(source_cert.cert_path.file_name().unwrap()),
        key_path: escaped_dir.join(source_cert.key_path.file_name().unwrap()),
        cert_der: source_cert.cert_der.clone(),
    });

    let error = resolver
        .validate_material_observer_source(&material_dir)
        .expect_err("managed source authorization must resolve symlinks before containment");

    assert!(
        error.contains("outside managed material directory"),
        "symlink escape rejection should identify the source divergence: {error}"
    );
}

#[cfg(unix)]
#[test]
fn managed_visible_pair_resolves_to_the_published_generation() {
    let temp_dir = TempDir::new().unwrap();
    let material_dir = temp_dir.path().join("material");
    let published_cert = write_named_test_cert_files(&temp_dir, "published_cert");
    std::fs::create_dir_all(&material_dir).unwrap();
    publish_real_dir_generation(&material_dir, "generation_a", &published_cert);
    let resolver = test_resolver(&TestCertFiles {
        cert_path: material_dir.join(FULLCHAIN_FILE_NAME),
        key_path: material_dir.join(PRIVATE_KEY_FILE_NAME),
        cert_der: published_cert.cert_der,
    });

    resolver
        .validate_material_observer_source(&material_dir)
        .expect("visible managed paths must resolve to the one published generation");
}

#[cfg(unix)]
#[test]
fn observer_initial_marker_requires_the_complete_served_chain() {
    let temp_dir = TempDir::new().unwrap();
    let cert_a = write_named_test_cert_files(&temp_dir, "cert_a");
    let extra_chain_cert = write_named_test_cert_files(&temp_dir, "extra_chain_cert");
    let resolver = test_resolver(&cert_a);
    let material_dir = temp_dir.path().join("material");
    let generation = publish_symlink_generation(&temp_dir, &material_dir, "generation_a", &cert_a);
    let mut fullchain = std::fs::read_to_string(&cert_a.cert_path).unwrap();
    fullchain.push_str(&std::fs::read_to_string(&extra_chain_cert.cert_path).unwrap());
    std::fs::write(generation.join(FULLCHAIN_FILE_NAME), fullchain).unwrap();

    assert_eq!(
        initial_successful_tls_material_generation(&material_dir, &resolver),
        None,
        "matching leaf DER must not hide a changed served certificate chain"
    );

    let mut last_successful_generation = None;
    assert_eq!(
        observe_tls_material_once(&material_dir, &resolver, &mut last_successful_generation),
        TlsMaterialObservation::Published(generation)
    );
    assert_eq!(resolver.current_key().cert.len(), 2);
}
