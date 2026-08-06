use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const DOCUMENT_PAGE_LIMIT: usize = 2;
pub(super) const MAX_DOCUMENT_PAGES: usize = 10_000;
pub(super) const MAX_DOCUMENT_ITEMS: usize = 1_000_000;
pub(super) const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MEILISEARCH_CLOUD_HOST_SUFFIX: &str = ".meilisearch.io";
/// Opt-in that makes the literal-loopback fixture seam reachable. It is read in
/// every profile, so the shipped binary can serve the live contract fixture,
/// and the seam stays closed unless an operator sets it to `1`.
pub(super) const MEILISEARCH_PREVIEW_LOOPBACK_ENV: &str = "FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TraversalLimits {
    pub(super) max_pages: usize,
    pub(super) max_items: usize,
}

impl Default for TraversalLimits {
    fn default() -> Self {
        Self {
            max_pages: MAX_DOCUMENT_PAGES,
            max_items: MAX_DOCUMENT_ITEMS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum MeilisearchErrorKind {
    Validation,
    Transport,
    Timeout,
    Redirect,
    Upstream,
    /// The upstream refused the credential's authorization for the requested
    /// action (Meilisearch `invalid_api_key`), which callers surface as a
    /// labelled 403 rather than a generic upstream failure.
    Forbidden,
    Decode,
    Schema,
    Progress,
    Limit,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(super) struct MeilisearchClientError {
    kind: MeilisearchErrorKind,
    message: &'static str,
}

impl MeilisearchClientError {
    pub(super) fn new(kind: MeilisearchErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    pub(super) fn kind(&self) -> MeilisearchErrorKind {
        self.kind
    }

    pub(super) fn safe_message(&self) -> &'static str {
        self.message
    }

    /// True for the single sanitized endpoint refusal every admission path
    /// returns, so callers can chain a fallback admission attempt without
    /// re-stating the refusal text or re-deriving the validation details.
    pub(super) fn is_endpoint_not_allowed(&self) -> bool {
        *self == meilisearch_endpoint_not_allowed()
    }
}

impl fmt::Debug for MeilisearchClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MeilisearchClientError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish()
    }
}

pub(super) struct MeilisearchClient {
    client: reqwest::Client,
    endpoint_origin: String,
    api_key: String,
    /// `None` for credential-scoped source discovery, which enumerates indexes
    /// instead of reading one. Source-bound operations require it explicitly so
    /// a discovery client can never silently read an empty index name.
    source_index: Option<String>,
}

impl MeilisearchClient {
    pub(super) fn new(
        endpoint: &str,
        api_key: &str,
        source_index: &str,
    ) -> Result<Self, MeilisearchClientError> {
        require_source_credentials_and_index(api_key, source_index)?;
        Self::from_vetted_cloud_endpoint(endpoint, api_key, Some(source_index))
    }

    /// Build a credential-scoped client for source discovery, which needs API
    /// credentials but no source index.
    pub(super) fn new_discovery(
        endpoint: &str,
        api_key: &str,
    ) -> Result<Self, MeilisearchClientError> {
        require_source_credentials(api_key)?;
        Self::from_vetted_cloud_endpoint(endpoint, api_key, None)
    }

    fn from_vetted_cloud_endpoint(
        endpoint: &str,
        api_key: &str,
        source_index: Option<&str>,
    ) -> Result<Self, MeilisearchClientError> {
        let target = flapjack::security::vet_strict_vendor_url_target(
            endpoint,
            &[MEILISEARCH_CLOUD_HOST_SUFFIX],
        )
        .map_err(|_| meilisearch_endpoint_not_allowed())?;
        let endpoint_origin = format!("https://{}", target.host);
        Self::from_vetted_target(
            &target.host,
            endpoint_origin,
            target.socket_addrs(),
            api_key,
            source_index,
        )
    }

    /// Admit the literal-loopback fixture endpoint the live contract harness
    /// serves. This compiles in every profile, so the shipped binary can reach
    /// the seam, but only behind the explicit
    /// `FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1` opt-in enforced below.
    pub(super) fn new_preview_loopback(
        endpoint: &str,
        api_key: &str,
        source_index: &str,
    ) -> Result<Self, MeilisearchClientError> {
        require_source_credentials_and_index(api_key, source_index)?;
        Self::from_admitted_loopback_endpoint(endpoint, api_key, Some(source_index))
    }

    /// Discovery counterpart to [`Self::new_preview_loopback`], under the same
    /// opt-in.
    pub(super) fn new_discovery_preview_loopback(
        endpoint: &str,
        api_key: &str,
    ) -> Result<Self, MeilisearchClientError> {
        require_source_credentials(api_key)?;
        Self::from_admitted_loopback_endpoint(endpoint, api_key, None)
    }

    fn from_admitted_loopback_endpoint(
        endpoint: &str,
        api_key: &str,
        source_index: Option<&str>,
    ) -> Result<Self, MeilisearchClientError> {
        // This seam exists only for the live contract fixture, and it is
        // production-reachable only when the operator sets the opt-in. Fail
        // before parsing or vetting attacker-controlled endpoints so the
        // disabled default cannot trigger DNS resolution as a side effect.
        if !preview_loopback_enabled() {
            return Err(meilisearch_preview_loopback_disabled());
        }
        let parsed =
            reqwest::Url::parse(endpoint).map_err(|_| meilisearch_endpoint_not_allowed())?;
        let parsed_host = parsed
            .host_str()
            .ok_or_else(meilisearch_endpoint_not_allowed)?;
        if parsed_host.eq_ignore_ascii_case("localhost")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != "/"
        {
            return Err(meilisearch_endpoint_not_allowed());
        }
        let ip = parsed_host
            .parse::<IpAddr>()
            .map_err(|_| meilisearch_endpoint_not_allowed())?;
        if !ip.is_loopback() {
            return Err(meilisearch_endpoint_not_allowed());
        }
        let target = flapjack::security::vet_outbound_url_target(endpoint, true)
            .map_err(|_| meilisearch_endpoint_not_allowed())?
            .ok_or_else(meilisearch_endpoint_not_allowed)?;
        let endpoint_origin = parsed.origin().ascii_serialization();
        Self::from_vetted_target(
            &target.host,
            endpoint_origin,
            target.socket_addrs(),
            api_key,
            source_index,
        )
    }

    fn from_vetted_target(
        endpoint_host: &str,
        endpoint_origin: String,
        endpoint_addresses: Vec<SocketAddr>,
        api_key: &str,
        source_index: Option<&str>,
    ) -> Result<Self, MeilisearchClientError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve_to_addrs(endpoint_host, &endpoint_addresses)
            .build()
            .map_err(|_| {
                MeilisearchClientError::new(
                    MeilisearchErrorKind::Transport,
                    "Failed to initialize Meilisearch client",
                )
            })?;
        Ok(Self {
            client,
            endpoint_origin,
            api_key: api_key.to_string(),
            source_index: source_index.map(str::to_string),
        })
    }

    #[cfg(test)]
    pub(super) fn for_test(
        endpoint_host: &str,
        endpoint_addresses: Vec<SocketAddr>,
        api_key: &str,
        source_index: &str,
    ) -> Result<Self, MeilisearchClientError> {
        Self::from_vetted_target(
            endpoint_host,
            format!("https://{endpoint_host}"),
            endpoint_addresses,
            api_key,
            Some(source_index),
        )
    }

    /// Discovery counterpart to [`Self::for_test`]: the same vetted-target
    /// builder with no source index bound.
    #[cfg(test)]
    pub(super) fn for_discovery_test(
        endpoint_host: &str,
        endpoint_origin: String,
        endpoint_addresses: Vec<SocketAddr>,
        api_key: &str,
    ) -> Result<Self, MeilisearchClientError> {
        Self::from_vetted_target(
            endpoint_host,
            endpoint_origin,
            endpoint_addresses,
            api_key,
            None,
        )
    }

    fn require_source_index(&self) -> Result<&str, MeilisearchClientError> {
        self.source_index.as_deref().ok_or_else(|| {
            MeilisearchClientError::new(
                MeilisearchErrorKind::Validation,
                "Meilisearch source index is required",
            )
        })
    }

    pub(super) fn build_http_request(
        &self,
        request: MeilisearchRequest,
    ) -> Result<reqwest::Request, MeilisearchClientError> {
        let method = match request.method {
            MeilisearchMethod::Get => reqwest::Method::GET,
            MeilisearchMethod::Post => reqwest::Method::POST,
        };
        let mut builder = self
            .client
            .request(method, format!("{}{}", self.endpoint_origin, request.path))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.api_key),
            );
        if let Some(body) = request.body {
            builder = builder.json(&body);
        }
        builder.build().map_err(|_| {
            MeilisearchClientError::new(
                MeilisearchErrorKind::Validation,
                "Meilisearch request could not be constructed",
            )
        })
    }

    pub(super) async fn capture_source<F>(
        &self,
        consume_page: F,
    ) -> Result<SourceCapture, MeilisearchClientError>
    where
        F: FnMut(Vec<Value>) -> Result<(), MeilisearchClientError>,
    {
        let source_index = self.require_source_index()?.to_string();
        let mut transport = ReqwestTransport { owner: self };
        capture_source_with_transport(&mut transport, &source_index, consume_page).await
    }

    pub(super) async fn observe_source(
        &self,
    ) -> Result<MeilisearchSourceObservation, MeilisearchClientError> {
        let source_index = self.require_source_index()?.to_string();
        let mut transport = ReqwestTransport { owner: self };
        observe_source_with_transport(&mut transport, &source_index).await
    }

    pub(super) async fn read_source_settings(&self) -> Result<Value, MeilisearchClientError> {
        let source_index = self.require_source_index()?.to_string();
        let mut transport = ReqwestTransport { owner: self };
        read_settings_with_transport(&mut transport, &source_index).await
    }

    pub(super) async fn require_read_access(&self) -> Result<(), MeilisearchClientError> {
        let source_index = self.require_source_index()?.to_string();
        let mut transport = ReqwestTransport { owner: self };
        require_read_access_with_transport(&mut transport, &source_index).await
    }

    /// Enumerate every index the supplied credentials can list.
    ///
    /// Discovery is credential-scoped, so it neither requires nor consumes a
    /// source index.
    pub(super) async fn list_indexes(
        &self,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<MeilisearchIndexListing, MeilisearchClientError> {
        let mut transport = ReqwestTransport { owner: self };
        list_indexes_with_transport(&mut transport, offset, limit).await
    }

    pub(super) async fn read_index_document_count(
        &self,
        index_uid: &str,
    ) -> Result<u64, MeilisearchClientError> {
        validate_source_index(index_uid)?;
        let mut transport = ReqwestTransport { owner: self };
        let stats = read_index_stats(
            &mut transport,
            &format!("/indexes/{}/stats", encoded_index_uid(index_uid)),
        )
        .await?;
        Ok(stats.number_of_documents as u64)
    }
}

