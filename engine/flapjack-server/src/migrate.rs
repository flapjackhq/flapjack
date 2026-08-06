//! Pure authenticated HTTP client for Flapjack's durable Algolia migration API.
//! Operator usage is owned by `engine/docs2/3_IMPLEMENTATION/OPS_CONFIGURATION.md`.

use crate::credentials::{validate_required_http_header_value, SecretSource};
use clap::{Args, Subcommand, ValueEnum};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use std::io::Read;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

mod output;

use output::{finish_failure, print_acknowledgement, print_preview, print_status};

const DEFAULT_POLL_INTERVAL: &str = "250ms";
const DEFAULT_TIMEOUT: &str = "1h";
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);
const MAX_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_HTTP_RESPONSE_BYTES: usize = 1024 * 1024;

// Canonical Stage 2 migration CLI exit-code table.
const EXIT_CONFIG: i32 = 2;
const EXIT_HTTP_REJECTION: i32 = 3;
const EXIT_TIMEOUT: i32 = 4;
const EXIT_FAILED_JOB: i32 = 5;
const EXIT_CANCELLED_JOB: i32 = 6;
const EXIT_CANCEL_TOO_LATE: i32 = 7;
const EXIT_ACK_TOO_EARLY: i32 = 8;
const EXIT_PREVIEW_HARD_REJECTIONS: i32 = 9;

#[derive(Args, Debug)]
pub(crate) struct MigrateArgs {
    #[command(flatten)]
    connection: MigrateConnectionArgs,

    /// Source migration provider
    #[arg(long, value_enum, default_value_t = SourceProvider::Algolia, global = true)]
    source_provider: SourceProvider,

    #[command(subcommand)]
    action: Option<MigrateAction>,

    /// Source Algolia application id
    #[arg(long, global = true)]
    app_id: Option<String>,

    /// Source Meilisearch endpoint or Typesense node URL
    #[arg(long, global = true)]
    source_endpoint: Option<String>,

    /// Environment variable containing the source provider API key
    #[arg(long, visible_alias = "algolia-key-env", global = true)]
    source_key_env: Option<String>,

    /// File containing the source provider API key
    #[arg(long, visible_alias = "algolia-key-file", global = true)]
    source_key_file: Option<PathBuf>,

    /// Read the source provider API key from stdin
    #[arg(long, visible_alias = "algolia-key-stdin", global = true)]
    source_key_stdin: bool,

    /// Source Algolia index
    #[arg(long, global = true)]
    source_index: Option<String>,

    /// Destination Flapjack index; defaults to the source index
    #[arg(long)]
    target_index: Option<String>,

    /// Atomically replace an existing destination index
    #[arg(long)]
    overwrite: bool,

    /// Delay between status requests, with ms, s, m, or h suffix
    #[arg(long)]
    poll_interval: Option<String>,

    /// Maximum time for migration polling or a preview request, with ms, s, m, or h suffix
    #[arg(long, default_value = DEFAULT_TIMEOUT, global = true)]
    timeout: String,
}

#[derive(Args, Debug)]
struct MigrateConnectionArgs {
    /// Base URL of an existing Flapjack server
    #[arg(long, global = true)]
    endpoint: Option<String>,

    /// Flapjack owner application id header value
    #[arg(long, default_value = "flapjack", global = true)]
    application_id: String,

    /// Environment variable containing the Flapjack admin API key
    #[arg(long, global = true)]
    api_key_env: Option<String>,

    /// File containing the Flapjack admin API key
    #[arg(long, global = true)]
    api_key_file: Option<PathBuf>,

    /// Read the Flapjack admin API key from stdin
    #[arg(long, global = true)]
    api_key_stdin: bool,

    /// Emit the server-returned status as JSON
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum SourceProvider {
    Algolia,
    Meilisearch,
    Typesense,
}

impl SourceProvider {
    fn label(self) -> &'static str {
        match self {
            Self::Algolia => "algolia",
            Self::Meilisearch => "meilisearch",
            Self::Typesense => "typesense",
        }
    }
}

impl std::fmt::Display for SourceProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Subcommand, Debug)]
enum MigrateAction {
    /// Report migration compatibility without importing data
    Preview,
    /// Request cooperative cancellation of an owned migration job
    Cancel(MigrationJobArgs),
    /// Acknowledge an owned terminal migration job
    Ack(MigrationJobArgs),
}

