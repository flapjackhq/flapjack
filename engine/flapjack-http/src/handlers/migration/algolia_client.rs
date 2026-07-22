use serde_json::Value;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

const ALGOLIA_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ALGOLIA_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_BACKOFF: Duration = Duration::from_millis(0);
const INDEX_LIST_HITS_PER_PAGE: usize = 100;
const SEARCH_HITS_PER_PAGE: usize = 1000;
const BROWSE_HITS_PER_PAGE: usize = 1000;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_INDEX_LIST_PAGES: usize = 10_000;
const MAX_INDEX_LIST_ITEMS: usize = 1_000_000;
const MAX_SEARCH_PAGES: usize = 10_000;
const MAX_SEARCH_ITEMS: usize = 1_000_000;
const MAX_BROWSE_PAGES: usize = 1_000_000;
const MAX_BROWSE_ITEMS: usize = 10_000_000;
const DEFAULT_QUIESCENCE_MAX_POLLS: usize = 1_200;
const DEFAULT_QUIESCENCE_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TraversalLimits {
    max_pages: usize,
    max_items: usize,
    max_response_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingTaskPolicy {
    OptionalForPublicList,
    RequiredForQuiescence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlgoliaHost {
    Data,
    Control,
}

const SEARCH_LIMITS: TraversalLimits = TraversalLimits {
    max_pages: MAX_SEARCH_PAGES,
    max_items: MAX_SEARCH_ITEMS,
    max_response_bytes: MAX_RESPONSE_BYTES,
};

const BROWSE_LIMITS: TraversalLimits = TraversalLimits {
    max_pages: MAX_BROWSE_PAGES,
    max_items: MAX_BROWSE_ITEMS,
    max_response_bytes: MAX_RESPONSE_BYTES,
};

const INDEX_LIST_LIMITS: TraversalLimits = TraversalLimits {
    max_pages: MAX_INDEX_LIST_PAGES,
    max_items: MAX_INDEX_LIST_ITEMS,
    max_response_bytes: MAX_RESPONSE_BYTES,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AlgoliaErrorKind {
    Validation,
    Timeout,
    Transport,
    RateLimit,
    Upstream,
    Server,
    Decode,
    Schema,
    Redirect,
    Progress,
    Limit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AlgoliaClientError {
    kind: AlgoliaErrorKind,
    message: &'static str,
}

#[derive(Debug)]
pub(super) enum BrowseError<E> {
    Client(AlgoliaClientError),
    Consumer(E),
}

impl<E> BrowseError<E> {
    #[cfg(test)]
    fn client_error(&self) -> Option<&AlgoliaClientError> {
        match self {
            Self::Client(error) => Some(error),
            Self::Consumer(_) => None,
        }
    }
}

impl<E> From<AlgoliaClientError> for BrowseError<E> {
    fn from(error: AlgoliaClientError) -> Self {
        Self::Client(error)
    }
}

impl AlgoliaClientError {
    pub(super) fn new(kind: AlgoliaErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    pub(super) fn kind(&self) -> AlgoliaErrorKind {
        self.kind
    }

    pub(super) fn safe_message(&self) -> &'static str {
        self.message
    }
}

/// Index metadata returned by Algolia's application-level index listing API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AlgoliaIndexRecord {
    pub(super) name: String,
    pub(super) entries: u64,
    pub(super) updated_at: String,
    pub(super) pending_task: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct QuiescencePolicy {
    pub(super) max_polls: usize,
    pub(super) poll_interval: Duration,
}

impl Default for QuiescencePolicy {
    fn default() -> Self {
        Self {
            max_polls: DEFAULT_QUIESCENCE_MAX_POLLS,
            poll_interval: DEFAULT_QUIESCENCE_POLL_INTERVAL,
        }
    }
}

/// Strict Algolia source client for migration-only HTTP access.
pub(super) struct AlgoliaClient {
    client: reqwest::Client,
    app_id: String,
    api_key: String,
    source_index: Option<String>,
}

impl AlgoliaClient {
    pub(super) fn new(app_id: &str, api_key: &str) -> Result<Self, AlgoliaClientError> {
        validate_app_id(app_id)?;
        if api_key.is_empty() {
            return Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Validation,
                "Algolia API key is required",
            ));
        }

        let client = reqwest::Client::builder()
            .connect_timeout(ALGOLIA_CONNECT_TIMEOUT)
            .timeout(ALGOLIA_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| {
                AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Failed to initialize Algolia client",
                )
            })?;

        Ok(Self {
            client,
            app_id: app_id.to_string(),
            api_key: api_key.to_string(),
            source_index: None,
        })
    }

    pub(super) fn for_source(
        app_id: &str,
        api_key: &str,
        source_index: &str,
    ) -> Result<Self, AlgoliaClientError> {
        if source_index.is_empty() {
            return Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Validation,
                "Algolia source index is required",
            ));
        }
        let mut client = Self::new(app_id, api_key)?;
        client.source_index = Some(source_index.to_string());
        Ok(client)
    }

    pub(super) fn source_index(&self) -> Result<&str, AlgoliaClientError> {
        self.source_index.as_deref().ok_or_else(|| {
            AlgoliaClientError::new(
                AlgoliaErrorKind::Validation,
                "Algolia source index is required",
            )
        })
    }

    pub(super) async fn settings(&self) -> Result<Value, AlgoliaClientError> {
        let path = self.index_path("settings")?;
        self.execute_json(AlgoliaMethod::Get, path, None).await
    }

    /// Fetch the complete settings JSON for an arbitrary index name (used to
    /// collect replica-owned settings during migration). Reuses the same URL
    /// encoding, response-size, and retry policy as every other source request,
    /// and keeps 404/missing-replica and any other non-2xx outcome in the typed,
    /// credential-scrubbed `AlgoliaClientError` owner.
    pub(super) async fn index_settings(
        &self,
        index_name: &str,
    ) -> Result<Value, AlgoliaClientError> {
        let path = index_path(index_name, "settings");
        self.execute_json(AlgoliaMethod::Get, path, None).await
    }

    pub(super) async fn list_indexes(&self) -> Result<Vec<AlgoliaIndexRecord>, AlgoliaClientError> {
        let mut transport = ReqwestTransport {
            client: &self.client,
        };
        list_indexes_with_transport(&mut transport, &self.app_id, &self.api_key).await
    }

    pub(super) async fn require_unretrievable_access(
        &self,
        settings: &Value,
    ) -> Result<(), AlgoliaClientError> {
        let mut transport = ReqwestTransport {
            client: &self.client,
        };
        require_unretrievable_access_with_transport(
            &mut transport,
            &self.app_id,
            &self.api_key,
            settings,
        )
        .await
    }

    pub(super) async fn wait_for_quiescent_source(
        &self,
    ) -> Result<AlgoliaIndexRecord, AlgoliaClientError> {
        let mut transport = ReqwestTransport {
            client: &self.client,
        };
        wait_for_quiescent_source_with_transport(
            &mut transport,
            &self.app_id,
            &self.api_key,
            self.source_index()?,
            QuiescencePolicy::default(),
            tokio::time::sleep,
        )
        .await
    }

    pub(super) async fn paginated_hits<F, E>(
        &self,
        endpoint: &str,
        consume_page: F,
    ) -> Result<(), BrowseError<E>>
    where
        F: FnMut(Vec<Value>) -> Result<(), E>,
    {
        let mut transport = ReqwestTransport {
            client: &self.client,
        };
        paginated_hits_with_transport(
            &mut transport,
            &self.app_id,
            &self.api_key,
            self.source_index()?,
            endpoint,
            consume_page,
        )
        .await
    }

    pub(super) async fn browse_documents<F, E>(&self, consume_page: F) -> Result<(), BrowseError<E>>
    where
        F: FnMut(Vec<Value>) -> Result<(), E>,
    {
        let mut transport = ReqwestTransport {
            client: &self.client,
        };
        browse_documents_with_transport(
            &mut transport,
            &self.app_id,
            &self.api_key,
            self.source_index()?,
            consume_page,
        )
        .await
    }

    async fn execute_json(
        &self,
        method: AlgoliaMethod,
        path: String,
        body: Option<Value>,
    ) -> Result<Value, AlgoliaClientError> {
        let request = plan_request(&self.app_id, &self.api_key, method, path, body)?;
        let mut transport = ReqwestTransport {
            client: &self.client,
        };
        execute_json_with_retry(&mut transport, request).await
    }

    fn index_path(&self, suffix: &str) -> Result<String, AlgoliaClientError> {
        Ok(index_path(self.source_index()?, suffix))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlgoliaMethod {
    Get,
    Post,
}

#[derive(Debug, Clone)]
struct RequestPolicy {
    connect_timeout: Duration,
    request_timeout: Duration,
    redirects_disabled: bool,
    proxy_disabled: bool,
}

#[derive(Debug, Clone)]
struct PlannedRequest {
    method: AlgoliaMethod,
    url: String,
    fallback_urls: Vec<String>,
    headers: Vec<(&'static str, String)>,
    body: Option<Value>,
    policy: RequestPolicy,
    max_response_bytes: usize,
}

#[derive(Debug, Clone)]
struct RawResponse {
    status: u16,
    body: Vec<u8>,
}

trait AlgoliaTransport {
    fn send<'a>(
        &'a mut self,
        request: PlannedRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RawResponse, AlgoliaClientError>> + Send + 'a>>;
}

struct ReqwestTransport<'a> {
    client: &'a reqwest::Client,
}

impl AlgoliaTransport for ReqwestTransport<'_> {
    fn send<'a>(
        &'a mut self,
        request: PlannedRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RawResponse, AlgoliaClientError>> + Send + 'a>> {
        Box::pin(async move {
            debug_assert_eq!(request.policy.connect_timeout, ALGOLIA_CONNECT_TIMEOUT);
            debug_assert_eq!(request.policy.request_timeout, ALGOLIA_REQUEST_TIMEOUT);
            debug_assert!(request.policy.redirects_disabled);
            debug_assert!(request.policy.proxy_disabled);
            let mut builder = match request.method {
                AlgoliaMethod::Get => self.client.get(&request.url),
                AlgoliaMethod::Post => self.client.post(&request.url),
            };
            for (name, value) in request.headers {
                builder = builder.header(name, value);
            }
            if let Some(body) = request.body {
                builder = builder.json(&body);
            }

            let max_response_bytes = request.max_response_bytes;
            let response = builder.send().await.map_err(classify_reqwest_error)?;
            let status = response.status().as_u16();
            let body = read_capped_response_body(response, max_response_bytes).await?;
            Ok(RawResponse { status, body })
        })
    }
}

