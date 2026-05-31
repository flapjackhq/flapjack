#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use clap::{parser::ValueSource, ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};
use flapjack_http::serve;

/// Top-level CLI definition for the `flapjack` binary.
///
/// Supports optional subcommands (`Uninstall`, `ResetAdminKey`) and server configuration
/// flags including data directory, bind address, port, local-dev instance isolation,
/// auto-port assignment, and authentication control.
#[derive(Parser)]
#[command(name = "flapjack", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[arg(long, env = "FLAPJACK_DATA_DIR", default_value = "./data")]
    data_dir: String,
    #[arg(long, env = "FLAPJACK_BIND_ADDR")]
    bind_addr: Option<String>,
    #[arg(long, env = "FLAPJACK_PORT")]
    port: Option<u16>,

    /// Local-dev instance name. Derives isolated defaults for data-dir and bind address.
    #[arg(long)]
    instance: Option<String>,

    /// Bind to 127.0.0.1:0 (OS-assigned ephemeral port). Prints resolved address at startup.
    #[arg(long)]
    auto_port: bool,

    /// Disable authentication entirely (not allowed in production)
    #[arg(long)]
    no_auth: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Remove Flapjack binary and clean up shell PATH entries
    Uninstall,
    /// Generate a new admin API key (replaces the current one in keys.json)
    ResetAdminKey,
}

/// Remove the install directory (`FLAPJACK_INSTALL` first, then `$HOME/.flapjack`) and clean shell rc PATH entries.
fn run_uninstall() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var("HOME").map_err(|_| "HOME environment variable not set")?;
    let install_dir =
        std::env::var("FLAPJACK_INSTALL").unwrap_or_else(|_| format!("{}/.flapjack", home));

    // Remove the install directory
    if std::path::Path::new(&install_dir).exists() {
        std::fs::remove_dir_all(&install_dir)?;
        eprintln!("Removed {}", install_dir);
    } else {
        eprintln!("Directory {} does not exist, skipping", install_dir);
    }

    // Clean PATH entries from shell config files
    let rc_files = [
        format!("{}/.bashrc", home),
        format!("{}/.bash_profile", home),
        format!("{}/.zshrc", home),
        format!("{}/.profile", home),
        format!("{}/.config/fish/config.fish", home),
    ];

    for rc_path in &rc_files {
        let path = std::path::Path::new(rc_path);
        if !path.exists() {
            continue;
        }

        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        if !contents.contains(".flapjack") {
            continue;
        }

        if let Some(new_contents) = strip_flapjack_path_entries(&contents) {
            std::fs::write(path, new_contents)?;
            eprintln!("Cleaned PATH entry from {}", rc_path);
        }
    }

    eprintln!("\nFlapjack has been uninstalled.");
    Ok(())
}

/// Remove installer marker blocks and `.flapjack` PATH lines, returning `None` when unchanged.
fn strip_flapjack_path_entries(contents: &str) -> Option<String> {
    let mut new_lines: Vec<&str> = Vec::new();
    let mut lines = contents.lines().peekable();
    let mut modified = false;

    while let Some(line) = lines.next() {
        if line.trim() == "# Flapjack" {
            if matches!(lines.peek(), Some(next_line) if next_line.contains(".flapjack")) {
                lines.next();
            }
            modified = true;
            continue;
        }

        if is_flapjack_path_line(line) {
            modified = true;
            continue;
        }

        new_lines.push(line);
    }

    if !modified {
        return None;
    }

    while new_lines.last() == Some(&"") {
        new_lines.pop();
    }

    let mut new_contents = new_lines.join("\n");
    if !new_contents.is_empty() {
        new_contents.push('\n');
    }
    Some(new_contents)
}

fn is_flapjack_path_line(line: &str) -> bool {
    (line.contains("export PATH") || line.contains("set -gx PATH")) && line.contains(".flapjack")
}

/// Parse CLI arguments and dispatch to the appropriate subcommand or start the HTTP server.
///
/// When no subcommand is given, resolves runtime configuration (data directory and bind address)
/// from explicit flags, `--instance` derivation, environment variables, or built-in defaults,
/// then launches the Flapjack HTTP server via `serve()`.
///
/// # Subcommands
///
/// - `Uninstall` — removes the install directory and cleans shell PATH entries.
/// - `ResetAdminKey` — generates and prints a new admin API key.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cmd = Cli::command();
    let matches = cmd.get_matches();
    let cli = Cli::from_arg_matches(&matches)?;

    match cli.command {
        Some(Command::Uninstall) => run_uninstall(),
        Some(Command::ResetAdminKey) => {
            let data_dir = resolve_data_dir(&cli, &matches)
                .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))?;
            run_reset_admin_key(&data_dir)
        }
        None => {
            let runtime = resolve_runtime_config(&cli, &matches)
                .map_err(|msg| std::io::Error::new(std::io::ErrorKind::InvalidInput, msg))?;
            std::env::set_var("FLAPJACK_DATA_DIR", &runtime.data_dir);
            std::env::set_var("FLAPJACK_BIND_ADDR", &runtime.bind_addr);
            if cli.no_auth {
                std::env::set_var("FLAPJACK_NO_AUTH", "1");
            }
            serve().await
        }
    }
}