#[derive(Args, Debug)]
struct MigrationJobArgs {
    /// Durable migration job UUID
    #[arg(long)]
    job_id: String,
}

#[derive(Clone, Copy)]
enum JobActionKind {
    Cancel,
    Ack,
}

#[derive(Clone, Copy)]
enum FailureKind {
    Config,
    HttpRejection,
    Timeout,
    FailedJob,
    CancelledJob,
    CancelTooLate,
    AckTooEarly,
}

impl FailureKind {
    fn exit_code(self) -> i32 {
        match self {
            Self::Config => EXIT_CONFIG,
            Self::HttpRejection => EXIT_HTTP_REJECTION,
            Self::Timeout => EXIT_TIMEOUT,
            Self::FailedJob => EXIT_FAILED_JOB,
            Self::CancelledJob => EXIT_CANCELLED_JOB,
            Self::CancelTooLate => EXIT_CANCEL_TOO_LATE,
            Self::AckTooEarly => EXIT_ACK_TOO_EARLY,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::HttpRejection => "http_rejection",
            Self::Timeout => "timeout",
            Self::FailedJob => "failed_job",
            Self::CancelledJob => "cancelled_job",
            Self::CancelTooLate => "cancel_too_late",
            Self::AckTooEarly => "migration_ack_too_early",
        }
    }
}

