use axum::http::HeaderValue;
use fs2::FileExt;
use std::fs::OpenOptions;
use std::net::SocketAddr;
use std::path::Path;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

use crate::admin_key_persistence::{
    ensure_admin_key_permissions, persist_admin_key_file, PermissionFailureMode,
};
use crate::auth::{generate_admin_key, generate_hex_key, KeyStore};
use std::sync::Arc;

/// Controls log output format: human-readable text (default) or structured JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorsMode {
    LoopbackOnly,
    Restricted(Vec<HeaderValue>),
}

const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 30;
const MIN_PRODUCTION_ADMIN_KEY_LENGTH: usize = 16;
pub const NO_AUTH_PUBLIC_BIND_WARNING: &str = "WARNING: FLAPJACK_NO_AUTH is enabled on a non-loopback or hostname bind address because FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND=1; this exposes unauthenticated Flapjack APIs publicly.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAuthValidationOutcome {
    Accepted,
    ExplicitlyAllowedPublicNoAuthBind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupAuthValidationError {
    NoAuthInProduction,
    MissingAdminKeyInProduction,
    AdminKeyTooShortInProduction,
    NoAuthPublicBind { bind_addr: SocketAddr },
    NoAuthHostnameBind { bind_addr: String },
}

/// Parse an optional raw string into a `LogFormat`.
/// Only the value `"json"` (case-insensitive) selects JSON mode;
/// all other values (including `None`, empty, or invalid) default to `Text`.
pub(crate) fn log_format_from_value(raw: Option<&str>) -> LogFormat {
    match raw {
        Some(val) if val.eq_ignore_ascii_case("json") => LogFormat::Json,
        _ => LogFormat::Text,
    }
}

/// Read `FLAPJACK_LOG_FORMAT` from the environment and parse into `LogFormat`.
pub(crate) fn log_format_from_env() -> LogFormat {
    log_format_from_value(std::env::var("FLAPJACK_LOG_FORMAT").ok().as_deref())
}

/// Parse `FLAPJACK_ALLOWED_ORIGINS`-style values.
///
/// - `None`/empty/whitespace-only: loopback-only mode.
/// - comma-separated origins: restricted mode using trimmed, valid `HeaderValue`s.
/// - invalid/empty segments are ignored.
pub fn cors_origins_from_value(raw: Option<&str>) -> CorsMode {
    let origins = raw
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter_map(|value| HeaderValue::from_str(value).ok())
        .collect::<Vec<_>>();

    if origins.is_empty() {
        CorsMode::LoopbackOnly
    } else {
        CorsMode::Restricted(origins)
    }
}

/// Read `FLAPJACK_ALLOWED_ORIGINS` and parse into a typed CORS mode.
pub(crate) fn cors_origins_from_env() -> CorsMode {
    cors_origins_from_value(std::env::var("FLAPJACK_ALLOWED_ORIGINS").ok().as_deref())
}

/// Validate startup auth policy before server initialization.
///
/// This is intentionally exposed for cross-crate security contract tests.
pub fn validate_startup_auth_policy(
    env_mode: &str,
    no_auth: bool,
    raw_admin_key: Option<&str>,
    resolved_bind_addr: &str,
    allow_no_auth_public_bind: bool,
) -> Result<StartupAuthValidationOutcome, StartupAuthValidationError> {
    if no_auth && env_mode == "production" {
        return Err(StartupAuthValidationError::NoAuthInProduction);
    }

    let admin_key = raw_admin_key.and_then(normalize_admin_key);
    match (env_mode, admin_key) {
        ("production", None) => Err(StartupAuthValidationError::MissingAdminKeyInProduction),
        ("production", Some(key)) if key.len() < MIN_PRODUCTION_ADMIN_KEY_LENGTH => {
            Err(StartupAuthValidationError::AdminKeyTooShortInProduction)
        }
        _ => validate_development_no_auth_bind(
            no_auth,
            resolved_bind_addr,
            allow_no_auth_public_bind,
        ),
    }
}

