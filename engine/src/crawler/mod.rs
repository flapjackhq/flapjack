//! Bounded public-HTTPS HTML crawler runtime.
//!
//! This module owns fetch admission, extraction, the closed declarative
//! transform, resource limits, and the final publication handoff. Durable run
//! truth and atomic replacement remain in the publication owner.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::join_all;
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::index::manager::publication::{
    ContentDigest, CrawlerPublicationCompletion, CrawlerRunCountersEvidence, CrawlerRunStore,
    PreStagedPublication, PublicationStagingBaseline, PublicationTarget,
};
use crate::security::{
    vet_crawler_url_target, CrawlerTargetAdmissionError, VettedCrawlerUrlTarget,
};
use crate::types::Document;
use crate::IndexManager;

pub const MAX_CRAWLER_DEPTH: u16 = 8;
pub const MAX_CRAWLER_PAGES: u32 = 2_048;
pub const MAX_CRAWLER_DECODED_BODY_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_CRAWLER_RECORD_BYTES: u64 = 256 * 1024;
pub const MAX_CRAWLER_RECORDS: u32 = 4_096;
pub const MAX_CRAWLER_CONCURRENCY: u16 = 16;
pub const MAX_CRAWLER_RUN_DURATION: Duration = Duration::from_secs(30 * 60);
const MAX_CRAWLER_DISCOVERY_QUEUE: usize = 2_048;
const MAX_METADATA_FIELDS: usize = 32;
const MAX_METADATA_KEY_BYTES: usize = 128;
const MAX_METADATA_VALUE_BYTES: usize = 4 * 1024;
const MAX_TITLE_BYTES: usize = 8 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_BODY_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_CRAWLER_DNS_ADMISSIONS: usize = MAX_CRAWLER_CONCURRENCY as usize;
static CRAWLER_DNS_ADMISSION_POOL: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CRAWLER_DNS_ADMISSIONS);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrawlerRuntimeLimits {
    pub max_depth: u16,
    pub max_pages: u32,
    pub max_decoded_body_bytes: u64,
    pub max_record_bytes: u64,
    pub max_records: u32,
    pub max_concurrency: u16,
}

