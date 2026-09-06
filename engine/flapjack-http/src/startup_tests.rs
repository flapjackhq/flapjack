//! Stub summary for engine/flapjack-http/src/startup_tests.rs.
use super::{
    acquire_data_dir_process_lock, build_log_layer_with_writer, build_tracing_subscriber,
    cors_origins_from_value, initialize_key_store, initialize_key_store_for_mode,
    load_server_config, log_format_from_value, normalize_admin_key, read_admin_key,
    shutdown_timeout_secs_from_value, startup_banner_urls, validate_startup_auth_policy, CorsMode,
    LogFormat, ServerConfig, StartupAuthValidationError, StartupAuthValidationOutcome,
    StartupPersistenceMode, TlsPaths, NO_AUTH_PUBLIC_BIND_WARNING,
};
use crate::test_helpers::{EnvVarRestoreGuard, ENV_MUTEX};
use axum::http::HeaderValue;
use flapjack_replication::config::NodeConfig;
use serde_json::Value;
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
        tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone(), false));

    tracing::subscriber::with_default(subscriber, action);

    writer.output()
}

/// TODO: Document with_log_format_env.
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
/// TODO: Document flapjack_log_format_env_selects_json_layer.
#[test]
fn flapjack_log_format_env_selects_json_layer() {
    with_log_format_env(Some("json"), || {
        let writer = TestWriter::new();
        let subscriber =
            tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone(), false));

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
/// TODO: Document flapjack_log_format_env_defaults_to_text_layer_for_invalid_values.
#[test]
fn flapjack_log_format_env_defaults_to_text_layer_for_invalid_values() {
    with_log_format_env(Some("bogus"), || {
        let writer = TestWriter::new();
        let subscriber =
            tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone(), false));

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

/// A non-terminal sink (log file, journald, pipe) must receive plain,
/// field-greppable text so downstream security-audit event scrapers can match
/// `field="value"` pairs. ANSI escape codes between the field name and `=`
/// would defeat that, so `text_ansi=false` must produce escape-free output.
#[test]
fn text_log_layer_omits_ansi_when_not_a_terminal() {
    with_log_format_env(None, || {
        let writer = TestWriter::new();
        let subscriber =
            tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone(), false));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                event = "ansi_probe_event",
                actor = "admin_api_key",
                "security event: admin action"
            );
        });

        let output = writer.output();
        assert!(
            !output.contains('\u{1b}'),
            "non-terminal text logs must contain no ANSI escape codes, got: {output:?}"
        );
        assert!(
            output.contains("event=\"ansi_probe_event\""),
            "audit event fields must be greppable as field=\"value\" in text logs, got: {output:?}"
        );
    });
}

/// The complementary arm: at an interactive terminal (`text_ansi=true`) the
/// text format keeps ANSI color. This proves the omission above is caused by
/// the flag, not by an unrelated formatter change.
#[test]
fn text_log_layer_keeps_ansi_for_a_terminal() {
    with_log_format_env(None, || {
        let writer = TestWriter::new();
        let subscriber =
            tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone(), true));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(event = "ansi_probe_event", "security event");
        });

        let output = writer.output();
        assert!(
            output.contains('\u{1b}'),
            "terminal text logs should retain ANSI color, got: {output:?}"
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

/// TODO: Document validate_startup_auth_policy_rejects_missing_blank_and_short_production_admin_key.
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
    for variant in ["Production", " production ", "PRODUCTION"] {
        assert_eq!(
            validate_startup_auth_policy(variant, false, None, "127.0.0.1:7700", false),
            Err(StartupAuthValidationError::MissingAdminKeyInProduction),
            "variant {variant:?} must keep production admin-key enforcement"
        );
    }
}

/// TODO: Document validate_startup_auth_policy_classifies_resolved_bind_posture.
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

/// TODO: Document resolved_node_config_bind_drives_startup_auth_policy.
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
/// TODO: Document json_mode_emits_valid_json_with_expected_fields.
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
/// TODO: Document json_mode_includes_span_context.
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
/// TODO: Document text_mode_emits_human_readable_non_json_output.
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
/// TODO: Document json_mode_includes_request_id_from_span.
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