fn validate_development_no_auth_bind(
    no_auth: bool,
    resolved_bind_addr: &str,
    allow_no_auth_public_bind: bool,
) -> Result<StartupAuthValidationOutcome, StartupAuthValidationError> {
    if !no_auth {
        return Ok(StartupAuthValidationOutcome::Accepted);
    }

    let bind_addr = match resolved_bind_addr.parse::<SocketAddr>() {
        Ok(bind_addr) => bind_addr,
        Err(_) if is_hostname_socket_address(resolved_bind_addr) => {
            return if allow_no_auth_public_bind {
                Ok(StartupAuthValidationOutcome::ExplicitlyAllowedPublicNoAuthBind)
            } else {
                Err(StartupAuthValidationError::NoAuthHostnameBind {
                    bind_addr: resolved_bind_addr.to_string(),
                })
            };
        }
        Err(_) => return Ok(StartupAuthValidationOutcome::Accepted),
    };

    if bind_addr.ip().is_loopback() {
        return Ok(StartupAuthValidationOutcome::Accepted);
    }

    if allow_no_auth_public_bind {
        Ok(StartupAuthValidationOutcome::ExplicitlyAllowedPublicNoAuthBind)
    } else {
        Err(StartupAuthValidationError::NoAuthPublicBind { bind_addr })
    }
}

fn is_hostname_socket_address(bind_addr: &str) -> bool {
    let Some((hostname, port)) = bind_addr.rsplit_once(':') else {
        return false;
    };
    let hostname = hostname.strip_suffix('.').unwrap_or(hostname);
    !hostname.is_empty()
        && port.parse::<u16>().is_ok()
        && hostname.split('.').all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

pub(crate) fn exit_for_startup_auth_validation_error(error: StartupAuthValidationError) -> ! {
    match error {
        StartupAuthValidationError::NoAuthInProduction => {
            eprintln!("ERROR: --no-auth cannot be used in production mode.");
        }
        StartupAuthValidationError::MissingAdminKeyInProduction => {
            let suggested = generate_hex_key();
            eprintln!("ERROR: FLAPJACK_ADMIN_KEY is required in production mode.");
            eprintln!("Suggested key: {}", suggested);
        }
        StartupAuthValidationError::AdminKeyTooShortInProduction => {
            eprintln!(
                "ERROR: FLAPJACK_ADMIN_KEY must be at least {} characters in production.",
                MIN_PRODUCTION_ADMIN_KEY_LENGTH
            );
        }
        StartupAuthValidationError::NoAuthPublicBind { bind_addr } => {
            eprintln!(
                "ERROR: FLAPJACK_NO_AUTH cannot be used with non-loopback bind address {bind_addr} unless FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND=1 is set."
            );
        }
        StartupAuthValidationError::NoAuthHostnameBind { bind_addr } => {
            eprintln!(
                "ERROR: FLAPJACK_NO_AUTH cannot be used with hostname bind address {bind_addr} unless FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND=1 is set."
            );
        }
    }
    std::process::exit(1);
}

/// Parse `FLAPJACK_SHUTDOWN_TIMEOUT_SECS`-style values.
///
/// Missing, empty, invalid, or non-positive values use the safe default of 30s.
pub(crate) fn shutdown_timeout_secs_from_value(raw: Option<&str>) -> u64 {
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_SECS)
}

/// Read `FLAPJACK_SHUTDOWN_TIMEOUT_SECS` and parse into shutdown timeout seconds.
pub(crate) fn shutdown_timeout_secs_from_env() -> u64 {
    shutdown_timeout_secs_from_value(
        std::env::var("FLAPJACK_SHUTDOWN_TIMEOUT_SECS")
            .ok()
            .as_deref(),
    )
}

fn build_log_layer_with_writer<S, W>(writer: W) -> Box<dyn Layer<S> + Send + Sync + 'static>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    match log_format_from_env() {
        LogFormat::Json => Box::new(tracing_subscriber::fmt::layer().json().with_writer(writer)),
        LogFormat::Text => Box::new(tracing_subscriber::fmt::layer().with_writer(writer)),
    }
}

fn build_env_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::new(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
}

fn install_global_tracing_dispatch(dispatch: tracing::Dispatch) {
    tracing::dispatcher::set_global_default(dispatch)
        .expect("global tracing subscriber already set");
}

