//! Stub summary for engine/src/vector/embedder.rs.
use std::sync::OnceLock;
#[cfg(test)]
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;

use super::config::{EmbedderConfig, EmbedderSource};
use super::VectorError;

// ── UserProvidedEmbedder ──

/// Embedder for user-supplied vectors. Cannot generate embeddings —
/// only validates dimensions of vectors provided via `_vectors` field.
#[derive(Debug)]
pub struct UserProvidedEmbedder {
    dimensions: usize,
}

impl UserProvidedEmbedder {
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }

    pub async fn embed_documents(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
        Err(VectorError::EmbeddingError(
            "userProvided embedder cannot generate embeddings; supply vectors via _vectors field"
                .into(),
        ))
    }

    pub async fn embed_query(&self, _text: &str) -> Result<Vec<f32>, VectorError> {
        Err(VectorError::EmbeddingError(
            "userProvided embedder cannot generate embeddings; supply vectors via _vectors field"
                .into(),
        ))
    }

    pub fn validate_vector(&self, vector: &[f32]) -> Result<(), VectorError> {
        if vector.len() != self.dimensions {
            return Err(VectorError::DimensionMismatch {
                expected: self.dimensions,
                got: vector.len(),
            });
        }
        Ok(())
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub fn source(&self) -> EmbedderSource {
        EmbedderSource::UserProvided
    }
}

// ── RestEmbedder ──

/// Generic HTTP embedder using request/response JSON templates.
#[derive(Debug)]
pub struct RestEmbedder {
    client: reqwest::Client,
    url: String,
    request_template: serde_json::Value,
    response_template: serde_json::Value,
    dimensions: usize,
}

// Stub — implementation follows in later items
impl RestEmbedder {
    /// Build an HTTP client from the embedder config, applying custom headers and
    /// extracting the endpoint URL, request/response templates, and dimensions.
    pub fn new(config: &EmbedderConfig) -> Result<Self, VectorError> {
        // Keep constructor validation split: shape validation first, then a
        // targeted security SSOT vet/pin pass for the long-lived HTTP client.
        config.validate_required_fields()?;
        let mut client_builder = reqwest::Client::builder();
        let headers_map = config.headers.clone().unwrap_or_default();

        // Build default headers
        let mut header_map = reqwest::header::HeaderMap::new();
        for (k, v) in &headers_map {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| VectorError::EmbeddingError(format!("invalid header name: {e}")))?;
            let val = reqwest::header::HeaderValue::from_str(v)
                .map_err(|e| VectorError::EmbeddingError(format!("invalid header value: {e}")))?;
            header_map.insert(name, val);
        }
        client_builder = client_builder.default_headers(header_map);

        // Construction-time pinning closes validate→send DNS TOCTOU for long-lived
        // embedders by reusing the same vetted host/port/IP tuple from security SSOT.
        let vetted_target = config
            .url
            .as_deref()
            .map(|url| vet_embedder_outbound_target(url, "rest"))
            .transpose()?
            .flatten();
        let client = build_embedder_http_client(client_builder, vetted_target.as_ref())?;

