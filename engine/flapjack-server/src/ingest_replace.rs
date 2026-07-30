use super::{
    duration_millis, input_ingest_error, process_source, Endpoint, FailureClassification,
    IngestAction, IngestArgs, IngestError, IngestFailure, IngestReport, OperationSink,
    RecordOperation, MAX_HTTP_RESPONSE_BYTES, MAX_RETRY_AFTER_DELAY, RETRYABLE_STATUSES,
    RETRY_ATTEMPT_LIMIT,
};
use serde::Deserialize;
use serde_json::Value;
use std::fs::File;
use std::io::{self, Read, Write};
use std::time::Duration;

const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STATUS_POLL_ATTEMPT_LIMIT: usize = 2_400;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

struct ReplaceSpoolSink {
    payload: tempfile::NamedTempFile,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BulkReplaceReceipt {
    #[serde(rename = "jobID")]
    job_id: String,
    topology: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BulkReplaceStatus {
    disposition: String,
    topology: Option<String>,
    objects_imported: Option<ImportedCount>,
}

#[derive(Deserialize)]
struct ImportedCount {
    imported: usize,
}

struct BulkReplaceClient<'a> {
    endpoint: Endpoint,
    index: &'a str,
    application_id: &'a str,
    api_key: &'a str,
    client: reqwest::blocking::Client,
}

struct BoundedHttpResponse {
    status: u16,
    retry_after: Duration,
    body: Vec<u8>,
}

enum PayloadSendError {
    Local(IngestError),
    Http(reqwest::Error),
}

pub(super) fn run(
    args: &IngestArgs,
    endpoint: Endpoint,
    api_key: String,
) -> Result<IngestReport, IngestFailure> {
    let mut report = IngestReport::default();
    let mut payload = ReplaceSpoolSink::new().map_err(|error| IngestFailure {
        message: format!("failed to create replacement payload spool: {error}"),
        api_key: api_key.clone(),
        classification: FailureClassification::LocalCleanup,
        report: Box::default(),
    })?;
    if let Err(error) = process_source(args, &mut payload, &mut report) {
        return Err(failure(error, api_key, report));
    }
    if let Err(error) = payload.finish() {
        return Err(IngestFailure {
            message: format!("failed to finalize replacement payload spool: {error}"),
            api_key,
            classification: FailureClassification::LocalCleanup,
            report: Box::new(report),
        });
    }

    let client = match BulkReplaceClient::new(endpoint, &args.index, &args.application_id, &api_key)
    {
        Ok(client) => client,
        Err(message) => {
            return Err(IngestFailure {
                message,
                api_key,
                classification: FailureClassification::Config,
                report: Box::new(report),
            });
        }
    };
    if let Err(error) = client.submit_and_wait(&payload, &mut report) {
        return Err(failure(error, api_key, report));
    }
    Ok(report)
}

fn failure(error: IngestError, api_key: String, report: IngestReport) -> IngestFailure {
    IngestFailure {
        message: error.message,
        api_key,
        classification: error.classification,
        report: Box::new(report),
    }
}

impl ReplaceSpoolSink {
    fn new() -> io::Result<Self> {
        Ok(Self {
            payload: tempfile::NamedTempFile::new()?,
        })
    }

    fn finish(&mut self) -> io::Result<()> {
        self.payload.as_file_mut().flush()
    }

    fn reopen(&self) -> io::Result<File> {
        self.payload.reopen()
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.payload.as_file().metadata()?.len())
    }
}

impl OperationSink for ReplaceSpoolSink {
    fn submit_operations(
        &mut self,
        operations: Vec<RecordOperation>,
        report: &mut IngestReport,
    ) -> Result<(), IngestError> {
        reject_delete_actions(&operations)?;
        let operation_count = operations.len();
        for operation in operations {
            serde_json::to_writer(self.payload.as_file_mut(), &Value::Object(operation.body))
                .map_err(|error| {
                    local_cleanup_error(format!("failed to encode replacement payload: {error}"))
                })?;
            self.payload
                .as_file_mut()
                .write_all(b"\n")
                .map_err(|error| {
                    local_cleanup_error(format!("failed to spool replacement payload: {error}"))
                })?;
        }
        report.attempted += operation_count;
        report.queue_high_watermark = report.queue_high_watermark.max(operation_count);
        Ok(())
    }
}

fn reject_delete_actions(operations: &[RecordOperation]) -> Result<(), IngestError> {
    if operations
        .iter()
        .any(|operation| operation.action == IngestAction::Delete)
    {
        return Err(input_ingest_error(
            "replace mode does not accept delete actions; omit absent objects instead".to_string(),
        ));
    }
    Ok(())
}

