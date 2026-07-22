use super::{
    acquire_data_dir_process_lock, build_log_layer_with_writer, build_tracing_subscriber,
    cors_origins_from_value, initialize_key_store, load_server_config, log_format_from_value,
    normalize_admin_key, read_admin_key, shutdown_timeout_secs_from_value,
    validate_startup_auth_policy, CorsMode, LogFormat, ServerConfig, StartupAuthValidationError,
    StartupAuthValidationOutcome, NO_AUTH_PUBLIC_BIND_WARNING,
};
use crate::test_helpers::{EnvVarRestoreGuard, ENV_MUTEX};
use axum::http::HeaderValue;
use flapjack_replication::config::NodeConfig;
use serde_json::Value;
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tracing_subscriber::layer::SubscriberExt;

// --- Shared test writer for capturing tracing output ---

#[derive(Clone)]
struct TestWriter(Arc<Mutex<Vec<u8>>>);

impl TestWriter {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    fn output(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl std::io::Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TestWriter {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[cfg(feature = "otel")]
fn capture_log_output(action: impl FnOnce()) -> String {
    let writer = TestWriter::new();
    let subscriber =
        tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone()));

    tracing::subscriber::with_default(subscriber, action);

    writer.output()
}

fn with_log_format_env<T>(value: Option<&str>, action: impl FnOnce() -> T) -> T {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let _restore = match value {
        Some(value) => EnvVarRestoreGuard::set("FLAPJACK_LOG_FORMAT", value),
        None => EnvVarRestoreGuard::remove("FLAPJACK_LOG_FORMAT"),
    };

    action()
}

// --- LogFormat parsing tests ---

#[test]
fn log_format_from_value_selects_json_for_json_input() {
    assert_eq!(log_format_from_value(Some("json")), LogFormat::Json);
    assert_eq!(log_format_from_value(Some("JSON")), LogFormat::Json);
    assert_eq!(log_format_from_value(Some("Json")), LogFormat::Json);
}

#[test]
fn log_format_from_value_defaults_to_text() {
    assert_eq!(log_format_from_value(None), LogFormat::Text);
    assert_eq!(log_format_from_value(Some("")), LogFormat::Text);
    assert_eq!(log_format_from_value(Some("text")), LogFormat::Text);
    assert_eq!(log_format_from_value(Some("TEXT")), LogFormat::Text);
    assert_eq!(log_format_from_value(Some("xml")), LogFormat::Text);
    assert_eq!(log_format_from_value(Some("bogus")), LogFormat::Text);
}
#[test]
fn flapjack_log_format_env_selects_json_layer() {
    with_log_format_env(Some("json"), || {
        let writer = TestWriter::new();
        let subscriber =
            tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("env-selected json logging");
        });

        let output = writer.output();
        let line = output
            .trim()
            .lines()
            .next()
            .expect("must emit one json line");
        let parsed: Value = serde_json::from_str(line).expect("output must be valid json");
        assert_eq!(parsed["fields"]["message"], "env-selected json logging");
    });
}
#[test]
fn flapjack_log_format_env_defaults_to_text_layer_for_invalid_values() {
    with_log_format_env(Some("bogus"), || {
        let writer = TestWriter::new();
        let subscriber =
            tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone()));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("env-selected text logging");
        });

        let output = writer.output();
        assert!(
            serde_json::from_str::<Value>(output.trim()).is_err(),
            "invalid FLAPJACK_LOG_FORMAT must fall back to text output"
        );
        assert!(
            output.contains("env-selected text logging"),
            "text output must include the logged message"
        );
    });
}

#[test]
fn shutdown_timeout_secs_from_value_defaults_to_30_when_unset() {
    assert_eq!(shutdown_timeout_secs_from_value(None), 30);
}

#[test]
fn shutdown_timeout_secs_from_value_parses_valid_integer() {
    assert_eq!(shutdown_timeout_secs_from_value(Some("45")), 45);
    assert_eq!(shutdown_timeout_secs_from_value(Some(" 7 ")), 7);
}

#[test]
fn shutdown_timeout_secs_from_value_defaults_to_30_for_invalid_empty_or_non_positive() {
    assert_eq!(shutdown_timeout_secs_from_value(Some("abc")), 30);
    assert_eq!(shutdown_timeout_secs_from_value(Some("")), 30);
    assert_eq!(shutdown_timeout_secs_from_value(Some("   ")), 30);
    assert_eq!(shutdown_timeout_secs_from_value(Some("0")), 30);
    assert_eq!(shutdown_timeout_secs_from_value(Some("-1")), 30);
}

