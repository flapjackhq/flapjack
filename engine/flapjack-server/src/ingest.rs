use clap::{Args, ValueEnum};
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_BATCH_SIZE: usize = 1_000;
const DEFAULT_ACTION_FIELD: &str = "_action";
const RETRY_ATTEMPT_LIMIT: usize = 3;
const MAX_RETRY_AFTER_DELAY: Duration = Duration::from_millis(100);
const MAX_HTTP_RESPONSE_BYTES: usize = 1024 * 1024;
const RETRYABLE_STATUSES: [u16; 2] = [429, 503];

const EXIT_CONFIG: i32 = 2;
const EXIT_PERMANENT_HTTP_REJECTION: i32 = 3;
const EXIT_OUTCOME_UNKNOWN: i32 = 4;
const EXIT_RETRY_EXHAUSTED: i32 = 5;
const EXIT_LOCAL_CLEANUP: i32 = 6;

#[derive(Args, Debug)]
pub(crate) struct IngestArgs {
    /// Base HTTP endpoint, for example http://127.0.0.1:7700
    #[arg(long)]
    endpoint: String,

    /// Destination index name
    #[arg(long)]
    index: String,

    /// Source JSON array or NDJSON path, or '-' for stdin
    #[arg(long)]
    source: String,

    /// Maximum records per parser batch
    #[arg(long, default_value_t = DEFAULT_BATCH_SIZE)]
    batch_size: usize,

    /// Algolia application id header value
    #[arg(long, default_value = "flapjack")]
    application_id: String,

    /// Environment variable containing the API key
    #[arg(long)]
    api_key_env: Option<String>,

    /// File containing the API key
    #[arg(long)]
    api_key_file: Option<PathBuf>,

    /// Read the API key from stdin
    #[arg(long)]
    api_key_stdin: bool,

    /// Source field to use as objectID before falling back to objectID/id
    #[arg(long)]
    object_id_field: Option<String>,

    /// Source field carrying upsert/delete action markers
    #[arg(long, default_value = DEFAULT_ACTION_FIELD)]
    action_field: String,

    /// Ingestion mode. replace is reserved until atomic publication support ships.
    #[arg(long, value_enum, default_value_t = IngestMode::Upsert)]
    mode: IngestMode,

    /// Prefix for generated x-flapjack-idempotency-key header values
    #[arg(long, default_value = "flapjack-ingest")]
    idempotency_key_prefix: String,

