//! Frozen internal crawler wire contract.
//!
//! PBV4 owns these DTOs and OpenAPI annotations without mounting the routes.
//! PBV5 mounts the same contract after durable run truth, bounded execution,
//! cancellation, and atomic publication have been composed together.

use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::{Uuid, Variant};

use crate::error_response::HandlerError;
use crate::handlers::AppState;
use flapjack::crawler::{
    CrawlerCancellation, CrawlerCanonicalField as RuntimeCanonicalField, CrawlerRuntime,
    CrawlerRuntimeErrorCode, CrawlerRuntimeLimits, CrawlerRuntimeOutcome, CrawlerRuntimeRequest,
    CrawlerSelectedField as RuntimeSelectedField, CrawlerTransformSpec,
    IndexCrawlerPublicationHandoff, MAX_CRAWLER_RUN_DURATION,
};
use flapjack::index::manager::publication::{
    ContentDigest, CrawlerRunAcknowledgeError, CrawlerRunCancelDispositionEvidence,
    CrawlerRunCountersEvidence, CrawlerRunErrorCodeEvidence, CrawlerRunExecutionClaim,
    CrawlerRunExecutionClaimDisposition, CrawlerRunStartDisposition, CrawlerRunStartError,
    CrawlerRunStore, CrawlerTerminalOutcome, PublicationTarget, PublicationTombstone,
};

pub const CRAWLER_WIRE_SCHEMA_VERSION: u8 = 1;

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u8::deserialize(deserializer)?;
    if version == CRAWLER_WIRE_SCHEMA_VERSION {
        Ok(version)
    } else {
        Err(serde::de::Error::custom(format!(
            "unsupported crawler wire schema_version {version}"
        )))
    }
}

/// Canonical crawler run identity. Every wire position uses this type so a
/// non-UUIDv7 identity cannot cross the handler boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct CrawlerRunId(Uuid);

impl utoipa::PartialSchema for CrawlerRunId {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .format(Some(utoipa::openapi::schema::SchemaFormat::KnownFormat(
                utoipa::openapi::schema::KnownFormat::Uuid,
            )))
            .pattern(Some(
                "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-7[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$",
            ))
            .description(Some("Caller-generated UUIDv7 crawler run identity"))
            .into()
    }
}

impl ToSchema for CrawlerRunId {}

impl CrawlerRunId {
    pub const REPLAY_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

    pub fn as_uuid(self) -> Uuid {
        self.0
    }

    /// Validate effect/replay admission against an injected clock. UUIDv7
    /// stores Unix epoch milliseconds in its most-significant 48 bits.
    pub fn validate_admission_at(self, now: SystemTime) -> Result<(), CrawlerRunIdAdmissionError> {
        let timestamp_ms = (self.0.as_u128() >> 80) as u64;
        let generated_at = UNIX_EPOCH + Duration::from_millis(timestamp_ms);
        let age = now
            .duration_since(generated_at)
            .map_err(|_| CrawlerRunIdAdmissionError::Future)?;
        if age > Self::REPLAY_WINDOW {
            return Err(CrawlerRunIdAdmissionError::Expired);
        }
        Ok(())
    }
}

impl TryFrom<Uuid> for CrawlerRunId {
    type Error = CrawlerRunIdParseError;

    fn try_from(run_id: Uuid) -> Result<Self, Self::Error> {
        if run_id.get_version_num() == 7 && run_id.get_variant() == Variant::RFC4122 {
            Ok(Self(run_id))
        } else {
            Err(CrawlerRunIdParseError)
        }
    }
}

impl From<CrawlerRunId> for Uuid {
    fn from(run_id: CrawlerRunId) -> Self {
        run_id.0
    }
}

impl Display for CrawlerRunId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<'de> Deserialize<'de> for CrawlerRunId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Uuid::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrawlerRunIdParseError;

impl Display for CrawlerRunIdParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("crawler run_id must be a caller-generated RFC 9562 UUIDv7")
    }
}

