use super::*;

#[cfg(unix)]
#[test]
#[serial]
fn publication_through_relative_material_dir_exposes_pair() {
    let fixture = TestDirectory::new("relative_publish");
    let _cwd = CwdGuard::change_to(fixture.path());
    let relative_dir = PathBuf::from("./data/ssl/acme");
    let private_key_pem = generated_private_key_pem();

    SslManager::write_certificate_files_to_dir(
        &relative_dir,
        TEST_CERTIFICATE_PEM,
        &private_key_pem,
    )
    .expect("publishing through a relative material dir must succeed");

    let certificate = fs::read_to_string(relative_dir.join("fullchain.pem"))
        .expect("the certificate must be readable through the relative visible path");
    assert_eq!(certificate, TEST_CERTIFICATE_PEM);
    let key = fs::read_to_string(relative_dir.join("privkey.pem"))
        .expect("the private key must be readable through the relative visible path");
    assert!(
        !key.trim().is_empty() && KeyPair::from_pem(&key).is_ok(),
        "relative-path publication must persist a parseable private key; got {key:?}"
    );
}

#[cfg(unix)]
#[test]
fn repeated_publication_retains_bounded_reader_safe_generations() {
    let fixture = TestDirectory::new("bounded_retention");
    let material_dir = fixture.path().join("material");

    for round in 0..4 {
        let private_key_pem = generated_private_key_pem();
        SslManager::write_certificate_files_to_dir(
            &material_dir,
            TEST_CERTIFICATE_PEM,
            &private_key_pem,
        )
        .unwrap_or_else(|error| panic!("round {round} publication must succeed: {error}"));

        let generations = generation_dirs(fixture.path());
        let expected_generation_count = if round == 0 { 1 } else { 2 };
        assert_eq!(
            generations.len(),
            expected_generation_count,
            "publication must retain the live generation and at most one reader-safe rollback; \
             after round {round} found {generations:?}"
        );
    }

    let (certificate, key) = material_files(&material_dir);
    assert!(
        certificate.is_some() && key.is_some(),
        "the retained generation must still expose a complete pair"
    );
}

#[cfg(unix)]
#[test]
fn publication_keeps_resolved_previous_generation_readable() {
    let fixture = TestDirectory::new("reader_safe_retention");
    let material_dir = fixture.path().join("material");
    let old_certificate = b"old certificate generation\n";
    let old_private_key = generated_private_key_pem();
    SslManager::write_certificate_files_to_dir(
        &material_dir,
        std::str::from_utf8(old_certificate).expect("fixture certificate must be utf-8"),
        &old_private_key,
    )
    .expect("the initial generation must publish");

    let resolved_old_generation = fixture
        .path()
        .join(fs::read_link(&material_dir).expect("the visible path must identify its generation"));
    let new_private_key = generated_private_key_pem();
    SslManager::write_certificate_files_to_dir(
        &material_dir,
        TEST_CERTIFICATE_PEM,
        &new_private_key,
    )
    .expect("the replacement generation must publish");

    assert_eq!(
        material_files(&resolved_old_generation),
        (
            Some(old_certificate.to_vec()),
            Some(old_private_key.into_bytes())
        ),
        "a reader that resolved the old generation before the swap must still be able to open its complete pair"
    );
    assert_eq!(
        material_files(&material_dir),
        (
            Some(TEST_CERTIFICATE_PEM.as_bytes().to_vec()),
            Some(new_private_key.into_bytes())
        )
    );
}

#[cfg(unix)]
#[test]
fn failed_publication_leaves_no_abandoned_generation() {
    let fixture = TestDirectory::new("failed_publish_cleanup");
    let material_path = fixture.path().join("material");
    fs::write(&material_path, b"not a directory")
        .expect("a plain file must seed the material path");

    let private_key_pem = generated_private_key_pem();
    let result = SslManager::write_certificate_files_to_dir(
        &material_path,
        TEST_CERTIFICATE_PEM,
        &private_key_pem,
    );

    assert!(
        result.is_err(),
        "publishing over a plain file must fail rather than clobber it"
    );
    let generations = generation_dirs(fixture.path());
    assert!(
        generations.is_empty(),
        "a failed publication must retire its abandoned staged generation; found {generations:?}"
    );
}

