use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const MAX_DOCUMENT_ITEMS: usize = 1_000_000;
pub(super) const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub(super) const TYPESENSE_PREVIEW_LOOPBACK_ENV: &str = "FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK";

/// Typesense 30.2 answers document export with one unpaginated stream, so the
/// only traversal budget is the item ceiling applied to that stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TraversalLimits {
    pub(super) max_items: usize,
}

impl Default for TraversalLimits {
    fn default() -> Self {
        Self {
            max_items: MAX_DOCUMENT_ITEMS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TypesenseErrorKind {
    Validation,
    Transport,
    Timeout,
    Redirect,
    Upstream,
    Schema,
    Progress,
    Limit,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub(super) struct TypesenseClientError {
    kind: TypesenseErrorKind,
    message: &'static str,
}

impl TypesenseClientError {
    pub(super) fn new(kind: TypesenseErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    pub(super) fn kind(&self) -> TypesenseErrorKind {
        self.kind
    }

    pub(super) fn safe_message(&self) -> &'static str {
        self.message
    }

    /// True for the single sanitized endpoint refusal every admission path
    /// returns, so callers can chain a fallback without re-stating it.
    pub(super) fn is_endpoint_not_allowed(&self) -> bool {
        *self == typesense_endpoint_not_allowed()
    }
}

impl fmt::Debug for TypesenseClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypesenseClientError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish()
    }
}

pub(super) struct TypesenseClient {
    client: reqwest::Client,
    endpoint_origin: String,
    api_key: String,
    /// `None` for credential-scoped source discovery, which enumerates
    /// collections instead of reading one. Source-bound operations require it
    /// explicitly so a discovery client can never silently read an empty name.
    source_collection: Option<String>,
}

impl TypesenseClient {
    pub(super) fn new(
        endpoint: &str,
        api_key: &str,
        source_collection: &str,
    ) -> Result<Self, TypesenseClientError> {
        require_source_credentials_and_collection(api_key, source_collection)?;
        Self::from_vetted_cloud_endpoint(endpoint, api_key, Some(source_collection))
    }

    /// Build a credential-scoped client for source discovery, which needs API
    /// credentials but no source collection.
    pub(super) fn new_discovery(
        endpoint: &str,
        api_key: &str,
    ) -> Result<Self, TypesenseClientError> {
        require_source_credentials(api_key)?;
        Self::from_vetted_cloud_endpoint(endpoint, api_key, None)
    }

    fn from_vetted_cloud_endpoint(
        endpoint: &str,
        api_key: &str,
        source_collection: Option<&str>,
    ) -> Result<Self, TypesenseClientError> {
        let target = flapjack::security::vet_typesense_cloud_url_target(endpoint)
            .map_err(|_| typesense_endpoint_not_allowed())?;
        Self::from_vetted_target(
            &target.host,
            format!("https://{}", target.host),
            target.socket_addrs(),
            api_key,
            source_collection,
        )
    }

    /// Admit loopback discovery for the live contract fixture in every profile,
    /// only under `FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1`. Refuse before
    /// parsing an attacker-controlled endpoint when the opt-in is absent, then
    /// admit only a literal loopback IP with no credentials, query, fragment,
    /// or path.
    pub(super) fn new_discovery_preview_loopback(
        endpoint: &str,
        api_key: &str,
    ) -> Result<Self, TypesenseClientError> {
        Self::from_admitted_loopback_endpoint(endpoint, api_key, None)
    }

    pub(super) fn new_preview_loopback(
        endpoint: &str,
        api_key: &str,
        source_collection: &str,
    ) -> Result<Self, TypesenseClientError> {
        Self::from_admitted_loopback_endpoint(endpoint, api_key, Some(source_collection))
    }

    fn from_admitted_loopback_endpoint(
        endpoint: &str,
        api_key: &str,
        source_collection: Option<&str>,
    ) -> Result<Self, TypesenseClientError> {
        match source_collection {
            Some(collection) => require_source_credentials_and_collection(api_key, collection)?,
            None => require_source_credentials(api_key)?,
        }
        // Fail before parsing or vetting attacker-controlled endpoints so the
        // disabled default cannot trigger DNS resolution as a side effect.
        if !preview_loopback_enabled() {
            return Err(typesense_preview_loopback_disabled());
        }
        let parsed = parse_literal_loopback_url(endpoint)?;
        let target = flapjack::security::vet_outbound_url_target(endpoint, true)
            .map_err(|_| typesense_endpoint_not_allowed())?
            .ok_or_else(typesense_endpoint_not_allowed)?;
        Self::from_vetted_target(
            &target.host,
            parsed.origin().ascii_serialization(),
            target.socket_addrs(),
            api_key,
            source_collection,
        )
    }

    fn from_vetted_target(
        endpoint_host: &str,
        endpoint_origin: String,
        endpoint_addresses: Vec<SocketAddr>,
        api_key: &str,
        source_collection: Option<&str>,
    ) -> Result<Self, TypesenseClientError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve_to_addrs(endpoint_host, &endpoint_addresses)
            .build()
            .map_err(|_| {
                TypesenseClientError::new(
                    TypesenseErrorKind::Transport,
                    "Failed to initialize Typesense client",
                )
            })?;
        Ok(Self {
            client,
            endpoint_origin,
            api_key: api_key.to_string(),
            source_collection: source_collection.map(str::to_string),
        })
    }

    #[cfg(test)]
    pub(super) fn for_test(
        endpoint_host: &str,
        endpoint_addresses: Vec<SocketAddr>,
        api_key: &str,
        source_collection: &str,
    ) -> Result<Self, TypesenseClientError> {
        Self::from_vetted_target(
            endpoint_host,
            format!("https://{endpoint_host}"),
            endpoint_addresses,
            api_key,
            Some(source_collection),
        )
    }

    /// Discovery counterpart to [`Self::for_test`]: the same vetted-target
    /// builder with no source collection bound.
    #[cfg(test)]
    pub(super) fn for_discovery_test(
        endpoint_host: &str,
        endpoint_origin: String,
        endpoint_addresses: Vec<SocketAddr>,
        api_key: &str,
    ) -> Result<Self, TypesenseClientError> {
        Self::from_vetted_target(
            endpoint_host,
            endpoint_origin,
            endpoint_addresses,
            api_key,
            None,
        )
    }

    fn require_source_collection(&self) -> Result<&str, TypesenseClientError> {
        self.source_collection.as_deref().ok_or_else(|| {
            TypesenseClientError::new(
                TypesenseErrorKind::Validation,
                "Typesense source collection is required",
            )
        })
    }

    /// Loopback admission tests in both profiles read this back without I/O.
    #[cfg(test)]
    pub(super) fn source_collection_for_test(&self) -> Option<&str> {
        self.source_collection.as_deref()
    }

    /// The single production transport constructor. Test decorators wrap this
    /// instead of re-implementing reqwest behavior, so byte-limit enforcement,
    /// error-kind mapping, and `resolve_to_addrs` pinning keep one owner.
    pub(super) fn transport(&self) -> ReqwestTransport<'_> {
        ReqwestTransport { owner: self }
    }