fn meilisearch_endpoint_not_allowed() -> MeilisearchClientError {
    MeilisearchClientError::new(
        MeilisearchErrorKind::Validation,
        "Meilisearch Cloud endpoint is not allowed",
    )
}

fn meilisearch_preview_loopback_disabled() -> MeilisearchClientError {
    MeilisearchClientError::new(
        MeilisearchErrorKind::Validation,
        "Meilisearch preview loopback endpoint is disabled",
    )
}

fn preview_loopback_enabled() -> bool {
    matches!(
        std::env::var(MEILISEARCH_PREVIEW_LOOPBACK_ENV).as_deref(),
        Ok("1")
    )
}

/// Credential-only admission for operations that are not bound to one index.
fn require_source_credentials(api_key: &str) -> Result<(), MeilisearchClientError> {
    if api_key.is_empty() {
        return Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Validation,
            "Meilisearch credentials are required",
        ));
    }
    Ok(())
}

fn require_source_credentials_and_index(
    api_key: &str,
    source_index: &str,
) -> Result<(), MeilisearchClientError> {
    if api_key.is_empty() || source_index.is_empty() {
        return Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Validation,
            "Meilisearch credentials and source index are required",
        ));
    }
    validate_source_index(source_index)?;
    Ok(())
}

pub(super) fn validate_source_index(source_index: &str) -> Result<(), MeilisearchClientError> {
    if source_index == "."
        || source_index == ".."
        || source_index.contains('/')
        || source_index.contains('\\')
        || source_index.contains('\0')
    {
        return Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Validation,
            "Meilisearch source index is invalid",
        ));
    }
    Ok(())
}