        Ok(Self {
            client,
            url: config.url.clone().unwrap_or_default(),
            request_template: config.request.clone().unwrap_or(serde_json::Value::Null),
            response_template: config.response.clone().unwrap_or(serde_json::Value::Null),
            dimensions: config.dimensions.unwrap_or(0),
        })
    }

    /// Send texts to the configured REST endpoint, batching if the request template
    /// supports it or falling back to one-request-per-text otherwise.
    pub async fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // Check if template supports batch (has {{..}} in an array with {{text}})
        if self.is_batch_template() {
            let body = self.render_batch_request(texts);
            let response = self.send_request(&body).await?;
            self.extract_batch_embeddings(&response)
        } else {
            // One request per text
            let mut results = Vec::with_capacity(texts.len());
            for text in texts {
                let body = self.render_single_request(text);
                let response = self.send_request(&body).await?;
                let embeddings = self.extract_single_embedding(&response)?;
                results.push(embeddings);
            }
            Ok(results)
        }
    }

    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>, VectorError> {
        let results = self.embed_documents(&[text]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| VectorError::EmbeddingError("empty response from embedder".into()))
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub fn source(&self) -> EmbedderSource {
        EmbedderSource::Rest
    }

    // ── Template rendering helpers ──

    fn is_batch_template(&self) -> bool {
        Self::json_contains_str(&self.request_template, "{{..}}")
    }

    fn render_single_request(&self, text: &str) -> serde_json::Value {
        Self::replace_text_placeholder(&self.request_template, text)
    }

    fn render_batch_request(&self, texts: &[&str]) -> serde_json::Value {
        Self::replace_batch_placeholders(&self.request_template, texts)
    }

    /// Walk JSON tree and replace `"{{text}}"` string values with actual text.
    fn replace_text_placeholder(value: &serde_json::Value, text: &str) -> serde_json::Value {
        match value {
            serde_json::Value::String(s) if s == "{{text}}" => {
                serde_json::Value::String(text.to_owned())
            }
            serde_json::Value::Object(map) => {
                let new_map: serde_json::Map<String, serde_json::Value> = map
                    .iter()
                    .map(|(k, v)| (k.clone(), Self::replace_text_placeholder(v, text)))
                    .collect();
                serde_json::Value::Object(new_map)
            }
            serde_json::Value::Array(arr) => {
                let new_arr: Vec<serde_json::Value> = arr
                    .iter()
                    .map(|v| Self::replace_text_placeholder(v, text))
                    .collect();
                serde_json::Value::Array(new_arr)
            }
            other => other.clone(),
        }
    }

    /// Walk JSON tree and replace arrays containing `["{{text}}", "{{..}}"]` with all texts.
    fn replace_batch_placeholders(value: &serde_json::Value, texts: &[&str]) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let new_map: serde_json::Map<String, serde_json::Value> = map
                    .iter()
                    .map(|(k, v)| (k.clone(), Self::replace_batch_placeholders(v, texts)))
                    .collect();
                serde_json::Value::Object(new_map)
            }
            serde_json::Value::Array(arr) => {
                // Check if this array has both {{text}} and {{..}}
                let has_text = arr.iter().any(|v| v.as_str() == Some("{{text}}"));
                let has_repeat = arr.iter().any(|v| v.as_str() == Some("{{..}}"));
                if has_text && has_repeat {
                    // Replace with all texts
                    let new_arr: Vec<serde_json::Value> = texts
                        .iter()
                        .map(|t| serde_json::Value::String(t.to_string()))
                        .collect();
                    serde_json::Value::Array(new_arr)
                } else {
                    let new_arr: Vec<serde_json::Value> = arr
                        .iter()
                        .map(|v| Self::replace_batch_placeholders(v, texts))
                        .collect();
                    serde_json::Value::Array(new_arr)
                }
            }
            serde_json::Value::String(s) if s == "{{text}}" && !texts.is_empty() => {
                serde_json::Value::String(texts[0].to_string())
            }
            other => other.clone(),
        }
    }

    /// POST a JSON body to the embedder URL and return the parsed JSON response,
    /// surfacing HTTP status errors with the response body.
    async fn send_request(
        &self,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, VectorError> {
        let resp = self
            .client
            .post(&self.url)
            .json(body)
            .send()
            .await
            .map_err(|e| VectorError::EmbeddingError(format!("HTTP request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "failed to read response body".into());
            return Err(VectorError::EmbeddingError(format!(
                "embedder returned {status}: {body_text}"
            )));
        }

        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| VectorError::EmbeddingError(format!("failed to parse response JSON: {e}")))
    }

    /// Extract a single embedding from the response using the response template.
    fn extract_single_embedding(
        &self,
        response: &serde_json::Value,
    ) -> Result<Vec<f32>, VectorError> {
        let path = Self::find_embedding_path(&self.response_template);
        let embedding_val = Self::navigate_path(response, &path);
        Self::value_to_f32_vec(embedding_val)
    }

    /// Extract batch embeddings from the response.
    fn extract_batch_embeddings(
        &self,
        response: &serde_json::Value,
    ) -> Result<Vec<Vec<f32>>, VectorError> {
        let path = Self::find_embedding_path(&self.response_template);
        // Check if template has {{..}} indicating array of embeddings
        if Self::response_has_batch_marker(&self.response_template) {
            // Navigate to the parent array
            let parent_path = &path[..path.len().saturating_sub(1)];
            let arr_val = Self::navigate_path(response, parent_path);
            match arr_val {
                serde_json::Value::Array(arr) => {
                    let last_key = path.last().map(|s| s.as_str()).unwrap_or("");
                    arr.iter()
                        .map(|item| {
                            let emb = if last_key.is_empty() {
                                item
                            } else {
                                item.get(last_key).unwrap_or(item)
                            };
                            Self::value_to_f32_vec(emb)
                        })
                        .collect()
                }
                _ => {
                    // Fall back to single embedding
                    let embedding_val = Self::navigate_path(response, &path);
                    Ok(vec![Self::value_to_f32_vec(embedding_val)?])
                }
            }
        } else {
            // Single embedding path
            let embedding_val = Self::navigate_path(response, &path);
            Ok(vec![Self::value_to_f32_vec(embedding_val)?])
        }
    }

    /// Find the path to `{{embedding}}` in the response template.
    fn find_embedding_path(template: &serde_json::Value) -> Vec<String> {
        let mut path = Vec::new();
        Self::find_embedding_recursive(template, &mut path);
        path
    }

    /// Depth-first search for the `{{embedding}}` placeholder in the response template,
    /// building the JSON key path as it descends.
    fn find_embedding_recursive(value: &serde_json::Value, path: &mut Vec<String>) -> bool {
        match value {
            serde_json::Value::String(s) if s == "{{embedding}}" => true,
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    path.push(k.clone());
                    if Self::find_embedding_recursive(v, path) {
                        return true;
                    }
                    path.pop();
                }
                false
            }
            serde_json::Value::Array(arr) => {
                for (i, v) in arr.iter().enumerate() {
                    path.push(i.to_string());
                    if Self::find_embedding_recursive(v, path) {
                        return true;
                    }
                    path.pop();
                }
                false
            }
            _ => false,
        }
    }

    fn response_has_batch_marker(template: &serde_json::Value) -> bool {
        Self::json_contains_str(template, "{{..}}")
    }

    fn json_contains_str(value: &serde_json::Value, target: &str) -> bool {
        match value {
            serde_json::Value::String(s) => s == target,
            serde_json::Value::Object(map) => {
                map.values().any(|v| Self::json_contains_str(v, target))
            }
            serde_json::Value::Array(arr) => arr.iter().any(|v| Self::json_contains_str(v, target)),
            _ => false,
        }
    }

    /// Navigate a JSON value by a path of keys.
    fn navigate_path<'a>(value: &'a serde_json::Value, path: &[String]) -> &'a serde_json::Value {
        let mut current = value;
        for key in path {
            current = match current {
                serde_json::Value::Object(map) => {
                    map.get(key.as_str()).unwrap_or(&serde_json::Value::Null)
                }
                serde_json::Value::Array(arr) => {
                    if let Ok(idx) = key.parse::<usize>() {
                        arr.get(idx).unwrap_or(&serde_json::Value::Null)
                    } else {
                        &serde_json::Value::Null
                    }
                }
                _ => &serde_json::Value::Null,
            };
        }
        current
    }

    /// Convert a JSON array of numbers into a `Vec<f32>` embedding vector,
    /// returning an error if any element is non-numeric.
    fn value_to_f32_vec(value: &serde_json::Value) -> Result<Vec<f32>, VectorError> {
        match value {
            serde_json::Value::Array(arr) => arr
                .iter()
                .map(|v| {
                    v.as_f64().map(|f| f as f32).ok_or_else(|| {
                        VectorError::EmbeddingError(
                            "embedding array contains non-numeric value".into(),
                        )
                    })
                })
                .collect(),
            _ => Err(VectorError::EmbeddingError(
                "expected array for embedding vector".into(),
            )),
        }
    }
}