/// TODO: Document initialize_key_store_persists_env_admin_key_with_restrictive_permissions.
#[cfg(unix)]
#[test]
fn initialize_key_store_persists_env_admin_key_with_restrictive_permissions() {
    let temp_dir = TempDir::new().unwrap();
    let server_config = ServerConfig {
        env_mode: "development".to_string(),
        no_auth: false,
        api_profile: crate::api_profile::ApiProfile::Full,
        disable_dashboard: false,
        allow_no_auth_public_bind: false,
        admin_key_env: Some("  env-admin-key  ".to_string()),
        replication_api_key_env: None,
        data_dir: temp_dir.path().display().to_string(),
        bind_addr: "127.0.0.1:7700".to_string(),
        tls_paths: None,
        node_config: NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            advertise_addr: None,
            peers: Vec::new(),
            bootstrap_peer: None,
        },
        _data_dir_lock: acquire_data_dir_process_lock(temp_dir.path()).unwrap(),
    };

    let initialized = initialize_key_store(&server_config, temp_dir.path()).unwrap();
    let metadata = std::fs::metadata(temp_dir.path().join(".admin_key")).unwrap();

    assert_eq!(initialized.admin_key, Some("env-admin-key".to_string()));
    assert!(!initialized.key_is_new);
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
}

#[test]
fn active_fence_rejects_credential_rotation_without_changing_protected_bytes() {
    let temp_dir = TempDir::new().unwrap();
    let mut server_config = ServerConfig {
        env_mode: "development".to_string(),
        no_auth: false,
        api_profile: crate::api_profile::ApiProfile::Full,
        disable_dashboard: false,
        allow_no_auth_public_bind: false,
        admin_key_env: Some("predecessor-admin-key".to_string()),
        replication_api_key_env: None,
        data_dir: temp_dir.path().display().to_string(),
        bind_addr: "127.0.0.1:7700".to_string(),
        tls_paths: None,
        node_config: NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            advertise_addr: None,
            peers: Vec::new(),
            bootstrap_peer: None,
        },
        _data_dir_lock: acquire_data_dir_process_lock(temp_dir.path()).unwrap(),
    };
    initialize_key_store(&server_config, temp_dir.path()).unwrap();
    let protected_paths = [".admin_key", "keys.json", "key_material.json"];
    let before = protected_paths.map(|name| std::fs::read(temp_dir.path().join(name)).unwrap());
    #[cfg(unix)]
    let before_inodes =
        protected_paths.map(|name| std::fs::metadata(temp_dir.path().join(name)).unwrap().ino());

    initialize_key_store_for_mode(
        &server_config,
        temp_dir.path(),
        StartupPersistenceMode::FenceActive,
    )
    .expect("a matching predecessor credential should load read-only");
    assert_eq!(
        protected_paths.map(|name| std::fs::read(temp_dir.path().join(name)).unwrap()),
        before,
        "matching credential bytes must not be republished"
    );
    #[cfg(unix)]
    assert_eq!(
        protected_paths
            .map(|name| { std::fs::metadata(temp_dir.path().join(name)).unwrap().ino() }),
        before_inodes,
        "matching credential files must not be atomically replaced"
    );

    server_config.admin_key_env = Some("successor-admin-key".to_string());
    let error = match initialize_key_store_for_mode(
        &server_config,
        temp_dir.path(),
        StartupPersistenceMode::FenceActive,
    ) {
        Ok(_) => panic!("an active persisted fence must not rotate predecessor credentials"),
        Err(error) => error,
    };

    assert!(error.contains("active release fence"));
    let after = protected_paths.map(|name| std::fs::read(temp_dir.path().join(name)).unwrap());
    assert_eq!(after, before, "credential-state hashes must remain exact");

    let retry_error = match initialize_key_store_for_mode(
        &server_config,
        temp_dir.path(),
        StartupPersistenceMode::FenceActive,
    ) {
        Ok(_) => panic!("a retried fenced startup must still reject credential rotation"),
        Err(error) => error,
    };
    assert!(retry_error.contains("active release fence"));
    let after_retry =
        protected_paths.map(|name| std::fs::read(temp_dir.path().join(name)).unwrap());
    assert_eq!(
        after_retry, before,
        "a retried fenced startup must preserve credential-state hashes"
    );
}