    /// Emit a JSON report
    #[arg(long)]
    report_json: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum IngestMode {
    Upsert,
    Replace,
}

#[derive(Default, Serialize)]
struct IngestReport {
    attempted: usize,
    confirmed_committed: usize,
    outcome_unknown: usize,
    retries: usize,
    last_retry_after_ms: Option<u64>,
    queue_high_watermark: usize,
    failure_classification: Option<FailureClassification>,
    error: Option<String>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureClassification {
    Config,
    Input,
    PermanentHttpRejection,
    RetryExhausted,
    OutcomeUnknown,
    ReplaceNotSupported,
    #[allow(dead_code)]
    LocalCleanup,
}

impl FailureClassification {
    fn exit_code(self) -> i32 {
        match self {
            Self::Config | Self::Input => EXIT_CONFIG,
            Self::PermanentHttpRejection => EXIT_PERMANENT_HTTP_REJECTION,
            Self::OutcomeUnknown => EXIT_OUTCOME_UNKNOWN,
            Self::ReplaceNotSupported => EXIT_CONFIG,
            Self::RetryExhausted => EXIT_RETRY_EXHAUSTED,
            Self::LocalCleanup => EXIT_LOCAL_CLEANUP,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IngestAction {
    Upsert,
    Delete,
}

struct RecordOperation {
    action: IngestAction,
    object_id: String,
    body: Map<String, Value>,
}

struct RawRecord(Map<String, Value>);

#[derive(Serialize)]
struct BatchEnvelope {
    requests: Vec<BatchRequest>,
}

#[derive(Serialize)]
struct BatchRequest {
    action: &'static str,
    body: Value,
}

struct Endpoint {
    host: String,
    port: u16,
    base_path: String,
}

struct HttpSink<'a> {
    endpoint: Endpoint,
    index: &'a str,
    application_id: &'a str,
    api_key: &'a str,
    idempotency_key_prefix: &'a str,
    idempotency_key_run_id: String,
    next_batch_id: usize,
}

pub(crate) fn run(args: &IngestArgs) -> Result<(), Box<dyn std::error::Error>> {
    let result = run_ingest(args);
    match result {
        Ok(report) => finish(report, args.report_json, None),
        Err(error) => {
            let mut report = *error.report;
            let redacted_message = redact(&error.message, &error.api_key);
            report.error = Some(redacted_message.clone());
            report.failure_classification = Some(error.classification);
            finish(
                report,
                args.report_json,
                Some((redacted_message, error.classification.exit_code())),
            )
        }
    }
}

fn finish(
    report: IngestReport,
    report_json: bool,
    error: Option<(String, i32)>,
) -> Result<(), Box<dyn std::error::Error>> {
    if report_json {
        println!("{}", serde_json::to_string(&report)?);
    }
    if let Some((message, exit_code)) = error {
        if !report_json {
            eprintln!("{}", message);
        } else if let Some(redacted) = report.error.as_deref() {
            eprintln!("{redacted}");
        }
        std::process::exit(exit_code);
    }
    Ok(())
}

struct IngestFailure {
    message: String,
    api_key: String,
    classification: FailureClassification,
    report: Box<IngestReport>,
}

struct IngestError {
    message: String,
    classification: FailureClassification,
}

fn run_ingest(args: &IngestArgs) -> Result<IngestReport, IngestFailure> {
    validate_args(args)?;
    let api_key = read_api_key(args)?;
    if args.mode == IngestMode::Replace {
        return Err(IngestFailure {
            message: "replace_not_supported: --mode replace requires the MIG-5 mutation-fence/publication contract and is not available in this beta".to_string(),
            api_key,
            classification: FailureClassification::ReplaceNotSupported,
            report: Box::default(),
        });
    }
    let endpoint =
        parse_endpoint(&args.endpoint).map_err(|message| input_error(message, &api_key))?;
    let mut report = IngestReport::default();
    let mut sink = HttpSink {
        endpoint,
        index: &args.index,
        application_id: &args.application_id,
        api_key: &api_key,
        idempotency_key_prefix: &args.idempotency_key_prefix,
        idempotency_key_run_id: new_ingest_run_id(),
        next_batch_id: 0,
    };

    match process_source(args, &mut sink, &mut report) {
        Ok(()) => Ok(report),
        Err(error) => Err(IngestFailure {
            message: error.message,
            api_key,
            classification: error.classification,
            report: Box::new(report),
        }),
    }
}

fn validate_args(args: &IngestArgs) -> Result<(), IngestFailure> {
    if args.batch_size == 0 {
        return Err(config_error("--batch-size must be greater than 0"));
    }
    if args.source == "-" && args.api_key_stdin {
        return Err(config_error(
            "--source - cannot be combined with --api-key-stdin; both consume stdin",
        ));
    }
    let sources = usize::from(args.api_key_env.is_some())
        + usize::from(args.api_key_file.is_some())
        + usize::from(args.api_key_stdin);
    if sources != 1 {
        return Err(config_error(
            "exactly one of --api-key-env, --api-key-file, or --api-key-stdin is required",
        ));
    }
    validate_http_header_value("--application-id", &args.application_id)
        .map_err(|message| config_error(&message))?;
    validate_http_header_value("--idempotency-key-prefix", &args.idempotency_key_prefix)
        .map_err(|message| config_error(&message))?;
    validate_request_target_value("--index", &args.index)
        .map_err(|message| config_error(&message))?;
    Ok(())
}

fn validate_http_header_value(name: &str, value: &str) -> Result<(), String> {
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(format!("{name} cannot contain HTTP control characters"));
    }
    Ok(())
}

fn validate_request_target_value(name: &str, value: &str) -> Result<(), String> {
    if value
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(format!(
            "{name} cannot contain HTTP whitespace or control characters"
        ));
    }
    Ok(())
}

fn config_error(message: &str) -> IngestFailure {
    IngestFailure {
        message: message.to_string(),
        api_key: String::new(),
        classification: FailureClassification::Config,
        report: Box::default(),
    }
}

fn input_error(message: String, api_key: &str) -> IngestFailure {
    IngestFailure {
        message,
        api_key: api_key.to_string(),
        classification: FailureClassification::Input,
        report: Box::default(),
    }
}

fn read_api_key(args: &IngestArgs) -> Result<String, IngestFailure> {
    let key = if let Some(env_var) = &args.api_key_env {
        std::env::var(env_var)
            .map_err(|_| config_error("API key environment variable is not set"))?
    } else if let Some(path) = &args.api_key_file {
        std::fs::read_to_string(path)
            .map_err(|error| config_error(&format!("failed to read API key file: {}", error)))?
    } else {
        let mut key = String::new();
        io::stdin().read_to_string(&mut key).map_err(|error| {
            config_error(&format!("failed to read API key from stdin: {error}"))
        })?;
        key
    };
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err(config_error("API key cannot be empty"));
    }
    validate_http_header_value("API key", &key).map_err(|message| config_error(&message))?;
    Ok(key)
}

fn process_source(
    args: &IngestArgs,
    sink: &mut HttpSink<'_>,
    report: &mut IngestReport,
) -> Result<(), IngestError> {
    let mut reader = BufReader::new(open_source(&args.source)?);
    let first = first_non_ws_byte(&mut reader)?;
    let Some(first) = first else {
        return Ok(());
    };
    if first == b'[' {
        process_json_array(reader, args, sink, report)
    } else {
        process_ndjson(reader, args, sink, report)
    }
}

fn open_source(source: &str) -> Result<Box<dyn Read>, IngestError> {
    if source == "-" {
        return Ok(Box::new(io::stdin()));
    }
    let file = File::open(source)
        .map_err(|error| input_ingest_error(format!("failed to open source: {error}")))?;
    Ok(Box::new(file))
}

fn first_non_ws_byte(reader: &mut BufReader<Box<dyn Read>>) -> Result<Option<u8>, IngestError> {
    let buffer = reader
        .fill_buf()
        .map_err(|error| input_ingest_error(format!("failed to read source: {error}")))?;
    Ok(buffer
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace()))
}