async fn read_capped_response_body(
    mut response: reqwest::Response,
    max_response_bytes: usize,
) -> Result<Vec<u8>, AlgoliaClientError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes as u64)
    {
        return Err(response_byte_limit_error());
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(classify_reqwest_error)? {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(response_byte_limit_error)?;
        if next_len > max_response_bytes {
            return Err(response_byte_limit_error());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn classify_reqwest_error(error: reqwest::Error) -> AlgoliaClientError {
    if error.is_timeout() {
        AlgoliaClientError::new(AlgoliaErrorKind::Timeout, "Algolia request timed out")
    } else if error.is_redirect() {
        AlgoliaClientError::new(AlgoliaErrorKind::Redirect, "Algolia redirect was rejected")
    } else {
        AlgoliaClientError::new(AlgoliaErrorKind::Transport, "Algolia request failed")
    }
}

fn validate_app_id(app_id: &str) -> Result<(), AlgoliaClientError> {
    if app_id.is_empty() || !app_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Validation,
            "Algolia appId is invalid",
        ));
    }
    Ok(())
}

fn algolia_host(app_id: &str, host: AlgoliaHost) -> Result<String, AlgoliaClientError> {
    validate_app_id(app_id)?;
    Ok(match host {
        AlgoliaHost::Data => format!("{}-dsn.algolia.net", app_id),
        AlgoliaHost::Control => format!("{}.algolia.net", app_id),
    })
}

