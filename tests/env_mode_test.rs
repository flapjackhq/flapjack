use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn production_mode_rejects_missing_key() {
    Command::new(assert_cmd::cargo::cargo_bin("flapjack-server"))
        .env("FLAPJACK_ENV", "production")
        .env_remove("FLAPJACK_ADMIN_KEY")
        .assert()
        .failure()
        .code(1)
        .stderr(contains(
            "FLAPJACK_ADMIN_KEY is required in production mode",
        ));
}

#[test]
fn production_mode_rejects_short_key() {
    Command::new(assert_cmd::cargo::cargo_bin("flapjack-server"))
        .env("FLAPJACK_ENV", "production")
        .env("FLAPJACK_ADMIN_KEY", "tooshort")
        .assert()
        .failure()
        .code(1)
        .stderr(contains("at least 16 characters"));
}

#[test]
fn development_mode_allows_missing_key() {
    Command::new(assert_cmd::cargo::cargo_bin("flapjack-server"))
        .env("FLAPJACK_ENV", "development")
        .env_remove("FLAPJACK_ADMIN_KEY")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:17798")
        .env("FLAPJACK_DATA_DIR", "/tmp/fj_test_dev_mode")
        .timeout(std::time::Duration::from_secs(2))
        .assert()
        .interrupted();
}

#[test]
fn production_mode_accepts_valid_key() {
    Command::new(assert_cmd::cargo::cargo_bin("flapjack-server"))
        .env("FLAPJACK_ENV", "production")
        .env("FLAPJACK_ADMIN_KEY", "abcdef0123456789")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:17799")
        .env("FLAPJACK_DATA_DIR", "/tmp/fj_test_prod_mode")
        .timeout(std::time::Duration::from_secs(2))
        .assert()
        .interrupted();
}