fn process_json_array(
    reader: BufReader<Box<dyn Read>>,
    args: &IngestArgs,
    sink: &mut HttpSink<'_>,
    report: &mut IngestReport,
) -> Result<(), IngestError> {
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let result = deserializer
        .deserialize_seq(JsonArrayVisitor { args, sink, report })
        .map_err(|error| input_ingest_error(format!("malformed JSON array source: {error}")))?;
    result?;
    deserializer
        .end()
        .map_err(|error| input_ingest_error(format!("trailing JSON array data: {error}")))
}

fn process_ndjson(
    reader: BufReader<Box<dyn Read>>,
    args: &IngestArgs,
    sink: &mut HttpSink<'_>,
    report: &mut IngestReport,
) -> Result<(), IngestError> {
    let mut pending = Vec::with_capacity(args.batch_size);
    for line in reader.lines() {
        let line = line
            .map_err(|error| input_ingest_error(format!("failed to read NDJSON line: {error}")))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record: RawRecord = serde_json::from_str(trimmed)
            .map_err(|error| input_ingest_error(format!("malformed NDJSON record: {error}")))?;
        pending.push(record_from_raw(record, args).map_err(input_ingest_error)?);
        if pending.len() == args.batch_size {
            send_batch(&mut pending, sink, report)?;
        }
    }
    send_batch(&mut pending, sink, report)
}

struct JsonArrayVisitor<'a, 'b> {
    args: &'a IngestArgs,
    sink: &'a mut HttpSink<'b>,
    report: &'a mut IngestReport,
}