#[test]
fn server_opens_persisted_fence_before_mutable_startup_owners() {
    let source = include_str!("server.rs");
    let fence_open = source
        .find("GlobalMutationFence::open(data_dir)")
        .expect("server startup must open the persisted mutation fence");
    let mode_selection = source
        .find("startup_persistence_mode(&global_mutation_fence).await")
        .expect("server startup must derive persistence mode from the opened fence");
    let key_store = source
        .find("initialize_key_store_for_mode(&server_config")
        .expect("credential startup must receive the fence-derived mode");
    let infrastructure = source
        .find("initialize_server_infrastructure_with_fence(")
        .expect("subsystem startup must receive the already-open fence");

    assert!(
        fence_open < mode_selection && mode_selection < key_store && key_store < infrastructure,
        "the persisted fence must be opened and classified before credentials and every downstream mutable startup owner"
    );
}

/// TODO: Document shared_admin_key_persistence_sets_restrictive_permissions.
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

/// TODO: Document flapjack_disable_dashboard_env_parses_true_and_defaults_false.
#[test]
fn flapjack_disable_dashboard_env_parses_true_and_defaults_false() {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let _disable_dashboard = EnvVarRestoreGuard::remove("FLAPJACK_DISABLE_DASHBOARD");
    let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
    let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());

        let server_config = load_server_config().expect("server config should load");

        assert!(!server_config.disable_dashboard);
    }

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _disable_dashboard_set = EnvVarRestoreGuard::set("FLAPJACK_DISABLE_DASHBOARD", "1");

        let server_config = load_server_config().expect("server config should load");

        assert!(server_config.disable_dashboard);
    }
}

/// TODO: Document flapjack_allow_no_auth_public_bind_env_parses_one_and_defaults_false.
#[test]
fn flapjack_allow_no_auth_public_bind_env_parses_one_and_defaults_false() {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let _allow_public_bind = EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND");
    let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
    let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());

        let server_config = load_server_config().expect("server config should load");

        assert!(!server_config.allow_no_auth_public_bind);
    }

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _allow_public_bind_set =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND", "true");

        let server_config = load_server_config().expect("server config should load");

        assert!(!server_config.allow_no_auth_public_bind);
    }

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _allow_public_bind_set =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND", "1");

        let server_config = load_server_config().expect("server config should load");

        assert!(server_config.allow_no_auth_public_bind);
    }
}

#[test]
fn replication_peer_api_key_env_is_normalized_without_persistence() {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let _admin_key = EnvVarRestoreGuard::remove("FLAPJACK_ADMIN_KEY");
    let _peers = EnvVarRestoreGuard::remove("FLAPJACK_PEERS");
    let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
    let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");
    let _allow_unauthenticated =
        EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS");
    let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
    let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");

        let server_config = load_server_config().expect("server config should load");

        assert_eq!(server_config.replication_api_key_env, None);
    }

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _peer_key = EnvVarRestoreGuard::set("FLAPJACK_REPLICATION_API_KEY", "  \n\t  ");

        let server_config = load_server_config().expect("server config should load");

        assert_eq!(server_config.replication_api_key_env, None);
    }

    {
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _peer_key =
            EnvVarRestoreGuard::set("FLAPJACK_REPLICATION_API_KEY", "  stage-2-peer-secret  ");

        let server_config = load_server_config().expect("server config should load");

        assert_eq!(
            server_config.replication_api_key_env,
            Some("stage-2-peer-secret".to_string())
        );
        assert!(
            !temp_dir.path().join(".admin_key").exists(),
            "loading the peer credential must not persist it as an admin key"
        );
    }
}

#[test]
fn replication_peer_api_key_rejects_invalid_header_characters() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let _peer_key = EnvVarRestoreGuard::set(
            "FLAPJACK_REPLICATION_API_KEY",
            "peer-secret\nforged-header: value",
        );

        load_server_config()
    };

    match result {
        Err(error) => assert_eq!(
            error,
            "FLAPJACK_REPLICATION_API_KEY contains characters that are invalid in an HTTP header"
        ),
        Ok(_) => panic!("startup must reject a peer key that cannot be sent"),
    }
}