// ── OpenAiEmbedder ──

/// OpenAI-compatible embedder (works with OpenAI, Azure, and proxies).
#[derive(Debug)]
pub struct OpenAiEmbedder {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    configured_dimensions: Option<usize>,
    detected_dimensions: OnceLock<usize>,
}

impl OpenAiEmbedder {
    /// Initialize the OpenAI-compatible embedder, requiring an API key and model name
    /// from the config. Supports custom base URLs for Azure/proxy endpoints.
    pub fn new(config: &EmbedderConfig) -> Result<Self, VectorError> {
        // Keep constructor validation split: shape validation first, then a
        // targeted security SSOT vet/pin pass for the long-lived HTTP client.
        config.validate_required_fields()?;
        let api_key = config
            .api_key
            .clone()
            .ok_or_else(|| VectorError::EmbeddingError("openAi embedder requires apiKey".into()))?;
        let model = config
            .model
            .clone()
            .unwrap_or_else(|| "text-embedding-3-small".into());
        let base_url = config
            .url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com".into());
        // Strip trailing slash for consistent URL building
        let base_url = base_url.trim_end_matches('/').to_owned();

        let vetted_target = config
            .url
            .as_deref()
            .map(|url| vet_embedder_outbound_target(url, "openAi"))
            .transpose()?
            .flatten();
        let client =
            build_embedder_http_client(reqwest::Client::builder(), vetted_target.as_ref())?;

        Ok(Self {
            client,
            api_key,
            model,
            base_url,
            configured_dimensions: config.dimensions,
            detected_dimensions: OnceLock::new(),
        })
    }