#[cfg(unix)]
#[test]
fn real_directory_migration_never_drops_visible_pair_on_failure() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = TestDirectory::new("migration_fault");
    let material_dir = fixture.path().join("material");
    let old_certificate = b"old certificate generation\n".to_vec();
    let old_private_key = generated_private_key_pem().into_bytes();
    let old_pair = seed_material_pair(&material_dir, &old_certificate, &old_private_key);

    let mut locked = fs::metadata(fixture.path())
        .expect("the fixture must exist")
        .permissions();
    locked.set_mode(0o500);
    fs::set_permissions(fixture.path(), locked).expect("the parent must be made read-only");
    let staging_rejected = fs::create_dir(fixture.path().join(".flapjack-acme-probe")).is_err();

    let new_private_key = generated_private_key_pem().into_bytes();
    let result = SslManager::write_certificate_files_to_dir(
        &material_dir,
        TEST_CERTIFICATE_PEM,
        std::str::from_utf8(&new_private_key).expect("generated key must be utf-8 PEM"),
    );
    let visible_pair = material_files(&material_dir);

    let mut restored = fs::metadata(fixture.path())
        .expect("the fixture must exist")
        .permissions();
    restored.set_mode(0o755);
    fs::set_permissions(fixture.path(), restored).expect("the parent must be restored");

    assert!(
        staging_rejected,
        "read-only parent must reject staging; running as root defeats this guard"
    );
    assert!(
        result.is_err(),
        "staging into a read-only parent must fail before mutating the visible path"
    );
    assert_eq!(
        visible_pair, old_pair,
        "a staging failure must leave the visible material pair fully intact"
    );
}

#[cfg(unix)]
#[test]
fn unrelated_current_symlink_cannot_skip_real_directory_adoption() {
    use std::os::unix::fs::symlink;

    let fixture = TestDirectory::new("unrelated_current");
    let material_dir = fixture.path().join("material");
    let old_certificate = b"old certificate generation\n".to_vec();
    let old_private_key = generated_private_key_pem().into_bytes();
    let old_pair = seed_material_pair(&material_dir, &old_certificate, &old_private_key);
    let unrelated_generation = fixture.path().join("operator-owned-generation");
    seed_material_pair(
        &unrelated_generation,
        b"operator certificate\n",
        generated_private_key_pem().as_bytes(),
    );
    symlink("../operator-owned-generation", material_dir.join("current"))
        .expect("unrelated current link must be seedable");

    let new_private_key = generated_private_key_pem().into_bytes();
    let result = SslManager::write_certificate_files_to_dir(
        &material_dir,
        TEST_CERTIFICATE_PEM,
        std::str::from_utf8(&new_private_key).expect("generated key must be utf-8 PEM"),
    );

    let new_pair = (
        Some(TEST_CERTIFICATE_PEM.as_bytes().to_vec()),
        Some(new_private_key),
    );
    let visible_pair = material_files(&material_dir);
    assert!(
        (result.is_err() && visible_pair == old_pair) || (result.is_ok() && visible_pair == new_pair),
        "publication must reject before mutation or fully repair an unrelated current link; result={result:?}; visible_pair={visible_pair:?}"
    );
}

#[cfg(unix)]
#[test]
fn partial_current_adoption_cannot_expose_mixed_pair_after_restart() {
    use std::os::unix::fs::symlink;

    let fixture = TestDirectory::new("partial_adoption");
    let material_dir = fixture.path().join("material");
    fs::create_dir(&material_dir).expect("material directory must be creatable");
    let old_generation = fixture.path().join(".flapjack-acme-material-123-0-boot");
    let old_certificate = b"old certificate generation\n".to_vec();
    let old_private_key = generated_private_key_pem().into_bytes();
    seed_material_pair(&old_generation, &old_certificate, &old_private_key);
    symlink(
        "../.flapjack-acme-material-123-0-boot",
        material_dir.join("current"),
    )
    .expect("current link must be seedable");
    symlink("current/fullchain.pem", material_dir.join("fullchain.pem"))
        .expect("certificate link must be seedable");
    fs::write(material_dir.join("privkey.pem"), &old_private_key)
        .expect("partial adoption must leave one real visible file");

    let first_private_key = generated_private_key_pem().into_bytes();
    SslManager::write_certificate_files_to_dir(
        &material_dir,
        TEST_CERTIFICATE_PEM,
        std::str::from_utf8(&first_private_key).expect("generated key must be utf-8 PEM"),
    )
    .expect("publication must repair a safely recoverable partial adoption");
    assert_eq!(
        material_files(&material_dir),
        (
            Some(TEST_CERTIFICATE_PEM.as_bytes().to_vec()),
            Some(first_private_key)
        )
    );

    let second_private_key = generated_private_key_pem().into_bytes();
    SslManager::write_certificate_files_to_dir(
        &material_dir,
        "second certificate generation\n",
        std::str::from_utf8(&second_private_key).expect("generated key must be utf-8 PEM"),
    )
    .expect("publication after restart repair must remain healthy");
    assert_eq!(
        material_files(&material_dir),
        (
            Some(b"second certificate generation\n".to_vec()),
            Some(second_private_key)
        )
    );
}