impl fmt::Debug for MeilisearchClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MeilisearchClient")
            .field("endpoint", &"<scrubbed>")
            .field("api_key", &"<scrubbed>")
            .field("source_index", &"<scrubbed>")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct DocumentPage {
    pub(super) results: Vec<Value>,
    pub(super) offset: usize,
    pub(super) limit: usize,
    pub(super) total: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IndexMetadata {
    pub(super) uid: String,
    pub(super) primary_key: Option<String>,
    pub(super) created_at: String,
    pub(super) updated_at: String,
}

/// One page of `GET /indexes`, preserving Meilisearch's own pagination triple
/// so callers report the upstream window rather than inferring it from the
/// returned slice length.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct MeilisearchIndexListing {
    pub(super) results: Vec<IndexMetadata>,
    pub(super) total: u64,
    pub(super) offset: u64,
    pub(super) limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct IndexStats {
    pub(super) number_of_documents: usize,
    field_distribution: BTreeMap<String, usize>,
    is_indexing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MeilisearchVersion {
    commit_sha: String,
    commit_date: String,
    #[serde(rename = "pkgVersion")]
    pub(super) package_version: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct SourceCapture {
    pub(super) metadata: StableIndexMetadata,
    pub(super) stats: IndexStats,
    pub(super) version: MeilisearchVersion,
    pub(super) settings: Value,
}

impl SourceCapture {
    pub(super) fn observation(&self) -> MeilisearchSourceObservation {
        MeilisearchSourceObservation {
            source_name: self.metadata.uid.clone(),
            primary_key: self.metadata.primary_key.clone(),
            updated_at: self.metadata.updated_at.clone(),
            document_count: self.stats.number_of_documents as u64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MeilisearchSourceObservation {
    pub(super) source_name: String,
    pub(super) primary_key: String,
    pub(super) updated_at: String,
    pub(super) document_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StableIndexMetadata {
    pub(super) uid: String,
    pub(super) primary_key: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MeilisearchMethod {
    Get,
    Post,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct MeilisearchRequest {
    pub(super) method: MeilisearchMethod,
    pub(super) path: String,
    pub(super) body: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MeilisearchResponse {
    pub(super) status: u16,
    pub(super) body: Vec<u8>,
}

pub(super) trait MeilisearchTransport {
    fn send<'a>(
        &'a mut self,
        request: MeilisearchRequest,
    ) -> Pin<
        Box<dyn Future<Output = Result<MeilisearchResponse, MeilisearchClientError>> + Send + 'a>,
    >;
}

struct ReqwestTransport<'a> {
    owner: &'a MeilisearchClient,
}

impl MeilisearchTransport for ReqwestTransport<'_> {
    fn send<'a>(
        &'a mut self,
        request: MeilisearchRequest,
    ) -> Pin<
        Box<dyn Future<Output = Result<MeilisearchResponse, MeilisearchClientError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let request = self.owner.build_http_request(request)?;
            let mut response = self.owner.client.execute(request).await.map_err(|error| {
                MeilisearchClientError::new(
                    if error.is_timeout() {
                        MeilisearchErrorKind::Timeout
                    } else {
                        MeilisearchErrorKind::Transport
                    },
                    if error.is_timeout() {
                        "Meilisearch request timed out"
                    } else {
                        "Meilisearch transport failed"
                    },
                )
            })?;
            let status = response.status().as_u16();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| {
                MeilisearchClientError::new(
                    MeilisearchErrorKind::Transport,
                    "Meilisearch response body failed",
                )
            })? {
                if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                    return Err(MeilisearchClientError::new(
                        MeilisearchErrorKind::Limit,
                        "Meilisearch response exceeded the byte limit",
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(MeilisearchResponse { status, body })
        })
    }
}

pub(super) fn encoded_index_uid(index_uid: &str) -> String {
    urlencoding::encode(index_uid).into_owned()
}

pub(super) fn decode_document_page(body: &[u8]) -> Result<DocumentPage, MeilisearchClientError> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Limit,
            "Meilisearch response exceeded the byte limit",
        ));
    }

    #[derive(Deserialize)]
    struct RawDocumentPage {
        results: Vec<Value>,
        offset: usize,
        limit: usize,
        total: usize,
    }

    let raw: RawDocumentPage = serde_json::from_slice(body).map_err(|_| {
        MeilisearchClientError::new(
            MeilisearchErrorKind::Progress,
            "Meilisearch document pagination is invalid",
        )
    })?;
    let end = raw.offset.checked_add(raw.results.len()).ok_or_else(|| {
        MeilisearchClientError::new(
            MeilisearchErrorKind::Progress,
            "Meilisearch document pagination is invalid",
        )
    })?;
    if raw.limit == 0 || raw.results.len() > raw.limit || raw.offset > raw.total || end > raw.total
    {
        return Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Progress,
            "Meilisearch document pagination is invalid",
        ));
    }

    Ok(DocumentPage {
        results: raw.results,
        offset: raw.offset,
        limit: raw.limit,
        total: raw.total,
    })
}