fn run_reset_admin_key(data_dir: &str) -> Result<(), Box<dyn std::error::Error>> {
    match flapjack_http::auth::reset_admin_key(std::path::Path::new(data_dir)) {
        Ok(new_key) => {
            println!("{}", new_key);
            Ok(())
        }
        Err(e) => {
            eprintln!("ERROR: {}", e);
            std::process::exit(1);
        }
    }
}

struct RuntimeConfig {
    data_dir: String,
    bind_addr: String,
}

fn resolve_runtime_config(cli: &Cli, matches: &ArgMatches) -> Result<RuntimeConfig, String> {
    let data_dir = resolve_data_dir(cli, matches)?;
    let bind_addr = resolve_bind_addr(cli, matches)?;
    Ok(RuntimeConfig {
        data_dir,
        bind_addr,
    })
}

fn resolve_data_dir(cli: &Cli, matches: &ArgMatches) -> Result<String, String> {
    if let Some(instance) = cli.instance.as_deref() {
        validate_instance_name(instance)?;
        if !is_set_on_command_line(matches, "data_dir") {
            return Ok(derive_instance_data_dir(instance));
        }
    }
    Ok(cli.data_dir.clone())
}

/// Resolve bind address precedence across CLI, instance mode, environment values, and loopback defaults.
fn resolve_bind_addr(cli: &Cli, matches: &ArgMatches) -> Result<String, String> {
    let bind_addr_from_cli = is_set_on_command_line(matches, "bind_addr");
    let port_from_cli = is_set_on_command_line(matches, "port");

    if cli.auto_port && bind_addr_from_cli {
        return Err("--auto-port cannot be used with --bind-addr".to_string());
    }

    if cli.auto_port && port_from_cli {
        return Err("--auto-port cannot be used with --port".to_string());
    }

    if bind_addr_from_cli {
        return Ok(cli
            .bind_addr
            .clone()
            .expect("bind_addr should be set when source is command line"));
    }

    if cli.auto_port {
        return Ok(loopback_bind_addr(0));
    }

    if port_from_cli {
        let port = cli
            .port
            .expect("port should be set when source is command line");
        return Ok(loopback_bind_addr(port));
    }

    if let Some(instance) = cli.instance.as_deref() {
        validate_instance_name(instance)?;
        return Ok(loopback_bind_addr(derive_instance_port(instance)));
    }

    if let Some(bind_addr) = &cli.bind_addr {
        return Ok(bind_addr.clone());
    }

    if let Some(port) = cli.port {
        return Ok(loopback_bind_addr(port));
    }

    Ok(loopback_bind_addr(7700))
}

fn is_set_on_command_line(matches: &ArgMatches, arg: &str) -> bool {
    matches.value_source(arg) == Some(ValueSource::CommandLine)
}

fn loopback_bind_addr(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

fn validate_instance_name(instance: &str) -> Result<(), String> {
    if instance.is_empty() {
        return Err("--instance cannot be empty".to_string());
    }

    let valid = instance
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err("--instance accepts only ASCII letters, numbers, '-' and '_'".to_string());
    }

    Ok(())
}

fn derive_instance_data_dir(instance: &str) -> String {
    std::env::temp_dir()
        .join("flapjack")
        .join(instance)
        .to_string_lossy()
        .to_string()
}