/// Build a tracing subscriber `Dispatch` suitable for use with
/// `tracing::dispatcher::set_global_default`. Composes `EnvFilter` (from
/// `RUST_LOG`) and the fmt layer. Accepts a writer for testability.
#[cfg(not(feature = "otel"))]
pub(crate) fn build_tracing_subscriber<W>(make_writer: W) -> tracing::Dispatch
where
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    let subscriber = tracing_subscriber::registry()
        .with(build_env_filter())
        .with(build_log_layer_with_writer(make_writer));
    tracing::Dispatch::new(subscriber)
}

/// Build a tracing subscriber `Dispatch` with an optional OTEL layer composed in.
/// Returns the dispatch and the OTEL shutdown guard (None when endpoint is unset).
#[cfg(feature = "otel")]
pub(crate) fn build_tracing_subscriber<W>(
    make_writer: W,
) -> (tracing::Dispatch, Option<crate::otel::OtelGuard>)
where
    W: for<'writer> tracing_subscriber::fmt::MakeWriter<'writer> + Send + Sync + 'static,
{
    let (otel_layer, guard) = crate::otel::try_init_otel_layer().unzip();

    let subscriber = tracing_subscriber::registry()
        .with(build_env_filter())
        .with(build_log_layer_with_writer(make_writer))
        .with(otel_layer);

    (tracing::Dispatch::new(subscriber), guard)
}

/// Initialize the global tracing subscriber. Call once at server startup,
/// before any tracing macros are used.
#[cfg(not(feature = "otel"))]
pub(crate) fn init_tracing() {
    install_global_tracing_dispatch(build_tracing_subscriber(std::io::stdout));
}

/// Initialize the global tracing subscriber with optional OTEL layer.
/// Returns the OTEL shutdown guard when `OTEL_EXPORTER_OTLP_ENDPOINT` is configured.
#[cfg(feature = "otel")]
pub(crate) fn init_tracing() -> Option<crate::otel::OtelGuard> {
    let (dispatch, guard) = build_tracing_subscriber(std::io::stdout);
    install_global_tracing_dispatch(dispatch);
    log_otel_startup_status(guard.is_some());
    guard
}

#[cfg(feature = "otel")]
fn log_otel_startup_status(otel_enabled: bool) {
    let status_message = if otel_enabled {
        "[otel] OTEL tracing initialized from OTEL_EXPORTER_OTLP_ENDPOINT"
    } else {
        "[otel] OTEL tracing disabled (OTEL_EXPORTER_OTLP_ENDPOINT unset, empty, or invalid)"
    };
    tracing::info!("{status_message}");
}

/// Log memory allocator and budget configuration. Extracted from
/// `load_server_config` so tracing init and post-init logging are separate concerns.
pub(crate) fn log_memory_configuration() {
    let observer = flapjack::MemoryObserver::global();
    let stats = observer.stats();
    let budget = flapjack::get_global_budget();
    tracing::info!(
        allocator = stats.allocator,
        memory_limit_mb = stats.system_limit_bytes / (1024 * 1024),
        limit_source = %stats.limit_source,
        high_watermark_pct = stats.high_watermark_pct,
        critical_pct = stats.critical_pct,
        max_concurrent_writers = budget.max_concurrent_writers(),
        "Memory configuration loaded"
    );
}

pub(crate) struct ServerConfig {
    pub env_mode: String,
    pub no_auth: bool,
    pub disable_dashboard: bool,
    pub allow_no_auth_public_bind: bool,
    pub admin_key_env: Option<String>,
    pub data_dir: String,
    pub bind_addr: String,
    pub _data_dir_lock: DataDirProcessLock,
}