#[test]
fn cors_origins_from_value_defaults_to_loopback_only_when_missing_or_empty() {
    assert_eq!(cors_origins_from_value(None), CorsMode::LoopbackOnly);
    assert_eq!(cors_origins_from_value(Some("")), CorsMode::LoopbackOnly);
    assert_eq!(cors_origins_from_value(Some("   ")), CorsMode::LoopbackOnly);
}

#[test]
fn cors_origins_from_value_parses_single_origin() {
    let mode = cors_origins_from_value(Some("https://allowed.example"));
    assert_eq!(
        mode,
        CorsMode::Restricted(vec![HeaderValue::from_static("https://allowed.example")])
    );
}

#[test]
fn cors_origins_from_value_parses_comma_separated_origins_with_trimmed_whitespace() {
    let mode = cors_origins_from_value(Some(
        "  https://allowed.example  , https://second.example  ",
    ));
    assert_eq!(
        mode,
        CorsMode::Restricted(vec![
            HeaderValue::from_static("https://allowed.example"),
            HeaderValue::from_static("https://second.example"),
        ])
    );
}

#[test]
fn cors_origins_from_value_ignores_trailing_commas_and_empty_segments() {
    let mode = cors_origins_from_value(Some("https://allowed.example, ,https://second.example,,"));
    assert_eq!(
        mode,
        CorsMode::Restricted(vec![
            HeaderValue::from_static("https://allowed.example"),
            HeaderValue::from_static("https://second.example"),
        ])
    );
}

#[test]
fn validate_startup_auth_policy_rejects_missing_blank_and_short_production_admin_key() {
    assert_eq!(
        validate_startup_auth_policy("production", false, None, "127.0.0.1:7700", false),
        Err(StartupAuthValidationError::MissingAdminKeyInProduction)
    );
    assert_eq!(
        validate_startup_auth_policy("production", false, Some("   "), "127.0.0.1:7700", false),
        Err(StartupAuthValidationError::MissingAdminKeyInProduction)
    );
    assert_eq!(
        validate_startup_auth_policy(
            "production",
            false,
            Some("short-key"),
            "127.0.0.1:7700",
            false
        ),
        Err(StartupAuthValidationError::AdminKeyTooShortInProduction)
    );
}

#[test]
fn validate_startup_auth_policy_classifies_resolved_bind_posture() {
    let validate_development = |no_auth, bind_addr, allow_public_bind| {
        validate_startup_auth_policy("development", no_auth, None, bind_addr, allow_public_bind)
    };
    let accepted = Ok(StartupAuthValidationOutcome::Accepted);
    let explicitly_allowed = Ok(StartupAuthValidationOutcome::ExplicitlyAllowedPublicNoAuthBind);

    assert_eq!(
        validate_development(true, "127.0.0.1:7700", false),
        accepted
    );
    assert_eq!(validate_development(true, "[::1]:7700", false), accepted);
    assert_eq!(
        validate_development(true, "0.0.0.0:7700", false),
        Err(StartupAuthValidationError::NoAuthPublicBind {
            bind_addr: "0.0.0.0:7700".parse::<SocketAddr>().unwrap(),
        })
    );
    assert_eq!(
        validate_development(true, "0.0.0.0:7700", true),
        explicitly_allowed
    );
    assert_eq!(
        validate_development(true, "[::]:7700", false),
        Err(StartupAuthValidationError::NoAuthPublicBind {
            bind_addr: "[::]:7700".parse::<SocketAddr>().unwrap(),
        })
    );
    assert_eq!(
        validate_development(true, "[::]:7700", true),
        explicitly_allowed
    );
    assert_eq!(
        validate_development(true, "public.example:7700", false),
        Err(StartupAuthValidationError::NoAuthHostnameBind {
            bind_addr: "public.example:7700".to_string(),
        })
    );
    assert_eq!(
        validate_development(true, "public.example:7700", true),
        explicitly_allowed
    );
    assert_eq!(validate_development(false, "0.0.0.0:7700", false), accepted);
    assert_eq!(validate_development(true, "not-a-socket", false), accepted);
    assert_eq!(
        validate_startup_auth_policy(
            "production",
            true,
            Some("1234567890abcdef"),
            "0.0.0.0:7700",
            true,
        ),
        Err(StartupAuthValidationError::NoAuthInProduction)
    );
    assert_eq!(
        validate_startup_auth_policy(
            "production",
            false,
            Some("1234567890abcdef"),
            "0.0.0.0:7700",
            false,
        ),
        accepted
    );
}