impl std::error::Error for CrawlerRunIdParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrawlerRunIdAdmissionError {
    Future,
    Expired,
}

impl Display for CrawlerRunIdAdmissionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Future => formatter.write_str("crawler run_id timestamp is in the future"),
            Self::Expired => formatter.write_str("crawler run_id is outside the replay window"),
        }
    }
}

impl std::error::Error for CrawlerRunIdAdmissionError {}

pub(crate) fn is_pbv4_crawler_path(path: &str) -> bool {
    if path == "/internal/crawler/runs" {
        return true;
    }
    let Some(suffix) = path.strip_prefix("/internal/crawler/runs/") else {
        return false;
    };
    let mut parts = suffix.split('/');
    let run_id = parts.next().filter(|part| !part.is_empty());
    let operation = parts.next();
    run_id.is_some()
        && parts.next().is_none()
        && matches!(operation, None | Some("cancel") | Some("ack"))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrawlerRunStartRequest {
    #[serde(deserialize_with = "deserialize_schema_version")]
    #[schema(minimum = 1, maximum = 1)]
    pub schema_version: u8,
    /// Caller-generated UUIDv7. Its embedded timestamp makes the seven-day
    /// replay admission window enforceable after acknowledged truth is pruned.
    pub run_id: CrawlerRunId,
    pub destination_index: String,
    pub start_url: String,
    pub limits: CrawlerRunLimits,
    pub transform: CrawlerTransform,
    pub max_run_duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrawlerRunLimits {
    pub max_depth: u16,
    pub max_pages: u32,
    pub max_decoded_body_bytes: u64,
    pub max_record_bytes: u64,
    pub max_records: u32,
    pub max_concurrency: u16,
}

/// One selected canonical extraction field. Omission is the PBV4 drop
/// operation; changing `output` is its rename operation.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrawlerSelectedField {
    pub source: CrawlerCanonicalField,
    pub output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CrawlerCanonicalField {
    Url,
    Title,
    Metadata,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrawlerTransform {
    pub fields: Vec<CrawlerSelectedField>,
    pub object_id: CrawlerObjectIdDerivation,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrawlerObjectIdDerivation {
    pub source: CrawlerCanonicalField,
    pub algorithm: CrawlerObjectIdAlgorithm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CrawlerObjectIdAlgorithm {
    Sha256,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrawlerRunCounters {
    pub fetched: u32,
    pub discovered: u32,
    pub transformed: u32,
    pub published: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrawlerPublicationFact {
    pub destination_index: String,
    pub task_id: i64,
    pub transaction_id: String,
    pub generation: String,
    pub digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CrawlerRunErrorCode {
    TargetRejected,
    DnsResolutionFailed,
    RedirectRejected,
    ContentTypeRejected,
    FetchTimeout,
    BodyLimitExceeded,
    CrawlLimitExceeded,
    TransformInvalid,
    TransformFailed,
    PublicationFailed,
    DeadlineExceeded,
    WorkerLost,
    InternalFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CrawlerRunOutcome {
    Running {
        #[serde(deserialize_with = "deserialize_schema_version")]
        #[schema(minimum = 1, maximum = 1)]
        schema_version: u8,
        run_id: CrawlerRunId,
        counters: CrawlerRunCounters,
    },
    Succeeded {
        #[serde(deserialize_with = "deserialize_schema_version")]
        #[schema(minimum = 1, maximum = 1)]
        schema_version: u8,
        run_id: CrawlerRunId,
        counters: CrawlerRunCounters,
        duration_ms: u64,
        publication: CrawlerPublicationFact,
    },
    Canceled {
        #[serde(deserialize_with = "deserialize_schema_version")]
        #[schema(minimum = 1, maximum = 1)]
        schema_version: u8,
        run_id: CrawlerRunId,
        counters: CrawlerRunCounters,
        duration_ms: u64,
    },
    Failed {
        #[serde(deserialize_with = "deserialize_schema_version")]
        #[schema(minimum = 1, maximum = 1)]
        schema_version: u8,
        run_id: CrawlerRunId,
        counters: CrawlerRunCounters,
        duration_ms: u64,
        error_code: CrawlerRunErrorCode,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CrawlerCancelDisposition {
    CancelRequested,
    AlreadyRequested,
    AlreadyTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrawlerRunCancelResponse {
    #[serde(deserialize_with = "deserialize_schema_version")]
    #[schema(minimum = 1, maximum = 1)]
    pub schema_version: u8,
    pub run_id: CrawlerRunId,
    pub disposition: CrawlerCancelDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CrawlerRunAckResponse {
    #[serde(deserialize_with = "deserialize_schema_version")]
    #[schema(minimum = 1, maximum = 1)]
    pub schema_version: u8,
    pub run_id: CrawlerRunId,
    pub acknowledged: bool,
}

fn runtime_unavailable() -> HandlerError {
    HandlerError::coded(
        StatusCode::SERVICE_UNAVAILABLE,
        "crawler_runtime_unavailable",
        "Crawler runtime is unavailable",
    )
}

fn invalid_request() -> HandlerError {
    HandlerError::coded(
        StatusCode::BAD_REQUEST,
        "crawler_request_invalid",
        "Invalid crawler request",
    )
}

fn run_not_found() -> HandlerError {
    HandlerError::coded(
        StatusCode::NOT_FOUND,
        "crawler_run_not_found",
        "Crawler run not found",
    )
}

fn store_for(state: &AppState) -> CrawlerRunStore {
    CrawlerRunStore::new(&state.manager.base_path)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn request_digest(request: &CrawlerRunStartRequest) -> Result<ContentDigest, HandlerError> {
    let canonical = serde_json::to_vec(request).map_err(|_| invalid_request())?;
    ContentDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(canonical))))
        .map_err(|_| invalid_request())
}

fn runtime_field(field: CrawlerCanonicalField) -> RuntimeCanonicalField {
    match field {
        CrawlerCanonicalField::Url => RuntimeCanonicalField::Url,
        CrawlerCanonicalField::Title => RuntimeCanonicalField::Title,
        CrawlerCanonicalField::Metadata => RuntimeCanonicalField::Metadata,
        CrawlerCanonicalField::Text => RuntimeCanonicalField::Text,
    }
}

fn runtime_request(request: &CrawlerRunStartRequest) -> CrawlerRuntimeRequest {
    CrawlerRuntimeRequest {
        start_url: request.start_url.clone(),
        limits: CrawlerRuntimeLimits {
            max_depth: request.limits.max_depth,
            max_pages: request.limits.max_pages,
            max_decoded_body_bytes: request.limits.max_decoded_body_bytes,
            max_record_bytes: request.limits.max_record_bytes,
            max_records: request.limits.max_records,
            max_concurrency: request.limits.max_concurrency,
        },
        transform: CrawlerTransformSpec {
            fields: request
                .transform
                .fields
                .iter()
                .map(|field| RuntimeSelectedField {
                    source: runtime_field(field.source),
                    output: field.output.clone(),
                })
                .collect(),
            object_id_source: runtime_field(request.transform.object_id.source),
        },
        max_run_duration: Duration::from_millis(request.max_run_duration_ms),
    }
}

fn counters(evidence: CrawlerRunCountersEvidence) -> CrawlerRunCounters {
    CrawlerRunCounters {
        fetched: evidence.fetched,
        discovered: evidence.discovered,
        transformed: evidence.transformed,
        published: evidence.published,
    }
}

fn error_code(evidence: CrawlerRunErrorCodeEvidence) -> CrawlerRunErrorCode {
    match evidence {
        CrawlerRunErrorCodeEvidence::TargetRejected => CrawlerRunErrorCode::TargetRejected,
        CrawlerRunErrorCodeEvidence::DnsResolutionFailed => {
            CrawlerRunErrorCode::DnsResolutionFailed
        }
        CrawlerRunErrorCodeEvidence::RedirectRejected => CrawlerRunErrorCode::RedirectRejected,
        CrawlerRunErrorCodeEvidence::ContentTypeRejected => {
            CrawlerRunErrorCode::ContentTypeRejected
        }
        CrawlerRunErrorCodeEvidence::FetchTimeout => CrawlerRunErrorCode::FetchTimeout,
        CrawlerRunErrorCodeEvidence::BodyLimitExceeded => CrawlerRunErrorCode::BodyLimitExceeded,
        CrawlerRunErrorCodeEvidence::CrawlLimitExceeded => CrawlerRunErrorCode::CrawlLimitExceeded,
        CrawlerRunErrorCodeEvidence::TransformInvalid => CrawlerRunErrorCode::TransformInvalid,
        CrawlerRunErrorCodeEvidence::TransformFailed => CrawlerRunErrorCode::TransformFailed,
        CrawlerRunErrorCodeEvidence::PublicationFailed => CrawlerRunErrorCode::PublicationFailed,
        CrawlerRunErrorCodeEvidence::DeadlineExceeded => CrawlerRunErrorCode::DeadlineExceeded,
        CrawlerRunErrorCodeEvidence::WorkerLost => CrawlerRunErrorCode::WorkerLost,
        CrawlerRunErrorCodeEvidence::InternalFailure => CrawlerRunErrorCode::InternalFailure,
    }
}

fn durable_outcome(
    run_id: CrawlerRunId,
    tombstone: PublicationTombstone,
) -> Result<CrawlerRunOutcome, HandlerError> {
    let run = tombstone.crawler_run.ok_or_else(runtime_unavailable)?;
    if run.run_id != run_id.to_string() {
        return Err(runtime_unavailable());
    }
    let Some(terminal) = run.terminal else {
        return Ok(CrawlerRunOutcome::Running {
            schema_version: CRAWLER_WIRE_SCHEMA_VERSION,
            run_id,
            counters: CrawlerRunCounters::default(),
        });
    };
    let terminal_counters = counters(terminal.counters);
    match terminal.outcome {
        CrawlerTerminalOutcome::Succeeded => {
            let publication = terminal.publication.ok_or_else(runtime_unavailable)?;
            Ok(CrawlerRunOutcome::Succeeded {
                schema_version: CRAWLER_WIRE_SCHEMA_VERSION,
                run_id,
                counters: terminal_counters,
                duration_ms: terminal.duration_ms,
                publication: CrawlerPublicationFact {
                    destination_index: publication.destination_index,
                    task_id: publication.task_id,
                    transaction_id: publication.transaction_id.as_str().to_string(),
                    generation: publication.generation.as_str().to_string(),
                    digest: publication.digest.as_str().to_string(),
                },
            })
        }
        CrawlerTerminalOutcome::Canceled => Ok(CrawlerRunOutcome::Canceled {
            schema_version: CRAWLER_WIRE_SCHEMA_VERSION,
            run_id,
            counters: terminal_counters,
            duration_ms: terminal.duration_ms,
        }),
        CrawlerTerminalOutcome::Failed { error_code: code } => Ok(CrawlerRunOutcome::Failed {
            schema_version: CRAWLER_WIRE_SCHEMA_VERSION,
            run_id,
            counters: terminal_counters,
            duration_ms: terminal.duration_ms,
            error_code: error_code(code),
        }),
    }
}

fn runtime_error_code(code: CrawlerRuntimeErrorCode) -> CrawlerRunErrorCodeEvidence {
    match code {
        CrawlerRuntimeErrorCode::TargetRejected => CrawlerRunErrorCodeEvidence::TargetRejected,
        CrawlerRuntimeErrorCode::DnsResolutionFailed => {
            CrawlerRunErrorCodeEvidence::DnsResolutionFailed
        }
        CrawlerRuntimeErrorCode::RedirectRejected => CrawlerRunErrorCodeEvidence::RedirectRejected,
        CrawlerRuntimeErrorCode::ContentTypeRejected => {
            CrawlerRunErrorCodeEvidence::ContentTypeRejected
        }
        CrawlerRuntimeErrorCode::FetchTimeout => CrawlerRunErrorCodeEvidence::FetchTimeout,
        CrawlerRuntimeErrorCode::BodyLimitExceeded => {
            CrawlerRunErrorCodeEvidence::BodyLimitExceeded
        }
        CrawlerRuntimeErrorCode::CrawlLimitExceeded => {
            CrawlerRunErrorCodeEvidence::CrawlLimitExceeded
        }
        CrawlerRuntimeErrorCode::TransformInvalid => CrawlerRunErrorCodeEvidence::TransformInvalid,
        CrawlerRuntimeErrorCode::TransformFailed => CrawlerRunErrorCodeEvidence::TransformFailed,
        CrawlerRuntimeErrorCode::PublicationFailed => {
            CrawlerRunErrorCodeEvidence::PublicationFailed
        }
        CrawlerRuntimeErrorCode::DeadlineExceeded => CrawlerRunErrorCodeEvidence::DeadlineExceeded,
        CrawlerRuntimeErrorCode::InternalFailure => CrawlerRunErrorCodeEvidence::InternalFailure,
    }
}

fn runtime_counters(
    counters: flapjack::crawler::CrawlerRuntimeCounters,
) -> CrawlerRunCountersEvidence {
    CrawlerRunCountersEvidence {
        fetched: counters.fetched,
        discovered: counters.discovered,
        transformed: counters.transformed,
        published: 0,
    }
}

fn persist_runtime_terminal(
    store: &CrawlerRunStore,
    run_id: CrawlerRunId,
    started_at_unix_ms: u64,
    outcome: CrawlerRuntimeOutcome<()>,
) {
    let (terminal, counters) = match outcome {
        CrawlerRuntimeOutcome::Handoff { .. } => return,
        CrawlerRuntimeOutcome::Canceled { counters, .. } => {
            (CrawlerTerminalOutcome::Canceled, runtime_counters(counters))
        }
        CrawlerRuntimeOutcome::Failed { code, counters, .. } => (
            CrawlerTerminalOutcome::Failed {
                error_code: runtime_error_code(code),
            },
            runtime_counters(counters),
        ),
    };
    let duration_ms = now_unix_ms().saturating_sub(started_at_unix_ms);
    if store
        .finish_runtime_without_publication(
            &run_id.to_string(),
            terminal,
            counters,
            duration_ms,
            now_unix_ms(),
        )
        .is_err()
    {
        tracing::error!(run_id = %run_id, "crawler terminal persistence failed");
    }
}

fn terminalize_abandoned_cancellation(
    store: &CrawlerRunStore,
    run_id: CrawlerRunId,
) -> Result<(), HandlerError> {
    let claim = match store
        .claim_canceled_terminalization(&run_id.to_string())
        .map_err(|_| runtime_unavailable())?
    {
        CrawlerRunExecutionClaimDisposition::Acquired(claim) => claim,
        CrawlerRunExecutionClaimDisposition::AlreadyExecuting
        | CrawlerRunExecutionClaimDisposition::NotRunnable => return Ok(()),
    };
    let started_at_unix_ms = store
        .load(&run_id.to_string())
        .map_err(|_| runtime_unavailable())?
        .and_then(|tombstone| tombstone.crawler_run)
        .and_then(|run| run.started_at_unix_ms)
        .ok_or_else(runtime_unavailable)?;
    let terminal_at_unix_ms = now_unix_ms();
    store
        .finish_without_publication(
            &run_id.to_string(),
            CrawlerTerminalOutcome::Canceled,
            CrawlerRunCountersEvidence::default(),
            terminal_at_unix_ms.saturating_sub(started_at_unix_ms),
            terminal_at_unix_ms,
        )
        .map_err(|_| runtime_unavailable())?;
    drop(claim);
    Ok(())
}

fn terminalize_expired_execution(
    store: &CrawlerRunStore,
    run_id: CrawlerRunId,
) -> Result<(), HandlerError> {
    let terminal_at_unix_ms = now_unix_ms();
    let legacy_max_duration_ms =
        u64::try_from(MAX_CRAWLER_RUN_DURATION.as_millis()).unwrap_or(u64::MAX);
    store
        .terminalize_expired_if_unowned(
            &run_id.to_string(),
            terminal_at_unix_ms,
            legacy_max_duration_ms,
        )
        .map_err(|_| runtime_unavailable())?;
    Ok(())
}

fn reconcile_unowned_run(
    store: &CrawlerRunStore,
    run_id: CrawlerRunId,
    retained: PublicationTombstone,
) -> Result<PublicationTombstone, HandlerError> {
    if retained
        .crawler_run
        .as_ref()
        .is_some_and(|run| run.cancel_requested_at_unix_ms.is_some() && run.terminal.is_none())
    {
        terminalize_abandoned_cancellation(store, run_id)?;
    }
    terminalize_expired_execution(store, run_id)?;
    store
        .load(&run_id.to_string())
        .map_err(|_| runtime_unavailable())?
        .ok_or_else(run_not_found)
}

struct CrawlerExecution {
    mutation_permit: crate::pause_registry::MutationPermit,
    claim: CrawlerRunExecutionClaim,
    digest: ContentDigest,
    request: CrawlerRunStartRequest,
    runtime_request: CrawlerRuntimeRequest,
    started_at_unix_ms: u64,
}

fn spawn_execution(state: Arc<AppState>, store: CrawlerRunStore, execution: CrawlerExecution) {
    let CrawlerExecution {
        mutation_permit,
        claim,
        digest,
        request,
        runtime_request,
        started_at_unix_ms,
    } = execution;
    tokio::spawn(async move {
        let _mutation_permit = mutation_permit;
        let _claim = claim;
        let run_id = request.run_id;
        if runtime_request.max_run_duration.is_zero() {
            persist_runtime_terminal(
                &store,
                run_id,
                started_at_unix_ms,
                CrawlerRuntimeOutcome::Failed {
                    code: CrawlerRuntimeErrorCode::DeadlineExceeded,
                    counters: Default::default(),
                    duration: Duration::ZERO,
                },
            );
            return;
        }
        let handoff = match IndexCrawlerPublicationHandoff::new(
            Arc::clone(&state.manager),
            store.clone(),
            request.destination_index,
            run_id.to_string(),
            digest,
            started_at_unix_ms,
        ) {
            Ok(handoff) => handoff,
            Err(_) => {
                persist_runtime_terminal(
                    &store,
                    run_id,
                    started_at_unix_ms,
                    CrawlerRuntimeOutcome::Failed {
                        code: CrawlerRuntimeErrorCode::InternalFailure,
                        counters: Default::default(),
                        duration: Duration::ZERO,
                    },
                );
                return;
            }
        };
        let cancellation_store = store.clone();
        let cancellation_run_id = run_id.to_string();
        let cancellation = CrawlerCancellation::with_durable_probe(move || {
            cancellation_store
                .cancellation_requested(&cancellation_run_id)
                .unwrap_or(true)
        });
        let outcome = CrawlerRuntime::default()
            .execute(runtime_request, cancellation, &handoff)
            .await;
        persist_runtime_terminal(&store, run_id, started_at_unix_ms, outcome);
    });
}

#[utoipa::path(
    post,
    path = "/internal/crawler/runs",
    tag = "internal-crawler",
    request_body = CrawlerRunStartRequest,
    responses(
        (status = 200, description = "Current durable run state or terminal outcome", body = CrawlerRunOutcome),
        (status = 400, description = "Invalid or out-of-policy request"),
        (status = 404, description = "Missing or invalid application ID and missing, invalid, or non-admin API keys are concealed as not found"),
        (status = 409, description = "run_id replay disagrees with retained truth"),
        (status = 429, description = "Unacknowledged durable run capacity is exhausted"),
        (status = 503, description = "Crawler runtime is not enabled")
    ),
    security(("application_id" = [], "api_key" = []))
)]
pub async fn start_crawler_run(
    State(state): State<Arc<AppState>>,
    Extension(mutation_permit): Extension<crate::pause_registry::MutationPermit>,
    Json(request): Json<CrawlerRunStartRequest>,
) -> Result<Json<CrawlerRunOutcome>, HandlerError> {
    let run_id_is_admissible = request
        .run_id
        .validate_admission_at(SystemTime::now())
        .is_ok();
    let destination_is_admissible =
        PublicationTarget::new(request.destination_index.clone()).is_ok();
    let mut runtime_request = runtime_request(&request);
    let runtime_is_admissible = runtime_request.validate_for_start().is_ok();
    let allow_new = run_id_is_admissible && destination_is_admissible && runtime_is_admissible;
    let digest = request_digest(&request)?;
    let run_id = request.run_id;
    let admitted_at_unix_ms = now_unix_ms();
    let deadline_at_unix_ms = admitted_at_unix_ms.saturating_add(request.max_run_duration_ms);
    let store = store_for(&state);
    let disposition = store
        .start_classified_with_deadline_admission(
            &run_id.to_string(),
            digest.clone(),
            admitted_at_unix_ms,
            Some(deadline_at_unix_ms),
            allow_new,
        )
        .map_err(|error| match error {
            CrawlerRunStartError::AdmissionRejected => invalid_request(),
            CrawlerRunStartError::Conflict => HandlerError::coded(
                StatusCode::CONFLICT,
                "crawler_run_conflict",
                "Crawler run conflicts with retained truth",
            ),
            CrawlerRunStartError::Capacity => HandlerError::coded(
                StatusCode::TOO_MANY_REQUESTS,
                "crawler_run_capacity_exhausted",
                "Crawler run capacity is exhausted",
            ),
            CrawlerRunStartError::Internal(_) => runtime_unavailable(),
        })?;
    let (retained, replayed) = match disposition {
        CrawlerRunStartDisposition::Started(retained)
        | CrawlerRunStartDisposition::Canceled(retained) => (retained, false),
        CrawlerRunStartDisposition::Replay(retained) => (retained, true),
    };
    let retained = if replayed
        && retained
            .crawler_run
            .as_ref()
            .is_some_and(|run| run.terminal.is_none())
    {
        reconcile_unowned_run(&store, run_id, retained)?
    } else {
        retained
    };
    if retained
        .crawler_run
        .as_ref()
        .is_some_and(|run| run.terminal.is_none() && run.cancel_requested_at_unix_ms.is_none())
    {
        let durable_started_at_unix_ms = retained
            .crawler_run
            .as_ref()
            .and_then(|run| run.started_at_unix_ms)
            .ok_or_else(runtime_unavailable)?;
        runtime_request.apply_elapsed_budget(durable_started_at_unix_ms, now_unix_ms());
        match store
            .claim_execution(&run_id.to_string())
            .map_err(|_| runtime_unavailable())?
        {
            CrawlerRunExecutionClaimDisposition::Acquired(claim) => spawn_execution(
                state,
                store,
                CrawlerExecution {
                    mutation_permit,
                    claim,
                    digest,
                    request,
                    runtime_request,
                    started_at_unix_ms: durable_started_at_unix_ms,
                },
            ),
            CrawlerRunExecutionClaimDisposition::AlreadyExecuting
            | CrawlerRunExecutionClaimDisposition::NotRunnable => {}
        }
    }
    Ok(Json(durable_outcome(run_id, retained)?))
}

#[utoipa::path(
    post,
    path = "/internal/crawler/runs/{run_id}/cancel",
    tag = "internal-crawler",
    params(("run_id" = CrawlerRunId, Path, description = "Durable UUIDv7 crawler run identity")),
    responses(
        (status = 202, description = "Durable cancellation disposition", body = CrawlerRunCancelResponse),
        (status = 400, description = "Invalid run_id"),
        (status = 404, description = "Authentication failures are concealed as not found"),
        (status = 503, description = "Crawler runtime is not enabled")
    ),
    security(("application_id" = [], "api_key" = []))
)]
pub async fn cancel_crawler_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<CrawlerRunId>,
) -> Result<(StatusCode, Json<CrawlerRunCancelResponse>), HandlerError> {
    let store = store_for(&state);
    if store
        .load(&run_id.to_string())
        .map_err(|_| runtime_unavailable())?
        .is_none()
    {
        run_id
            .validate_admission_at(SystemTime::now())
            .map_err(|_| invalid_request())?;
    }
    let disposition = store
        .request_cancel_with_disposition(&run_id.to_string(), now_unix_ms())
        .map_err(|_| runtime_unavailable())?;
    if !matches!(
        disposition,
        CrawlerRunCancelDispositionEvidence::AlreadyTerminal
    ) {
        terminalize_abandoned_cancellation(&store, run_id)?;
    }
    let disposition = match disposition {
        CrawlerRunCancelDispositionEvidence::CancelRequested => {
            CrawlerCancelDisposition::CancelRequested
        }
        CrawlerRunCancelDispositionEvidence::AlreadyRequested => {
            CrawlerCancelDisposition::AlreadyRequested
        }
        CrawlerRunCancelDispositionEvidence::AlreadyTerminal => {
            CrawlerCancelDisposition::AlreadyTerminal
        }
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(CrawlerRunCancelResponse {
            schema_version: CRAWLER_WIRE_SCHEMA_VERSION,
            run_id,
            disposition,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/internal/crawler/runs/{run_id}",
    tag = "internal-crawler",
    params(("run_id" = CrawlerRunId, Path, description = "Durable UUIDv7 crawler run identity")),
    responses(
        (status = 200, description = "Current durable run state or terminal outcome", body = CrawlerRunOutcome),
        (status = 400, description = "Invalid run_id"),
        (status = 404, description = "No durable run truth is retained for run_id, or authentication failed and was concealed"),
        (status = 503, description = "Crawler runtime is not enabled")
    ),
    security(("application_id" = [], "api_key" = []))
)]
pub async fn get_crawler_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<CrawlerRunId>,
) -> Result<Json<CrawlerRunOutcome>, HandlerError> {
    let store = store_for(&state);
    let initial = store
        .load(&run_id.to_string())
        .map_err(|_| runtime_unavailable())?
        .ok_or_else(run_not_found)?;
    let retained = reconcile_unowned_run(&store, run_id, initial)?;
    Ok(Json(durable_outcome(run_id, retained)?))
}

#[utoipa::path(
    post,
    path = "/internal/crawler/runs/{run_id}/ack",
    tag = "internal-crawler",
    params(("run_id" = CrawlerRunId, Path, description = "Durable UUIDv7 crawler run identity")),
    responses(
        (status = 200, description = "Durable idempotent acknowledgement", body = CrawlerRunAckResponse),
        (status = 400, description = "Invalid run_id"),
        (status = 404, description = "No terminal run truth is retained for run_id, or authentication failed and was concealed"),
        (status = 409, description = "Run is not terminal"),
        (status = 503, description = "Crawler runtime is not enabled")
    ),
    security(("application_id" = [], "api_key" = []))
)]
pub async fn ack_crawler_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<CrawlerRunId>,
) -> Result<Json<CrawlerRunAckResponse>, HandlerError> {
    store_for(&state)
        .acknowledge_classified(&run_id.to_string(), now_unix_ms())
        .map_err(|error| match error {
            CrawlerRunAcknowledgeError::NotFound => run_not_found(),
            CrawlerRunAcknowledgeError::NotTerminal => HandlerError::coded(
                StatusCode::CONFLICT,
                "crawler_run_not_terminal",
                "Crawler run is not terminal",
            ),
            CrawlerRunAcknowledgeError::Internal(_) => runtime_unavailable(),
        })?;
    Ok(Json(CrawlerRunAckResponse {
        schema_version: CRAWLER_WIRE_SCHEMA_VERSION,
        run_id,
        acknowledged: true,
    }))
}
