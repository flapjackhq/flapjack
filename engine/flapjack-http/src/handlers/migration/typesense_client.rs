use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const DOCUMENT_PAGE_LIMIT: usize = 100;
pub(super) const MAX_DOCUMENT_PAGES: usize = 10_000;
pub(super) const MAX_DOCUMENT_ITEMS: usize = 1_000_000;
pub(super) const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TraversalLimits {
    pub(super) max_pages: usize,
    pub(super) max_items: usize,
    pub(super) page_size: usize,
}

impl Default for TraversalLimits {
    fn default() -> Self {
        Self {
            max_pages: MAX_DOCUMENT_PAGES,
            max_items: MAX_DOCUMENT_ITEMS,
            page_size: DOCUMENT_PAGE_LIMIT,
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
    source_collection: String,
}

impl TypesenseClient {
    pub(super) fn new(
        endpoint: &str,
        api_key: &str,
        source_collection: &str,
    ) -> Result<Self, TypesenseClientError> {
        if api_key.is_empty() || source_collection.is_empty() {
            return Err(TypesenseClientError::new(
                TypesenseErrorKind::Validation,
                "Typesense credentials and source collection are required",
            ));
        }
        let target =
            flapjack::security::vet_typesense_cloud_url_target(endpoint).map_err(|_| {
                TypesenseClientError::new(
                    TypesenseErrorKind::Validation,
                    "Typesense Cloud endpoint is not allowed",
                )
            })?;
        Self::from_vetted_target(
            &target.host,
            target.socket_addrs(),
            api_key,
            source_collection,
        )
    }

    fn from_vetted_target(
        endpoint_host: &str,
        endpoint_addresses: Vec<SocketAddr>,
        api_key: &str,
        source_collection: &str,
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
            endpoint_origin: format!("https://{endpoint_host}"),
            api_key: api_key.to_string(),
            source_collection: source_collection.to_string(),
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
            endpoint_addresses,
            api_key,
            source_collection,
        )
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
        let mut transport = ReqwestTransport { owner: self };
        capture_source_with_transport(&mut transport, &self.source_collection, consume_page).await
    }

    #[allow(dead_code)]
    pub(super) async fn observe_source(
        &self,
    ) -> Result<TypesenseSourceObservation, TypesenseClientError> {
        let mut transport = ReqwestTransport { owner: self };
        observe_source_with_transport(&mut transport, &self.source_collection).await
    }

    pub(super) async fn read_source_settings(&self) -> Result<Value, TypesenseClientError> {
        let mut transport = ReqwestTransport { owner: self };
        read_settings_with_transport(&mut transport, &self.source_collection).await
    }

    #[allow(dead_code)]
    pub(super) async fn require_read_access(&self) -> Result<(), TypesenseClientError> {
        let mut transport = ReqwestTransport { owner: self };
        require_read_access_with_transport(&mut transport, &self.source_collection).await
    }
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

#[derive(Debug, Clone, PartialEq)]
pub(super) struct DocumentPage {
    pub(super) documents: Vec<Value>,
    pub(super) page: usize,
    pub(super) found: usize,
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

struct ReqwestTransport<'a> {
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
                    return Err(TypesenseClientError::new(
                        TypesenseErrorKind::Limit,
                        "Typesense response exceeded the byte limit",
                    ));
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

pub(super) fn decode_document_page(body: &[u8]) -> Result<DocumentPage, TypesenseClientError> {
    decode_document_page_with_limit(body, DOCUMENT_PAGE_LIMIT)
}

fn decode_document_page_with_limit(
    body: &[u8],
    page_size: usize,
) -> Result<DocumentPage, TypesenseClientError> {
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(TypesenseClientError::new(
            TypesenseErrorKind::Limit,
            "Typesense response exceeded the byte limit",
        ));
    }

    let documents = decode_exported_documents(body)?;
    if page_size == 0 || documents.len() > page_size {
        return Err(document_progress_error());
    }
    Ok(DocumentPage {
        found: documents.len(),
        documents,
        page: 1,
    })
}

fn decode_exported_documents(body: &[u8]) -> Result<Vec<Value>, TypesenseClientError> {
    let stream = serde_json::Deserializer::from_slice(body).into_iter::<Value>();
    let mut documents = Vec::new();
    for item in stream {
        let document = item.map_err(|_| document_progress_error())?;
        if !document.is_object() {
            return Err(document_progress_error());
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
    let encoded = encoded_collection_name(collection);
    let mut expected_page = 1usize;
    let mut observed_items = 0usize;

    loop {
        if page_exceeds_traversal_budget(expected_page, limits, expected_items, observed_items) {
            return Err(document_limit_error());
        }
        let response = transport
            .send(TypesenseRequest {
                method: TypesenseMethod::Get,
                path: format!(
                    "/collections/{encoded}/documents/export?page={expected_page}&per_page={}",
                    limits.page_size
                ),
                body: None,
            })
            .await?;
        validate_response_status(response.status)?;
        let page = decode_document_page_with_limit(&response.body, limits.page_size)?;
        validate_page_progress(&page, expected_page)?;

        let page_items = page.documents.len();
        if page_items == 0 {
            return if expected_items == Some(observed_items) {
                Ok(())
            } else {
                Err(document_progress_error())
            };
        }
        if expected_items.is_some_and(|expected| observed_items >= expected) {
            return Err(document_progress_error());
        }
        observed_items = observed_items
            .checked_add(page_items)
            .ok_or_else(document_progress_error)?;
        if expected_items.is_some_and(|expected| observed_items > expected) {
            return Err(document_progress_error());
        }
        if page.found > limits.max_items || observed_items > limits.max_items {
            return Err(document_limit_error());
        }

        expected_page += 1;
        consume_page(page.documents)?;
        if page_items < limits.page_size {
            return Ok(());
        }
    }
}

pub(super) fn page_exceeds_traversal_budget(
    expected_page: usize,
    limits: TraversalLimits,
    expected_items: Option<usize>,
    observed_items: usize,
) -> bool {
    if expected_page <= limits.max_pages {
        return false;
    }
    let is_counted_completion_probe = expected_items == Some(observed_items)
        && expected_page.checked_sub(1) == Some(limits.max_pages);
    !is_counted_completion_probe
}

fn validate_page_progress(
    page: &DocumentPage,
    expected_page: usize,
) -> Result<(), TypesenseClientError> {
    if page.page != 1 || expected_page == 0 {
        return Err(document_progress_error());
    }
    Ok(())
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
            path: format!(
                "/collections/{}/documents/export?page=1&per_page={DOCUMENT_PAGE_LIMIT}",
                encoded_collection_name(collection)
            ),
            body: None,
        })
        .await?;
    validate_response_status(response.status)?;
    let page = decode_document_page(&response.body)?;
    if page.documents.is_empty() && collection_metadata.num_documents != 0 {
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
    let mut observed_documents = 0usize;
    let mut consume_documents = |documents: Vec<Value>| {
        observed_documents = observed_documents
            .checked_add(documents.len())
            .ok_or_else(document_limit_error)?;
        consume_page(documents)
    };
    fetch_document_pages_with_expected_count(
        transport,
        collection_name,
        TraversalLimits::default(),
        Some(before_collection.num_documents),
        &mut consume_documents,
    )
    .await?;
    let after_collection = read_collection(transport, collection_name).await?;
    if before_collection != after_collection
        || observed_documents != before_collection.num_documents
    {
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
        return Err(TypesenseClientError::new(
            TypesenseErrorKind::Limit,
            "Typesense response exceeded the byte limit",
        ));
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