fn algolia_fallback_hosts(app_id: &str) -> Result<Vec<String>, AlgoliaClientError> {
    validate_app_id(app_id)?;
    Ok((1..=3)
        .map(|host_index| format!("{}-{}.algolianet.com", app_id, host_index))
        .collect())
}

fn encoded_index(index_name: &str) -> String {
    urlencoding::encode(index_name).into_owned()
}

fn index_path(index_name: &str, suffix: &str) -> String {
    format!("/1/indexes/{}/{}", encoded_index(index_name), suffix)
}

fn key_path(api_key: &str) -> String {
    format!("/1/keys/{}", urlencoding::encode(api_key))
}

fn plan_request(
    app_id: &str,
    api_key: &str,
    method: AlgoliaMethod,
    path: String,
    body: Option<Value>,
) -> Result<PlannedRequest, AlgoliaClientError> {
    plan_request_with_host_and_response_limit(
        app_id,
        api_key,
        AlgoliaHost::Data,
        method,
        path,
        body,
        MAX_RESPONSE_BYTES,
    )
}

fn plan_control_request(
    app_id: &str,
    api_key: &str,
    method: AlgoliaMethod,
    path: String,
    body: Option<Value>,
) -> Result<PlannedRequest, AlgoliaClientError> {
    plan_request_with_host_and_response_limit(
        app_id,
        api_key,
        AlgoliaHost::Control,
        method,
        path,
        body,
        MAX_RESPONSE_BYTES,
    )
}