pub(super) async fn fetch_document_pages_with_transport<T, F>(
    transport: &mut T,
    index_uid: &str,
    limits: TraversalLimits,
    mut consume_page: F,
) -> Result<(), MeilisearchClientError>
where
    T: MeilisearchTransport,
    F: FnMut(Vec<Value>) -> Result<(), MeilisearchClientError>,
{
    let path = format!("/indexes/{}/documents/fetch", encoded_index_uid(index_uid));
    let mut expected_offset = 0usize;
    let mut expected_total = None;
    let mut page_count = 0usize;

    loop {
        if page_count >= limits.max_pages {
            return Err(document_limit_error());
        }
        let response = transport
            .send(MeilisearchRequest {
                method: MeilisearchMethod::Post,
                path: path.clone(),
                body: Some(serde_json::json!({
                    "offset": expected_offset,
                    "limit": DOCUMENT_PAGE_LIMIT,
                })),
            })
            .await?;
        validate_response_status(response.status)?;
        let page = decode_document_page(&response.body)?;
        validate_page_progress(&page, expected_offset, expected_total)?;

        let page_items = page.results.len();
        let next_offset = expected_offset
            .checked_add(page_items)
            .ok_or_else(document_progress_error)?;
        if page.total > limits.max_items || next_offset > limits.max_items {
            return Err(document_limit_error());
        }
        if page_items == 0 && next_offset < page.total {
            return Err(document_progress_error());
        }

        expected_total = Some(page.total);
        page_count += 1;
        consume_page(page.results)?;
        expected_offset = next_offset;
        if expected_offset == page.total {
            return Ok(());
        }
    }
}