impl<'a> BulkReplaceClient<'a> {
    fn new(
        endpoint: Endpoint,
        index: &'a str,
        application_id: &'a str,
        api_key: &'a str,
    ) -> Result<Self, String> {
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| format!("failed to configure bulk replacement client: {error}"))?;
        Ok(Self {
            endpoint,
            index,
            application_id,
            api_key,
            client,
        })
    }

    fn submit_and_wait(
        &self,
        payload: &ReplaceSpoolSink,
        report: &mut IngestReport,
    ) -> Result<(), IngestError> {
        let receipt = self
            .submit(payload, report)
            .map_err(|error| preserve_unknown_count(error, report))?;
        self.poll_terminal(&receipt.job_id, report)
            .map_err(|error| preserve_unknown_count(error, report))
    }

    fn submit(
        &self,
        payload: &ReplaceSpoolSink,
        report: &mut IngestReport,
    ) -> Result<BulkReplaceReceipt, IngestError> {
        let mut url = self.url("/1/migrations/bulk-replace")?;
        url.query_pairs_mut().append_pair("indexName", self.index);
        for attempt in 1..=RETRY_ATTEMPT_LIMIT {
            let response = self.send_payload(url.clone(), payload);
            match response {
                Ok(response) => {
                    let response = read_bounded_response(response)?;
                    if response.status == 429 && attempt < RETRY_ATTEMPT_LIMIT {
                        record_retry(report, response.retry_after);
                        continue;
                    }
                    return parse_submit_response(response, report);
                }
                Err(PayloadSendError::Local(error)) => return Err(error),
                Err(PayloadSendError::Http(error))
                    if error.is_connect() && attempt < RETRY_ATTEMPT_LIMIT =>
                {
                    record_retry(report, Duration::ZERO);
                }
                Err(PayloadSendError::Http(error)) if error.is_connect() => {
                    return Err(retry_exhausted_error(format!(
                        "bulk replacement connection attempts exhausted: {error}"
                    )));
                }
                Err(PayloadSendError::Http(error)) => {
                    mark_outcome_unknown(report);
                    return Err(outcome_unknown_error(format!(
                        "bulk replacement submission outcome is unknown: {error}"
                    )));
                }
            }
        }
        Err(retry_exhausted_error(
            "bulk replacement submission attempts exhausted".to_string(),
        ))
    }

    fn send_payload(
        &self,
        url: reqwest::Url,
        payload: &ReplaceSpoolSink,
    ) -> Result<reqwest::blocking::Response, PayloadSendError> {
        let file = payload.reopen().map_err(|error| {
            PayloadSendError::Local(local_cleanup_error(format!(
                "failed to reopen replacement payload spool: {error}"
            )))
        })?;
        let content_length = payload.len().map_err(|error| {
            PayloadSendError::Local(local_cleanup_error(format!(
                "failed to inspect replacement payload spool: {error}"
            )))
        })?;
        self.request(self.client.post(url))
            .header("content-type", "application/x-ndjson")
            .header(reqwest::header::CONTENT_LENGTH, content_length)
            .body(reqwest::blocking::Body::new(file))
            .send()
            .map_err(PayloadSendError::Http)
    }

    fn poll_terminal(&self, job_id: &str, report: &mut IngestReport) -> Result<(), IngestError> {
        let url = self.url(&format!("/1/migrations/bulk-replace/{job_id}"))?;
        for _ in 0..STATUS_POLL_ATTEMPT_LIMIT {
            let response = self.send_status_with_retries(url.clone(), report)?;
            if response.status != 200 {
                mark_outcome_unknown(report);
                return Err(outcome_unknown_error(response_error_message(
                    response.status,
                    &response.body,
                )));
            }
            let status = parse_status(&response.body, report)?;
            match status.disposition.as_str() {
                "running" => std::thread::sleep(STATUS_POLL_INTERVAL),
                "succeeded" => {
                    report.confirmed_committed = confirmed_count(status.objects_imported, report)?;
                    return Ok(());
                }
                "failed" | "cancelled" => {
                    return Err(permanent_http_error(format!(
                        "bulk replacement job reached terminal {} disposition",
                        status.disposition
                    )));
                }
                other => {
                    mark_outcome_unknown(report);
                    return Err(outcome_unknown_error(format!(
                        "bulk replacement server returned unknown disposition {other}"
                    )));
                }
            }
        }
        mark_outcome_unknown(report);
        Err(outcome_unknown_error(
            "bulk replacement status polling exceeded its bounded attempt limit".to_string(),
        ))
    }

    fn send_status_with_retries(
        &self,
        url: reqwest::Url,
        report: &mut IngestReport,
    ) -> Result<BoundedHttpResponse, IngestError> {
        for attempt in 1..=RETRY_ATTEMPT_LIMIT {
            match self.request(self.client.get(url.clone())).send() {
                Ok(response) => {
                    let response = read_bounded_response(response)?;
                    if RETRYABLE_STATUSES.contains(&response.status)
                        && attempt < RETRY_ATTEMPT_LIMIT
                    {
                        record_retry(report, response.retry_after);
                        continue;
                    }
                    return Ok(response);
                }
                Err(error)
                    if attempt < RETRY_ATTEMPT_LIMIT
                        && (error.is_connect() || error.is_timeout()) =>
                {
                    record_retry(report, Duration::ZERO);
                }
                Err(error) => {
                    return Err(outcome_unknown_error(format!(
                        "bulk replacement status request failed: {error}"
                    )));
                }
            }
        }
        Err(outcome_unknown_error(
            "bulk replacement status retries exhausted".to_string(),
        ))
    }

    fn request(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        builder
            .header("x-algolia-application-id", self.application_id)
            .header("x-algolia-api-key", self.api_key)
    }

    fn url(&self, path: &str) -> Result<reqwest::Url, IngestError> {
        reqwest::Url::parse(&format!(
            "http://{}:{}{}{}",
            self.endpoint.host, self.endpoint.port, self.endpoint.base_path, path
        ))
        .map_err(|error| input_ingest_error(format!("invalid bulk replacement URL: {error}")))
    }
}