#[test]
fn resolved_node_config_bind_drives_startup_auth_policy() {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let _bind_addr = EnvVarRestoreGuard::set("FLAPJACK_BIND_ADDR", "127.0.0.1:0");
    let _node_id = EnvVarRestoreGuard::remove("FLAPJACK_NODE_ID");
    let _peers = EnvVarRestoreGuard::remove("FLAPJACK_PEERS");
    let temp_dir = TempDir::new().unwrap();
    let node_json = serde_json::json!({
        "node_id": "stage-2-node",
        "bind_addr": "0.0.0.0:0",
        "peers": []
    });
    std::fs::write(temp_dir.path().join("node.json"), node_json.to_string())
        .expect("test node.json must be writable");

    let node_config = NodeConfig::load_or_default(temp_dir.path());

    assert_eq!(node_config.bind_addr, "0.0.0.0:0");
    assert_eq!(
        validate_startup_auth_policy("development", true, None, &node_config.bind_addr, false),
        Err(StartupAuthValidationError::NoAuthPublicBind {
            bind_addr: "0.0.0.0:0".parse::<SocketAddr>().unwrap()
        })
    );
}

#[test]
fn no_auth_public_bind_warning_text_is_canonical() {
    assert!(
        NO_AUTH_PUBLIC_BIND_WARNING.contains("FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND=1"),
        "warning must name the explicit public no-auth override"
    );
}

// --- JSON output format tests ---
#[test]
fn json_mode_emits_valid_json_with_expected_fields() {
    let writer = TestWriter::new();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(writer.clone()),
    );

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(key = "val", "test message");
    });

    let output = writer.output();
    let lines: Vec<&str> = output.trim().lines().collect();
    assert!(!lines.is_empty(), "JSON mode must emit at least one line");

    for line in &lines {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("each log line must be valid JSON");
        let obj = parsed
            .as_object()
            .expect("each JSON line must be an object");

        // tracing_subscriber JSON format: timestamp, level, target, fields.message
        assert!(obj.contains_key("timestamp"), "missing 'timestamp' field");
        assert!(obj.contains_key("level"), "missing 'level' field");
        assert!(obj.contains_key("target"), "missing 'target' field");
        assert!(obj.contains_key("fields"), "missing 'fields' field");

        let fields = obj["fields"]
            .as_object()
            .expect("'fields' must be an object");
        assert!(
            fields.contains_key("message"),
            "missing 'fields.message' field"
        );
        assert_eq!(fields["message"], "test message");
    }
}
#[test]
fn json_mode_includes_span_context() {
    let writer = TestWriter::new();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(writer.clone()),
    );

    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("test_span", some_field = "some_value");
        let _guard = span.enter();
        tracing::info!("inside span");
    });

    let output = writer.output();
    let line = output.trim().lines().next().expect("must emit a log line");
    let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
    let obj = parsed.as_object().unwrap();

    // Span context appears under "spans" array in tracing_subscriber JSON format
    assert!(
        obj.contains_key("spans"),
        "JSON output with active span must include 'spans' field"
    );
    let spans = obj["spans"].as_array().expect("'spans' must be an array");
    assert!(!spans.is_empty(), "spans array must not be empty");

    let span_obj = spans[0].as_object().unwrap();
    assert_eq!(span_obj["name"], "test_span");
    assert_eq!(span_obj["some_field"], "some_value");
}

// --- Text output format test ---
#[test]
fn text_mode_emits_human_readable_non_json_output() {
    let writer = TestWriter::new();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(writer.clone()));

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("text mode message");
    });

    let output = writer.output();
    assert!(
        !output.trim().is_empty(),
        "text mode must emit at least one line"
    );
    // Text format should NOT parse as valid JSON
    let parse_result: Result<serde_json::Value, _> = serde_json::from_str(output.trim());
    assert!(
        parse_result.is_err(),
        "text mode output must not be valid JSON"
    );
    // But should contain the message text
    assert!(
        output.contains("text mode message"),
        "text output must contain the log message"
    );
}