#[test]
fn startup_accepts_replication_peers_with_peer_api_key() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _admin_key = EnvVarRestoreGuard::remove("FLAPJACK_ADMIN_KEY");
        let _peer_key =
            EnvVarRestoreGuard::set("FLAPJACK_REPLICATION_API_KEY", "  stage-2-peer-secret  ");
        let _allow_unauthenticated =
            EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS");
        let _peers =
            EnvVarRestoreGuard::set("FLAPJACK_PEERS", "node-b=https://node-b.example.com:7700");
        let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
        let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");
        let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
        let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");

        load_server_config().map(|server_config| server_config.replication_api_key_env)
    };

    assert_eq!(
        result.expect("HTTPS replication peers with a peer credential should permit startup"),
        Some("stage-2-peer-secret".to_string()),
        "startup must preserve the normalized peer credential for authenticated replication"
    );
}

#[test]
fn startup_rejects_cleartext_static_peer_instead_of_serving_standalone() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _peer_key =
            EnvVarRestoreGuard::set("FLAPJACK_REPLICATION_API_KEY", "stage-2-peer-secret");
        let _allow_cleartext =
            EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS");
        let _peers =
            EnvVarRestoreGuard::set("FLAPJACK_PEERS", "node-b=http://node-b.example.com:7700");
        let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
        let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");

        load_server_config().map(|_| ())
    };

    let error = result.expect_err("rejected static topology must fail startup");
    assert!(
        error.contains("node-b") && error.contains("http://node-b.example.com:7700"),
        "startup error must identify the rejected static peer: {error}"
    );
    assert!(
        error.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1"),
        "startup error must name the cleartext override: {error}"
    );
}

#[test]
fn startup_rejects_cleartext_bootstrap_peer_instead_of_becoming_a_seed() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _peer_key =
            EnvVarRestoreGuard::set("FLAPJACK_REPLICATION_API_KEY", "stage-2-peer-secret");
        let _allow_cleartext =
            EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS");
        let _peers = EnvVarRestoreGuard::remove("FLAPJACK_PEERS");
        let _bootstrap_peer = EnvVarRestoreGuard::set(
            "FLAPJACK_BOOTSTRAP_PEER",
            "http://bootstrap.example.com:7700",
        );
        let _advertise_addr =
            EnvVarRestoreGuard::set("FLAPJACK_ADVERTISE_ADDR", "https://seed.example.com:7700");

        load_server_config().map(|_| ())
    };

    let error = result.expect_err("rejected bootstrap topology must fail startup");
    assert!(
        error.contains("bootstrap") && error.contains("http://bootstrap.example.com:7700"),
        "startup error must identify the rejected bootstrap peer: {error}"
    );
    assert!(
        error.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1"),
        "startup error must name the cleartext override: {error}"
    );
}

#[test]
fn startup_refuses_replication_peers_without_peer_api_key() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _admin_key = EnvVarRestoreGuard::remove("FLAPJACK_ADMIN_KEY");
        let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");
        let _allow_unauthenticated =
            EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS");
        let _peers =
            EnvVarRestoreGuard::set("FLAPJACK_PEERS", "node-b=https://node-b.example.com:7700");
        let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
        let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");
        let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
        let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");

        load_server_config().map(|_| ())
    };

    match result {
        Ok(()) => {
            panic!(
                "replication peers from FLAPJACK_PEERS must require FLAPJACK_REPLICATION_API_KEY"
            )
        }
        Err(error) => {
            assert!(
                error.contains("FLAPJACK_REPLICATION_API_KEY"),
                "error must name the missing peer credential setting, got: {error}"
            );
            assert!(
                error.contains("FLAPJACK_PEERS") || error.contains("node-b"),
                "error must identify the replication intent that triggered validation, got: {error}"
            );
        }
    }
}