fn plan_request_with_response_limit(
    app_id: &str,
    api_key: &str,
    method: AlgoliaMethod,
    path: String,
    body: Option<Value>,
    max_response_bytes: usize,
) -> Result<PlannedRequest, AlgoliaClientError> {
    plan_request_with_host_and_response_limit(
        app_id,
        api_key,
        AlgoliaHost::Data,
        method,
        path,
        body,
        max_response_bytes,
    )
}

fn plan_control_request_with_response_limit(
    app_id: &str,
    api_key: &str,
    method: AlgoliaMethod,
    path: String,
    body: Option<Value>,
    max_response_bytes: usize,
) -> Result<PlannedRequest, AlgoliaClientError> {
    plan_request_with_host_and_response_limit(
        app_id,
        api_key,
        AlgoliaHost::Control,
        method,
        path,
        body,
        max_response_bytes,
    )
}

fn plan_request_with_host_and_response_limit(
    app_id: &str,
    api_key: &str,
    host: AlgoliaHost,
    method: AlgoliaMethod,
    path: String,
    body: Option<Value>,
    max_response_bytes: usize,
) -> Result<PlannedRequest, AlgoliaClientError> {
    let host = algolia_host(app_id, host)?;
    let fallback_urls = algolia_fallback_hosts(app_id)?
        .into_iter()
        .map(|fallback_host| format!("https://{}{}", fallback_host, path))
        .collect();
    Ok(PlannedRequest {
        method,
        url: format!("https://{}{}", host, path),
        fallback_urls,
        headers: vec![
            ("x-algolia-application-id", app_id.to_string()),
            ("x-algolia-api-key", api_key.to_string()),
            ("content-type", "application/json".to_string()),
        ],
        body,
        policy: RequestPolicy {
            connect_timeout: ALGOLIA_CONNECT_TIMEOUT,
            request_timeout: ALGOLIA_REQUEST_TIMEOUT,
            redirects_disabled: true,
            proxy_disabled: true,
        },
        max_response_bytes,
    })
}

async fn execute_json_with_retry<T: AlgoliaTransport>(
    transport: &mut T,
    request: PlannedRequest,
) -> Result<Value, AlgoliaClientError> {
    let mut last_error = None;
    let urls = std::iter::once(request.url.as_str())
        .chain(request.fallback_urls.iter().map(String::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let attempt_count = urls.len();

    for (attempt_index, url) in urls.into_iter().enumerate() {
        let mut request = request.clone();
        request.url = url;
        match execute_json_once(transport, request).await {
            Ok(value) => return Ok(value),
            Err(error) if should_retry(error.kind()) && attempt_index + 1 < attempt_count => {
                last_error = Some(error);
                if !RETRY_BACKOFF.is_zero() {
                    tokio::time::sleep(RETRY_BACKOFF).await;
                }
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        AlgoliaClientError::new(AlgoliaErrorKind::Transport, "Algolia request failed")
    }))
}

async fn execute_json_once<T: AlgoliaTransport>(
    transport: &mut T,
    request: PlannedRequest,
) -> Result<Value, AlgoliaClientError> {
    let max_response_bytes = request.max_response_bytes;
    let response = transport.send(request).await?;
    match response.status {
        200..=299 => decode_response_body(response.body, max_response_bytes),
        300..=399 => Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Redirect,
            "Algolia redirect was rejected",
        )),
        429 => Err(AlgoliaClientError::new(
            AlgoliaErrorKind::RateLimit,
            "Algolia rate limit exceeded",
        )),
        500..=599 => Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Server,
            "Algolia upstream returned an error",
        )),
        _ => Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Upstream,
            "Algolia upstream rejected the request",
        )),
    }
}

fn should_retry(kind: AlgoliaErrorKind) -> bool {
    matches!(
        kind,
        AlgoliaErrorKind::Timeout
            | AlgoliaErrorKind::Transport
            | AlgoliaErrorKind::RateLimit
            | AlgoliaErrorKind::Server
    )
}