fn validate_page_progress(
    page: &DocumentPage,
    expected_offset: usize,
    expected_total: Option<usize>,
) -> Result<(), MeilisearchClientError> {
    if page.offset != expected_offset
        || page.limit != DOCUMENT_PAGE_LIMIT
        || expected_total.is_some_and(|total| total != page.total)
    {
        return Err(document_progress_error());
    }
    Ok(())
}

/// Index discovery is `GET /indexes` with `offset`/`limit` pagination. Only the
/// parameters the caller supplied are sent, so an unpaginated request inherits
/// the upstream's own default window.
fn index_listing_path(offset: Option<u64>, limit: Option<u64>) -> String {
    let mut query = Vec::new();
    if let Some(offset) = offset {
        query.push(format!("offset={offset}"));
    }
    if let Some(limit) = limit {
        query.push(format!("limit={limit}"));
    }
    if query.is_empty() {
        "/indexes".to_string()
    } else {
        format!("/indexes?{}", query.join("&"))
    }
}

pub(super) async fn list_indexes_with_transport<T: MeilisearchTransport>(
    transport: &mut T,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<MeilisearchIndexListing, MeilisearchClientError> {
    decode_json_value(
        read_json_with_status_policy(
            transport,
            &index_listing_path(offset, limit),
            validate_discovery_response_status,
        )
        .await?,
    )
}

/// Discovery adds one rule to the shared status policy: a source key without the
/// `indexes.get` ACL fails upstream with 403 `invalid_api_key`, and that refusal
/// must stay distinguishable from a generic upstream failure so callers can
/// report which permission is missing.
fn validate_discovery_response_status(status: u16) -> Result<(), MeilisearchClientError> {
    if status == 403 {
        return Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Forbidden,
            "Meilisearch source credentials lack the indexes.get action",
        ));
    }
    validate_response_status(status)
}