    /// Send a batch of texts to the OpenAI embeddings endpoint, detect dimensions on
    /// first call, and validate dimension consistency on subsequent calls.
    pub async fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let url = format!("{}/v1/embeddings", self.base_url);
        let mut body = serde_json::json!({
            "input": texts,
            "model": self.model,
            "encoding_format": "float"
        });
        if let Some(dims) = self.configured_dimensions {
            body["dimensions"] = serde_json::json!(dims);
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| VectorError::EmbeddingError(format!("OpenAI request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "failed to read response body".into());
            // Try to parse OpenAI error format
            if let Ok(error_json) = serde_json::from_str::<serde_json::Value>(&body_text) {
                if let Some(msg) = error_json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    return Err(VectorError::EmbeddingError(format!(
                        "OpenAI API error ({status}): {msg}"
                    )));
                }
            }
            return Err(VectorError::EmbeddingError(format!(
                "OpenAI API error ({status}): {body_text}"
            )));
        }

        let response: serde_json::Value = resp.json().await.map_err(|e| {
            VectorError::EmbeddingError(format!("failed to parse OpenAI response: {e}"))
        })?;

        // Parse data array, order by index
        let data = response
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| {
                VectorError::EmbeddingError("OpenAI response missing `data` array".into())
            })?;

        let mut indexed: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
        for item in data {
            let index = item
                .get("index")
                .and_then(|i| i.as_u64())
                .unwrap_or(indexed.len() as u64) as usize;
            let embedding = item
                .get("embedding")
                .and_then(|e| e.as_array())
                .ok_or_else(|| {
                    VectorError::EmbeddingError("OpenAI response item missing `embedding`".into())
                })?;
            let vec: Vec<f32> = embedding
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();

            // Auto-detect dimensions from first response
            let _ = self.detected_dimensions.set(vec.len());

            indexed.push((index, vec));
        }

        // Sort by index to ensure correct ordering
        indexed.sort_by_key(|(i, _)| *i);
        Ok(indexed.into_iter().map(|(_, v)| v).collect())
    }

    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>, VectorError> {
        let results = self.embed_documents(&[text]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| VectorError::EmbeddingError("empty response from OpenAI".into()))
    }

    pub fn dimensions(&self) -> usize {
        if let Some(d) = self.configured_dimensions {
            return d;
        }
        // Return auto-detected dimensions, or 0 if no embeddings have been made yet
        self.detected_dimensions.get().copied().unwrap_or(0)
    }

    pub fn source(&self) -> EmbedderSource {
        EmbedderSource::OpenAi
    }
}