impl<'de> Visitor<'de> for JsonArrayVisitor<'_, '_> {
    type Value = Result<(), IngestError>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON array of objects")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut pending = Vec::with_capacity(self.args.batch_size);
        while let Some(record) = seq.next_element::<RawRecord>()? {
            let operation = match record_from_raw(record, self.args) {
                Ok(operation) => operation,
                Err(message) => return Ok(Err(input_ingest_error(message))),
            };
            pending.push(operation);
            if pending.len() == self.args.batch_size {
                if let Err(message) = send_batch(&mut pending, self.sink, self.report) {
                    return Ok(Err(message));
                }
            }
        }
        Ok(send_batch(&mut pending, self.sink, self.report))
    }
}

impl<'de> Deserialize<'de> for RawRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawRecordVisitor)
    }
}

struct RawRecordVisitor;

impl<'de> Visitor<'de> for RawRecordVisitor {
    type Value = RawRecord;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON object without duplicate top-level keys")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut body = Map::new();
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            if body.insert(key.clone(), value).is_some() {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON object key: {key}"
                )));
            }
        }
        Ok(RawRecord(body))
    }
}

fn record_from_raw(record: RawRecord, args: &IngestArgs) -> Result<RecordOperation, String> {
    let RawRecord(mut body) = record;
    let action = parse_action(body.remove(&args.action_field).as_ref())?;
    let object_id = resolve_object_id(&body, args)?;
    if action == IngestAction::Upsert {
        body.insert("objectID".to_string(), Value::String(object_id.clone()));
    }
    Ok(RecordOperation {
        action,
        object_id,
        body,
    })
}

fn parse_action(value: Option<&Value>) -> Result<IngestAction, String> {
    let Some(value) = value else {
        return Ok(IngestAction::Upsert);
    };
    match value.as_str() {
        Some("upsert" | "addObject") => Ok(IngestAction::Upsert),
        Some("delete" | "deleteObject") => Ok(IngestAction::Delete),
        Some(other) => Err(format!("unknown action: {other}")),
        None => Err("action field must be a string".to_string()),
    }
}

fn resolve_object_id(body: &Map<String, Value>, args: &IngestArgs) -> Result<String, String> {
    let candidate = args
        .object_id_field
        .as_ref()
        .and_then(|field| body.get(field))
        .or_else(|| body.get("objectID"))
        .or_else(|| body.get("id"));
    candidate
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            "missing object identity: expected objectID, id, or configured field".to_string()
        })
}

fn send_batch(
    pending: &mut Vec<RecordOperation>,
    sink: &mut HttpSink<'_>,
    report: &mut IngestReport,
) -> Result<(), IngestError> {
    if pending.is_empty() {
        return Ok(());
    }
    let operations = std::mem::take(pending);
    report.attempted += operations.len();
    report.queue_high_watermark = report.queue_high_watermark.max(operations.len());
    for envelope in homogeneous_envelopes(operations) {
        let count = envelope.requests.len();
        match sink.send(&envelope, report) {
            Ok(()) => report.confirmed_committed += count,
            Err(SendError {
                kind: SendErrorKind::Unknown,
                message,
            }) => {
                report.outcome_unknown += count;
                return Err(IngestError {
                    message,
                    classification: FailureClassification::OutcomeUnknown,
                });
            }
            Err(SendError {
                kind: SendErrorKind::RetryExhaustedBeforeSend,
                message,
            }) => {
                return Err(IngestError {
                    message,
                    classification: FailureClassification::RetryExhausted,
                });
            }
            Err(SendError {
                kind: SendErrorKind::Permanent,
                message,
            }) => {
                return Err(IngestError {
                    message,
                    classification: FailureClassification::PermanentHttpRejection,
                });
            }
        }
    }
    Ok(())
}

fn input_ingest_error(message: String) -> IngestError {
    IngestError {
        message,
        classification: FailureClassification::Input,
    }
}

fn homogeneous_envelopes(operations: Vec<RecordOperation>) -> Vec<BatchEnvelope> {
    let mut envelopes = Vec::new();
    let mut current_action = None;
    let mut requests = Vec::new();
    for operation in operations {
        if current_action.is_some() && current_action != Some(operation.action) {
            envelopes.push(BatchEnvelope { requests });
            requests = Vec::new();
        }
        current_action = Some(operation.action);
        requests.push(batch_request(operation));
    }
    if !requests.is_empty() {
        envelopes.push(BatchEnvelope { requests });
    }
    envelopes
}