struct MigrationFailure {
    kind: FailureKind,
    message: String,
    status: Option<Box<MigrationStatus>>,
    secrets: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MigrationRequest<'a> {
    #[serde(flatten)]
    source_connection: SourceConnection<'a>,
    api_key: &'a str,
    source_index: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_index: Option<&'a str>,
    overwrite: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MigrationPreviewResponse {
    report: MigrationPreviewReport,
    source_counts: MigrationPreviewSourceCounts,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MigrationPreviewReport {
    entries: Vec<MigrationPreviewReportEntry>,
    summary: MigrationPreviewReportSummary,
    report_digest: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MigrationPreviewReportSummary {
    total_entries: usize,
    hard_rejections: usize,
    warnings: usize,
    scope_gaps: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MigrationPreviewReportEntry {
    severity: String,
    code: String,
    resource: String,
    json_path: String,
}

#[derive(Deserialize)]
struct MigrationPreviewSourceCounts {
    indexes: usize,
    records: usize,
}

struct MigrationPreviewPayload {
    response: MigrationPreviewResponse,
    json: Value,
}

#[derive(Clone, Copy)]
enum SourceConnection<'a> {
    Algolia(&'a str),
    Meilisearch(&'a str),
    Typesense(&'a str),
}

impl Serialize for SourceConnection<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            Self::Algolia(app_id) => map.serialize_entry("appId", app_id)?,
            Self::Meilisearch(endpoint) => map.serialize_entry("endpoint", endpoint)?,
            Self::Typesense(node) => map.serialize_entry("node", node)?,
        }
        map.end()
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MigrationStatus {
    job_id: String,
    phase: String,
    disposition: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_index: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    topology: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    export_progress: Option<MigrationExportProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    terminal_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings_applied: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    objects_imported: Option<MigrationCount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    synonyms_imported: Option<MigrationCount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rules_imported: Option<MigrationCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<Value>,
}

#[derive(Clone, Deserialize, Serialize)]
struct MigrationCount {
    imported: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MigrationExportProgress {
    completed: u64,
    total: u64,
}

struct MigrationClientFailure {
    kind: FailureKind,
    message: String,
}

struct ValidatedConnection {
    endpoint: reqwest::Url,
    timeout: Duration,
}

struct ValidatedSubmit<'a> {
    poll_interval: Duration,
    connection: ValidatedConnection,
    source_connection: SourceConnection<'a>,
    source_index: &'a str,
}

struct MigrationSecrets {
    flapjack_api_key: String,
    source_api_key: String,
}

impl MigrationSecrets {
    fn redaction_secrets(&self) -> [&str; 2] {
        [&self.flapjack_api_key, &self.source_api_key]
    }
}

#[derive(Deserialize)]
struct ServerErrorBody {
    code: String,
    status: u16,
    message: String,
}

enum MigrationSuccess {
    Status {
        status: Box<MigrationStatus>,
        secrets: Vec<String>,
    },
    Preview {
        preview: Box<MigrationPreviewPayload>,
        secrets: Vec<String>,
    },
    Acknowledged(String),
}

impl MigrationSuccess {
    fn status(status: MigrationStatus, secrets: &[&str]) -> Self {
        Self::Status {
            status: Box::new(status),
            secrets: secrets.iter().map(|secret| (*secret).to_string()).collect(),
        }
    }
}

struct MigrationClient<'a> {
    endpoint: reqwest::Url,
    application_id: &'a str,
    api_key: &'a str,
    /// Provider-selected first path segment under `1/migrations/` (e.g. `algolia`).
    route_segment: &'a str,
    client: reqwest::blocking::Client,
}

pub(crate) fn run(args: &MigrateArgs) -> Result<(), Box<dyn std::error::Error>> {
    let result = std::thread::scope(|scope| {
        scope.spawn(|| execute(args)).join().unwrap_or_else(|_| {
            Err(config_failure(
                "migration HTTP client thread panicked".to_string(),
            ))
        })
    });
    match result {
        Ok(MigrationSuccess::Status { status, secrets }) => {
            print_status(&status, args.connection.json, &secrets)?;
            Ok(())
        }
        Ok(MigrationSuccess::Preview { preview, secrets }) => {
            print_preview(
                &preview.response,
                &preview.json,
                args.connection.json,
                &secrets,
            )?;
            // Hard rejections are a CI-gating preview failure even though HTTP succeeded.
            if preview.response.report.summary.hard_rejections > 0 {
                std::process::exit(EXIT_PREVIEW_HARD_REJECTIONS);
            }
            Ok(())
        }
        Ok(MigrationSuccess::Acknowledged(job_id)) => {
            print_acknowledgement(&job_id, args.connection.json)
        }
        Err(failure) => finish_failure(failure, args.connection.json),
    }
}

fn execute(args: &MigrateArgs) -> Result<MigrationSuccess, MigrationFailure> {
    match args.action.as_ref() {
        None => run_migration(args),
        Some(MigrateAction::Preview) => run_preview(args),
        Some(MigrateAction::Cancel(job)) => {
            run_job_action(args, &job.job_id, JobActionKind::Cancel)
        }
        Some(MigrateAction::Ack(job)) => run_job_action(args, &job.job_id, JobActionKind::Ack),
    }
}

fn run_migration(args: &MigrateArgs) -> Result<MigrationSuccess, MigrationFailure> {
    let validated = validate_args(args)?;
    let ValidatedConnection { endpoint, timeout } = validated.connection;
    let credentials = read_migration_secrets(args)?;
    let secrets = credentials.redaction_secrets();
    let client =
        short_request_migration_client(args, endpoint, timeout, &credentials.flapjack_api_key)
            .map_err(|message| failure_with_secrets(FailureKind::Config, message, &secrets))?;
    let request = MigrationRequest {
        source_connection: validated.source_connection,
        api_key: &credentials.source_api_key,
        source_index: validated.source_index,
        target_index: args.target_index.as_deref(),
        overwrite: args.overwrite,
    };
    let admitted = client
        .submit(&request)
        .map_err(|error| failure_with_secrets(error.kind, error.message, &secrets))?;
    if admitted.disposition != "running" {
        return terminal_result(admitted, &secrets)
            .map(|status| MigrationSuccess::status(status, &secrets));
    }
    poll_until_terminal(
        &client,
        admitted,
        validated.poll_interval,
        timeout,
        &secrets,
    )
    .map(|status| MigrationSuccess::status(status, &secrets))
}

fn run_preview(args: &MigrateArgs) -> Result<MigrationSuccess, MigrationFailure> {
    if let Some(flag) = args.explicit_submit_only_flag() {
        return Err(submit_only_flag_failure(flag));
    }
    let validated = validate_args(args)?;
    let ValidatedConnection { endpoint, timeout } = validated.connection;
    let credentials = read_migration_secrets(args)?;
    let secrets = credentials.redaction_secrets();
    let client = MigrationClient::new(
        &args.connection,
        endpoint,
        timeout,
        &credentials.flapjack_api_key,
        args.source_provider.label(),
    )
    .map_err(|message| failure_with_secrets(FailureKind::Config, message, &secrets))?;
    let request = MigrationRequest {
        source_connection: validated.source_connection,
        api_key: &credentials.source_api_key,
        source_index: validated.source_index,
        target_index: None,
        overwrite: false,
    };
    let preview = client
        .preview(&request)
        .map_err(|error| failure_with_secrets(error.kind, error.message, &secrets))?;
    Ok(MigrationSuccess::Preview {
        preview: Box::new(preview),
        secrets: secrets.iter().map(|secret| (*secret).to_string()).collect(),
    })
}

fn run_job_action(
    args: &MigrateArgs,
    job_id: &str,
    action: JobActionKind,
) -> Result<MigrationSuccess, MigrationFailure> {
    // Preview accepts the source connection flags but rejects the submit-only
    // ones, so the retry these two rejections name must differ.
    if let Some(flag) = args.explicit_source_flag() {
        return Err(config_failure(format!(
            "{flag} is only valid when submitting a migration or previewing one"
        )));
    }
    if let Some(flag) = args.explicit_submit_only_flag() {
        return Err(submit_only_flag_failure(flag));
    }
    validate_job_id(job_id)
        .map_err(|reason| config_failure(format!("invalid --job-id: {reason}")))?;
    let ValidatedConnection { endpoint, timeout } = validate_flapjack_args(args)?;
    let api_key = flapjack_secret_source(&args.connection)
        .read("API key")
        .map_err(config_failure)?;
    let secrets = [api_key.as_str()];
    let client = short_request_migration_client(args, endpoint, timeout, &api_key)
        .map_err(|message| failure_with_secrets(FailureKind::Config, message, &secrets))?;
    match action {
        JobActionKind::Cancel => client
            .cancel(job_id)
            .map(|status| MigrationSuccess::status(status, &secrets))
            .map_err(|error| failure_with_secrets(error.kind, error.message, &secrets)),
        JobActionKind::Ack => client
            .acknowledge(job_id)
            .map(|()| MigrationSuccess::Acknowledged(job_id.to_string()))
            .map_err(|error| failure_with_secrets(error.kind, error.message, &secrets)),
    }
}

fn validate_args(args: &MigrateArgs) -> Result<ValidatedSubmit<'_>, MigrationFailure> {
    let connection = validate_flapjack_args(args)?;
    let source_connection = validate_source_connection(args)?;
    let source_index = args
        .source_index
        .as_deref()
        .ok_or_else(|| config_failure("--source-index is required".to_string()))?;
    let source_api_key_source = source_api_key_secret_source(args);
    source_api_key_source
        .validate_exactly_one(
            "--source-key-env (alias --algolia-key-env), --source-key-file (alias --algolia-key-file), or --source-key-stdin (alias --algolia-key-stdin)",
        )
        .map_err(config_failure)?;
    if args.connection.api_key_stdin && args.source_key_stdin {
        return Err(config_failure(
            "--api-key-stdin cannot be combined with --source-key-stdin (alias --algolia-key-stdin); both consume stdin"
                .to_string(),
        ));
    }
    validate_required_http_header_value("--source-index", source_index).map_err(config_failure)?;
    if let Some(target_index) = args.target_index.as_deref() {
        validate_required_http_header_value("--target-index", target_index)
            .map_err(config_failure)?;
    }
    let poll_interval = parse_bounded_duration(
        "--poll-interval",
        args.poll_interval
            .as_deref()
            .unwrap_or(DEFAULT_POLL_INTERVAL),
        MAX_POLL_INTERVAL,
    )?;
    Ok(ValidatedSubmit {
        poll_interval,
        connection,
        source_connection,
        source_index,
    })
}

fn read_migration_secrets(args: &MigrateArgs) -> Result<MigrationSecrets, MigrationFailure> {
    let flapjack_api_key = flapjack_secret_source(&args.connection)
        .read("API key")
        .map_err(config_failure)?;
    let source_api_key = source_api_key_secret_source(args)
        .read("source API key")
        .map_err(|message| {
            failure_with_secrets(FailureKind::Config, message, &[flapjack_api_key.as_str()])
        })?;
    Ok(MigrationSecrets {
        flapjack_api_key,
        source_api_key,
    })
}

/// Sole owner of the rejection wording for flags only a submission accepts, so
/// preview and the job actions cannot drift into contradicting each other.
fn submit_only_flag_failure(flag: &str) -> MigrationFailure {
    config_failure(format!("{flag} is only valid when submitting a migration"))
}

fn short_request_migration_client<'a>(
    args: &'a MigrateArgs,
    endpoint: reqwest::Url,
    timeout: Duration,
    api_key: &'a str,
) -> Result<MigrationClient<'a>, String> {
    MigrationClient::new(
        &args.connection,
        endpoint,
        timeout.min(Duration::from_secs(30)),
        api_key,
        args.source_provider.label(),
    )
}

impl MigrateArgs {
    fn explicit_source_flag(&self) -> Option<&'static str> {
        [
            (self.app_id.is_some(), "--app-id"),
            (self.source_endpoint.is_some(), "--source-endpoint"),
            (
                self.source_key_env.is_some(),
                "--source-key-env (alias --algolia-key-env)",
            ),
            (
                self.source_key_file.is_some(),
                "--source-key-file (alias --algolia-key-file)",
            ),
            (
                self.source_key_stdin,
                "--source-key-stdin (alias --algolia-key-stdin)",
            ),
            (self.source_index.is_some(), "--source-index"),
        ]
        .into_iter()
        .find_map(|(is_present, flag)| is_present.then_some(flag))
    }

