//! Stub summary for ai_provider.rs.
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(test)]
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct AiProviderConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub pinned_connect_addrs: Option<Vec<std::net::SocketAddr>>,
}

#[derive(Debug, Clone)]
pub struct AiChatRequest {
    pub query: String,
    pub context_snippets: Vec<String>,
    pub model_override: Option<String>,
    /// Prior conversation turns to include in the prompt (oldest-first).
    pub conversation_history: Vec<crate::conversation_store::ConversationMessage>,
}

#[derive(Debug)]
pub struct StreamAnswer {
    pub answer: String,
    pub chunks: Vec<String>,
}

#[derive(Debug)]
pub enum AiProviderError {
    Configuration(String),
    Upstream(String),
}

impl std::fmt::Display for AiProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiProviderError::Configuration(msg) => write!(f, "{msg}"),
            AiProviderError::Upstream(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for AiProviderError {}

#[derive(Debug, Clone)]
pub enum AiProvider {
    OpenAiCompatible(OpenAiCompatibleProvider),
    Stub(StubAiProvider),
}

impl AiProvider {
    pub fn openai_compatible(config: AiProviderConfig) -> Self {
        Self::OpenAiCompatible(OpenAiCompatibleProvider::new(config))
    }

    pub fn stub() -> Self {
        Self::Stub(StubAiProvider)
    }

    pub async fn generate_answer(
        &self,
        request: &AiChatRequest,
    ) -> Result<String, AiProviderError> {
        match self {
            AiProvider::OpenAiCompatible(provider) => provider.generate_answer(request).await,
            AiProvider::Stub(provider) => {
                Ok(provider.generate_answer(&request.query, &request.context_snippets))
            }
        }
    }

    pub async fn stream_chunks(
        &self,
        request: &AiChatRequest,
    ) -> Result<StreamAnswer, AiProviderError> {
        let answer = self.generate_answer(request).await?;
        let chunks = chunk_answer_for_sse(&answer);
        Ok(StreamAnswer { answer, chunks })
    }
}

/// Stub provider that concatenates search snippets into a synthetic answer.
/// No external API calls — suitable for contract testing and development.
#[derive(Debug, Clone, Copy)]
pub struct StubAiProvider;

impl StubAiProvider {
    pub fn generate_answer(&self, query: &str, context_snippets: &[String]) -> String {
        if context_snippets.is_empty() {
            return format!("No relevant results found for: {query}");
        }
        let joined = context_snippets.join(" | ");
        format!("Based on your search for \"{query}\": {joined}")
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
    config: AiProviderConfig,
}

impl OpenAiCompatibleProvider {
    pub fn new(config: AiProviderConfig) -> Self {
        Self {
            client: build_openai_client(&config),
            config,
        }
    }

    /// Send a non-streaming chat completion request to the configured endpoint and return the assistant's reply.
    ///
    /// Build the messages array from conversation history and context snippets, POST to
    /// the `/v1/chat/completions` endpoint using bearer-token auth, and extract
    /// `choices[0].message.content` from the response.
    ///
    /// # Arguments
    ///
    /// * `request` — Chat request containing the user query, context snippets, optional model override, and conversation history.
    ///
    /// # Returns
    ///
    /// The assistant's response text on success.
    ///
    /// # Errors
    ///
    /// Returns `AiProviderError::Upstream` if the HTTP request fails, the provider returns a non-2xx status, the response body cannot be deserialized, or the choices array is empty.
    async fn generate_answer(&self, request: &AiChatRequest) -> Result<String, AiProviderError> {
        let model = request
            .model_override
            .clone()
            .unwrap_or_else(|| self.config.model.clone());
        let messages = build_messages(request);
        let endpoint = normalize_chat_completions_endpoint(&self.config.base_url);

        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false
        });

        let response = self
            .client
            .post(endpoint)
            .bearer_auth(&self.config.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AiProviderError::Upstream(format!("AI request failed: {e}")))?;

        let response = response.error_for_status().map_err(|e| {
            AiProviderError::Upstream(format!("AI provider returned an error status: {e}"))
        })?;

        let payload: OpenAiCompletionResponse = response.json().await.map_err(|e| {
            AiProviderError::Upstream(format!("AI response JSON decode failed: {e}"))
        })?;

        let answer = payload
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .ok_or_else(|| {
                AiProviderError::Upstream("AI response missing choices[0].message.content".into())
            })?;

        Ok(answer)
    }
}