#[cfg(unix)]
#[test]
fn publication_retention_preserves_sibling_material_owner_and_unrelated_prefixed_data() {
    let fixture = TestDirectory::new("owner_scoped_retention");
    let material_a = fixture.path().join("material");
    let material_b = fixture.path().join("material-2");

    let key_a = generated_private_key_pem();
    SslManager::write_certificate_files_to_dir(&material_a, TEST_CERTIFICATE_PEM, &key_a)
        .expect("first material owner must publish");
    let key_b = generated_private_key_pem();
    SslManager::write_certificate_files_to_dir(&material_b, "certificate b\n", &key_b)
        .expect("second material owner must publish");
    let sibling_pair_before = material_files(&material_b);
    let unrelated = fixture.path().join(".flapjack-acme-operator-owned-data");
    fs::create_dir(&unrelated).expect("unrelated prefixed data must be seedable");
    fs::write(unrelated.join("sentinel"), b"keep").expect("sentinel must be writable");

    let replacement_key = generated_private_key_pem();
    SslManager::write_certificate_files_to_dir(&material_a, "certificate a2\n", &replacement_key)
        .expect("republishing one owner must succeed");

    assert_eq!(
        material_files(&material_b),
        sibling_pair_before,
        "retention for one material path must not delete a sibling owner's live generation"
    );
    assert!(
        unrelated.join("sentinel").exists(),
        "retention must not delete unrelated prefixed data in the shared parent"
    );
}

#[test]
fn successful_publication_is_not_failed_by_retention_error() {
    let fixture = TestDirectory::new("retention_warning");
    let material_dir = fixture.path().join("material");
    let private_key = generated_private_key_pem();
    let _fault = MaterialPublicationFaultGuard::inject(MaterialPublicationFault::Retention);

    let result = SslManager::write_certificate_files_to_dir(
        &material_dir,
        TEST_CERTIFICATE_PEM,
        &private_key,
    );

    assert!(
        result.is_ok(),
        "retention must not mask publication: {result:?}"
    );
    assert_eq!(
        material_files(&material_dir),
        (
            Some(TEST_CERTIFICATE_PEM.as_bytes().to_vec()),
            Some(private_key.into_bytes())
        )
    );
}

#[test]
fn staging_failures_retire_partial_generations_for_write_and_sync_boundaries() {
    let fault_cases = [
        MaterialPublicationFault::CertificateWrite,
        MaterialPublicationFault::PrivateKeyWrite,
        MaterialPublicationFault::FileSync,
        MaterialPublicationFault::GenerationSync,
        MaterialPublicationFault::ParentSync,
    ];

    for fault in fault_cases {
        let fixture = TestDirectory::new("stage_fault_cleanup");
        let material_dir = fixture.path().join("material");
        let _fault = MaterialPublicationFaultGuard::inject(fault);

        for attempt in 0..3 {
            let private_key_pem = generated_private_key_pem();
            let result = SslManager::write_certificate_files_to_dir(
                &material_dir,
                TEST_CERTIFICATE_PEM,
                &private_key_pem,
            );
            assert!(
                result.is_err(),
                "attempt {attempt} with injected {fault:?} must fail"
            );
            assert_eq!(
                generation_dirs(fixture.path()),
                Vec::<String>::new(),
                "attempt {attempt} with injected {fault:?} must retire its partial generation"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn link_swap_failures_do_not_accumulate_internal_staging_links() {
    let fixture = TestDirectory::new("link_swap_failure_cleanup");
    let material_dir = fixture.path().join("material");
    seed_material_pair(
        &material_dir,
        b"old certificate generation\n",
        generated_private_key_pem().as_bytes(),
    );

    for attempt in 0..3 {
        let private_key = generated_private_key_pem();
        let _fault =
            MaterialPublicationFaultGuard::inject(MaterialPublicationFault::LinkSwapAfterStaging);
        let result = SslManager::write_certificate_files_to_dir(
            &material_dir,
            TEST_CERTIFICATE_PEM,
            &private_key,
        );

        assert!(
            result.is_err(),
            "attempt {attempt} must fail after staging the internal link"
        );
        assert_eq!(
            internal_staging_links(&material_dir),
            Vec::<String>::new(),
            "attempt {attempt} must retire its internal staging link"
        );
    }
}

#[cfg(unix)]
#[test]
fn publication_restart_retires_abandoned_internal_staging_links() {
    use std::os::unix::fs::symlink;

    let fixture = TestDirectory::new("link_swap_restart_cleanup");
    let material_dir = fixture.path().join("material");
    seed_material_pair(
        &material_dir,
        b"old certificate generation\n",
        generated_private_key_pem().as_bytes(),
    );
    symlink(
        "current",
        material_dir.join(".flapjack-acme-current-123-0-staging"),
    )
    .expect("an abandoned owner staging link must be seedable");

    let private_key = generated_private_key_pem();
    SslManager::write_certificate_files_to_dir(&material_dir, TEST_CERTIFICATE_PEM, &private_key)
        .expect("publication after restart must succeed");

    assert_eq!(
        internal_staging_links(&material_dir),
        Vec::<String>::new(),
        "restart publication must retire abandoned owner staging links"
    );
}