    pub(super) fn build_http_request(
        &self,
        request: TypesenseRequest,
    ) -> Result<reqwest::Request, TypesenseClientError> {
        let method = match request.method {
            TypesenseMethod::Get => reqwest::Method::GET,
        };
        let mut builder = self
            .client
            .request(method, format!("{}{}", self.endpoint_origin, request.path));
        if let Some(body) = request.body {
            builder = builder.json(&body);
        }
        let mut built = builder.build().map_err(|_| {
            TypesenseClientError::new(
                TypesenseErrorKind::Validation,
                "Typesense request could not be constructed",
            )
        })?;
        let mut api_key = reqwest::header::HeaderValue::from_str(&self.api_key).map_err(|_| {
            TypesenseClientError::new(
                TypesenseErrorKind::Validation,
                "Typesense request could not be constructed",
            )
        })?;
        api_key.set_sensitive(true);
        built.headers_mut().insert(
            reqwest::header::HeaderName::from_static("x-typesense-api-key"),
            api_key,
        );
        Ok(built)
    }

    pub(super) async fn capture_source<F>(
        &self,
        consume_page: F,
    ) -> Result<TypesenseSourceCapture, TypesenseClientError>
    where
        F: FnMut(Vec<Value>) -> Result<(), TypesenseClientError>,
    {
        let source_collection = self.require_source_collection()?.to_string();
        let mut transport = self.transport();
        capture_source_with_transport(&mut transport, &source_collection, consume_page).await
    }