fn batch_request(operation: RecordOperation) -> BatchRequest {
    match operation.action {
        IngestAction::Upsert => BatchRequest {
            action: "addObject",
            body: Value::Object(operation.body),
        },
        IngestAction::Delete => BatchRequest {
            action: "deleteObject",
            body: serde_json::json!({ "objectID": operation.object_id }),
        },
    }
}

struct SendError {
    kind: SendErrorKind,
    message: String,
}

enum SendErrorKind {
    Permanent,
    RetryExhaustedBeforeSend,
    Unknown,
}

enum AttemptOutcome {
    Success,
    Retryable {
        message: String,
        retry_after: Duration,
        ambiguous_on_exhaustion: bool,
    },
    Permanent(String),
}

impl HttpSink<'_> {
    fn send(
        &mut self,
        envelope: &BatchEnvelope,
        report: &mut IngestReport,
    ) -> Result<(), SendError> {
        let body = serde_json::to_string(envelope)
            .map_err(|error| permanent_send_error(format!("failed to encode batch: {error}")))?;
        let path = format!(
            "{}/1/indexes/{}/batch",
            self.endpoint.base_path.trim_end_matches('/'),
            self.index
        );
        let key = self.next_idempotency_key();
        let mut last_retryable = None;
        let mut exhausted_attempt_is_ambiguous = false;

        for attempt in 1..=RETRY_ATTEMPT_LIMIT {
            match self.send_once(&path, &body, &key) {
                AttemptOutcome::Success => return Ok(()),
                AttemptOutcome::Permanent(message) => return Err(permanent_send_error(message)),
                AttemptOutcome::Retryable {
                    message,
                    retry_after,
                    ambiguous_on_exhaustion,
                } => {
                    exhausted_attempt_is_ambiguous |= ambiguous_on_exhaustion;
                    last_retryable = Some(message);
                    if attempt == RETRY_ATTEMPT_LIMIT {
                        break;
                    }
                    report.retries += 1;
                    report.last_retry_after_ms = Some(duration_millis(retry_after));
                    std::thread::sleep(retry_after);
                }
            }
        }

        let message = last_retryable.unwrap_or_else(|| "retry attempts exhausted".to_string());
        if exhausted_attempt_is_ambiguous {
            Err(SendError {
                kind: SendErrorKind::Unknown,
                message,
            })
        } else {
            Err(SendError {
                kind: SendErrorKind::RetryExhaustedBeforeSend,
                message,
            })
        }
    }

    fn send_once(&self, path: &str, body: &str, key: &str) -> AttemptOutcome {
        let mut stream = match TcpStream::connect((self.endpoint.host.as_str(), self.endpoint.port))
        {
            Ok(stream) => stream,
            Err(error) => {
                return retryable_before_send(format!("failed to connect to sink: {error}"));
            }
        };
        if let Err(error) = stream.set_read_timeout(Some(Duration::from_secs(5))) {
            return retryable_before_send(format!("failed to configure sink: {error}"));
        }
        if let Err(error) = stream.set_write_timeout(Some(Duration::from_secs(5))) {
            return retryable_before_send(format!("failed to configure sink: {error}"));
        }
        let request = self.http_request(path, body, key);
        if let Err(error) = stream.write_all(request.as_bytes()) {
            return AttemptOutcome::Retryable {
                message: format!("failed to send batch: {error}"),
                retry_after: Duration::ZERO,
                ambiguous_on_exhaustion: true,
            };
        }
        let mut response = Vec::new();
        if let Err(error) = (&mut stream)
            .take((MAX_HTTP_RESPONSE_BYTES + 1) as u64)
            .read_to_end(&mut response)
        {
            return AttemptOutcome::Retryable {
                message: format!("lost response after send: {error}"),
                retry_after: Duration::ZERO,
                ambiguous_on_exhaustion: true,
            };
        }
        if response.len() > MAX_HTTP_RESPONSE_BYTES {
            return AttemptOutcome::Retryable {
                message: format!(
                    "sink response too large: exceeded {MAX_HTTP_RESPONSE_BYTES} bytes"
                ),
                retry_after: Duration::ZERO,
                ambiguous_on_exhaustion: true,
            };
        }
        classify_response(&response)
    }

    fn next_idempotency_key(&mut self) -> String {
        let key = format!(
            "{}-{}-{}",
            self.idempotency_key_prefix, self.idempotency_key_run_id, self.next_batch_id
        );
        self.next_batch_id += 1;
        key
    }

    fn http_request(&self, path: &str, body: &str, key: &str) -> String {
        format!(
            "POST {path} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nx-algolia-application-id: {}\r\nx-algolia-api-key: {}\r\nx-flapjack-idempotency-key: {}\r\n\r\n{}",
            self.endpoint.host,
            self.endpoint.port,
            body.len(),
            self.application_id,
            self.api_key,
            key,
            body
        )
    }
}