fn validate_response_status(status: u16) -> Result<(), MeilisearchClientError> {
    match status {
        200..=299 => Ok(()),
        300..=399 => Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Redirect,
            "Meilisearch redirect was refused",
        )),
        401 | 403 => Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Upstream,
            "Meilisearch source credentials lack required read access",
        )),
        _ => Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Upstream,
            "Meilisearch request failed",
        )),
    }
}

pub(super) async fn observe_source_with_transport<T: MeilisearchTransport>(
    transport: &mut T,
    index_uid: &str,
) -> Result<MeilisearchSourceObservation, MeilisearchClientError> {
    let encoded_uid = encoded_index_uid(index_uid);
    let metadata =
        read_index_metadata(transport, &format!("/indexes/{encoded_uid}"), index_uid).await?;
    let stats = read_index_stats(transport, &format!("/indexes/{encoded_uid}/stats")).await?;
    require_quiescent_stats(&stats)?;
    require_terminal_tasks(
        transport,
        &format!("/tasks?indexUids={encoded_uid}&limit=1000"),
    )
    .await?;
    let _: MeilisearchVersion = decode_json_value(read_json(transport, "/version").await?)?;
    Ok(MeilisearchSourceObservation {
        source_name: metadata.uid,
        primary_key: metadata.primary_key,
        updated_at: metadata.updated_at,
        document_count: stats.number_of_documents as u64,
    })
}

pub(super) async fn read_settings_with_transport<T: MeilisearchTransport>(
    transport: &mut T,
    index_uid: &str,
) -> Result<Value, MeilisearchClientError> {
    let settings = read_json(
        transport,
        &format!("/indexes/{}/settings", encoded_index_uid(index_uid)),
    )
    .await?;
    if !settings.is_object() {
        return Err(schema_error());
    }
    Ok(settings)
}

pub(super) async fn require_read_access_with_transport<T: MeilisearchTransport>(
    transport: &mut T,
    index_uid: &str,
) -> Result<(), MeilisearchClientError> {
    observe_source_with_transport(transport, index_uid).await?;
    read_settings_with_transport(transport, index_uid).await?;
    let response = transport
        .send(MeilisearchRequest {
            method: MeilisearchMethod::Post,
            path: format!("/indexes/{}/documents/fetch", encoded_index_uid(index_uid)),
            body: Some(serde_json::json!({
                "offset": 0,
                "limit": DOCUMENT_PAGE_LIMIT,
            })),
        })
        .await?;
    validate_response_status(response.status)?;
    decode_document_page(&response.body)?;
    Ok(())
}

fn document_progress_error() -> MeilisearchClientError {
    MeilisearchClientError::new(
        MeilisearchErrorKind::Progress,
        "Meilisearch document pagination is invalid",
    )
}

fn document_limit_error() -> MeilisearchClientError {
    MeilisearchClientError::new(
        MeilisearchErrorKind::Limit,
        "Meilisearch document traversal exceeded a safety limit",
    )
}

pub(super) async fn capture_source_with_transport<T, F>(
    transport: &mut T,
    index_uid: &str,
    mut consume_page: F,
) -> Result<SourceCapture, MeilisearchClientError>
where
    T: MeilisearchTransport,
    F: FnMut(Vec<Value>) -> Result<(), MeilisearchClientError>,
{
    let encoded_uid = encoded_index_uid(index_uid);
    let metadata_path = format!("/indexes/{encoded_uid}");
    let stats_path = format!("/indexes/{encoded_uid}/stats");
    let tasks_path = format!("/tasks?indexUids={encoded_uid}&limit=1000");
    let settings_path = format!("/indexes/{encoded_uid}/settings");

    let before_metadata = read_index_metadata(transport, &metadata_path, index_uid).await?;
    let before_stats = read_index_stats(transport, &stats_path).await?;
    require_quiescent_stats(&before_stats)?;
    require_terminal_tasks(transport, &tasks_path).await?;
    let version = read_json(transport, "/version").await?;
    let version = decode_json_value(version)?;
    let settings = read_json(transport, &settings_path).await?;
    if !settings.is_object() {
        return Err(schema_error());
    }

    let mut observed_documents = 0usize;
    fetch_document_pages_with_transport(
        transport,
        index_uid,
        TraversalLimits::default(),
        |documents| {
            observed_documents = observed_documents
                .checked_add(documents.len())
                .ok_or_else(document_limit_error)?;
            consume_page(documents)
        },
    )
    .await?;

    require_terminal_tasks(transport, &tasks_path).await?;
    let after_stats = read_index_stats(transport, &stats_path).await?;
    let after_metadata = read_index_metadata(transport, &metadata_path, index_uid).await?;
    if before_metadata != after_metadata
        || before_stats != after_stats
        || observed_documents != before_stats.number_of_documents
    {
        return Err(source_changed_error());
    }
    require_quiescent_stats(&after_stats)?;

    Ok(SourceCapture {
        metadata: before_metadata,
        stats: before_stats,
        version,
        settings,
    })
}