#[test]
fn startup_refuses_node_json_replication_peers_without_peer_api_key() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let node_json = serde_json::json!({
            "node_id": "node-a",
            "bind_addr": "127.0.0.1:0",
            "peers": [{
                "node_id": "node-b",
                "addr": "https://node-b.example.com:7700"
            }]
        });
        std::fs::write(temp_dir.path().join("node.json"), node_json.to_string())
            .expect("test node.json must be writable");
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _admin_key = EnvVarRestoreGuard::remove("FLAPJACK_ADMIN_KEY");
        let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");
        let _allow_unauthenticated =
            EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS");
        let _peers = EnvVarRestoreGuard::remove("FLAPJACK_PEERS");
        let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
        let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");
        let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
        let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");

        load_server_config().map(|_| ())
    };

    match result {
        Ok(()) => {
            panic!("replication peers from node.json must require FLAPJACK_REPLICATION_API_KEY")
        }
        Err(error) => {
            assert!(
                error.contains("FLAPJACK_REPLICATION_API_KEY"),
                "error must name the missing peer credential setting, got: {error}"
            );
            assert!(
                error.contains("node.json") || error.contains("node-b"),
                "error must identify the persisted replication intent, got: {error}"
            );
        }
    }
}

#[test]
fn startup_unauthenticated_peer_escape_repermits_and_warns() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _admin_key = EnvVarRestoreGuard::remove("FLAPJACK_ADMIN_KEY");
        let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");
        let _allow_unauthenticated =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS", "1");
        let _peers =
            EnvVarRestoreGuard::set("FLAPJACK_PEERS", "node-b=https://node-b.example.com:7700");
        let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
        let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");
        let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
        let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");
        let writer = TestWriter::new();
        let subscriber =
            tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone(), false));

        tracing::subscriber::with_default(subscriber, load_server_config)
            .map(|server_config| (server_config.replication_api_key_env, writer.output()))
    };

    let (replication_api_key_env, output) =
        result.expect("explicit unauthenticated peer override should permit startup");
    assert_eq!(replication_api_key_env, None);
    assert!(
        output.contains("WARNING") || output.contains("warning"),
        "override use must emit a loud warning, got: {output:?}"
    );
    assert!(
        output.contains("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS=1"),
        "warning must name the unauthenticated peer override, got: {output:?}"
    );
    assert!(
        output.contains("FLAPJACK_REPLICATION_API_KEY"),
        "warning must name the missing peer credential setting, got: {output:?}"
    );
}

#[test]
fn startup_unauthenticated_peer_escape_still_requires_tls_for_authenticated_analytics() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _no_auth = EnvVarRestoreGuard::remove("FLAPJACK_NO_AUTH");
        let _admin_key = EnvVarRestoreGuard::set("FLAPJACK_ADMIN_KEY", "analytics-admin-secret");
        let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");
        let _allow_unauthenticated =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS", "1");
        let _allow_cleartext =
            EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS");
        let _peers =
            EnvVarRestoreGuard::set("FLAPJACK_PEERS", "node-b=http://node-b.example.com:7700");
        let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
        let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");
        let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
        let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");

        load_server_config().map(|_| ())
    };

    let error = result.expect_err(
        "authenticated analytics must not forward caller credentials to a cleartext peer",
    );
    assert!(
        error.contains("analytics") && error.contains("caller API keys"),
        "refusal must identify the credential-bearing analytics path, got: {error}"
    );
    assert!(
        error.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1"),
        "refusal must name the explicit cleartext override, got: {error}"
    );
}

#[test]
fn no_auth_cluster_still_rejects_cleartext_analytics_peers() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _no_auth = EnvVarRestoreGuard::set("FLAPJACK_NO_AUTH", "1");
        let _admin_key = EnvVarRestoreGuard::remove("FLAPJACK_ADMIN_KEY");
        let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");
        let _allow_unauthenticated =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS", "1");
        let _allow_cleartext =
            EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS");
        let _peers =
            EnvVarRestoreGuard::set("FLAPJACK_PEERS", "node-b=http://node-b.example.com:7700");
        let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
        let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");
        let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
        let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");

        load_server_config().map(|_| ())
    };

    let error = result.expect_err("no-auth routes may still receive and forward caller API keys");
    assert!(
        error.contains("analytics") && error.contains("caller API keys"),
        "refusal must identify the credential-bearing analytics path, got: {error}"
    );
    assert!(
        error.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1"),
        "refusal must name the explicit cleartext override, got: {error}"
    );
}