// --- request_id in JSON span context test ---
#[test]
fn json_mode_includes_request_id_from_span() {
    let writer = TestWriter::new();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_writer(writer.clone()),
    );

    tracing::subscriber::with_default(subscriber, || {
        // Mirror the span shape from request_id_middleware in middleware.rs
        let span = tracing::info_span!("http_request", request_id = tracing::field::Empty);
        span.record("request_id", tracing::field::display("test-req-id-123"));
        let _guard = span.enter();
        tracing::info!("handling request");
    });

    let output = writer.output();
    let line = output.trim().lines().next().expect("must emit a log line");
    let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
    let obj = parsed.as_object().unwrap();

    let spans = obj["spans"]
        .as_array()
        .expect("must include 'spans' in JSON output");
    let http_span = spans
        .iter()
        .find(|s| s["name"] == "http_request")
        .expect("must include http_request span");

    assert_eq!(
        http_span["request_id"], "test-req-id-123",
        "request_id must appear in the http_request span context"
    );
}

#[test]
fn normalize_admin_key_rejects_blank_values() {
    assert_eq!(normalize_admin_key("  \n\t  "), None);
    assert_eq!(
        normalize_admin_key("  test-admin-key  "),
        Some("test-admin-key".to_string())
    );
}

#[test]
fn read_admin_key_rejects_blank_files() {
    let temp_dir = TempDir::new().unwrap();
    let admin_key_file = temp_dir.path().join(".admin_key");
    std::fs::write(&admin_key_file, "   \n").unwrap();

    let error = read_admin_key(&admin_key_file).unwrap_err();

    assert!(
        error.contains("empty"),
        "blank .admin_key files must be rejected"
    );
}

#[cfg(unix)]
#[test]
fn initialize_key_store_persists_env_admin_key_with_restrictive_permissions() {
    let temp_dir = TempDir::new().unwrap();
    let server_config = ServerConfig {
        env_mode: "development".to_string(),
        no_auth: false,
        disable_dashboard: false,
        allow_no_auth_public_bind: false,
        admin_key_env: Some("  env-admin-key  ".to_string()),
        data_dir: temp_dir.path().display().to_string(),
        bind_addr: "127.0.0.1:7700".to_string(),
        _data_dir_lock: acquire_data_dir_process_lock(temp_dir.path()).unwrap(),
    };

    let (_key_store, admin_key, key_is_new) = initialize_key_store(&server_config, temp_dir.path());
    let metadata = std::fs::metadata(temp_dir.path().join(".admin_key")).unwrap();

    assert_eq!(admin_key, Some("env-admin-key".to_string()));
    assert!(!key_is_new);
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
}

#[cfg(unix)]
#[test]
fn shared_admin_key_persistence_sets_restrictive_permissions() {
    let temp_dir = TempDir::new().unwrap();
    let admin_key_file = temp_dir.path().join(".admin_key");

    crate::admin_key_persistence::persist_admin_key_file(
        &admin_key_file,
        "shared-persist-key",
        crate::admin_key_persistence::PermissionFailureMode::ReturnError,
    )
    .expect("shared persistence helper should write admin key");

    let metadata = std::fs::metadata(&admin_key_file).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::read_to_string(&admin_key_file).unwrap(),
        "shared-persist-key"
    );
}

#[cfg(unix)]
#[test]
fn shared_permission_enforcer_sets_restrictive_permissions_for_existing_files() {
    let temp_dir = TempDir::new().unwrap();
    let admin_key_file = temp_dir.path().join(".admin_key");
    std::fs::write(&admin_key_file, "existing-key").unwrap();

    crate::admin_key_persistence::ensure_admin_key_permissions(
        &admin_key_file,
        crate::admin_key_persistence::PermissionFailureMode::ReturnError,
    )
    .expect("permission enforcer should succeed for writable test file");

    let metadata = std::fs::metadata(&admin_key_file).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
}

/// Verify the capabilities line contains the correct enabled/disabled labels
/// matching the compiled feature flags.
#[test]
fn startup_banner_shows_capabilities() {
    let line = super::format_capabilities_line();

    let vs_expected = if cfg!(feature = "vector-search") {
        "vector-search: enabled"
    } else {
        "vector-search: disabled"
    };
    let local_expected = if cfg!(feature = "vector-search-local") {
        "local-embeddings: enabled"
    } else {
        "local-embeddings: disabled"
    };

    assert!(
        line.contains(vs_expected),
        "capabilities line should contain '{}', got: {}",
        vs_expected,
        line
    );
    assert!(
        line.contains(local_expected),
        "capabilities line should contain '{}', got: {}",
        local_expected,
        line
    );
}