    fn explicit_submit_only_flag(&self) -> Option<&'static str> {
        [
            (self.target_index.is_some(), "--target-index"),
            (self.overwrite, "--overwrite"),
            (self.poll_interval.is_some(), "--poll-interval"),
        ]
        .into_iter()
        .find_map(|(is_present, flag)| is_present.then_some(flag))
    }
}

fn validate_flapjack_args(args: &MigrateArgs) -> Result<ValidatedConnection, MigrationFailure> {
    let endpoint =
        validate_endpoint(args.connection.endpoint.as_deref()).map_err(config_failure)?;
    flapjack_secret_source(&args.connection)
        .validate_exactly_one("--api-key-env, --api-key-file, or --api-key-stdin")
        .map_err(config_failure)?;
    validate_required_http_header_value("--application-id", &args.connection.application_id)
        .map_err(config_failure)?;
    let timeout = parse_bounded_duration("--timeout", &args.timeout, MAX_TIMEOUT)?;
    Ok(ValidatedConnection { endpoint, timeout })
}

fn validate_source_connection(
    args: &MigrateArgs,
) -> Result<SourceConnection<'_>, MigrationFailure> {
    match args.source_provider {
        SourceProvider::Algolia => validate_algolia_connection(args),
        SourceProvider::Meilisearch | SourceProvider::Typesense => {
            validate_endpoint_source_connection(args)
        }
    }
}