#[test]
fn startup_unauthenticated_peer_escape_does_not_expose_admin_key_to_cleartext_bootstrap() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _admin_key = EnvVarRestoreGuard::set("FLAPJACK_ADMIN_KEY", "bootstrap-admin-secret");
        let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");
        let _allow_unauthenticated =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS", "1");
        let _allow_cleartext =
            EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS");
        let _peers = EnvVarRestoreGuard::remove("FLAPJACK_PEERS");
        let _bootstrap_peer = EnvVarRestoreGuard::set(
            "FLAPJACK_BOOTSTRAP_PEER",
            "http://bootstrap.example.com:7700",
        );
        let _advertise_addr =
            EnvVarRestoreGuard::set("FLAPJACK_ADVERTISE_ADDR", "https://joiner.example.com:7700");
        let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
        let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");

        load_server_config().map(|_| ())
    };

    let error = result.expect_err(
        "the unauthenticated replication escape must not send the admin key to HTTP bootstrap",
    );
    assert!(
        error.contains("admin API key"),
        "refusal must identify the credential at risk, got: {error}"
    );
    assert!(
        error.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1"),
        "refusal must name the separate cleartext transport override, got: {error}"
    );
}

#[test]
fn startup_cleartext_escape_explicitly_repermits_admin_authenticated_bootstrap() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _admin_key = EnvVarRestoreGuard::set("FLAPJACK_ADMIN_KEY", "bootstrap-admin-secret");
        let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");
        let _allow_unauthenticated =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS", "1");
        let _allow_cleartext =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS", "1");
        let _peers = EnvVarRestoreGuard::remove("FLAPJACK_PEERS");
        let _bootstrap_peer = EnvVarRestoreGuard::set(
            "FLAPJACK_BOOTSTRAP_PEER",
            "http://bootstrap.example.com:7700",
        );
        let _advertise_addr =
            EnvVarRestoreGuard::set("FLAPJACK_ADVERTISE_ADDR", "https://joiner.example.com:7700");
        let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
        let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");
        let writer = TestWriter::new();
        let subscriber =
            tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone(), false));

        tracing::subscriber::with_default(subscriber, load_server_config)
            .map(|config| (config.node_config.bootstrap_peer, writer.output()))
    };

    let (bootstrap_peer, output) =
        result.expect("both explicit escapes should permit cleartext bootstrap");
    assert_eq!(
        bootstrap_peer.as_deref(),
        Some("http://bootstrap.example.com:7700")
    );
    assert!(
        output.contains("admin API key")
            && output.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1"),
        "warning must name the credential and transport escape, got: {output:?}"
    );
}

#[test]
fn startup_node_json_unauthenticated_peer_escape_repermits_and_warns() {
    let result = {
        let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
        let temp_dir = TempDir::new().unwrap();
        let node_json = serde_json::json!({
            "node_id": "node-a",
            "bind_addr": "127.0.0.1:0",
            "peers": [{
                "node_id": "node-b",
                "addr": "https://node-b.example.com:7700"
            }]
        });
        std::fs::write(temp_dir.path().join("node.json"), node_json.to_string())
            .expect("test node.json must be writable");
        let _data_dir =
            EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
        let _admin_key = EnvVarRestoreGuard::remove("FLAPJACK_ADMIN_KEY");
        let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");
        let _allow_unauthenticated =
            EnvVarRestoreGuard::set("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS", "1");
        let _peers = EnvVarRestoreGuard::remove("FLAPJACK_PEERS");
        let _bootstrap_peer = EnvVarRestoreGuard::remove("FLAPJACK_BOOTSTRAP_PEER");
        let _advertise_addr = EnvVarRestoreGuard::remove("FLAPJACK_ADVERTISE_ADDR");
        let _cert_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_CERT_PATH");
        let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");
        let writer = TestWriter::new();
        let subscriber =
            tracing_subscriber::registry().with(build_log_layer_with_writer(writer.clone(), false));

        tracing::subscriber::with_default(subscriber, load_server_config)
            .map(|server_config| (server_config.replication_api_key_env, writer.output()))
    };

    let (replication_api_key_env, output) =
        result.expect("explicit unauthenticated peer override should permit node.json topology");
    assert_eq!(replication_api_key_env, None);
    assert!(
        output.contains("WARNING") || output.contains("warning"),
        "node.json override use must emit a loud warning, got: {output:?}"
    );
    assert!(
        output.contains("FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS=1"),
        "warning must name the unauthenticated peer override, got: {output:?}"
    );
    assert!(
        output.contains("FLAPJACK_REPLICATION_API_KEY"),
        "warning must name the missing peer credential setting, got: {output:?}"
    );
    assert!(
        output.contains("node.json") || output.contains("node-b"),
        "warning must identify the persisted replication intent, got: {output:?}"
    );
}