async fn read_index_metadata<T: MeilisearchTransport>(
    transport: &mut T,
    path: &str,
    expected_uid: &str,
) -> Result<StableIndexMetadata, MeilisearchClientError> {
    let raw: IndexMetadata = decode_json_value(read_json(transport, path).await?)?;
    let primary_key = raw
        .primary_key
        .filter(|key| !key.is_empty())
        .ok_or_else(schema_error)?;
    if raw.uid != expected_uid {
        return Err(source_changed_error());
    }
    Ok(StableIndexMetadata {
        uid: raw.uid,
        primary_key,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}

async fn read_index_stats<T: MeilisearchTransport>(
    transport: &mut T,
    path: &str,
) -> Result<IndexStats, MeilisearchClientError> {
    decode_json_value(read_json(transport, path).await?)
}

fn require_quiescent_stats(stats: &IndexStats) -> Result<(), MeilisearchClientError> {
    if stats.is_indexing {
        return Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Progress,
            "Meilisearch source is not quiescent",
        ));
    }
    Ok(())
}

async fn require_terminal_tasks<T: MeilisearchTransport>(
    transport: &mut T,
    path: &str,
) -> Result<(), MeilisearchClientError> {
    #[derive(Deserialize)]
    struct Task {
        status: String,
    }
    #[derive(Deserialize)]
    struct TaskPage {
        results: Vec<Task>,
        total: usize,
        limit: usize,
    }

    let page: TaskPage = decode_json_value(read_json(transport, path).await?)?;
    if page.limit == 0
        || page.total != page.results.len()
        || page
            .results
            .iter()
            .any(|task| !matches!(task.status.as_str(), "succeeded" | "failed" | "canceled"))
    {
        return Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Progress,
            "Meilisearch task state is not terminal",
        ));
    }
    Ok(())
}

async fn read_json<T: MeilisearchTransport>(
    transport: &mut T,
    path: &str,
) -> Result<Value, MeilisearchClientError> {
    read_json_with_status_policy(transport, path, validate_response_status).await
}

async fn read_json_with_status_policy<T: MeilisearchTransport>(
    transport: &mut T,
    path: &str,
    validate_status: fn(u16) -> Result<(), MeilisearchClientError>,
) -> Result<Value, MeilisearchClientError> {
    let response = transport
        .send(MeilisearchRequest {
            method: MeilisearchMethod::Get,
            path: path.to_string(),
            body: None,
        })
        .await?;
    validate_status(response.status)?;
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(MeilisearchClientError::new(
            MeilisearchErrorKind::Limit,
            "Meilisearch response exceeded the byte limit",
        ));
    }
    serde_json::from_slice(&response.body).map_err(|_| schema_error())
}

fn decode_json_value<T: serde::de::DeserializeOwned>(
    value: Value,
) -> Result<T, MeilisearchClientError> {
    serde_json::from_value(value).map_err(|_| schema_error())
}

fn schema_error() -> MeilisearchClientError {
    MeilisearchClientError::new(
        MeilisearchErrorKind::Schema,
        "Meilisearch response schema is invalid",
    )
}

fn source_changed_error() -> MeilisearchClientError {
    MeilisearchClientError::new(
        MeilisearchErrorKind::Progress,
        "Meilisearch source changed during export",
    )
}