fn validate_algolia_connection(
    args: &MigrateArgs,
) -> Result<SourceConnection<'_>, MigrationFailure> {
    if args.source_endpoint.is_some() {
        return Err(config_failure(
            "--source-endpoint is not valid with --source-provider algolia".to_string(),
        ));
    }
    let app_id = args
        .app_id
        .as_deref()
        .ok_or_else(|| config_failure("--app-id is required".to_string()))?;
    validate_required_http_header_value("--app-id", app_id).map_err(config_failure)?;
    Ok(SourceConnection::Algolia(app_id))
}

fn validate_endpoint_source_connection(
    args: &MigrateArgs,
) -> Result<SourceConnection<'_>, MigrationFailure> {
    let provider = args.source_provider;
    if args.app_id.is_some() {
        return Err(config_failure(format!(
            "--app-id is not valid with --source-provider {provider}"
        )));
    }
    let source_endpoint = args.source_endpoint.as_deref().ok_or_else(|| {
        config_failure(format!(
            "--source-endpoint is required for --source-provider {provider}"
        ))
    })?;
    validate_source_endpoint(source_endpoint).map_err(config_failure)?;
    match provider {
        SourceProvider::Meilisearch => Ok(SourceConnection::Meilisearch(source_endpoint)),
        SourceProvider::Typesense => Ok(SourceConnection::Typesense(source_endpoint)),
        SourceProvider::Algolia => {
            unreachable!("algolia source connection is validated separately")
        }
    }
}

fn validate_endpoint(endpoint: Option<&str>) -> Result<reqwest::Url, String> {
    let endpoint = endpoint.ok_or_else(|| "--endpoint is required".to_string())?;
    let endpoint =
        reqwest::Url::parse(endpoint).map_err(|error| format!("invalid --endpoint: {error}"))?;
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
        return Err("--endpoint must be an absolute http or https URL".to_string());
    }
    if endpoint.scheme() == "http" && !endpoint_targets_loopback(&endpoint) {
        return Err(
            "--endpoint must use https unless it targets localhost or a loopback IP".to_string(),
        );
    }
    Ok(endpoint)
}