/// Loads startup configuration from environment variables for mode/auth, optional
/// dashboard lockdown, public no-auth bind override, admin key, data directory,
/// and bind address, then
/// initializes logging and acquires the per-process data directory lock.
pub(crate) fn load_server_config() -> ServerConfig {
    let env_mode = std::env::var("FLAPJACK_ENV").unwrap_or_else(|_| "development".into());
    let no_auth = std::env::var("FLAPJACK_NO_AUTH")
        .ok()
        .filter(|value| value == "1")
        .is_some();
    let disable_dashboard = std::env::var("FLAPJACK_DISABLE_DASHBOARD")
        .ok()
        .filter(|value| value == "1")
        .is_some();
    let allow_no_auth_public_bind = std::env::var("FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND")
        .ok()
        .filter(|value| value == "1")
        .is_some();

    let raw_admin_key_env = std::env::var("FLAPJACK_ADMIN_KEY").ok();
    let admin_key_env = raw_admin_key_env.as_deref().and_then(normalize_admin_key);

    let data_dir = std::env::var("FLAPJACK_DATA_DIR").unwrap_or_else(|_| "./data".to_string());
    let data_dir_lock = match acquire_data_dir_process_lock(Path::new(&data_dir)) {
        Ok(lock) => lock,
        Err(message) => {
            eprintln!("ERROR: {}", message);
            std::process::exit(1);
        }
    };

    let bind_addr =
        std::env::var("FLAPJACK_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:7700".to_string());

    ServerConfig {
        env_mode,
        no_auth,
        disable_dashboard,
        allow_no_auth_public_bind,
        admin_key_env,
        data_dir,
        bind_addr,
        _data_dir_lock: data_dir_lock,
    }
}

/// Resolve API key storage for server startup.
///
/// - `--no-auth`: no key, no keystore.
/// - `FLAPJACK_ADMIN_KEY`: persisted into `.admin_key` and loaded into memory.
/// - existing `.admin_key`: reused.
/// - missing `.admin_key`: auto-generate and save a new key.
pub(crate) fn initialize_key_store(
    server_config: &ServerConfig,
    data_dir: &Path,
) -> (Option<Arc<KeyStore>>, Option<String>, bool) {
    let admin_key_file = data_dir.join(".admin_key");
    let (admin_key, key_is_new) = resolve_admin_key(server_config, &admin_key_file);

    let key_store = admin_key.as_ref().map(|key| {
        let key_store = Arc::new(KeyStore::load_or_create(data_dir, key));
        tracing::info!("API key authentication enabled");
        key_store
    });

    (key_store, admin_key, key_is_new)
}

fn resolve_admin_key(
    server_config: &ServerConfig,
    admin_key_file: &Path,
) -> (Option<String>, bool) {
    if server_config.no_auth {
        return (None, false);
    }

    if let Some(key) = server_config
        .admin_key_env
        .as_deref()
        .and_then(normalize_admin_key)
    {
        warn_on_failed_admin_key_persist(admin_key_file, &key);
        return (Some(key), false);
    }

    if admin_key_file.exists() {
        return (
            Some(load_existing_admin_key(
                admin_key_file,
                server_config.data_dir.as_str(),
            )),
            false,
        );
    }

    (Some(create_admin_key(admin_key_file)), true)
}