fn classify_response(response: &[u8]) -> AttemptOutcome {
    if response.is_empty() {
        return AttemptOutcome::Retryable {
            message: "lost response after sink read the batch".to_string(),
            retry_after: Duration::ZERO,
            ambiguous_on_exhaustion: true,
        };
    }
    let text = String::from_utf8_lossy(response);
    let Some((head, _body)) = text.split_once("\r\n\r\n") else {
        return AttemptOutcome::Retryable {
            message: "invalid sink response after send".to_string(),
            retry_after: Duration::ZERO,
            ambiguous_on_exhaustion: true,
        };
    };
    let status = match head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
    {
        Some(status) => status,
        None => {
            return AttemptOutcome::Retryable {
                message: "invalid sink response after send".to_string(),
                retry_after: Duration::ZERO,
                ambiguous_on_exhaustion: true,
            };
        }
    };
    if (200..300).contains(&status) {
        return AttemptOutcome::Success;
    }
    if RETRYABLE_STATUSES.contains(&status) {
        return AttemptOutcome::Retryable {
            message: format!("sink returned retryable HTTP {status}"),
            retry_after: bounded_retry_after(head),
            ambiguous_on_exhaustion: false,
        };
    }
    AttemptOutcome::Permanent(format!(
        "sink returned HTTP {status}: {}",
        redact_redirect_locations(&text)
    ))
}

fn retryable_before_send(message: String) -> AttemptOutcome {
    AttemptOutcome::Retryable {
        message,
        retry_after: Duration::ZERO,
        ambiguous_on_exhaustion: false,
    }
}

fn permanent_send_error(message: String) -> SendError {
    SendError {
        kind: SendErrorKind::Permanent,
        message,
    }
}

fn bounded_retry_after(head: &str) -> Duration {
    let Some(value) = response_header(head, "retry-after") else {
        return Duration::ZERO;
    };
    let Ok(seconds) = value.parse::<u64>() else {
        return Duration::ZERO;
    };
    Duration::from_secs(seconds).min(MAX_RETRY_AFTER_DELAY)
}

fn response_header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn new_ingest_run_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

fn parse_endpoint(endpoint: &str) -> Result<Endpoint, String> {
    validate_request_target_value("endpoint", endpoint)?;
    let rest = endpoint
        .strip_prefix("http://")
        .ok_or_else(|| "only http:// ingest endpoints are supported in this stage".to_string())?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (
            host.to_string(),
            port.parse::<u16>()
                .map_err(|_| "endpoint port must be numeric".to_string())?,
        ),
        None => (authority.to_string(), 80),
    };
    if host.is_empty() {
        return Err("endpoint host cannot be empty".to_string());
    }
    Ok(Endpoint {
        host,
        port,
        base_path: format!("/{}", path.trim_matches('/'))
            .trim_end_matches('/')
            .to_string(),
    })
}

fn redact(message: &str, api_key: &str) -> String {
    if api_key.is_empty() {
        redact_redirect_locations(message)
    } else {
        redact_redirect_locations(&message.replace(api_key, "[REDACTED]"))
    }
}

fn redact_redirect_locations(message: &str) -> String {
    message
        .lines()
        .map(|line| {
            if line.to_ascii_lowercase().starts_with("location:") {
                "Location: [REDACTED]".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