fn decode_response_body(
    body: Vec<u8>,
    max_response_bytes: usize,
) -> Result<Value, AlgoliaClientError> {
    if body.len() > max_response_bytes {
        return Err(response_byte_limit_error());
    }
    serde_json::from_slice(&body).map_err(|_| {
        AlgoliaClientError::new(
            AlgoliaErrorKind::Decode,
            "Algolia response was not valid JSON",
        )
    })
}

fn response_byte_limit_error() -> AlgoliaClientError {
    AlgoliaClientError::new(
        AlgoliaErrorKind::Limit,
        "Algolia response exceeded the migration byte limit",
    )
}

async fn list_indexes_with_transport<T: AlgoliaTransport>(
    transport: &mut T,
    app_id: &str,
    api_key: &str,
) -> Result<Vec<AlgoliaIndexRecord>, AlgoliaClientError> {
    list_indexes_with_transport_and_limits(transport, app_id, api_key, INDEX_LIST_LIMITS).await
}

async fn list_indexes_with_transport_and_limits<T: AlgoliaTransport>(
    transport: &mut T,
    app_id: &str,
    api_key: &str,
    limits: TraversalLimits,
) -> Result<Vec<AlgoliaIndexRecord>, AlgoliaClientError> {
    list_indexes_with_transport_policy(
        transport,
        app_id,
        api_key,
        limits,
        PendingTaskPolicy::OptionalForPublicList,
    )
    .await
}

async fn list_indexes_for_quiescence_with_transport<T: AlgoliaTransport>(
    transport: &mut T,
    app_id: &str,
    api_key: &str,
) -> Result<Vec<AlgoliaIndexRecord>, AlgoliaClientError> {
    list_indexes_with_transport_policy(
        transport,
        app_id,
        api_key,
        INDEX_LIST_LIMITS,
        PendingTaskPolicy::RequiredForQuiescence,
    )
    .await
}

async fn list_indexes_with_transport_policy<T: AlgoliaTransport>(
    transport: &mut T,
    app_id: &str,
    api_key: &str,
    limits: TraversalLimits,
    pending_task_policy: PendingTaskPolicy,
) -> Result<Vec<AlgoliaIndexRecord>, AlgoliaClientError> {
    let mut page = 0usize;
    let mut indexes = Vec::new();
    let mut seen_signatures = HashSet::new();

    loop {
        enforce_page_limit(page, limits.max_pages)?;
        let path = format!(
            "/1/indexes?page={}&hitsPerPage={}",
            page, INDEX_LIST_HITS_PER_PAGE
        );
        let request = plan_control_request_with_response_limit(
            app_id,
            api_key,
            AlgoliaMethod::Get,
            path,
            None,
            limits.max_response_bytes,
        )?;
        let response = execute_json_with_retry(transport, request).await?;
        let (items, nb_pages) = parse_index_list_page(&response, page, pending_task_policy)?;
        let signature = items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !signature.is_empty() && !seen_signatures.insert(signature) {
            return Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Progress,
                "Algolia index pagination repeated page content",
            ));
        }
        indexes.extend(items);
        if indexes.len() > limits.max_items {
            return Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Limit,
                "Algolia index listing exceeded the migration item limit",
            ));
        }
        if page + 1 >= nb_pages {
            return Ok(indexes);
        }
        page += 1;
    }
}

async fn key_allows_unretrievable_with_transport<T: AlgoliaTransport>(
    transport: &mut T,
    app_id: &str,
    api_key: &str,
) -> Result<bool, AlgoliaClientError> {
    let request =
        plan_control_request(app_id, api_key, AlgoliaMethod::Get, key_path(api_key), None)?;
    let response = execute_json_with_retry(transport, request).await?;
    let acl = response
        .get("acl")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AlgoliaClientError::new(AlgoliaErrorKind::Schema, "Algolia key ACL was missing")
        })?;
    Ok(acl
        .iter()
        .filter_map(Value::as_str)
        .any(acl_allows_unretrievable))
}

async fn require_unretrievable_access_with_transport<T: AlgoliaTransport>(
    transport: &mut T,
    app_id: &str,
    api_key: &str,
    settings: &Value,
) -> Result<(), AlgoliaClientError> {
    if !settings_has_unretrievable_attributes(settings) {
        return Ok(());
    }
    if key_allows_unretrievable_with_transport(transport, app_id, api_key).await? {
        return Ok(());
    }
    Err(AlgoliaClientError::new(
        AlgoliaErrorKind::Validation,
        "Algolia key cannot export unretrievable attributes",
    ))
}

fn settings_has_unretrievable_attributes(settings: &Value) -> bool {
    settings
        .get("unretrievableAttributes")
        .and_then(Value::as_array)
        .is_some_and(|attributes| !attributes.is_empty())
}