fn derive_instance_port(instance: &str) -> u16 {
    const BASE_PORT: u16 = 18000;
    const PORT_SPAN: u16 = 8000;
    // FNV-1a: stable across Rust versions, platforms, and compilations.
    let mut hash: u64 = 14_695_981_039_346_656_037;
    for byte in instance.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    BASE_PORT + (hash as u16) % PORT_SPAN
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn parse_cli(args: &[&str]) -> (Cli, ArgMatches) {
        let matches = Cli::command()
            .try_get_matches_from(args)
            .expect("args should parse");
        let cli = Cli::from_arg_matches(&matches).expect("matches should parse into Cli");
        (cli, matches)
    }

    #[test]
    fn derive_instance_port_is_deterministic_and_in_range() {
        let a = derive_instance_port("branch_a");
        let b = derive_instance_port("branch_a");
        assert_eq!(a, b, "same instance should map to same port");
        assert!(
            (18000..26000).contains(&a),
            "derived port must stay in configured range"
        );
    }

    /// Assert that `derive_instance_port` produces fixed, algorithm-stable port numbers for known inputs.
    ///
    /// Guards against accidental changes to the FNV-1a implementation that would silently
    /// reassign ports for running `--instance` sessions after a rebuild. Also verifies that
    /// a set of common branch names map to distinct ports.
    #[test]
    fn derive_instance_port_stable_known_values() {
        // FNV-1a is algorithm-stable: these exact values must not drift.
        // If this test fails, any running instances using --instance will get
        // a different port after a rebuild, breaking parallel dev sessions.
        assert_eq!(
            derive_instance_port("branch_a"),
            18000 + (fnv1a("branch_a") as u16) % 8000
        );
        assert_eq!(
            derive_instance_port("main"),
            18000 + (fnv1a("main") as u16) % 8000
        );

        // Verify no two common branch names collide.
        let names = [
            "branch_a",
            "branch_b",
            "main",
            "dev",
            "feature_xyz",
            "fix_bug_123",
        ];
        let ports: Vec<u16> = names.iter().map(|n| derive_instance_port(n)).collect();
        let unique: std::collections::HashSet<_> = ports.iter().copied().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "common branch names must get distinct ports: {:?}",
            ports
        );
    }

    fn fnv1a(s: &str) -> u64 {
        let mut hash: u64 = 14_695_981_039_346_656_037;
        for byte in s.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
        hash
    }

    #[test]
    fn validate_instance_name_rejects_invalid_chars() {
        assert!(validate_instance_name("valid_name-123").is_ok());
        assert!(validate_instance_name("bad/name").is_err());
        assert!(validate_instance_name("bad space").is_err());
        assert!(validate_instance_name("").is_err());
    }

    #[test]
    fn auto_port_overrides_env_bind_settings() {
        let _guard = ENV_MUTEX.lock().expect("lock env mutex");
        std::env::set_var("FLAPJACK_BIND_ADDR", "127.0.0.1:19001");
        std::env::set_var("FLAPJACK_PORT", "19002");

        let (cli, matches) = parse_cli(&["flapjack", "--auto-port"]);
        let bind_addr = resolve_bind_addr(&cli, &matches).expect("resolve bind addr");

        std::env::remove_var("FLAPJACK_BIND_ADDR");
        std::env::remove_var("FLAPJACK_PORT");

        assert_eq!(bind_addr, "127.0.0.1:0");
    }

    /// Asserts that the bare no-env, no-CLI default resolves to loopback-only.
    /// This locks the native default so a Dockerfile ENV cannot silently widen host exposure.
    #[test]
    fn bare_default_resolves_to_loopback() {
        let _guard = ENV_MUTEX.lock().expect("lock env mutex");
        // Clear any env vars that could influence resolution
        std::env::remove_var("FLAPJACK_BIND_ADDR");
        std::env::remove_var("FLAPJACK_PORT");

        let (cli, matches) = parse_cli(&["flapjack"]);
        let bind_addr = resolve_bind_addr(&cli, &matches).expect("resolve bind addr");

        assert_eq!(
            bind_addr, "127.0.0.1:7700",
            "bare default must be loopback-only; container images override via ENV, not code"
        );
    }

    #[test]
    fn auto_port_rejects_explicit_conflicting_flags() {
        let (cli_with_port, matches_with_port) =
            parse_cli(&["flapjack", "--auto-port", "--port", "7701"]);
        assert_eq!(
            resolve_bind_addr(&cli_with_port, &matches_with_port).unwrap_err(),
            "--auto-port cannot be used with --port"
        );

        let (cli_with_bind, matches_with_bind) =
            parse_cli(&["flapjack", "--auto-port", "--bind-addr", "127.0.0.1:7701"]);
        assert_eq!(
            resolve_bind_addr(&cli_with_bind, &matches_with_bind).unwrap_err(),
            "--auto-port cannot be used with --bind-addr"
        );
    }

    #[test]
    fn strip_flapjack_path_entries_removes_marker_block_and_path_lines() {
        let contents = [
            "export PATH=\"$HOME/bin:$PATH\"",
            "# Flapjack",
            "export PATH=\"$HOME/.flapjack/bin:$PATH\"",
            "set -gx PATH $HOME/.flapjack/bin $PATH",
            "echo done",
            "",
        ]
        .join("\n");

        let stripped = strip_flapjack_path_entries(&contents).expect("expected cleanup");
        assert_eq!(
            stripped,
            ["export PATH=\"$HOME/bin:$PATH\"", "echo done", ""].join("\n")
        );
    }

    #[test]
    fn run_uninstall_cleans_rc_files_when_default_install_dir_is_missing() {
        let _guard = ENV_MUTEX.lock().expect("lock env mutex");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let test_home: PathBuf =
            std::env::temp_dir().join(format!("flapjack-server-uninstall-missing-{unique}"));
        std::fs::create_dir_all(&test_home).expect("create temp home");
        let bashrc_path = test_home.join(".bashrc");
        std::fs::write(
            &bashrc_path,
            [
                "# Flapjack",
                "export PATH=\"$HOME/.flapjack/bin:$PATH\"",
                "export PATH=\"$HOME/bin:$PATH\"",
                "",
            ]
            .join("\n"),
        )
        .expect("write .bashrc");

        let previous_home = std::env::var("HOME").ok();
        let previous_install = std::env::var("FLAPJACK_INSTALL").ok();
        std::env::set_var("HOME", &test_home);
        std::env::remove_var("FLAPJACK_INSTALL");

        let result = run_uninstall();

        if let Some(value) = previous_home {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(value) = previous_install {
            std::env::set_var("FLAPJACK_INSTALL", value);
        } else {
            std::env::remove_var("FLAPJACK_INSTALL");
        }

        assert!(
            result.is_ok(),
            "run_uninstall should succeed for missing dir"
        );
        assert!(
            !test_home.join(".flapjack").exists(),
            "default install dir should stay absent"
        );
        let cleaned = std::fs::read_to_string(&bashrc_path).expect("read cleaned .bashrc");
        assert_eq!(
            cleaned,
            ["export PATH=\"$HOME/bin:$PATH\"", ""].join("\n"),
            "rc cleanup should remove flapjack marker block"
        );

        std::fs::remove_dir_all(&test_home).expect("cleanup temp home");
    }

    #[test]
    fn run_uninstall_prefers_flapjack_install_env_over_default() {
        let _guard = ENV_MUTEX.lock().expect("lock env mutex");
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let test_home: PathBuf =
            std::env::temp_dir().join(format!("flapjack-server-uninstall-env-{unique}"));
        let default_install = test_home.join(".flapjack");
        let custom_install = test_home.join("custom-install");
        std::fs::create_dir_all(&default_install).expect("create default install dir");
        std::fs::create_dir_all(&custom_install).expect("create custom install dir");
        std::fs::write(default_install.join("keep.txt"), "keep").expect("seed default dir");
        std::fs::write(custom_install.join("remove.txt"), "remove").expect("seed custom dir");

        let zshrc_path = test_home.join(".zshrc");
        std::fs::write(
            &zshrc_path,
            [
                "export PATH=\"$HOME/bin:$PATH\"",
                "export PATH=\"$HOME/.flapjack/bin:$PATH\"",
                "",
            ]
            .join("\n"),
        )
        .expect("write .zshrc");

        let previous_home = std::env::var("HOME").ok();
        let previous_install = std::env::var("FLAPJACK_INSTALL").ok();
        std::env::set_var("HOME", &test_home);
        std::env::set_var(
            "FLAPJACK_INSTALL",
            custom_install.to_string_lossy().to_string(),
        );

        let result = run_uninstall();

        if let Some(value) = previous_home {
            std::env::set_var("HOME", value);
        } else {
            std::env::remove_var("HOME");
        }
        if let Some(value) = previous_install {
            std::env::set_var("FLAPJACK_INSTALL", value);
        } else {
            std::env::remove_var("FLAPJACK_INSTALL");
        }

        assert!(result.is_ok(), "run_uninstall should succeed");
        assert!(
            default_install.exists(),
            "default install dir should not be removed when FLAPJACK_INSTALL is set"
        );
        assert!(
            !custom_install.exists(),
            "custom install dir from FLAPJACK_INSTALL should be removed"
        );
        let cleaned = std::fs::read_to_string(&zshrc_path).expect("read cleaned .zshrc");
        assert_eq!(
            cleaned,
            ["export PATH=\"$HOME/bin:$PATH\"", ""].join("\n"),
            "rc cleanup should remove flapjack path line"
        );

        std::fs::remove_dir_all(&test_home).expect("cleanup temp home");
    }
}