fn parse_submit_response(
    response: BoundedHttpResponse,
    report: &mut IngestReport,
) -> Result<BulkReplaceReceipt, IngestError> {
    if response.status != 202 {
        let message = response_error_message(response.status, &response.body);
        return Err(match response.status {
            404 | 405 | 501 | 503 => replace_not_supported_error(message),
            429 => retry_exhausted_error(message),
            _ => permanent_http_error(message),
        });
    }
    let receipt: BulkReplaceReceipt = serde_json::from_slice(&response.body).map_err(|error| {
        mark_outcome_unknown(report);
        outcome_unknown_error(format!(
            "bulk replacement server returned an incompatible admission receipt: {error}"
        ))
    })?;
    if receipt.topology != "single_node_only" || receipt.job_id.is_empty() {
        mark_outcome_unknown(report);
        return Err(outcome_unknown_error(
            "bulk replacement server returned an incompatible topology receipt".to_string(),
        ));
    }
    Ok(receipt)
}

fn parse_status(body: &[u8], report: &mut IngestReport) -> Result<BulkReplaceStatus, IngestError> {
    let status: BulkReplaceStatus = serde_json::from_slice(body).map_err(|error| {
        mark_outcome_unknown(report);
        outcome_unknown_error(format!(
            "bulk replacement server returned an incompatible status receipt: {error}"
        ))
    })?;
    if status.topology.as_deref() != Some("single_node_only") {
        mark_outcome_unknown(report);
        return Err(outcome_unknown_error(
            "bulk replacement status omitted the single-node topology contract".to_string(),
        ));
    }
    Ok(status)
}

fn confirmed_count(
    imported: Option<ImportedCount>,
    report: &mut IngestReport,
) -> Result<usize, IngestError> {
    imported.map(|count| count.imported).ok_or_else(|| {
        mark_outcome_unknown(report);
        outcome_unknown_error(
            "successful bulk replacement status omitted the committed object count".to_string(),
        )
    })
}

fn read_bounded_response(
    mut response: reqwest::blocking::Response,
) -> Result<BoundedHttpResponse, IngestError> {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::ZERO)
        .min(MAX_RETRY_AFTER_DELAY);
    let mut body = Vec::new();
    (&mut response)
        .take((MAX_HTTP_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| {
            outcome_unknown_error(format!("failed to read bulk replacement response: {error}"))
        })?;
    if body.len() > MAX_HTTP_RESPONSE_BYTES {
        return Err(outcome_unknown_error(format!(
            "bulk replacement response exceeded {MAX_HTTP_RESPONSE_BYTES} bytes"
        )));
    }
    Ok(BoundedHttpResponse {
        status,
        retry_after,
        body,
    })
}

fn record_retry(report: &mut IngestReport, delay: Duration) {
    report.retries += 1;
    report.last_retry_after_ms = Some(duration_millis(delay));
    std::thread::sleep(delay);
}

fn mark_outcome_unknown(report: &mut IngestReport) {
    report.outcome_unknown = report.attempted;
}

fn preserve_unknown_count(error: IngestError, report: &mut IngestReport) -> IngestError {
    if matches!(error.classification, FailureClassification::OutcomeUnknown) {
        mark_outcome_unknown(report);
    }
    error
}

fn response_error_message(status: u16, body: &[u8]) -> String {
    format!(
        "bulk replacement server returned HTTP {status}: {}",
        String::from_utf8_lossy(body)
    )
}

fn local_cleanup_error(message: String) -> IngestError {
    classified_error(message, FailureClassification::LocalCleanup)
}

fn permanent_http_error(message: String) -> IngestError {
    classified_error(message, FailureClassification::PermanentHttpRejection)
}

fn replace_not_supported_error(message: String) -> IngestError {
    classified_error(message, FailureClassification::ReplaceNotSupported)
}

fn retry_exhausted_error(message: String) -> IngestError {
    classified_error(message, FailureClassification::RetryExhausted)
}

fn outcome_unknown_error(message: String) -> IngestError {
    classified_error(message, FailureClassification::OutcomeUnknown)
}

fn classified_error(message: String, classification: FailureClassification) -> IngestError {
    IngestError {
        message,
        classification,
    }
}