fn acl_allows_unretrievable(acl: &str) -> bool {
    matches!(acl, "admin" | "seeUnretrievableAttributes")
}

async fn wait_for_quiescent_source_with_transport<T, S, SleepFuture>(
    transport: &mut T,
    app_id: &str,
    api_key: &str,
    source_index: &str,
    policy: QuiescencePolicy,
    mut sleep: S,
) -> Result<AlgoliaIndexRecord, AlgoliaClientError>
where
    T: AlgoliaTransport,
    S: FnMut(Duration) -> SleepFuture,
    SleepFuture: Future<Output = ()>,
{
    for poll_index in 0..policy.max_polls {
        let indexes =
            list_indexes_for_quiescence_with_transport(transport, app_id, api_key).await?;
        let selected = selected_index_record(&indexes, source_index)?;
        if !selected.pending_task {
            return Ok(selected);
        }
        if poll_index + 1 < policy.max_polls {
            sleep(policy.poll_interval).await;
        }
    }
    Err(AlgoliaClientError::new(
        AlgoliaErrorKind::Progress,
        "Algolia source index did not become quiescent",
    ))
}

fn selected_index_record(
    indexes: &[AlgoliaIndexRecord],
    source_index: &str,
) -> Result<AlgoliaIndexRecord, AlgoliaClientError> {
    let mut matches = indexes
        .iter()
        .filter(|index| index.name == source_index)
        .cloned();
    let selected = matches.next().ok_or_else(|| {
        AlgoliaClientError::new(
            AlgoliaErrorKind::Progress,
            "Algolia source index metadata was missing",
        )
    })?;
    if matches.next().is_some() {
        return Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Progress,
            "Algolia source index metadata was duplicated",
        ));
    }
    Ok(selected)
}

fn parse_index_list_page(
    value: &Value,
    expected_page: usize,
    pending_task_policy: PendingTaskPolicy,
) -> Result<(Vec<AlgoliaIndexRecord>, usize), AlgoliaClientError> {
    let page = match value.get("page") {
        Some(_) => required_usize(value, "page")?,
        None => expected_page,
    };
    let nb_pages = required_usize(value, "nbPages")?;
    if page != expected_page || (nb_pages > 0 && page >= nb_pages) {
        return Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Progress,
            "Algolia index pagination did not progress",
        ));
    }
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AlgoliaClientError::new(
                AlgoliaErrorKind::Schema,
                "Algolia indexes page was missing items",
            )
        })?;
    reject_nonempty_zero_nb_pages(
        nb_pages,
        items.len(),
        "Algolia index pagination did not progress",
    )?;
    if nb_pages > 0 && items.is_empty() && expected_page + 1 < nb_pages {
        return Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Progress,
            "Algolia index pagination ended ambiguously",
        ));
    }
    let mut parsed = Vec::with_capacity(items.len());
    for item in items {
        let object = item.as_object().ok_or_else(|| {
            AlgoliaClientError::new(
                AlgoliaErrorKind::Schema,
                "Algolia index entry was malformed",
            )
        })?;
        let name = object.get("name").and_then(Value::as_str).ok_or_else(|| {
            AlgoliaClientError::new(
                AlgoliaErrorKind::Schema,
                "Algolia index entry was missing name",
            )
        })?;
        let entries = object
            .get("entries")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AlgoliaClientError::new(
                    AlgoliaErrorKind::Schema,
                    "Algolia index entry was missing entries",
                )
            })?;
        let updated_at = object
            .get("updatedAt")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AlgoliaClientError::new(
                    AlgoliaErrorKind::Schema,
                    "Algolia index entry was missing updatedAt",
                )
            })?;
        let pending_task = parse_pending_task(object.get("pendingTask"), pending_task_policy)?;
        parsed.push(AlgoliaIndexRecord {
            name: name.to_string(),
            entries,
            updated_at: updated_at.to_string(),
            pending_task,
        });
    }
    Ok((parsed, nb_pages))
}

fn parse_pending_task(
    value: Option<&Value>,
    policy: PendingTaskPolicy,
) -> Result<bool, AlgoliaClientError> {
    match (value.and_then(Value::as_bool), policy) {
        (Some(pending_task), _) => Ok(pending_task),
        (None, PendingTaskPolicy::OptionalForPublicList) => Ok(false),
        (None, PendingTaskPolicy::RequiredForQuiescence) => Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Schema,
            "Algolia index entry was missing pendingTask",
        )),
    }
}