fn normalize_admin_key(raw_key: &str) -> Option<String> {
    let trimmed = raw_key.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn warn_on_failed_admin_key_persist(admin_key_file: &Path, key: &str) {
    if let Err(error) = persist_admin_key(admin_key_file, key) {
        tracing::warn!("Failed to save admin key to .admin_key: {}", error);
    }
}

fn load_existing_admin_key(admin_key_file: &Path, data_dir: &str) -> String {
    match read_admin_key(admin_key_file) {
        Ok(key) => {
            if let Err(error) =
                ensure_admin_key_permissions(admin_key_file, PermissionFailureMode::WarnAndContinue)
            {
                tracing::warn!("Failed to set .admin_key permissions: {}", error);
            }
            key
        }
        Err(error) => exit_with_admin_key_reset_hint(&error, data_dir),
    }
}

fn shell_quote_argument(value: &str) -> String {
    let is_shell_safe = !value.is_empty()
        && value.bytes().all(|byte| {
            matches!(
                byte,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'_' | b'-'
            )
        });

    if is_shell_safe {
        value.to_string()
    } else {
        // Single-quote the path so operators can paste the hint verbatim even when
        // their data directory contains spaces or other shell metacharacters.
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

fn format_reset_admin_key_command(data_dir: &str) -> String {
    format!(
        "flapjack --data-dir {} reset-admin-key",
        shell_quote_argument(data_dir)
    )
}

fn read_admin_key(admin_key_file: &Path) -> Result<String, String> {
    let raw_key = std::fs::read_to_string(admin_key_file).map_err(|error| {
        format!(
            "Failed to read .admin_key file {}: {}",
            admin_key_file.display(),
            error
        )
    })?;
    normalize_admin_key(&raw_key)
        .ok_or_else(|| format!(".admin_key file {} is empty", admin_key_file.display()))
}

fn persist_admin_key(admin_key_file: &Path, key: &str) -> std::io::Result<()> {
    persist_admin_key_file(admin_key_file, key, PermissionFailureMode::ReturnError)
        .map_err(std::io::Error::other)
}

fn create_admin_key(admin_key_file: &Path) -> String {
    let key = generate_admin_key();
    ensure_admin_key_directory(admin_key_file);
    persist_admin_key_or_exit(admin_key_file, &key);
    key
}

fn ensure_admin_key_directory(admin_key_file: &Path) {
    if let Some(parent) = admin_key_file.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            exit_with_startup_error(format!("Failed to create data directory: {}", error));
        }
    }
}

fn persist_admin_key_or_exit(admin_key_file: &Path, key: &str) {
    if let Err(error) = persist_admin_key(admin_key_file, key) {
        exit_with_startup_error(format!("Failed to save admin key: {}", error));
    }
}

fn exit_with_admin_key_reset_hint(error: &str, data_dir: &str) -> ! {
    eprintln!("❌ Error: {}", error);
    eprintln!("   Run: {}", format_reset_admin_key_command(data_dir));
    std::process::exit(1);
}

fn exit_with_startup_error(message: String) -> ! {
    eprintln!("❌ Error: {}", message);
    std::process::exit(1);
}

/// Wait for SIGINT (Ctrl+C) or SIGTERM, whichever comes first.
pub(crate) async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl+c");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("[shutdown] Received SIGINT (Ctrl+C)"),
        _ = terminate => tracing::info!("[shutdown] Received SIGTERM"),
    }
}

pub(crate) struct DataDirProcessLock {
    file: std::fs::File,
}

impl Drop for DataDirProcessLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Acquire an exclusive filesystem lock on the data directory.
///
/// Create `data_dir` if it does not exist, then attempt a non-blocking exclusive
/// lock on `<data_dir>/.process.lock`. The returned `DataDirProcessLock` holds the
/// lock for its lifetime and releases it on drop.
///
/// # Arguments
///
/// * `data_dir` — Path to the Flapjack data directory.
///
/// # Returns
///
/// A `DataDirProcessLock` guard on success, or a human-readable error message
/// explaining the failure (directory creation error, lock contention, or I/O error).
pub(crate) fn acquire_data_dir_process_lock(data_dir: &Path) -> Result<DataDirProcessLock, String> {
    std::fs::create_dir_all(data_dir).map_err(|e| {
        format!(
            "Failed to create data directory {}: {}",
            data_dir.display(),
            e
        )
    })?;

    let lock_path = data_dir.join(".process.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| {
            format!(
                "Failed to open process lock file {}: {}",
                lock_path.display(),
                e
            )
        })?;

    match file.try_lock_exclusive() {
        Ok(()) => Ok(DataDirProcessLock { file }),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(format!(
            "Data directory already in use: {}. Use unique --data-dir per instance.",
            lock_path.display()
        )),
        Err(e) => Err(format!(
            "Failed to acquire process lock {}: {}",
            lock_path.display(),
            e
        )),
    }
}

pub(crate) enum AuthStatus {
    NewKey(String),
    KeyInFile,
    Disabled,
}

fn capability_status(enabled: bool) -> &'static str {
    if enabled {
        "enabled"
    } else {
        "disabled"
    }
}

/// Build a human-readable capabilities summary line for the startup banner.
///
/// Uses compile-time feature flags so the output is deterministic per build.
pub(crate) fn format_capabilities_line() -> String {
    format!(
        "Capabilities: vector-search: {}, local-embeddings: {}",
        capability_status(cfg!(feature = "vector-search")),
        capability_status(cfg!(feature = "vector-search-local"))
    )
}