/// TODO: Document vet_embedder_outbound_target.
fn vet_embedder_outbound_target(
    raw_url: &str,
    source: &str,
) -> Result<Option<crate::security::VettedOutboundUrlTarget>, VectorError> {
    match crate::security::vet_outbound_url_target(
        raw_url,
        crate::security::allow_local_outbound_urls(),
    ) {
        Ok(vetted_target) => Ok(vetted_target),
        Err(error) => {
            // Preserve pre-existing direct-constructor behavior for localhost-backed
            // tests and callsites while still pinning any host that successfully vets.
            // Policy rejection remains owned by intake/load validation boundaries.
            if is_direct_constructor_localhost_or_private(raw_url) {
                return Ok(None);
            }
            Err(VectorError::EmbeddingError(format!(
                "{source} embedder URL {error}"
            )))
        }
    }
}

/// TODO: Document is_direct_constructor_localhost_or_private.
fn is_direct_constructor_localhost_or_private(raw_url: &str) -> bool {
    let parsed = match reqwest::Url::parse(raw_url) {
        Ok(parsed) => parsed,
        Err(_) => return false,
    };
    let host = match parsed.host_str() {
        Some(host) => host,
        None => return false,
    };

    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    let allow_local = crate::security::allow_local_outbound_urls();
    matches!(
        crate::security::first_blocked_outbound_host_ip(
            host,
            parsed.port_or_known_default(),
            allow_local
        ),
        Some((_ip, "private or local destination"))
    )
}

/// TODO: Document build_embedder_http_client.
fn build_embedder_http_client(
    mut client_builder: reqwest::ClientBuilder,
    vetted_target: Option<&crate::security::VettedOutboundUrlTarget>,
) -> Result<reqwest::Client, VectorError> {
    // Keep the original URL host in request formatting/TLS SNI; only pin connect
    // addresses from already-vetted resolution output.
    if let Some(target) = vetted_target {
        let addrs: Vec<std::net::SocketAddr> = target
            .resolved_ips
            .iter()
            .map(|ip| std::net::SocketAddr::new(*ip, target.port.unwrap_or(0)))
            .collect();
        if !addrs.is_empty() {
            client_builder = client_builder.resolve_to_addrs(&target.host, &addrs);
        }
    }
    let client_builder = apply_test_dns_resolver(client_builder);
    client_builder
        .build()
        .map_err(|e| VectorError::EmbeddingError(format!("failed to build HTTP client: {e}")))
}

#[cfg(test)]
fn apply_test_dns_resolver(mut client_builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    if let Some(resolver) = take_test_dns_resolver() {
        client_builder = client_builder
            .dns_resolver2(resolver)
            .connect_timeout(Duration::from_millis(250))
            .timeout(Duration::from_millis(600));
    }
    client_builder
}

#[cfg(not(test))]
fn apply_test_dns_resolver(client_builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    client_builder
}

#[cfg(test)]
fn test_dns_resolver_slot() -> &'static Mutex<Option<Arc<dyn reqwest::dns::Resolve>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<dyn reqwest::dns::Resolve>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn take_test_dns_resolver() -> Option<Arc<dyn reqwest::dns::Resolve>> {
    test_dns_resolver_slot()
        .lock()
        .expect("test DNS resolver slot mutex poisoned")
        .clone()
}

#[cfg(test)]
pub struct TestDnsResolverGuard {
    previous: Option<Arc<dyn reqwest::dns::Resolve>>,
}

#[cfg(test)]
impl Drop for TestDnsResolverGuard {
    fn drop(&mut self) {
        *test_dns_resolver_slot()
            .lock()
            .expect("test DNS resolver slot mutex poisoned") = self.previous.take();
    }
}

#[cfg(test)]
pub fn install_test_dns_resolver(resolver: Arc<dyn reqwest::dns::Resolve>) -> TestDnsResolverGuard {
    let mut slot = test_dns_resolver_slot()
        .lock()
        .expect("test DNS resolver slot mutex poisoned");
    let previous = slot.replace(resolver);
    TestDnsResolverGuard { previous }
}

// ── FastEmbedEmbedder ──