async fn paginated_hits_with_transport<Transport, F, E>(
    transport: &mut Transport,
    app_id: &str,
    api_key: &str,
    source_index: &str,
    endpoint: &str,
    consume_page: F,
) -> Result<(), BrowseError<E>>
where
    Transport: AlgoliaTransport,
    F: FnMut(Vec<Value>) -> Result<(), E>,
{
    paginated_hits_with_transport_and_limits(
        transport,
        app_id,
        api_key,
        source_index,
        endpoint,
        SEARCH_LIMITS,
        consume_page,
    )
    .await
}

async fn paginated_hits_with_transport_and_limits<Transport, F, E>(
    transport: &mut Transport,
    app_id: &str,
    api_key: &str,
    source_index: &str,
    endpoint: &str,
    limits: TraversalLimits,
    mut consume_page: F,
) -> Result<(), BrowseError<E>>
where
    Transport: AlgoliaTransport,
    F: FnMut(Vec<Value>) -> Result<(), E>,
{
    let mut page = 0usize;
    let mut item_count = 0usize;
    let mut seen_object_ids = HashSet::new();
    loop {
        enforce_page_limit(page, limits.max_pages)?;
        let body = serde_json::json!({
            "query": "",
            "hitsPerPage": SEARCH_HITS_PER_PAGE,
            "page": page
        });
        let request = plan_request_with_response_limit(
            app_id,
            api_key,
            AlgoliaMethod::Post,
            index_path(source_index, endpoint),
            Some(body),
            limits.max_response_bytes,
        )?;
        let response = execute_json_with_retry(transport, request).await?;
        let (page_items, nb_pages) = parse_raw_hits_page(&response, page, &mut seen_object_ids)?;
        item_count += page_items.len();
        if item_count > limits.max_items {
            return Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Limit,
                "Algolia search pagination exceeded the migration item limit",
            )
            .into());
        }
        consume_page(page_items).map_err(BrowseError::Consumer)?;
        if page + 1 >= nb_pages {
            return Ok(());
        }
        page += 1;
    }
}

fn parse_raw_hits_page(
    value: &Value,
    expected_page: usize,
    seen_object_ids: &mut HashSet<String>,
) -> Result<(Vec<Value>, usize), AlgoliaClientError> {
    let (page, nb_pages) = if value.get("nbPages").is_some() {
        (
            required_usize(value, "page")?,
            required_usize(value, "nbPages")?,
        )
    } else {
        let nb_hits = required_usize(value, "nbHits")?;
        (expected_page, nb_hits.div_ceil(SEARCH_HITS_PER_PAGE))
    };
    if page != expected_page || (nb_pages > 0 && page >= nb_pages) {
        return Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Progress,
            "Algolia search pagination did not progress",
        ));
    }
    let hits = value.get("hits").and_then(Value::as_array).ok_or_else(|| {
        AlgoliaClientError::new(
            AlgoliaErrorKind::Schema,
            "Algolia hits page was missing hits",
        )
    })?;
    reject_nonempty_zero_nb_pages(
        nb_pages,
        hits.len(),
        "Algolia search pagination did not progress",
    )?;
    if nb_pages > 0 && hits.is_empty() && expected_page + 1 < nb_pages {
        return Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Progress,
            "Algolia search pagination ended ambiguously",
        ));
    }

    Ok((
        parse_raw_hits(
            hits,
            seen_object_ids,
            "Algolia search hit was malformed",
            true,
        )?,
        nb_pages,
    ))
}

fn reject_nonempty_zero_nb_pages(
    nb_pages: usize,
    item_count: usize,
    message: &'static str,
) -> Result<(), AlgoliaClientError> {
    if nb_pages == 0 && item_count > 0 {
        Err(AlgoliaClientError::new(AlgoliaErrorKind::Progress, message))
    } else {
        Ok(())
    }
}

async fn browse_documents_with_transport<T, F, E>(
    transport: &mut T,
    app_id: &str,
    api_key: &str,
    source_index: &str,
    consume_page: F,
) -> Result<(), BrowseError<E>>
where
    T: AlgoliaTransport,
    F: FnMut(Vec<Value>) -> Result<(), E>,
{
    browse_documents_with_transport_and_limits(
        transport,
        app_id,
        api_key,
        source_index,
        BROWSE_LIMITS,
        consume_page,
    )
    .await
}