fn validate_source_endpoint(source_endpoint: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(source_endpoint)
        .map_err(|error| format!("invalid --source-endpoint: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("--source-endpoint must be an absolute http or https URL".to_string());
    }
    // Unlike --endpoint, the CLI never sends credentials to this URL; it only forwards the
    // string to Flapjack, where the vendor allowlist has a single server-owned implementation.
    Ok(())
}

fn endpoint_targets_loopback(endpoint: &reqwest::Url) -> bool {
    let Some(host) = endpoint.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn flapjack_secret_source(args: &MigrateConnectionArgs) -> SecretSource<'_> {
    SecretSource::new(
        args.api_key_env.as_deref(),
        args.api_key_file.as_deref(),
        args.api_key_stdin,
    )
}

fn source_api_key_secret_source(args: &MigrateArgs) -> SecretSource<'_> {
    SecretSource::new(
        args.source_key_env.as_deref(),
        args.source_key_file.as_deref(),
        args.source_key_stdin,
    )
}

fn parse_bounded_duration(
    flag: &str,
    value: &str,
    maximum: Duration,
) -> Result<Duration, MigrationFailure> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1_u64)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3_600_000)
    } else {
        return Err(config_failure(format!(
            "{flag} must use an ms, s, m, or h suffix"
        )));
    };
    let amount = number
        .parse::<u64>()
        .map_err(|_| config_failure(format!("{flag} must be a positive whole duration")))?;
    let millis = amount
        .checked_mul(multiplier)
        .ok_or_else(|| config_failure(format!("{flag} is too large")))?;
    let duration = Duration::from_millis(millis);
    if duration.is_zero() || duration > maximum {
        return Err(config_failure(format!(
            "{flag} must be greater than zero and at most {}ms",
            maximum.as_millis()
        )));
    }
    Ok(duration)
}

fn poll_until_terminal(
    client: &MigrationClient<'_>,
    mut status: MigrationStatus,
    poll_interval: Duration,
    timeout: Duration,
    secrets: &[&str],
) -> Result<MigrationStatus, MigrationFailure> {
    let started_at = Instant::now();
    loop {
        if started_at.elapsed() >= timeout {
            return Err(timeout_failure(status, timeout, secrets));
        }
        status = client.status(&status.job_id).map_err(|error| {
            let mut failure = failure_with_secrets(error.kind, error.message, secrets);
            failure.status = Some(Box::new(status.clone()));
            failure
        })?;
        if status.disposition != "running" {
            return terminal_result(status, secrets);
        }
        let remaining = timeout.saturating_sub(started_at.elapsed());
        std::thread::sleep(poll_interval.min(remaining));
    }
}

fn terminal_result(
    status: MigrationStatus,
    secrets: &[&str],
) -> Result<MigrationStatus, MigrationFailure> {
    match status.disposition.as_str() {
        "succeeded" => Ok(status),
        "failed" => Err(terminal_failure(FailureKind::FailedJob, status, secrets)),
        "cancelled" => Err(terminal_failure(FailureKind::CancelledJob, status, secrets)),
        disposition => Err(failure_with_secrets(
            FailureKind::HttpRejection,
            format!("migration server returned unknown disposition {disposition}"),
            secrets,
        )),
    }
}

fn terminal_failure(
    kind: FailureKind,
    status: MigrationStatus,
    secrets: &[&str],
) -> MigrationFailure {
    MigrationFailure {
        kind,
        message: format!(
            "migration job {} reached {} disposition",
            status.job_id, status.disposition
        ),
        status: Some(Box::new(status)),
        secrets: secrets.iter().map(|secret| (*secret).to_string()).collect(),
    }
}

fn timeout_failure(
    status: MigrationStatus,
    timeout: Duration,
    secrets: &[&str],
) -> MigrationFailure {
    MigrationFailure {
        kind: FailureKind::Timeout,
        message: format!(
            "migration job {} timed out after {}ms while {}",
            status.job_id,
            timeout.as_millis(),
            status.disposition
        ),
        status: Some(Box::new(status)),
        secrets: secrets.iter().map(|secret| (*secret).to_string()).collect(),
    }
}