/// TODO: Document build_openai_client.
fn build_openai_client(config: &AiProviderConfig) -> reqwest::Client {
    let mut builder = reqwest::Client::builder();

    // Keep URL host in `base_url` as SSOT for endpoint construction + TLS/SNI.
    // Only pin the connect address list to the already-vetted resolution output.
    if let Some(host) = reqwest::Url::parse(&config.base_url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_string()))
    {
        if let Some(addrs) = &config.pinned_connect_addrs {
            builder = builder.resolve_to_addrs(&host, addrs);
        }
    }

    #[cfg(test)]
    if let Some(resolver) = take_test_dns_resolver() {
        return builder
            .dns_resolver2(resolver)
            .connect_timeout(Duration::from_millis(250))
            .timeout(Duration::from_millis(600))
            .build()
            .expect("failed to build OpenAI test client with custom DNS resolver");
    }

    builder
        .build()
        .expect("failed to build OpenAI client for outbound requests")
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
pub(crate) struct TestDnsResolverGuard {
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
pub(crate) fn install_test_dns_resolver(
    resolver: Arc<dyn reqwest::dns::Resolve>,
) -> TestDnsResolverGuard {
    let mut slot = test_dns_resolver_slot()
        .lock()
        .expect("test DNS resolver slot mutex poisoned");
    let previous = slot.replace(resolver);
    TestDnsResolverGuard { previous }
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiCompletionResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Debug, Deserialize, Serialize)]
struct OpenAiMessage {
    content: Option<String>,
}

/// Build the OpenAI-compatible messages array for a chat request.
///
/// Interleaves prior conversation history (user/assistant turns) with the current user message.
/// The current user message prepends retrieved context snippets when available.
fn build_messages(request: &AiChatRequest) -> Vec<serde_json::Value> {
    let mut messages: Vec<serde_json::Value> = request
        .conversation_history
        .iter()
        .map(|msg| serde_json::json!({"role": msg.role, "content": msg.content}))
        .collect();

    let current_content = if request.context_snippets.is_empty() {
        request.query.clone()
    } else {
        format!(
            "Question: {}\n\nContext:\n{}",
            request.query,
            request.context_snippets.join("\n")
        )
    };
    messages.push(serde_json::json!({"role": "user", "content": current_content}));
    messages
}

fn normalize_chat_completions_endpoint(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        return base.to_string();
    }
    if base.ends_with("/v1") {
        return format!("{base}/chat/completions");
    }
    format!("{base}/v1/chat/completions")
}

pub fn chunk_answer_for_sse(answer: &str) -> Vec<String> {
    answer
        .split_whitespace()
        .map(std::string::ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_with_snippets() {
        let provider = StubAiProvider;
        let answer =
            provider.generate_answer("rust", &["Rust is fast.".into(), "Rust is safe.".into()]);
        assert!(answer.contains("rust"));
        assert!(answer.contains("Rust is fast."));
        assert!(answer.contains("Rust is safe."));
    }

    #[test]
    fn stub_empty_snippets() {
        let provider = StubAiProvider;
        let answer = provider.generate_answer("test", &[]);
        assert!(answer.contains("No relevant results found"));
        assert!(answer.contains("test"));
    }

    #[test]
    fn normalize_endpoint_appends_v1_and_chat_path() {
        assert_eq!(
            normalize_chat_completions_endpoint("https://api.openai.com"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_chat_completions_endpoint("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            normalize_chat_completions_endpoint("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn chunk_answer_for_sse_splits_by_words() {
        let chunks = chunk_answer_for_sse("hello from provider");
        assert_eq!(chunks, vec!["hello", "from", "provider"]);
    }
}