async fn browse_documents_with_transport_and_limits<T, F, E>(
    transport: &mut T,
    app_id: &str,
    api_key: &str,
    source_index: &str,
    limits: TraversalLimits,
    mut consume_page: F,
) -> Result<(), BrowseError<E>>
where
    T: AlgoliaTransport,
    F: FnMut(Vec<Value>) -> Result<(), E>,
{
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_object_ids = HashSet::new();
    let mut page_count = 0usize;
    let mut item_count = 0usize;

    loop {
        enforce_page_limit(page_count, limits.max_pages)?;
        let body = match cursor.as_deref() {
            Some(cursor) => serde_json::json!({ "cursor": cursor }),
            None => serde_json::json!({
                "hitsPerPage": BROWSE_HITS_PER_PAGE,
                "attributesToRetrieve": ["*"]
            }),
        };
        let request = plan_request_with_response_limit(
            app_id,
            api_key,
            AlgoliaMethod::Post,
            index_path(source_index, "browse"),
            Some(body),
            limits.max_response_bytes,
        )?;
        let response = execute_json_with_retry(transport, request).await?;
        let hits = response
            .get("hits")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AlgoliaClientError::new(
                    AlgoliaErrorKind::Schema,
                    "Algolia browse page was missing hits",
                )
            })?;
        let documents = parse_raw_hits(
            hits,
            &mut seen_object_ids,
            "Algolia browse hit was malformed",
            false,
        )?;
        let next_cursor = parse_browse_cursor(&response)?;
        if documents.is_empty() {
            if next_cursor.is_some() {
                return Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Progress,
                    "Algolia browse returned a cursor without progress",
                )
                .into());
            }
            return Ok(());
        }

        item_count += documents.len();
        if item_count > limits.max_items {
            return Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Limit,
                "Algolia browse exceeded the migration item limit",
            )
            .into());
        }
        match next_cursor {
            Some(next) => {
                if cursor.as_deref() == Some(next.as_str()) || !seen_cursors.insert(next.clone()) {
                    return Err(AlgoliaClientError::new(
                        AlgoliaErrorKind::Progress,
                        "Algolia browse cursor did not progress",
                    )
                    .into());
                }
                consume_page(documents).map_err(BrowseError::Consumer)?;
                cursor = Some(next);
                page_count += 1;
            }
            None => {
                consume_page(documents).map_err(BrowseError::Consumer)?;
                return Ok(());
            }
        }
    }
}

fn parse_browse_cursor(response: &Value) -> Result<Option<String>, AlgoliaClientError> {
    response
        .get("cursor")
        .map(|cursor| {
            cursor.as_str().map(str::to_string).ok_or_else(|| {
                AlgoliaClientError::new(
                    AlgoliaErrorKind::Progress,
                    "Algolia browse cursor was malformed",
                )
            })
        })
        .transpose()
}

fn parse_raw_hits(
    hits: &[Value],
    seen_object_ids: &mut HashSet<String>,
    malformed_message: &'static str,
    strip_search_decorations: bool,
) -> Result<Vec<Value>, AlgoliaClientError> {
    let mut items = Vec::with_capacity(hits.len());
    for hit in hits {
        let object = hit
            .as_object()
            .ok_or_else(|| AlgoliaClientError::new(AlgoliaErrorKind::Schema, malformed_message))?;
        let object_id = object
            .get("objectID")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                AlgoliaClientError::new(
                    AlgoliaErrorKind::Schema,
                    "Algolia hit was missing a string objectID",
                )
            })?;
        if !seen_object_ids.insert(object_id.to_string()) {
            return Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Schema,
                "Algolia hit contained a duplicate objectID",
            ));
        }
        let mut item = hit.clone();
        if strip_search_decorations {
            // Rule and synonym search results decorate saved definitions with
            // transport-only fields that must not enter canonical export hashes.
            let item = item
                .as_object_mut()
                .expect("validated as an object before cloning");
            item.remove("_highlightResult");
            item.remove("_metadata");
        }
        items.push(item);
    }
    Ok(items)
}

fn required_usize(value: &Value, key: &'static str) -> Result<usize, AlgoliaClientError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            AlgoliaClientError::new(
                AlgoliaErrorKind::Schema,
                "Algolia page metadata was missing",
            )
        })
}

fn enforce_page_limit(page: usize, limit: usize) -> Result<(), AlgoliaClientError> {
    if page >= limit {
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Limit,
            "Algolia pagination exceeded the migration page limit",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "algolia_client_tests.rs"]
mod client_tests;