impl<'a> MigrationClient<'a> {
    fn new(
        args: &'a MigrateConnectionArgs,
        endpoint: reqwest::Url,
        request_timeout: Duration,
        api_key: &'a str,
        route_segment: &'a str,
    ) -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(request_timeout.min(Duration::from_secs(5)))
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("failed to configure migration HTTP client: {error}"))?;
        Ok(Self {
            endpoint,
            application_id: &args.application_id,
            api_key,
            route_segment,
            client,
        })
    }

    fn submit(
        &self,
        request: &MigrationRequest<'_>,
    ) -> Result<MigrationStatus, MigrationClientFailure> {
        let url = self.url(&format!("1/migrations/{}", self.route_segment))?;
        let response = self
            .authenticated(self.client.post(url))
            .json(request)
            .send()
            .map_err(|error| transport_failure("migration submission", error))?;
        parse_response(response, 202, "migration submission")
    }

    fn preview(
        &self,
        request: &MigrationRequest<'_>,
    ) -> Result<MigrationPreviewPayload, MigrationClientFailure> {
        let url = self.url(&format!("1/migrations/{}/preview", self.route_segment))?;
        let response = self
            .authenticated(self.client.post(url))
            .json(request)
            .send()
            .map_err(|error| transport_failure("migration preview", error))?;
        parse_preview_response(response)
    }

    fn status(&self, job_id: &str) -> Result<MigrationStatus, MigrationClientFailure> {
        validate_job_id(job_id).map_err(|reason| {
            http_rejection(format!(
                "migration server returned an invalid jobId: {reason}"
            ))
        })?;
        let url = self.url(&format!("1/migrations/{}/{job_id}", self.route_segment))?;
        let response = self
            .authenticated(self.client.get(url))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .send()
            .map_err(|error| transport_failure("migration status request", error))?;
        parse_response(response, 200, "migration status")
    }

    fn cancel(&self, job_id: &str) -> Result<MigrationStatus, MigrationClientFailure> {
        let response = self.send_job_action(job_id, "cancel", "migration cancellation")?;
        parse_response(response, 200, "migration cancellation")
    }

    fn acknowledge(&self, job_id: &str) -> Result<(), MigrationClientFailure> {
        let response = self.send_job_action(job_id, "acknowledge", "migration acknowledgement")?;
        parse_empty_response(response, 204, "migration acknowledgement")
    }

    fn send_job_action(
        &self,
        job_id: &str,
        action: &str,
        operation: &str,
    ) -> Result<reqwest::blocking::Response, MigrationClientFailure> {
        validate_job_id(job_id).map_err(|reason| {
            http_rejection(format!(
                "migration action received invalid job ID: {reason}"
            ))
        })?;
        let url = self.url(&format!(
            "1/migrations/{}/{job_id}/{action}",
            self.route_segment
        ))?;
        self.authenticated(self.client.post(url))
            .send()
            .map_err(|error| transport_failure(operation, error))
    }

    fn authenticated(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        request
            .header("x-algolia-application-id", self.application_id)
            .header("x-algolia-api-key", self.api_key)
    }

    fn url(&self, relative_path: &str) -> Result<reqwest::Url, MigrationClientFailure> {
        let mut endpoint = self.endpoint.clone();
        let mut path = endpoint.path().trim_end_matches('/').to_string();
        path.push('/');
        path.push_str(relative_path);
        endpoint.set_path(&path);
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        Ok(endpoint)
    }
}

fn validate_job_id(job_id: &str) -> Result<(), &'static str> {
    let valid = job_id.len() == 36
        && job_id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if !valid {
        return Err("expected 8-4-4-4-12 hexadecimal UUID format");
    }
    Ok(())
}

fn transport_failure(operation: &str, error: reqwest::Error) -> MigrationClientFailure {
    let kind = if error.is_timeout() {
        FailureKind::Timeout
    } else {
        FailureKind::HttpRejection
    };
    let action = if error.is_timeout() {
        "timed out"
    } else {
        "failed"
    };
    MigrationClientFailure {
        kind,
        message: format!("{operation} {action}: {error}"),
    }
}

fn parse_response(
    response: reqwest::blocking::Response,
    expected_status: u16,
    operation: &str,
) -> Result<MigrationStatus, MigrationClientFailure> {
    let (status, body) = read_response(response, operation)?;
    require_status(status, &body, expected_status, operation)?;
    let parsed: MigrationStatus = serde_json::from_slice(&body).map_err(|error| {
        http_rejection(format!(
            "{operation} returned an incompatible response: {error}"
        ))
    })?;
    validate_job_id(&parsed.job_id).map_err(|reason| {
        http_rejection(format!("{operation} returned an invalid jobId: {reason}"))
    })?;
    Ok(parsed)
}