#[test]
fn tls_paths_constructor_rejects_partial_pairs() {
    assert_eq!(
        TlsPaths::from_optional_paths(Some("cert.pem"), None::<&str>).unwrap_err(),
        "--ssl-cert-path cannot be used without --ssl-key-path"
    );
    assert_eq!(
        TlsPaths::from_optional_paths(None::<&str>, Some("key.pem")).unwrap_err(),
        "--ssl-key-path cannot be used without --ssl-cert-path"
    );
}

#[test]
fn tls_paths_constructor_returns_paths_or_none() {
    let paths = TlsPaths::from_optional_paths(Some("cert.pem"), Some("key.pem"))
        .expect("tls paths should resolve")
        .expect("tls paths should be present");

    assert_eq!(paths.cert_path, std::path::PathBuf::from("cert.pem"));
    assert_eq!(paths.key_path, std::path::PathBuf::from("key.pem"));
    assert_eq!(
        TlsPaths::from_optional_paths(None::<&str>, None::<&str>)
            .expect("missing tls paths should resolve"),
        None
    );
}

#[test]
fn tls_paths_env_loading_uses_shared_pairing_rule() {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let temp_dir = TempDir::new().unwrap();
    let _data_dir = EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
    let _cert_path = EnvVarRestoreGuard::set("FLAPJACK_SSL_CERT_PATH", "cert.pem");
    let _key_path = EnvVarRestoreGuard::set("FLAPJACK_SSL_KEY_PATH", "key.pem");

    let server_config = load_server_config().expect("server config should load");

    let paths = server_config
        .tls_paths
        .expect("tls paths should load from environment");
    assert_eq!(paths.cert_path, std::path::PathBuf::from("cert.pem"));
    assert_eq!(paths.key_path, std::path::PathBuf::from("key.pem"));
}

#[test]
fn tls_paths_env_loading_rejects_partial_pairs() {
    let _guard = ENV_MUTEX.lock().expect("env mutex should lock");
    let temp_dir = TempDir::new().unwrap();
    let _data_dir = EnvVarRestoreGuard::set("FLAPJACK_DATA_DIR", temp_dir.path().to_str().unwrap());
    let _cert_path = EnvVarRestoreGuard::set("FLAPJACK_SSL_CERT_PATH", "cert.pem");
    let _key_path = EnvVarRestoreGuard::remove("FLAPJACK_SSL_KEY_PATH");

    match load_server_config() {
        Ok(_) => panic!("partial tls env config should fail"),
        Err(error) => assert_eq!(
            error,
            "--ssl-cert-path cannot be used without --ssl-key-path"
        ),
    }
}

#[test]
fn startup_banner_urls_use_one_scheme_seam() {
    let http_urls = startup_banner_urls("127.0.0.1:7700", "http");
    assert_eq!(http_urls.base, "http://127.0.0.1:7700");
    assert_eq!(http_urls.dashboard, "http://127.0.0.1:7700/dashboard");
    assert_eq!(http_urls.swagger, "http://127.0.0.1:7700/swagger-ui");

    let https_urls = startup_banner_urls("127.0.0.1:7700", "https");
    assert_eq!(https_urls.base, "https://127.0.0.1:7700");
    assert_eq!(https_urls.dashboard, "https://127.0.0.1:7700/dashboard");
    assert_eq!(https_urls.swagger, "https://127.0.0.1:7700/swagger-ui");
}

// --- Tracing subscriber builder tests ---

/// TODO: Document build_tracing_subscriber_produces_working_dispatch.
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

/// TODO: Document build_tracing_subscriber_returns_none_guard_without_endpoint.
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

/// TODO: Document otel_startup_status_logs_initialized_message.
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

/// TODO: Document otel_startup_status_logs_disabled_message.
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
/// dashboard lockdown, public no-auth bind override, admin key, replication peer\n\
/// API key, data directory, bind address, and optional TLS paths, then\n\
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
