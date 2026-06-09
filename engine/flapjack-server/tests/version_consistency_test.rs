#[test]
fn cargo_package_version_matches_stage_release() {
    assert_eq!(env!("CARGO_PKG_VERSION"), "1.0.10");
}