/// Convert a model name string to a fastembed EmbeddingModel enum variant.
///
/// # Arguments
///
/// - `model`: Optional model name (case-insensitive). Defaults to "bge-small-en-v1.5" if None.
///
/// # Returns
///
/// The corresponding `fastembed::EmbeddingModel` enum variant.
///
/// # Errors
///
/// Returns `VectorError::EmbeddingError` if the model name is not recognized, including a list of supported models.
#[cfg(feature = "vector-search-local")]
fn parse_embedding_model(model: Option<&str>) -> Result<fastembed::EmbeddingModel, VectorError> {
    match model.map(|s| s.to_lowercase()).as_deref() {
        None | Some("bge-small-en-v1.5") => Ok(fastembed::EmbeddingModel::BGESmallENV15),
        Some("bge-base-en-v1.5") => Ok(fastembed::EmbeddingModel::BGEBaseENV15),
        Some("bge-large-en-v1.5") => Ok(fastembed::EmbeddingModel::BGELargeENV15),
        Some("all-minilm-l6-v2") => Ok(fastembed::EmbeddingModel::AllMiniLML6V2),
        Some("all-minilm-l12-v2") => Ok(fastembed::EmbeddingModel::AllMiniLML12V2),
        Some("nomic-embed-text-v1.5") => Ok(fastembed::EmbeddingModel::NomicEmbedTextV15),
        Some("multilingual-e5-small") => Ok(fastembed::EmbeddingModel::MultilingualE5Small),
        Some(unknown) => Err(VectorError::EmbeddingError(format!(
            "unknown fastembed model: \"{unknown}\". Supported models: \
             bge-small-en-v1.5, bge-base-en-v1.5, bge-large-en-v1.5, \
             all-MiniLM-L6-v2, all-MiniLM-L12-v2, nomic-embed-text-v1.5, \
             multilingual-e5-small"
        ))),
    }
}

/// Local ONNX embedder using fastembed. Wraps `TextEmbedding` in a Mutex
/// because `embed()` requires `&mut self`. Uses `spawn_blocking` to run
/// the synchronous ONNX inference off the async runtime.
#[cfg(feature = "vector-search-local")]
pub struct FastEmbedEmbedder {
    model: std::sync::Arc<std::sync::Mutex<fastembed::TextEmbedding>>,
    dimensions: usize,
}

#[cfg(feature = "vector-search-local")]
impl std::fmt::Debug for FastEmbedEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FastEmbedEmbedder")
            .field("dimensions", &self.dimensions)
            .finish()
    }
}

#[cfg(feature = "vector-search-local")]
impl FastEmbedEmbedder {
    /// Load a local fastembed model, validating that configured dimensions match the
    /// model's native dimensions. Respects `FASTEMBED_CACHE_DIR` for model storage.
    pub fn new(config: &EmbedderConfig) -> Result<Self, VectorError> {
        let model_enum = parse_embedding_model(config.model.as_deref())?;

        let model_info = fastembed::TextEmbedding::get_model_info(&model_enum)
            .map_err(|e| VectorError::EmbeddingError(format!("failed to get model info: {e}")))?;
        let dim = model_info.dim;

        if let Some(configured_dim) = config.dimensions {
            if configured_dim != dim {
                return Err(VectorError::EmbeddingError(format!(
                    "configured dimensions ({configured_dim}) do not match model dimensions ({dim})"
                )));
            }
        }

        let mut options =
            fastembed::TextInitOptions::new(model_enum).with_show_download_progress(true);

        if let Ok(cache_dir) = std::env::var("FASTEMBED_CACHE_DIR") {
            options = options.with_cache_dir(std::path::PathBuf::from(cache_dir));
        }

        let text_embedding = fastembed::TextEmbedding::try_new(options).map_err(|e| {
            VectorError::EmbeddingError(format!("failed to initialize fastembed model: {e}"))
        })?;

        Ok(Self {
            model: std::sync::Arc::new(std::sync::Mutex::new(text_embedding)),
            dimensions: dim,
        })
    }

    /// Run embedding inference on a background thread via `spawn_blocking` to avoid
    /// blocking the async runtime; the model is mutex-guarded for thread safety.
    pub async fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let model = self.model.clone();
        let owned_texts: Vec<String> = texts.iter().map(|s| s.to_string()).collect();