fn parse_preview_response(
    response: reqwest::blocking::Response,
) -> Result<MigrationPreviewPayload, MigrationClientFailure> {
    const OPERATION: &str = "migration preview";
    let (status, body) = read_response(response, OPERATION)?;
    require_status(status, &body, 200, OPERATION)?;
    let json: Value = serde_json::from_slice(&body).map_err(|error| {
        http_rejection(format!(
            "{OPERATION} returned an incompatible response: {error}"
        ))
    })?;
    let response: MigrationPreviewResponse =
        serde_json::from_value(json.clone()).map_err(|error| {
            http_rejection(format!(
                "{OPERATION} returned an incompatible response: {error}"
            ))
        })?;
    validate_preview_report(&response.report)?;
    Ok(MigrationPreviewPayload { response, json })
}

fn validate_preview_report(report: &MigrationPreviewReport) -> Result<(), MigrationClientFailure> {
    let mut hard_rejections = 0;
    let mut warnings = 0;
    let mut scope_gaps = 0;
    for entry in &report.entries {
        match entry.severity.as_str() {
            "HardRejection" => hard_rejections += 1,
            "Warning" => warnings += 1,
            "ScopeGap" => scope_gaps += 1,
            _ => {
                return Err(http_rejection(
                    "migration preview returned an incompatible response: unknown report severity"
                        .to_string(),
                ));
            }
        }
    }
    let summary = &report.summary;
    if summary.total_entries != report.entries.len()
        || summary.hard_rejections != hard_rejections
        || summary.warnings != warnings
        || summary.scope_gaps != scope_gaps
    {
        return Err(http_rejection(
            "migration preview returned an incompatible response: report summary does not match entries"
                .to_string(),
        ));
    }
    Ok(())
}

fn parse_empty_response(
    response: reqwest::blocking::Response,
    expected_status: u16,
    operation: &str,
) -> Result<(), MigrationClientFailure> {
    let (status, body) = read_response(response, operation)?;
    require_status(status, &body, expected_status, operation)?;
    if !body.is_empty() {
        return Err(http_rejection(format!(
            "{operation} returned an unexpected response body"
        )));
    }
    Ok(())
}

fn read_response(
    mut response: reqwest::blocking::Response,
    operation: &str,
) -> Result<(u16, Vec<u8>), MigrationClientFailure> {
    let status = response.status().as_u16();
    let mut body = Vec::new();
    (&mut response)
        .take((MAX_HTTP_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| http_rejection(format!("failed to read {operation} response: {error}")))?;
    if body.len() > MAX_HTTP_RESPONSE_BYTES {
        return Err(http_rejection(format!(
            "{operation} response exceeded {MAX_HTTP_RESPONSE_BYTES} bytes"
        )));
    }
    Ok((status, body))
}

fn require_status(
    status: u16,
    body: &[u8],
    expected_status: u16,
    operation: &str,
) -> Result<(), MigrationClientFailure> {
    if status == expected_status {
        return Ok(());
    }
    if let Ok(error) = serde_json::from_slice::<ServerErrorBody>(body) {
        let kind = match error.code.as_str() {
            "cancel_too_late" => FailureKind::CancelTooLate,
            "migration_ack_too_early" => FailureKind::AckTooEarly,
            _ => FailureKind::HttpRejection,
        };
        return Err(MigrationClientFailure {
            kind,
            message: format!(
                "{operation} returned HTTP {status}: code={} status={} message={}",
                error.code, error.status, error.message
            ),
        });
    }
    Err(http_rejection(format!(
        "{operation} returned HTTP {status}: {}",
        String::from_utf8_lossy(body)
    )))
}

fn http_rejection(message: String) -> MigrationClientFailure {
    MigrationClientFailure {
        kind: FailureKind::HttpRejection,
        message,
    }
}

fn config_failure(message: String) -> MigrationFailure {
    MigrationFailure {
        kind: FailureKind::Config,
        message,
        status: None,
        secrets: Vec::new(),
    }
}

fn failure_with_secrets(kind: FailureKind, message: String, secrets: &[&str]) -> MigrationFailure {
    MigrationFailure {
        kind,
        message,
        status: None,
        secrets: secrets.iter().map(|secret| (*secret).to_string()).collect(),
    }
}