impl CrawlerRuntimeLimits {
    fn validate(self) -> Result<(), CrawlerRuntimeErrorCode> {
        let valid = self.max_depth > 0
            && self.max_depth <= MAX_CRAWLER_DEPTH
            && self.max_pages > 0
            && self.max_pages <= MAX_CRAWLER_PAGES
            && self.max_decoded_body_bytes > 0
            && self.max_decoded_body_bytes <= MAX_CRAWLER_DECODED_BODY_BYTES
            && self.max_record_bytes > 0
            && self.max_record_bytes <= MAX_CRAWLER_RECORD_BYTES
            && self.max_records > 0
            && self.max_records <= MAX_CRAWLER_RECORDS
            && self.max_concurrency > 0
            && self.max_concurrency <= MAX_CRAWLER_CONCURRENCY;
        valid
            .then_some(())
            .ok_or(CrawlerRuntimeErrorCode::CrawlLimitExceeded)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrawlerCanonicalField {
    Url,
    Title,
    Metadata,
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlerSelectedField {
    pub source: CrawlerCanonicalField,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlerTransformSpec {
    pub fields: Vec<CrawlerSelectedField>,
    pub object_id_source: CrawlerCanonicalField,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalCrawlerRecord {
    pub url: String,
    pub title: String,
    pub metadata: BTreeMap<String, String>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct CompiledCrawlerTransform {
    fields: Vec<CrawlerSelectedField>,
    object_id_source: CrawlerCanonicalField,
}

impl CompiledCrawlerTransform {
    pub fn compile(spec: CrawlerTransformSpec) -> Result<Self, CrawlerRuntimeErrorCode> {
        if !valid_transform_field_count(spec.fields.len()) {
            return Err(CrawlerRuntimeErrorCode::TransformInvalid);
        }
        let mut sources = HashSet::new();
        let mut outputs = HashSet::new();
        for field in &spec.fields {
            if !valid_transform_output(&field.output)
                || field.output == "objectID"
                || !sources.insert(field.source)
                || !outputs.insert(field.output.clone())
            {
                return Err(CrawlerRuntimeErrorCode::TransformInvalid);
            }
        }
        Ok(Self {
            fields: spec.fields,
            object_id_source: spec.object_id_source,
        })
    }

    /// The preview and execution paths intentionally share this one pure owner.
    pub fn apply(&self, record: &CanonicalCrawlerRecord) -> Result<Value, CrawlerRuntimeErrorCode> {
        let object_id_bytes = canonical_field_bytes(record, self.object_id_source)?;
        let object_id = hex::encode(Sha256::digest(object_id_bytes));
        let mut output = Map::new();
        output.insert("objectID".to_string(), Value::String(object_id));
        for field in &self.fields {
            output.insert(
                field.output.clone(),
                canonical_field_value(record, field.source),
            );
        }
        Ok(Value::Object(output))
    }

    pub fn preview(
        &self,
        record: &CanonicalCrawlerRecord,
    ) -> Result<Value, CrawlerRuntimeErrorCode> {
        self.apply(record)
    }
}

fn valid_transform_field_count(count: usize) -> bool {
    (1..=4).contains(&count)
}

fn valid_transform_output(output: &str) -> bool {
    let bytes = output.as_bytes();
    let Some((&first, rest)) = bytes.split_first() else {
        return false;
    };
    bytes.len() <= 64
        && (first.is_ascii_alphabetic() || first == b'_')
        && rest
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.' | b'-'))
}

fn canonical_field_value(record: &CanonicalCrawlerRecord, field: CrawlerCanonicalField) -> Value {
    match field {
        CrawlerCanonicalField::Url => Value::String(record.url.clone()),
        CrawlerCanonicalField::Title => Value::String(record.title.clone()),
        CrawlerCanonicalField::Metadata => Value::Object(
            record
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                .collect(),
        ),
        CrawlerCanonicalField::Text => Value::String(record.text.clone()),
    }
}

fn canonical_field_bytes(
    record: &CanonicalCrawlerRecord,
    field: CrawlerCanonicalField,
) -> Result<Vec<u8>, CrawlerRuntimeErrorCode> {
    match field {
        CrawlerCanonicalField::Metadata => serde_json::to_vec(&record.metadata)
            .map_err(|_| CrawlerRuntimeErrorCode::TransformFailed),
        CrawlerCanonicalField::Url => Ok(record.url.as_bytes().to_vec()),
        CrawlerCanonicalField::Title => Ok(record.title.as_bytes().to_vec()),
        CrawlerCanonicalField::Text => Ok(record.text.as_bytes().to_vec()),
    }
}

#[derive(Debug, Clone)]
pub struct CrawlerRuntimeRequest {
    pub start_url: String,
    pub limits: CrawlerRuntimeLimits,
    pub transform: CrawlerTransformSpec,
    pub max_run_duration: Duration,
}

impl CrawlerRuntimeRequest {
    /// Validate all pure, server-owned admission policy before durable start.
    /// DNS and transport admission intentionally remain asynchronous runtime
    /// work so HTTP start can return Running inside the proxy boundary.
    pub fn validate_for_start(&self) -> Result<(), CrawlerRuntimeErrorCode> {
        validate_request(self)?;
        CompiledCrawlerTransform::compile(self.transform.clone()).map(|_| ())
    }

    /// Carry the original durable run budget across process loss. Wall-clock
    /// elapsed time is the only restart-stable evidence; within one process the
    /// execution guard remains monotonic.
    pub fn apply_elapsed_budget(&mut self, started_at_unix_ms: u64, now_unix_ms: u64) {
        let elapsed = Duration::from_millis(now_unix_ms.saturating_sub(started_at_unix_ms));
        self.max_run_duration = self.max_run_duration.saturating_sub(elapsed);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CrawlerRuntimeCounters {
    pub fetched: u32,
    pub discovered: u32,
    pub transformed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrawlerRuntimeErrorCode {
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
    InternalFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlerPublicationBatch {
    pub records: Vec<Value>,
}

pub type CrawlerBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Goal 1 implements this seam with the atomic replacement owner and must call
/// `guard.check()` again inside its publication exclusion boundary.
pub trait CrawlerPublicationHandoff: Send + Sync {
    type Receipt: Send;

    fn handoff<'a>(
        &'a self,
        batch: CrawlerPublicationBatch,
        counters: CrawlerRuntimeCounters,
        guard: &'a CrawlerExecutionGuard,
    ) -> CrawlerBoxFuture<'a, Result<Self::Receipt, ()>>;
}

/// Thin composition of the crawler batch with the existing staged replacement
/// and crawler run-store owners. It allocates no independent publication state.
pub struct IndexCrawlerPublicationHandoff {
    manager: Arc<IndexManager>,
    store: CrawlerRunStore,
    destination_index: String,
    run_id: String,
    request_digest: ContentDigest,
    staging_baseline: PublicationStagingBaseline,
    started_at_unix_ms: u64,
}

impl IndexCrawlerPublicationHandoff {
    pub fn new(
        manager: Arc<IndexManager>,
        store: CrawlerRunStore,
        destination_index: String,
        run_id: String,
        request_digest: ContentDigest,
        started_at_unix_ms: u64,
    ) -> crate::error::Result<Self> {
        PublicationTarget::new(destination_index.clone())?;
        let staging_baseline = manager.capture_replacement_staging_baseline(&destination_index)?;
        Ok(Self {
            manager,
            store,
            destination_index,
            run_id,
            request_digest,
            staging_baseline,
            started_at_unix_ms,
        })
    }

    async fn publish(
        &self,
        batch: CrawlerPublicationBatch,
        runtime_counters: CrawlerRuntimeCounters,
        guard: &CrawlerExecutionGuard,
    ) -> crate::error::Result<()> {
        guard
            .check()
            .map_err(|_| crate::error::FlapjackError::Io("crawler publication fenced".into()))?;
        let target = PublicationTarget::new(self.destination_index.clone())?;
        let publication = PreStagedPublication::prepare(&self.manager.base_path, target)?;
        if let Err(error) = populate_crawler_staging(&publication, &batch.records).await {
            let _ = publication.abort();
            return Err(error);
        }
        let counters = CrawlerRunCountersEvidence {
            fetched: runtime_counters.fetched,
            discovered: runtime_counters.discovered,
            transformed: runtime_counters.transformed,
            published: u32::try_from(batch.records.len()).map_err(|_| {
                crate::error::FlapjackError::Io("crawler publication count overflow".into())
            })?,
        };
        let publication_task = self.manager.reserve_noop_task(&self.destination_index)?;
        let terminal_at_unix_ms = now_unix_ms();
        let completion = CrawlerPublicationCompletion {
            run_id: self.run_id.clone(),
            request_digest: self.request_digest.clone(),
            counters,
            duration_ms: terminal_at_unix_ms.saturating_sub(self.started_at_unix_ms),
            terminal_at_unix_ms,
            task_id: publication_task.numeric_id,
        };
        let publication = publication.with_crawler_completion(
            &self.store,
            completion,
            guard.deadline().into_std(),
        )?;
        self.manager
            .publish_crawler_from_pre_staged_with_reserved_task(
                publication,
                &self.destination_index,
                self.staging_baseline,
                publication_task,
            )
            .await?;
        Ok(())
    }
}

impl CrawlerPublicationHandoff for IndexCrawlerPublicationHandoff {
    type Receipt = ();

    fn handoff<'a>(
        &'a self,
        batch: CrawlerPublicationBatch,
        counters: CrawlerRuntimeCounters,
        guard: &'a CrawlerExecutionGuard,
    ) -> CrawlerBoxFuture<'a, Result<Self::Receipt, ()>> {
        Box::pin(async move {
            self.publish(batch, counters, guard).await.map_err(|_| {
                tracing::error!(run_id = %self.run_id, "crawler publication failed");
            })
        })
    }
}

async fn populate_crawler_staging(
    publication: &PreStagedPublication,
    records: &[Value],
) -> crate::error::Result<()> {
    let staging_parent =
        publication.paths().staging.parent().ok_or_else(|| {
            crate::error::FlapjackError::Io("crawler staging has no parent".into())
        })?;
    let staging_tenant = publication
        .paths()
        .staging
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| crate::error::FlapjackError::Io("crawler staging name is invalid".into()))?;
    let staging_manager = IndexManager::new(staging_parent);
    staging_manager.create_tenant(staging_tenant)?;
    let documents = records
        .iter()
        .map(Document::from_json)
        .collect::<crate::error::Result<Vec<_>>>()?;
    if !documents.is_empty() {
        staging_manager
            .add_documents_durable(staging_tenant, documents)
            .await?;
    }
    staging_manager.drain_all_write_queues().await?;
    staging_manager.unload(&staging_tenant.to_string())?;
    staging_manager.scrub_transient_runtime_artifacts(staging_tenant)?;
    Ok(())
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(duration_millis)
        .unwrap_or_default()
}

#[derive(Debug)]
pub enum CrawlerRuntimeOutcome<T> {
    Handoff {
        receipt: T,
        counters: CrawlerRuntimeCounters,
        duration: Duration,
    },
    Canceled {
        counters: CrawlerRuntimeCounters,
        duration: Duration,
    },
    Failed {
        code: CrawlerRuntimeErrorCode,
        counters: CrawlerRuntimeCounters,
        duration: Duration,
    },
}

#[derive(Debug, Default)]
struct CrawlerCancellationState {
    canceled: AtomicBool,
    changed: tokio::sync::Notify,
}

#[derive(Clone, Default)]
pub struct CrawlerCancellation {
    state: Arc<CrawlerCancellationState>,
    durable_probe: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl std::fmt::Debug for CrawlerCancellation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CrawlerCancellation")
            .field("canceled", &self.state.canceled.load(Ordering::Acquire))
            .field("has_durable_probe", &self.durable_probe.is_some())
            .finish()
    }
}

impl CrawlerCancellation {
    pub fn with_durable_probe(probe: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            state: Arc::new(CrawlerCancellationState::default()),
            durable_probe: Some(Arc::new(probe)),
        }
    }

    pub fn cancel(&self) {
        self.state.canceled.store(true, Ordering::Release);
        self.state.changed.notify_waiters();
    }

    fn is_canceled(&self) -> bool {
        self.state.canceled.load(Ordering::Acquire)
            || self.durable_probe.as_ref().is_some_and(|probe| probe())
    }

    async fn cancelled(&self) {
        loop {
            let changed = self.state.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.is_canceled() {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrawlerGuardFailure {
    Canceled,
    DeadlineExceeded,
}

#[derive(Debug, Clone)]
pub struct CrawlerExecutionGuard {
    cancellation: CrawlerCancellation,
    started_at: tokio::time::Instant,
    deadline: tokio::time::Instant,
}

impl CrawlerExecutionGuard {
    fn new(cancellation: CrawlerCancellation, duration: Duration) -> Self {
        let started_at = tokio::time::Instant::now();
        Self {
            cancellation,
            started_at,
            deadline: started_at + duration,
        }
    }

    pub fn check(&self) -> Result<(), CrawlerGuardFailure> {
        if self.cancellation.is_canceled() {
            return Err(CrawlerGuardFailure::Canceled);
        }
        if tokio::time::Instant::now() >= self.deadline {
            return Err(CrawlerGuardFailure::DeadlineExceeded);
        }
        Ok(())
    }

    pub fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }

    fn elapsed(&self) -> Duration {
        tokio::time::Instant::now().saturating_duration_since(self.started_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlerFetchResponse {
    pub status: u16,
    pub content_type: Option<String>,
    /// Decoded response bytes. Production decoding is performed by reqwest and
    /// the fetcher enforces the cap before appending each decoded chunk.
    pub decoded_body: Vec<u8>,
}

pub trait CrawlerFetcher: Send + Sync {
    fn fetch<'a>(
        &'a self,
        target: &'a VettedCrawlerUrlTarget,
        max_decoded_body_bytes: u64,
        guard: &'a CrawlerExecutionGuard,
    ) -> CrawlerBoxFuture<'a, Result<CrawlerFetchResponse, CrawlerRuntimeErrorCode>>;
}

#[derive(Debug, Clone)]
struct ReqwestCrawlerTransportPolicy {
    connect_timeout: Duration,
    response_header_timeout: Duration,
    response_body_timeout: Duration,
    built_in_roots: bool,
    extra_root_certificates: Vec<reqwest::Certificate>,
    #[cfg(test)]
    body_started: Option<Arc<tokio::sync::Notify>>,
}

impl Default for ReqwestCrawlerTransportPolicy {
    fn default() -> Self {
        Self {
            connect_timeout: CONNECT_TIMEOUT,
            response_header_timeout: RESPONSE_HEADER_TIMEOUT,
            response_body_timeout: RESPONSE_BODY_TIMEOUT,
            built_in_roots: true,
            extra_root_certificates: Vec::new(),
            #[cfg(test)]
            body_started: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReqwestCrawlerFetcher {
    policy: ReqwestCrawlerTransportPolicy,
}

#[cfg(test)]
impl ReqwestCrawlerFetcher {
    fn for_hermetic_test(
        root: reqwest::Certificate,
        connect_timeout: Duration,
        response_header_timeout: Duration,
        response_body_timeout: Duration,
    ) -> Self {
        Self {
            policy: ReqwestCrawlerTransportPolicy {
                connect_timeout,
                response_header_timeout,
                response_body_timeout,
                built_in_roots: false,
                extra_root_certificates: vec![root],
                body_started: None,
            },
        }
    }
}

impl CrawlerFetcher for ReqwestCrawlerFetcher {
    fn fetch<'a>(
        &'a self,
        target: &'a VettedCrawlerUrlTarget,
        max_decoded_body_bytes: u64,
        guard: &'a CrawlerExecutionGuard,
    ) -> CrawlerBoxFuture<'a, Result<CrawlerFetchResponse, CrawlerRuntimeErrorCode>> {
        Box::pin(async move {
            guard_to_runtime(guard.check())?;
            let fixture_transport = configured_fixture_transport(target)?;
            let connect_addrs = fixture_transport
                .as_ref()
                .map_or_else(|| target.socket_addrs(), |(endpoint, _)| vec![*endpoint]);
            let mut client_builder = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(self.policy.connect_timeout)
                .tls_built_in_root_certs(fixture_transport.is_none() && self.policy.built_in_roots)
                .resolve_to_addrs(&target.host, &connect_addrs);
            if let Some((_, fixture_root)) = fixture_transport {
                client_builder = client_builder.add_root_certificate(fixture_root);
            } else {
                for certificate in &self.policy.extra_root_certificates {
                    client_builder = client_builder.add_root_certificate(certificate.clone());
                }
            }
            let client = client_builder
                .build()
                .map_err(|_| CrawlerRuntimeErrorCode::InternalFailure)?;
            let header_deadline = std::cmp::min(
                guard.deadline(),
                tokio::time::Instant::now() + self.policy.response_header_timeout,
            );
            let response = tokio::time::timeout_at(
                header_deadline,
                client
                    .get(target.canonical_url.clone())
                    .header(ACCEPT, "text/html, application/xhtml+xml")
                    .header(USER_AGENT, "FlapjackCrawler/1")
                    .send(),
            )
            .await
            .map_err(|_| deadline_or_fetch_timeout(guard))?
            .map_err(|_| CrawlerRuntimeErrorCode::FetchTimeout)?;

            let status = response.status().as_u16();
            validate_response_status(status)?;
            let content_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            validate_html_content_type(content_type.as_deref())?;
            let mut response = response;
            let mut decoded_body = Vec::new();
            let body_deadline = std::cmp::min(
                guard.deadline(),
                tokio::time::Instant::now() + self.policy.response_body_timeout,
            );
            #[cfg(test)]
            if let Some(body_started) = &self.policy.body_started {
                body_started.notify_one();
            }
            loop {
                guard_to_runtime(guard.check())?;
                let chunk = tokio::time::timeout_at(body_deadline, response.chunk())
                    .await
                    .map_err(|_| deadline_or_fetch_timeout(guard))?
                    .map_err(|_| CrawlerRuntimeErrorCode::FetchTimeout)?;
                let Some(chunk) = chunk else {
                    break;
                };
                append_decoded_chunk(&mut decoded_body, &chunk, max_decoded_body_bytes)?;
            }
            guard_to_runtime(guard.check())?;
            Ok(CrawlerFetchResponse {
                status,
                content_type,
                decoded_body,
            })
        })
    }
}

#[cfg(any(test, feature = "fault-injection"))]
fn configured_fixture_transport(
    target: &VettedCrawlerUrlTarget,
) -> Result<Option<(std::net::SocketAddr, reqwest::Certificate)>, CrawlerRuntimeErrorCode> {
    let Some(config) = crate::security::crawler_fixture_transport_config()
        .map_err(|_| CrawlerRuntimeErrorCode::InternalFailure)?
    else {
        return Ok(None);
    };
    if config.host != target.host {
        return Ok(None);
    }
    let ca_pem =
        std::fs::read(config.ca_path).map_err(|_| CrawlerRuntimeErrorCode::InternalFailure)?;
    let root = reqwest::Certificate::from_pem(&ca_pem)
        .map_err(|_| CrawlerRuntimeErrorCode::InternalFailure)?;
    Ok(Some((config.endpoint, root)))
}

#[cfg(not(any(test, feature = "fault-injection")))]
fn configured_fixture_transport(
    _target: &VettedCrawlerUrlTarget,
) -> Result<Option<(std::net::SocketAddr, reqwest::Certificate)>, CrawlerRuntimeErrorCode> {
    Ok(None)
}

fn append_decoded_chunk(
    body: &mut Vec<u8>,
    chunk: &[u8],
    max_decoded_body_bytes: u64,
) -> Result<(), CrawlerRuntimeErrorCode> {
    let next_len = body
        .len()
        .checked_add(chunk.len())
        .ok_or(CrawlerRuntimeErrorCode::BodyLimitExceeded)?;
    if next_len as u64 > max_decoded_body_bytes {
        return Err(CrawlerRuntimeErrorCode::BodyLimitExceeded);
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn deadline_or_fetch_timeout(guard: &CrawlerExecutionGuard) -> CrawlerRuntimeErrorCode {
    if matches!(guard.check(), Err(CrawlerGuardFailure::DeadlineExceeded)) {
        CrawlerRuntimeErrorCode::DeadlineExceeded
    } else {
        CrawlerRuntimeErrorCode::FetchTimeout
    }
}

fn guard_to_runtime(
    status: Result<(), CrawlerGuardFailure>,
) -> Result<(), CrawlerRuntimeErrorCode> {
    match status {
        Ok(()) => Ok(()),
        Err(CrawlerGuardFailure::Canceled) => Err(CrawlerRuntimeErrorCode::InternalFailure),
        Err(CrawlerGuardFailure::DeadlineExceeded) => {
            Err(CrawlerRuntimeErrorCode::DeadlineExceeded)
        }
    }
}

pub struct CrawlerRuntime<F = ReqwestCrawlerFetcher> {
    fetcher: F,
}

impl Default for CrawlerRuntime<ReqwestCrawlerFetcher> {
    fn default() -> Self {
        Self {
            fetcher: ReqwestCrawlerFetcher::default(),
        }
    }
}

impl<F> CrawlerRuntime<F>
where
    F: CrawlerFetcher,
{
    pub fn new(fetcher: F) -> Self {
        Self { fetcher }
    }

    pub async fn execute<H>(
        &self,
        request: CrawlerRuntimeRequest,
        cancellation: CrawlerCancellation,
        handoff: &H,
    ) -> CrawlerRuntimeOutcome<H::Receipt>
    where
        H: CrawlerPublicationHandoff,
    {
        let counters = CrawlerRuntimeCounters::default();
        if let Err(code) = validate_request(&request) {
            return CrawlerRuntimeOutcome::Failed {
                code,
                counters,
                duration: Duration::ZERO,
            };
        }
        let transform = match CompiledCrawlerTransform::compile(request.transform) {
            Ok(transform) => transform,
            Err(code) => {
                return CrawlerRuntimeOutcome::Failed {
                    code,
                    counters,
                    duration: Duration::ZERO,
                }
            }
        };
        let guard = CrawlerExecutionGuard::new(cancellation, request.max_run_duration);
        self.execute_admitted(
            request.start_url,
            request.limits,
            transform,
            guard,
            None,
            handoff,
        )
        .await
    }

    async fn execute_admitted<H>(
        &self,
        start_url: String,
        limits: CrawlerRuntimeLimits,
        transform: CompiledCrawlerTransform,
        guard: CrawlerExecutionGuard,
        pre_admitted_start: Option<VettedCrawlerUrlTarget>,
        handoff: &H,
    ) -> CrawlerRuntimeOutcome<H::Receipt>
    where
        H: CrawlerPublicationHandoff,
    {
        let mut counters = CrawlerRuntimeCounters::default();
        let start_target = match pre_admitted_start {
            Some(target) => target,
            None => match admit_target(start_url, &guard).await {
                Ok(target) => target,
                Err(error) => return terminal_from_error(error, counters, &guard),
            },
        };
        let origin = start_target.canonical_url.origin().ascii_serialization();
        let start_identity = start_target.canonical_url.as_str().to_owned();
        let mut admitted_targets = HashMap::new();
        admitted_targets.insert(start_identity.clone(), start_target);
        let mut queue = VecDeque::from([(start_identity.clone(), 0_u16)]);
        let mut seen = HashSet::from([start_identity]);
        let mut records = Vec::new();

        while !queue.is_empty() {
            if let Err(error) = guard.check() {
                return terminal_from_guard(error, counters, &guard);
            }
            let batch_len = usize::min(limits.max_concurrency as usize, queue.len());
            let mut batch = Vec::with_capacity(batch_len);
            for _ in 0..batch_len {
                if let Err(error) = guard.check() {
                    return terminal_from_guard(error, counters, &guard);
                }
                let (url, depth) = queue
                    .pop_front()
                    .expect("bounded batch length came from queue");
                let target = match admitted_targets.remove(&url) {
                    Some(target) => Ok(target),
                    None => admit_target(url.clone(), &guard).await,
                };
                let target = match target {
                    Ok(target) => target,
                    Err(error) => return terminal_from_error(error, counters, &guard),
                };
                batch.push((url, depth, target));
            }

            let fetched = join_all(batch.iter().map(|(_, _, target)| {
                self.fetcher
                    .fetch(target, limits.max_decoded_body_bytes, &guard)
            }))
            .await;

            for ((url, depth, _target), response) in batch.into_iter().zip(fetched) {
                if let Err(error) = guard.check() {
                    return terminal_from_guard(error, counters, &guard);
                }
                let response = match response {
                    Ok(response) => response,
                    Err(code) => {
                        if guard.cancellation.is_canceled() {
                            return terminal_from_guard(
                                CrawlerGuardFailure::Canceled,
                                counters,
                                &guard,
                            );
                        }
                        return terminal_from_error(code, counters, &guard);
                    }
                };
                let page = match validate_fetch_response(response, limits.max_decoded_body_bytes) {
                    Ok(page) => page,
                    Err(code) => return terminal_from_error(code, counters, &guard),
                };
                counters.fetched += 1;
                if let Err(error) = guard.check() {
                    return terminal_from_guard(error, counters, &guard);
                }

                let extracted = match extract_html(
                    &url,
                    &page,
                    limits.max_record_bytes as usize,
                    (depth < limits.max_depth).then_some((&origin, &seen)),
                    limits.max_pages as usize,
                    &guard,
                ) {
                    Ok(extracted) => extracted,
                    Err(CrawlerStepFailure::Guard(error)) => {
                        return terminal_from_guard(error, counters, &guard)
                    }
                    Err(CrawlerStepFailure::Runtime(code)) => {
                        return terminal_from_error(code, counters, &guard)
                    }
                };
                let transformed = match transform.apply(&extracted.record) {
                    Ok(record) => record,
                    Err(code) => return terminal_from_error(code, counters, &guard),
                };
                let record_bytes = match serde_json::to_vec(&transformed) {
                    Ok(bytes) => bytes.len() as u64,
                    Err(_) => {
                        return terminal_from_error(
                            CrawlerRuntimeErrorCode::TransformFailed,
                            counters,
                            &guard,
                        )
                    }
                };
                if record_bytes > limits.max_record_bytes {
                    return terminal_from_error(
                        CrawlerRuntimeErrorCode::CrawlLimitExceeded,
                        counters,
                        &guard,
                    );
                }
                records.push(transformed);
                counters.transformed += 1;
                if records.len() > limits.max_records as usize {
                    return terminal_from_error(
                        CrawlerRuntimeErrorCode::CrawlLimitExceeded,
                        counters,
                        &guard,
                    );
                }

                if depth < limits.max_depth {
                    for canonical in extracted.links {
                        if let Err(error) = guard.check() {
                            return terminal_from_guard(error, counters, &guard);
                        }
                        if seen.insert(canonical.clone()) {
                            counters.discovered += 1;
                            if seen.len() > limits.max_pages as usize
                                || queue.len() >= MAX_CRAWLER_DISCOVERY_QUEUE
                            {
                                return terminal_from_error(
                                    CrawlerRuntimeErrorCode::CrawlLimitExceeded,
                                    counters,
                                    &guard,
                                );
                            }
                            queue.push_back((canonical, depth + 1));
                        }
                    }
                }
            }
        }

        records.sort_by(|left, right| {
            left.get("objectID")
                .and_then(Value::as_str)
                .cmp(&right.get("objectID").and_then(Value::as_str))
        });
        if let Err(error) = guard.check() {
            return terminal_from_guard(error, counters, &guard);
        }
        let receipt = handoff
            .handoff(CrawlerPublicationBatch { records }, counters, &guard)
            .await;
        match receipt {
            Ok(receipt) => CrawlerRuntimeOutcome::Handoff {
                receipt,
                counters,
                duration: guard.elapsed(),
            },
            Err(()) => match guard.check() {
                Ok(()) => terminal_from_error(
                    CrawlerRuntimeErrorCode::PublicationFailed,
                    counters,
                    &guard,
                ),
                Err(error) => terminal_from_guard(error, counters, &guard),
            },
        }
    }
}

fn validate_request(request: &CrawlerRuntimeRequest) -> Result<(), CrawlerRuntimeErrorCode> {
    request.limits.validate()?;
    if request.max_run_duration.is_zero() || request.max_run_duration > MAX_CRAWLER_RUN_DURATION {
        return Err(CrawlerRuntimeErrorCode::CrawlLimitExceeded);
    }
    Ok(())
}

async fn admit_target(
    raw_url: String,
    guard: &CrawlerExecutionGuard,
) -> Result<VettedCrawlerUrlTarget, CrawlerRuntimeErrorCode> {
    guard_to_runtime(guard.check())?;
    let permit = tokio::select! {
        biased;
        _ = guard.cancellation.cancelled() => {
            return Err(CrawlerRuntimeErrorCode::InternalFailure);
        }
        permit = tokio::time::timeout_at(
            guard.deadline(),
            CRAWLER_DNS_ADMISSION_POOL.acquire(),
        ) => {
            permit
                .map_err(|_| CrawlerRuntimeErrorCode::DeadlineExceeded)?
                .map_err(|_| CrawlerRuntimeErrorCode::InternalFailure)?
        }
    };

    // `ToSocketAddrs` cannot be interrupted after entering libc. Move the
    // permit into that worker so canceled or deadline-expired lookups can
    // detach only within this fixed global cardinality; queued admissions are
    // themselves cancelable and never create another blocking worker.
    let mut lookup = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        vet_crawler_url_target(&raw_url)
    });
    let admitted = tokio::select! {
        biased;
        _ = guard.cancellation.cancelled() => {
            return Err(CrawlerRuntimeErrorCode::InternalFailure);
        }
        result = tokio::time::timeout_at(guard.deadline(), &mut lookup) => {
            result
                .map_err(|_| CrawlerRuntimeErrorCode::DeadlineExceeded)?
                .map_err(|_| CrawlerRuntimeErrorCode::InternalFailure)?
        }
    }
    .map_err(|error| match error {
        CrawlerTargetAdmissionError::TargetRejected => CrawlerRuntimeErrorCode::TargetRejected,
        CrawlerTargetAdmissionError::DnsResolutionFailed => {
            CrawlerRuntimeErrorCode::DnsResolutionFailed
        }
    })?;
    guard_to_runtime(guard.check())?;
    Ok(admitted)
}

fn validate_fetch_response(
    response: CrawlerFetchResponse,
    max_decoded_body_bytes: u64,
) -> Result<Vec<u8>, CrawlerRuntimeErrorCode> {
    validate_response_status(response.status)?;
    validate_html_content_type(response.content_type.as_deref())?;
    if response.decoded_body.len() as u64 > max_decoded_body_bytes {
        return Err(CrawlerRuntimeErrorCode::BodyLimitExceeded);
    }
    Ok(response.decoded_body)
}

fn validate_response_status(status: u16) -> Result<(), CrawlerRuntimeErrorCode> {
    if (300..400).contains(&status) {
        return Err(CrawlerRuntimeErrorCode::RedirectRejected);
    }
    if !(200..300).contains(&status) {
        return Err(CrawlerRuntimeErrorCode::TargetRejected);
    }
    Ok(())
}

fn validate_html_content_type(content_type: Option<&str>) -> Result<(), CrawlerRuntimeErrorCode> {
    let is_html = content_type.is_some_and(|value| {
        let media_type = value.split(';').next().unwrap_or_default().trim();
        media_type.eq_ignore_ascii_case("text/html")
            || media_type.eq_ignore_ascii_case("application/xhtml+xml")
    });
    if !is_html {
        return Err(CrawlerRuntimeErrorCode::ContentTypeRejected);
    }
    Ok(())
}

struct ExtractedPage {
    record: CanonicalCrawlerRecord,
    links: Vec<String>,
}

enum CrawlerStepFailure {
    Guard(CrawlerGuardFailure),
    Runtime(CrawlerRuntimeErrorCode),
}

fn extract_html(
    url: &str,
    body: &[u8],
    max_record_bytes: usize,
    discovery: Option<(&str, &HashSet<String>)>,
    max_pages: usize,
    guard: &CrawlerExecutionGuard,
) -> Result<ExtractedPage, CrawlerStepFailure> {
    guard.check().map_err(CrawlerStepFailure::Guard)?;
    let source = String::from_utf8_lossy(body);
    let document = Html::parse_document(&source);
    let title_selector = Selector::parse("title").expect("static title selector is valid");
    let meta_selector = Selector::parse("meta").expect("static meta selector is valid");
    let body_selector = Selector::parse("body").expect("static body selector is valid");
    let link_selector = Selector::parse("a[href]").expect("static link selector is valid");

    let title = match document.select(&title_selector).next() {
        Some(element) => normalize_bounded_text(element.text(), MAX_TITLE_BYTES, guard)?,
        None => String::new(),
    };
    let mut metadata = BTreeMap::new();
    for element in document.select(&meta_selector) {
        guard.check().map_err(CrawlerStepFailure::Guard)?;
        if metadata.len() >= MAX_METADATA_FIELDS {
            break;
        }
        let value = element.value();
        let Some(key) = value.attr("name").or_else(|| value.attr("property")) else {
            continue;
        };
        let Some(content) = value.attr("content") else {
            continue;
        };
        let key = truncate_utf8(key.trim().to_ascii_lowercase(), MAX_METADATA_KEY_BYTES);
        if key.is_empty() {
            continue;
        }
        metadata
            .entry(key)
            .or_insert_with(|| truncate_utf8(content.trim().to_owned(), MAX_METADATA_VALUE_BYTES));
    }

    let text_budget = max_record_bytes.min(MAX_CRAWLER_RECORD_BYTES as usize);
    let text = match document.select(&body_selector).next() {
        Some(body) => visible_body_text(body, text_budget, guard)?,
        None => String::new(),
    };
    let mut links = Vec::new();
    if let Some((origin, globally_seen)) = discovery {
        let mut page_seen = HashSet::new();
        for element in document.select(&link_selector) {
            guard.check().map_err(CrawlerStepFailure::Guard)?;
            let Some(href) = element.value().attr("href") else {
                continue;
            };
            let Some(canonical) = canonical_same_origin_link(url, href, origin) else {
                continue;
            };
            if globally_seen.contains(&canonical) || !page_seen.insert(canonical.clone()) {
                continue;
            }
            if globally_seen.len() + page_seen.len() > max_pages
                || page_seen.len() > MAX_CRAWLER_DISCOVERY_QUEUE
            {
                return Err(CrawlerStepFailure::Runtime(
                    CrawlerRuntimeErrorCode::CrawlLimitExceeded,
                ));
            }
            links.push(canonical);
        }
    }
    guard.check().map_err(CrawlerStepFailure::Guard)?;

    Ok(ExtractedPage {
        record: CanonicalCrawlerRecord {
            url: url.to_owned(),
            title,
            metadata,
            text,
        },
        links,
    })
}

fn visible_body_text(
    body: ElementRef<'_>,
    max_bytes: usize,
    guard: &CrawlerExecutionGuard,
) -> Result<String, CrawlerStepFailure> {
    let mut fragments = Vec::new();
    for node in body.descendants() {
        guard.check().map_err(CrawlerStepFailure::Guard)?;
        let Some(text) = node.value().as_text() else {
            continue;
        };
        let hidden = node
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|element| {
                matches!(
                    element.value().name(),
                    "script" | "style" | "noscript" | "template"
                )
            });
        if !hidden {
            fragments.push(text.as_ref());
        }
    }
    normalize_bounded_text(fragments, max_bytes, guard)
}

fn normalize_bounded_text<'a>(
    fragments: impl IntoIterator<Item = &'a str>,
    max_bytes: usize,
    guard: &CrawlerExecutionGuard,
) -> Result<String, CrawlerStepFailure> {
    let mut output = String::new();
    'fragments: for fragment in fragments {
        guard.check().map_err(CrawlerStepFailure::Guard)?;
        for word in fragment.split_whitespace() {
            guard.check().map_err(CrawlerStepFailure::Guard)?;
            let separator = usize::from(!output.is_empty());
            if output.len() + separator + word.len() > max_bytes {
                break 'fragments;
            }
            if separator == 1 {
                output.push(' ');
            }
            output.push_str(word);
        }
    }
    Ok(output)
}

fn truncate_utf8(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn canonical_same_origin_link(base: &str, href: &str, origin: &str) -> Option<String> {
    let base = reqwest::Url::parse(base).ok()?;
    let mut target = base.join(href).ok()?;
    if target.scheme() != "https"
        || !target.username().is_empty()
        || target.password().is_some()
        || target.origin().ascii_serialization() != origin
    {
        return None;
    }
    target.set_fragment(None);
    Some(target.to_string())
}

fn terminal_from_guard<T>(
    error: CrawlerGuardFailure,
    counters: CrawlerRuntimeCounters,
    guard: &CrawlerExecutionGuard,
) -> CrawlerRuntimeOutcome<T> {
    match error {
        CrawlerGuardFailure::Canceled => CrawlerRuntimeOutcome::Canceled {
            counters,
            duration: guard.elapsed(),
        },
        CrawlerGuardFailure::DeadlineExceeded => CrawlerRuntimeOutcome::Failed {
            code: CrawlerRuntimeErrorCode::DeadlineExceeded,
            counters,
            duration: guard.elapsed(),
        },
    }
}

fn terminal_from_error<T>(
    code: CrawlerRuntimeErrorCode,
    counters: CrawlerRuntimeCounters,
    guard: &CrawlerExecutionGuard,
) -> CrawlerRuntimeOutcome<T> {
    if guard.cancellation.is_canceled() {
        return terminal_from_guard(CrawlerGuardFailure::Canceled, counters, guard);
    }
    CrawlerRuntimeOutcome::Failed {
        code,
        counters,
        duration: guard.elapsed(),
    }
}

#[cfg(test)]
mod tests;