        tokio::task::spawn_blocking(move || {
            let mut guard = model.lock().map_err(|e| {
                VectorError::EmbeddingError(format!("fastembed mutex poisoned: {e}"))
            })?;
            guard.embed(owned_texts, None).map_err(|e| {
                VectorError::EmbeddingError(format!("fastembed embedding failed: {e}"))
            })
        })
        .await
        .map_err(|e| VectorError::EmbeddingError(format!("fastembed task panicked: {e}")))?
    }

    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>, VectorError> {
        let results = self.embed_documents(&[text]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| VectorError::EmbeddingError("empty response from fastembed".into()))
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub fn source(&self) -> EmbedderSource {
        EmbedderSource::FastEmbed
    }
}

// ── Embedder Enum ──

/// Dispatch enum for all embedder types. Uses enum dispatch instead of
/// trait objects because async fn in traits is not dyn-safe in Rust 1.93.
#[derive(Debug)]
pub enum Embedder {
    UserProvided(UserProvidedEmbedder),
    Rest(Box<RestEmbedder>),
    OpenAi(Box<OpenAiEmbedder>),
    #[cfg(feature = "vector-search-local")]
    FastEmbed(Box<FastEmbedEmbedder>),
}

impl Embedder {
    pub async fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, VectorError> {
        match self {
            Embedder::UserProvided(e) => e.embed_documents(texts).await,
            Embedder::Rest(e) => e.embed_documents(texts).await,
            Embedder::OpenAi(e) => e.embed_documents(texts).await,
            #[cfg(feature = "vector-search-local")]
            Embedder::FastEmbed(e) => e.embed_documents(texts).await,
        }
    }

    pub async fn embed_query(&self, text: &str) -> Result<Vec<f32>, VectorError> {
        match self {
            Embedder::UserProvided(e) => e.embed_query(text).await,
            Embedder::Rest(e) => e.embed_query(text).await,
            Embedder::OpenAi(e) => e.embed_query(text).await,
            #[cfg(feature = "vector-search-local")]
            Embedder::FastEmbed(e) => e.embed_query(text).await,
        }
    }

    pub fn dimensions(&self) -> usize {
        match self {
            Embedder::UserProvided(e) => e.dimensions(),
            Embedder::Rest(e) => e.dimensions(),
            Embedder::OpenAi(e) => e.dimensions(),
            #[cfg(feature = "vector-search-local")]
            Embedder::FastEmbed(e) => e.dimensions(),
        }
    }

    pub fn source(&self) -> EmbedderSource {
        match self {
            Embedder::UserProvided(e) => e.source(),
            Embedder::Rest(e) => e.source(),
            Embedder::OpenAi(e) => e.source(),
            #[cfg(feature = "vector-search-local")]
            Embedder::FastEmbed(e) => e.source(),
        }
    }
}

/// Factory: validate required fields and create the appropriate embedder
/// variant. URL-policy ownership remains in `EmbedderConfig::validate`; REST
/// and OpenAI constructors do a narrow `vet_outbound_url_target` pass to pin
/// connect addresses for long-lived clients without persisting derived state.
pub fn create_embedder(config: &EmbedderConfig) -> Result<Embedder, VectorError> {
    config.validate_required_fields()?;
    match config.source {
        EmbedderSource::UserProvided => {
            let dims = config.dimensions.unwrap_or(0);
            Ok(Embedder::UserProvided(UserProvidedEmbedder::new(dims)))
        }
        EmbedderSource::Rest => {
            let embedder = RestEmbedder::new(config)?;
            Ok(Embedder::Rest(Box::new(embedder)))
        }
        EmbedderSource::OpenAi => {
            let embedder = OpenAiEmbedder::new(config)?;
            Ok(Embedder::OpenAi(Box::new(embedder)))
        }
        #[cfg(feature = "vector-search-local")]
        EmbedderSource::FastEmbed => {
            let embedder = FastEmbedEmbedder::new(config)?;
            Ok(Embedder::FastEmbed(Box::new(embedder)))
        }
        #[cfg(not(feature = "vector-search-local"))]
        EmbedderSource::FastEmbed => Err(VectorError::EmbeddingError(
            "local embedding (source: \"fastEmbed\") requires the server to be compiled with the `vector-search-local` feature".into(),
        )),
    }
}

#[cfg(test)]
#[path = "embedder_tests.rs"]
mod tests;