#[test]
fn flapjack_disable_dashboard_env_parses_true_and_defaults_false() {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let _disable_dashboard = EnvVarRestoreGuard::remove("FLAPJACK_DISABLE_DASHBOARD");

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());

        let server_config = load_server_config();

        assert!(!server_config.disable_dashboard);
    }

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _disable_dashboard_set = EnvVarRestoreGuard::set("FLAPJACK_DISABLE_DASHBOARD", "1");

        let server_config = load_server_config();

        assert!(server_config.disable_dashboard);
    }
}

#[test]
fn flapjack_allow_no_auth_public_bind_env_parses_one_and_defaults_false() {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let _allow_public_bind = EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND");

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());

        let server_config = load_server_config();

        assert!(!server_config.allow_no_auth_public_bind);
    }

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _allow_public_bind_set =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND", "true");

        let server_config = load_server_config();

        assert!(!server_config.allow_no_auth_public_bind);
    }

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _allow_public_bind_set =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND", "1");

        let server_config = load_server_config();

        assert!(server_config.allow_no_auth_public_bind);
    }
}

// --- Tracing subscriber builder tests ---

#[test]
fn build_tracing_subscriber_produces_working_dispatch() {
    let _guard = ENV_MUTEX.lock().expect("env mutex");
    let _rust_log = EnvVarRestoreGuard::set("RUST_LOG", "info");
    let _log_format = EnvVarRestoreGuard::remove("FLAPJACK_LOG_FORMAT");

    let writer = TestWriter::new();

    #[cfg(not(feature = "otel"))]
    let dispatch = build_tracing_subscriber(writer.clone());
    #[cfg(feature = "otel")]
    let (dispatch, _otel_guard) = build_tracing_subscriber(writer.clone());

    tracing::dispatcher::with_default(&dispatch, || {
        tracing::info!("subscriber-init-smoke-test");
    });

    let output = writer.output();
    assert!(
        output.contains("subscriber-init-smoke-test"),
        "expected subscriber to capture log output, got: {output}"
    );
}

#[cfg(feature = "otel")]
#[test]
fn build_tracing_subscriber_returns_none_guard_without_endpoint() {
    let _guard = ENV_MUTEX.lock().expect("env mutex");
    let _rust_log = EnvVarRestoreGuard::set("RUST_LOG", "info");
    let _otel = EnvVarRestoreGuard::remove("OTEL_EXPORTER_OTLP_ENDPOINT");
    let _log_format = EnvVarRestoreGuard::remove("FLAPJACK_LOG_FORMAT");

    let writer = TestWriter::new();
    let (_dispatch, otel_guard) = build_tracing_subscriber(writer);

    assert!(
        otel_guard.is_none(),
        "expected no OtelGuard when OTEL_EXPORTER_OTLP_ENDPOINT is unset"
    );
}

#[cfg(feature = "otel")]
#[test]
fn otel_startup_status_logs_initialized_message() {
    let output = capture_log_output(|| {
        super::log_otel_startup_status(true);
    });

    assert!(
        output.contains("[otel] OTEL tracing initialized"),
        "expected OTEL startup initialization log line, got: {output}"
    );
}

#[cfg(feature = "otel")]
#[test]
fn otel_startup_status_logs_disabled_message() {
    let output = capture_log_output(|| {
        super::log_otel_startup_status(false);
    });

    assert!(
        output.contains("OTEL_EXPORTER_OTLP_ENDPOINT unset, empty, or invalid"),
        "expected OTEL disabled log line to describe all disabled cases, got: {output}"
    );
}

/// Verifies the doc comment on load_server_config only claims fields it actually loads.
#[test]
fn load_server_config_doc_lists_only_fields_loaded_here() {
    let source = include_str!("startup.rs");
    let expected_doc =
        "/// Loads startup configuration from environment variables for mode/auth, optional\n\
/// dashboard lockdown, public no-auth bind override, admin key, data directory,\n\
/// and bind address, then\n\
/// initializes logging and acquires the per-process data directory lock.";
    let stale_doc =
            "/// Loads server configuration from environment variables: data directory, bind address,\n\
/// auth mode, admin key, SSL settings, replication config, and operational flags.";
    assert!(
        source.contains(&format!(
            "{expected_doc}\npub(crate) fn load_server_config()"
        )),
        "load_server_config doc should describe only the config fields and setup performed here"
    );
    assert!(
        !source.contains(&format!("{stale_doc}\npub(crate) fn load_server_config()")),
        "load_server_config doc must not claim SSL/replication/operational flags loading"
    );
}