fn print_new_key_banner(key: &str, url: &str, data_dir: &str) {
    use colored::Colorize;
    let reset_admin_key_command = format_reset_admin_key_command(data_dir);

    println!();
    println!(
        "  {}",
        "! Save this API key \u{2014} it won\u{2019}t be shown again!"
            .yellow()
            .bold()
    );
    println!();
    println!(
        "  \u{1F511}  Admin API Key:  {}",
        key.cyan().bold().on_black()
    );
    println!();
    println!(
        "     {} Copy this key to a safe place (password manager, secrets vault)",
        "1.".cyan().bold()
    );
    println!(
        "     {} Use it to authenticate API requests:",
        "2.".cyan().bold()
    );
    println!(
        "        {}",
        format!("curl -H 'X-Algolia-API-Key: {}' \\", key).dimmed()
    );
    println!(
        "        {}",
        "     -H 'X-Algolia-Application-ID: flapjack' \\".dimmed()
    );
    println!("        {}", format!("     {}/1/indexes", url).dimmed());
    println!();
    println!(
        "     {} Stored in: {}",
        "\u{2713}".green(),
        format!("{}/.admin_key", data_dir).cyan()
    );
    println!(
        "     {} Keys hashed at rest {}",
        "\u{2713}".green(),
        "(SHA-256 + unique salt)".dimmed()
    );
    println!(
        "     {} Never commit {} to version control",
        "!".yellow(),
        ".admin_key".cyan()
    );
    println!(
        "     {} If lost: {}",
        "\u{2192}".dimmed(),
        reset_admin_key_command.cyan()
    );
    println!(
        "     {} Production: set {} env var",
        "\u{2192}".dimmed(),
        "FLAPJACK_ADMIN_KEY".cyan()
    );
}

fn print_existing_key_banner(data_dir: &str) {
    use colored::Colorize;

    let key_file = format!("{}/.admin_key", data_dir);
    println!();
    println!(
        "  {} Auth enabled  {}",
        "\u{2713}".green(),
        format!("(loaded from {})", key_file).dimmed()
    );
}

fn print_auth_disabled_banner() {
    use colored::Colorize;

    println!();
    println!(
        "  {} {}",
        "!".yellow().bold(),
        "Auth disabled \u{2014} all routes publicly accessible".yellow()
    );
    println!(
        "    {}",
        "Only use --no-auth for local development/testing".dimmed()
    );
}

/// Prints the server startup banner with bind address, auth status, and timing.
pub(crate) fn print_startup_banner(
    bind_addr: &str,
    auth: AuthStatus,
    startup_ms: u128,
    data_dir: &str,
) {
    use colored::Colorize;
    use std::io::Write;

    let url = format!("http://{}", bind_addr);
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let timing = format!("ready in {}ms", startup_ms);

    println!();
    println!(
        "  {} {}  {}",
        "\u{1F95E} Flapjack".bold(),
        version.as_str().dimmed(),
        timing.as_str().dimmed(),
    );
    println!();
    println!(
        "  {}  Local:      {}",
        "\u{2192}".green(),
        url.as_str().cyan()
    );
    let dash = format!("{}/dashboard", url);
    println!(
        "  {}  Dashboard:  {}",
        "\u{2192}".green(),
        dash.as_str().cyan()
    );
    let docs = format!("{}/swagger-ui", url);
    println!(
        "  {}  API Docs:   {}",
        "\u{2192}".green(),
        docs.as_str().cyan()
    );
    println!(
        "  {}  {}",
        "\u{2192}".green(),
        format_capabilities_line().as_str().dimmed()
    );

    match auth {
        AuthStatus::NewKey(ref key) => print_new_key_banner(key, &url, data_dir),
        AuthStatus::KeyInFile => print_existing_key_banner(data_dir),
        AuthStatus::Disabled => print_auth_disabled_banner(),
    }
    println!();
    // Tests and wrappers often pipe stdout; force flush so startup lines are observable
    // before process shutdown/timeouts.
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