    #[allow(dead_code)]
    pub(super) async fn observe_source(
        &self,
    ) -> Result<TypesenseSourceObservation, TypesenseClientError> {
        let source_collection = self.require_source_collection()?.to_string();
        let mut transport = self.transport();
        observe_source_with_transport(&mut transport, &source_collection).await
    }

    pub(super) async fn read_source_settings(&self) -> Result<Value, TypesenseClientError> {
        let source_collection = self.require_source_collection()?.to_string();
        let mut transport = self.transport();
        read_settings_with_transport(&mut transport, &source_collection).await
    }

    #[allow(dead_code)]
    pub(super) async fn require_read_access(&self) -> Result<(), TypesenseClientError> {
        let source_collection = self.require_source_collection()?.to_string();
        let mut transport = self.transport();
        require_read_access_with_transport(&mut transport, &source_collection).await
    }

    /// Enumerate every collection the supplied credentials can list.
    ///
    /// Discovery is credential-scoped, so it neither requires nor consumes a
    /// source collection.
    pub(super) async fn list_collections(
        &self,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<Vec<TypesenseCollectionSummary>, TypesenseClientError> {
        let mut transport = self.transport();
        list_collections_with_transport(&mut transport, offset, limit).await
    }
}

fn typesense_endpoint_not_allowed() -> TypesenseClientError {
    TypesenseClientError::new(
        TypesenseErrorKind::Validation,
        "Typesense Cloud endpoint is not allowed",
    )
}

fn typesense_preview_loopback_disabled() -> TypesenseClientError {
    TypesenseClientError::new(
        TypesenseErrorKind::Validation,
        "Typesense preview loopback endpoint is disabled",
    )
}

fn preview_loopback_enabled() -> bool {
    matches!(
        std::env::var(TYPESENSE_PREVIEW_LOOPBACK_ENV).as_deref(),
        Ok("1")
    )
}

/// Parse only the literal-loopback URL shape the explicit fixture opt-in may
/// admit. The client owns this classification so handler fallbacks cannot
/// drift from constructor admission.
pub(super) fn parse_literal_loopback_url(
    endpoint: &str,
) -> Result<reqwest::Url, TypesenseClientError> {
    let parsed = reqwest::Url::parse(endpoint).map_err(|_| typesense_endpoint_not_allowed())?;
    let parsed_host = parsed
        .host_str()
        .ok_or_else(typesense_endpoint_not_allowed)?;
    if parsed_host.eq_ignore_ascii_case("localhost")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
        || !parsed_host
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    {
        return Err(typesense_endpoint_not_allowed());
    }
    Ok(parsed)
}

/// Credential-only admission for operations that are not bound to one collection.
fn require_source_credentials(api_key: &str) -> Result<(), TypesenseClientError> {
    if api_key.is_empty() {
        return Err(TypesenseClientError::new(
            TypesenseErrorKind::Validation,
            "Typesense credentials are required",
        ));
    }
    Ok(())
}

fn require_source_credentials_and_collection(
    api_key: &str,
    source_collection: &str,
) -> Result<(), TypesenseClientError> {
    if api_key.is_empty() || source_collection.is_empty() {
        return Err(TypesenseClientError::new(
            TypesenseErrorKind::Validation,
            "Typesense credentials and source collection are required",
        ));
    }
    Ok(())
}

impl fmt::Debug for TypesenseClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypesenseClient")
            .field("endpoint", &"<scrubbed>")
            .field("api_key", &"<scrubbed>")
            .field("source_collection", &"<scrubbed>")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct TypesenseCollection {
    pub(super) name: String,
    pub(super) num_documents: usize,
    pub(super) created_at: u64,
    fields: Vec<Value>,
    #[serde(default)]
    default_sorting_field: Option<String>,
    #[serde(default)]
    enable_nested_fields: Option<bool>,
    #[serde(default)]
    token_separators: Option<Vec<String>>,
    #[serde(default)]
    symbols_to_index: Option<Vec<String>>,
    #[serde(default)]
    synonym_sets: Option<Vec<String>>,
    #[serde(default)]
    curation_sets: Option<Vec<String>>,
    #[serde(flatten)]
    extra_settings: BTreeMap<String, Value>,
}

/// One collection as returned by `GET /collections` with the field schema
/// excluded. Discovery only needs the summary surface, so this deliberately does
/// not reuse [`TypesenseCollection`], whose required `fields` never arrives.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct TypesenseCollectionSummary {
    pub(super) name: String,
    pub(super) num_documents: u64,
    pub(super) created_at: u64,
    #[serde(default)]
    pub(super) default_sorting_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TypesenseSourceObservation {
    pub(super) source_name: String,
    pub(super) updated_at: String,
    pub(super) document_count: u64,
    pub(super) schema_hash: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct TypesenseSourceCapture {
    pub(super) collection: TypesenseCollection,
    pub(super) settings: Value,
}

impl TypesenseSourceCapture {
    pub(super) fn observation(&self) -> TypesenseSourceObservation {
        observation_from_collection(&self.collection)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TypesenseMethod {
    Get,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct TypesenseRequest {
    pub(super) method: TypesenseMethod,
    pub(super) path: String,
    pub(super) body: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TypesenseResponse {
    pub(super) status: u16,
    pub(super) body: Vec<u8>,
}

pub(super) trait TypesenseTransport {
    fn send<'a>(
        &'a mut self,
        request: TypesenseRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TypesenseResponse, TypesenseClientError>> + Send + 'a>>;
}

pub(super) struct ReqwestTransport<'a> {
    owner: &'a TypesenseClient,
}

impl TypesenseTransport for ReqwestTransport<'_> {
    fn send<'a>(
        &'a mut self,
        request: TypesenseRequest,
    ) -> Pin<Box<dyn Future<Output = Result<TypesenseResponse, TypesenseClientError>> + Send + 'a>>
    {
        Box::pin(async move {
            let request = self.owner.build_http_request(request)?;
            let mut response = self.owner.client.execute(request).await.map_err(|error| {
                TypesenseClientError::new(
                    if error.is_timeout() {
                        TypesenseErrorKind::Timeout
                    } else {
                        TypesenseErrorKind::Transport
                    },
                    if error.is_timeout() {
                        "Typesense request timed out"
                    } else {
                        "Typesense transport failed"
                    },
                )
            })?;
            let status = response.status().as_u16();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| {
                TypesenseClientError::new(
                    TypesenseErrorKind::Transport,
                    "Typesense response body failed",
                )
            })? {
                if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                    return Err(response_byte_limit_error());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(TypesenseResponse { status, body })
        })
    }
}

pub(super) fn encoded_collection_name(collection: &str) -> String {
    urlencoding::encode(collection).into_owned()
}

/// Typesense 30.2 export is one unpaginated stream, so the whole response body
/// is the export's only page, bounded by the default item ceiling.
pub(super) fn decode_document_page(body: &[u8]) -> Result<Vec<Value>, TypesenseClientError> {
    decode_exported_documents(body, MAX_DOCUMENT_ITEMS)
}

/// Decode one bounded export stream: the byte ceiling first, then the JSONL
/// values, with the item ceiling applied as values arrive so an oversized stream
/// is refused before it is fully materialized.
fn decode_exported_documents(
    body: &[u8],
    max_items: usize,
) -> Result<Vec<Value>, TypesenseClientError> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(response_byte_limit_error());
    }
    let stream = serde_json::Deserializer::from_slice(body).into_iter::<Value>();
    let mut documents = Vec::new();
    for item in stream {
        let document = item.map_err(|_| document_progress_error())?;
        if !document.is_object() {
            return Err(document_progress_error());
        }
        if documents.len() == max_items {
            return Err(document_limit_error());
        }
        documents.push(document);
    }
    Ok(documents)
}

#[cfg(test)]
pub(super) async fn fetch_document_pages_with_transport<T, F>(
    transport: &mut T,
    collection: &str,
    limits: TraversalLimits,
    mut consume_page: F,
) -> Result<(), TypesenseClientError>
where
    T: TypesenseTransport,
    F: FnMut(Vec<Value>) -> Result<(), TypesenseClientError>,
{
    fetch_document_pages_with_expected_count(transport, collection, limits, None, &mut consume_page)
        .await
}

#[cfg(test)]
pub(super) async fn fetch_document_pages_with_expected_count_for_test<T, F>(
    transport: &mut T,
    collection: &str,
    limits: TraversalLimits,
    expected_items: usize,
    mut consume_page: F,
) -> Result<(), TypesenseClientError>
where
    T: TypesenseTransport,
    F: FnMut(Vec<Value>) -> Result<(), TypesenseClientError>,
{
    fetch_document_pages_with_expected_count(
        transport,
        collection,
        limits,
        Some(expected_items),
        &mut consume_page,
    )
    .await
}

/// Typesense 30.2 answers `GET /collections/<name>/documents/export` with one
/// unpaginated JSONL stream and ignores `page`/`per_page`, so a complete capture
/// is exactly one request whose whole body is decoded once. Fabricating query
/// pagination silently truncated every export to the first response and then
/// failed progress validation, which is the defect this owner replaces.
async fn fetch_document_pages_with_expected_count<T, F>(
    transport: &mut T,
    collection: &str,
    limits: TraversalLimits,
    expected_items: Option<usize>,
    consume_page: &mut F,
) -> Result<(), TypesenseClientError>
where
    T: TypesenseTransport,
    F: FnMut(Vec<Value>) -> Result<(), TypesenseClientError>,
{
    if expected_items.is_some_and(|expected| expected > limits.max_items) {
        return Err(document_limit_error());
    }
    let response = transport
        .send(TypesenseRequest {
            method: TypesenseMethod::Get,
            path: document_export_path(collection),
            body: None,
        })
        .await?;
    validate_response_status(response.status)?;
    let documents = decode_exported_documents(&response.body, limits.max_items)?;
    if expected_items.is_some_and(|expected| expected != documents.len()) {
        return Err(document_progress_error());
    }
    if documents.is_empty() {
        return Ok(());
    }
    consume_page(documents)
}

/// The single export request shape: no query at all. Typesense 30.2 has no
/// `page`/`per_page` contract on this endpoint, so any such query would be
/// ignored while making the request look paginated.
fn document_export_path(collection: &str) -> String {
    format!(
        "/collections/{}/documents/export",
        encoded_collection_name(collection)
    )
}

fn validate_response_status(status: u16) -> Result<(), TypesenseClientError> {
    match status {
        200..=299 => Ok(()),
        300..=399 => Err(TypesenseClientError::new(
            TypesenseErrorKind::Redirect,
            "Typesense redirect was refused",
        )),
        401 | 403 => Err(TypesenseClientError::new(
            TypesenseErrorKind::Upstream,
            "Typesense source credentials lack required read access",
        )),
        _ => Err(TypesenseClientError::new(
            TypesenseErrorKind::Upstream,
            "Typesense request failed",
        )),
    }
}

/// Collection discovery is `GET /collections` paginated by `offset`/`limit`.
/// It is the only paginated Typesense endpoint this client uses: document
/// export (`document_export_path`) is a single unpaginated stream, and the two
/// must not be conflated. `exclude_fields=fields` keeps the summary bounded.
fn collection_listing_path(offset: Option<u64>, limit: Option<u64>) -> String {
    let mut query = vec!["exclude_fields=fields".to_string()];
    if let Some(offset) = offset {
        query.push(format!("offset={offset}"));
    }
    if let Some(limit) = limit {
        query.push(format!("limit={limit}"));
    }
    format!("/collections?{}", query.join("&"))
}

pub(super) async fn list_collections_with_transport<T: TypesenseTransport>(
    transport: &mut T,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<Vec<TypesenseCollectionSummary>, TypesenseClientError> {
    // Typesense returns collections newest-first; the decoded order is the
    // contract, so this preserves the upstream sequence verbatim.
    decode_json_value(read_json(transport, &collection_listing_path(offset, limit)).await?)
}

#[allow(dead_code)]
pub(super) async fn observe_source_with_transport<T: TypesenseTransport>(
    transport: &mut T,
    collection: &str,
) -> Result<TypesenseSourceObservation, TypesenseClientError> {
    let collection = read_collection(transport, collection).await?;
    Ok(observation_from_collection(&collection))
}

pub(super) async fn read_settings_with_transport<T: TypesenseTransport>(
    transport: &mut T,
    collection: &str,
) -> Result<Value, TypesenseClientError> {
    let collection = read_collection(transport, collection).await?;
    Ok(settings_from_collection(&collection))
}

pub(super) async fn require_read_access_with_transport<T: TypesenseTransport>(
    transport: &mut T,
    collection: &str,
) -> Result<(), TypesenseClientError> {
    let collection_metadata = read_collection(transport, collection).await?;
    let response = transport
        .send(TypesenseRequest {
            method: TypesenseMethod::Get,
            path: document_export_path(collection),
            body: None,
        })
        .await?;
    validate_response_status(response.status)?;
    let documents = decode_document_page(&response.body)?;
    if documents.is_empty() && collection_metadata.num_documents != 0 {
        return Err(document_progress_error());
    }
    Ok(())
}

pub(super) async fn capture_source_with_transport<T, F>(
    transport: &mut T,
    collection_name: &str,
    mut consume_page: F,
) -> Result<TypesenseSourceCapture, TypesenseClientError>
where
    T: TypesenseTransport,
    F: FnMut(Vec<Value>) -> Result<(), TypesenseClientError>,
{
    let before_collection = read_collection(transport, collection_name).await?;
    // The export-stream owner already refuses any stream whose value count does
    // not equal the advertised document count, so capture only has to prove the
    // collection metadata did not move underneath the read.
    fetch_document_pages_with_expected_count(
        transport,
        collection_name,
        TraversalLimits::default(),
        Some(before_collection.num_documents),
        &mut consume_page,
    )
    .await?;
    let after_collection = read_collection(transport, collection_name).await?;
    if before_collection != after_collection {
        return Err(source_changed_error());
    }
    let settings = settings_from_collection(&before_collection);
    Ok(TypesenseSourceCapture {
        collection: before_collection,
        settings,
    })
}

async fn read_collection<T: TypesenseTransport>(
    transport: &mut T,
    collection: &str,
) -> Result<TypesenseCollection, TypesenseClientError> {
    let raw: TypesenseCollection = decode_json_value(
        read_json(
            transport,
            &format!("/collections/{}", encoded_collection_name(collection)),
        )
        .await?,
    )?;
    if raw.fields.iter().any(|field| !field.is_object()) {
        return Err(schema_error());
    }
    if raw.name != collection {
        return Err(source_changed_error());
    }
    Ok(raw)
}

async fn read_json<T: TypesenseTransport>(
    transport: &mut T,
    path: &str,
) -> Result<Value, TypesenseClientError> {
    let response = transport
        .send(TypesenseRequest {
            method: TypesenseMethod::Get,
            path: path.to_string(),
            body: None,
        })
        .await?;
    validate_response_status(response.status)?;
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(response_byte_limit_error());
    }
    serde_json::from_slice(&response.body).map_err(|_| schema_error())
}

fn decode_json_value<T: serde::de::DeserializeOwned>(
    value: Value,
) -> Result<T, TypesenseClientError> {
    serde_json::from_value(value).map_err(|_| schema_error())
}

fn observation_from_collection(collection: &TypesenseCollection) -> TypesenseSourceObservation {
    TypesenseSourceObservation {
        source_name: collection.name.clone(),
        updated_at: collection.created_at.to_string(),
        document_count: collection.num_documents as u64,
        schema_hash: collection_schema_hash(collection),
    }
}

fn collection_schema_hash(collection: &TypesenseCollection) -> String {
    let schema_identity = json!({
        "fields": &collection.fields,
        "default_sorting_field": &collection.default_sorting_field,
        "enable_nested_fields": collection.enable_nested_fields,
        "token_separators": &collection.token_separators,
        "symbols_to_index": &collection.symbols_to_index,
        "synonym_sets": &collection.synonym_sets,
        "curation_sets": &collection.curation_sets,
        "extra_settings": &collection.extra_settings,
    });
    hex::encode(Sha256::digest(
        serde_json::to_vec(&schema_identity)
            .expect("Typesense schema identity value should serialize"),
    ))
}

fn settings_from_collection(collection: &TypesenseCollection) -> Value {
    let mut settings = serde_json::Map::new();
    settings.extend(collection.extra_settings.clone());
    settings.insert("fields".to_string(), json!(&collection.fields));
    settings.insert(
        "default_sorting_field".to_string(),
        json!(&collection.default_sorting_field),
    );
    settings.insert(
        "enable_nested_fields".to_string(),
        json!(collection.enable_nested_fields),
    );
    settings.insert(
        "token_separators".to_string(),
        json!(&collection.token_separators),
    );
    settings.insert(
        "symbols_to_index".to_string(),
        json!(&collection.symbols_to_index),
    );
    settings.insert("synonym_sets".to_string(), json!(&collection.synonym_sets));
    settings.insert(
        "curation_sets".to_string(),
        json!(&collection.curation_sets),
    );
    Value::Object(settings)
}

fn document_progress_error() -> TypesenseClientError {
    TypesenseClientError::new(
        TypesenseErrorKind::Progress,
        "Typesense document pagination is invalid",
    )
}

fn response_byte_limit_error() -> TypesenseClientError {
    TypesenseClientError::new(
        TypesenseErrorKind::Limit,
        "Typesense response exceeded the byte limit",
    )
}

fn document_limit_error() -> TypesenseClientError {
    TypesenseClientError::new(
        TypesenseErrorKind::Limit,
        "Typesense document traversal exceeded a safety limit",
    )
}

fn schema_error() -> TypesenseClientError {
    TypesenseClientError::new(
        TypesenseErrorKind::Schema,
        "Typesense response schema is invalid",
    )
}

fn source_changed_error() -> TypesenseClientError {
    TypesenseClientError::new(
        TypesenseErrorKind::Progress,
        "Typesense source changed during export",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SingleResponseTransport {
        expected_path: &'static str,
        response: Option<TypesenseResponse>,
    }

    impl SingleResponseTransport {
        fn with_json(expected_path: &'static str, status: u16, body: Value) -> Self {
            Self {
                expected_path,
                response: Some(TypesenseResponse {
                    status,
                    body: serde_json::to_vec(&body).expect("test body must serialize"),
                }),
            }
        }
    }

    impl TypesenseTransport for SingleResponseTransport {
        fn send<'a>(
            &'a mut self,
            request: TypesenseRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<TypesenseResponse, TypesenseClientError>> + Send + 'a>,
        > {
            Box::pin(async move {
                assert_eq!(request.method, TypesenseMethod::Get);
                assert_eq!(request.path, self.expected_path);
                assert_eq!(request.body, None);
                self.response.take().ok_or_else(|| {
                    TypesenseClientError::new(
                        TypesenseErrorKind::Transport,
                        "unexpected duplicate Typesense test request",
                    )
                })
            })
        }
    }

    #[tokio::test]
    async fn list_collections_accepts_offset_without_limit_known_answer() {
        let mut transport = SingleResponseTransport::with_json(
            "/collections?exclude_fields=fields&offset=1",
            200,
            json!([{
                "name": "fj_ts_migration_categories",
                "num_documents": 2,
                "created_at": 1_785_020_400u64,
                "default_sorting_field": "priority"
            }]),
        );

        let collections = list_collections_with_transport(&mut transport, Some(1), None)
            .await
            .expect("offset without limit is a Typesense 30.2 success window");

        assert_eq!(
            collections,
            vec![TypesenseCollectionSummary {
                name: "fj_ts_migration_categories".to_string(),
                num_documents: 2,
                created_at: 1_785_020_400,
                default_sorting_field: Some("priority".to_string()),
            }]
        );
    }

    #[tokio::test]
    async fn list_collections_surfaces_exhausted_offset_rejection() {
        let mut transport = SingleResponseTransport::with_json(
            "/collections?exclude_fields=fields&offset=2&limit=1",
            400,
            json!({ "message": "Invalid offset param." }),
        );

        let error = list_collections_with_transport(&mut transport, Some(2), Some(1))
            .await
            .expect_err("offset equal to collection count is rejected by Typesense 30.2");

        assert_eq!(error.kind(), TypesenseErrorKind::Upstream);
        assert_eq!(error.safe_message(), "Typesense request failed");
    }
}
