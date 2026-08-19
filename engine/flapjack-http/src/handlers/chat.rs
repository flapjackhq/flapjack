//! Stub summary for chat.rs.
use axum::{
    extract::{Extension, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    Json,
};
use serde::{Deserialize, Serialize};
use std::{convert::Infallible, sync::Arc};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use utoipa::ToSchema;

use super::AppState;
use crate::ai_provider::{AiChatRequest, AiProvider, AiProviderConfig, AiProviderError};
use crate::error_response::json_error;
use flapjack::index::SearchOptions;
use flapjack::types::field_value_to_json_value;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub query: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    #[schema(value_type = Option<Vec<Object>>)]
    pub conversation_history: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub stream: Option<bool>,
    /// Client-supplied conversation ID for multi-turn continuation.
    #[serde(default)]
    pub conversation_id: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChatResponse {
    pub answer: String,
    #[schema(value_type = Vec<Object>)]
    pub sources: Vec<serde_json::Value>,
    pub conversation_id: String,
    #[serde(rename = "queryID")]
    pub query_id: String,
}

/// Handle `POST /:indexName/chat` requests.
///
/// Validate that the target index exists and is in NeuralSearch mode, resolve the AI provider configuration, perform keyword retrieval for RAG context, and return either a JSON response or an SSE stream depending on the `Accept` header and `stream` flag.
#[utoipa::path(post, path = "/1/indexes/{indexName}/chat", tag = "chat",
    params(("indexName" = String, Path, description = "Index name")),
    request_body = ChatRequest,
    responses(
        (status = 200, description = "Chat answer (JSON) or SSE stream when stream=true", body = ChatResponse,
            content_type = "application/json"),
        (status = 404, description = "Index not found or not in neural search mode"),
        (status = 500, description = "Search or provider request failed")
    ),
    security(("api_key" = [])))]
pub async fn chat_index(
    State(state): State<Arc<AppState>>,
    Path(index_name): Path<String>,
    headers: HeaderMap,
    api_key: Option<Extension<crate::auth::ApiKey>>,
    Json(req): Json<ChatRequest>,
) -> Response {
    // Validate index exists and is in NeuralSearch mode.
    let settings = match state.manager.get_settings(&index_name) {
        Some(s) => s,
        None => {
            return json_error(
                StatusCode::NOT_FOUND,
                format!("Index '{}' does not exist", index_name),
            );
        }
    };

    if !settings.is_neural_search_active() {
        return json_error(
            StatusCode::NOT_FOUND,
            "Chat requires NeuralSearch mode. Set index mode to 'neuralSearch' via PUT /settings.",
        );
    }

    let provider_config = match resolve_provider_config(&settings, &req) {
        Ok(config) => config,
        Err(message) => return provider_error_response(AiProviderError::Configuration(message)),
    };

    // Use stub provider for testing when base_url is "stub", otherwise use real provider
    let provider = if provider_config.base_url == "stub" {
        AiProvider::stub()
    } else {
        AiProvider::openai_compatible(provider_config)
    };

    // Resolve or create conversation ID.
    let conversation_id = req
        .conversation_id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| format!("conv_{}", uuid_v4_simple()));
    let query_id = format!("q_{}", uuid_v4_simple());

    // Load bounded conversation history for multi-turn continuity.
    let history = state.conversation_store.get_history(&conversation_id);

    // RAG retrieval: same path for both JSON and SSE.
    let can_see_unretrievable_attributes = api_key.as_ref().is_some_and(|key| {
        key.0
            .acl
            .iter()
            .any(|acl| acl == "seeUnretrievableAttributes")
    });

    let (sources, snippets) = match retrieve_sources_and_snippets(
        &state,
        &settings,
        &index_name,
        &req.query,
        can_see_unretrievable_attributes,
    ) {
        Ok(result) => result,
        Err(e) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Search failed: {}", e),
            );
        }
    };

    let accept_sse = wants_sse(&headers, req.stream.unwrap_or(false));

    let ai_request = AiChatRequest {
        query: req.query.clone(),
        context_snippets: snippets,
        model_override: req.model.clone(),
        conversation_history: history,
    };

    if accept_sse {
        let stream_answer = match provider.stream_chunks(&ai_request).await {
            Ok(stream_answer) => stream_answer,
            Err(err) => return provider_error_response(err),
        };

        // Save the exchange asynchronously — we now have the exact answer from the provider.
        let conv_store = state.conversation_store.clone();
        let conv_id_clone = conversation_id.clone();
        let query_clone = req.query.clone();
        let answer_preview = stream_answer.answer;
        tokio::spawn(async move {
            conv_store.append_exchange(&conv_id_clone, query_clone, answer_preview);
        });

        return sse_from_chunks(stream_answer.chunks, query_id, conversation_id, sources)
            .into_response();
    }

    // JSON path.
    let answer = match provider.generate_answer(&ai_request).await {
        Ok(answer) => answer,
        Err(err) => return provider_error_response(err),
    };

    // Persist exchange in conversation store for future turns.
    state
        .conversation_store
        .append_exchange(&conversation_id, req.query.clone(), answer.clone());

    let response = ChatResponse {
        answer,
        sources,
        conversation_id,
        query_id,
    };

    (StatusCode::OK, Json(response)).into_response()
}

/// Perform keyword retrieval and build sources + text snippets for RAG context assembly.
fn retrieve_sources_and_snippets(
    state: &AppState,
    settings: &flapjack::index::settings::IndexSettings,
    index_name: &str,
    query: &str,
    can_see_unretrievable_attributes: bool,
) -> Result<(Vec<serde_json::Value>, Vec<String>), flapjack::FlapjackError> {
    let search_result = state.manager.search_with_options(
        index_name,
        query,
        &SearchOptions {
            limit: 10,
            ..Default::default()
        },
    )?;

    let sources: Vec<serde_json::Value> = search_result
        .documents
        .iter()
        .map(|scored_doc| {
            let mut doc_map = serde_json::Map::new();
            doc_map.insert(
                "objectID".to_string(),
                serde_json::Value::String(scored_doc.document.id.clone()),
            );
            for (key, value) in &scored_doc.document.fields {
                // Chat sources must honor the same retrieval ACLs as normal search hits.
                if !settings.should_retrieve_with_acl(key, can_see_unretrievable_attributes) {
                    continue;
                }
                doc_map.insert(key.clone(), field_value_to_json_value(value));
            }
            serde_json::Value::Object(doc_map)
        })
        .collect();

    let snippets: Vec<String> = search_result
        .documents
        .iter()
        .map(|scored_doc| {
            let parts: Vec<String> = scored_doc
                .document
                .fields
                .iter()
                .filter_map(|(key, value)| {
                    if !settings.should_retrieve_with_acl(key, can_see_unretrievable_attributes) {
                        return None;
                    }
                    if let flapjack::types::FieldValue::Text(text) = value {
                        Some(format!("{key}: {text}"))
                    } else {
                        None
                    }
                })
                .collect();
            if parts.is_empty() {
                scored_doc.document.id.clone()
            } else {
                parts.join(". ")
            }
        })
        .collect();

    Ok((sources, snippets))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexAiProviderSettings {
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
}

fn parse_index_provider_settings(
    settings: &flapjack::index::settings::IndexSettings,
) -> Option<IndexAiProviderSettings> {
    settings
        .user_data
        .as_ref()
        .and_then(|v| v.get("aiProvider"))
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}

/// Merge AI provider configuration from index-level settings, environment variables, and the incoming request.
///
/// Precedence (highest wins):
/// - `base_url` / `api_key`: index settings > environment variables.
/// - `model`: request body > index settings > environment variable > default `"gpt-4o-mini"`.
///
/// # Returns
///
/// Fully resolved `AiProviderConfig`, or a human-readable error string when required fields are missing.
fn resolve_provider_config(
    settings: &flapjack::index::settings::IndexSettings,
    req: &ChatRequest,
) -> Result<AiProviderConfig, String> {
    let env_base_url = normalized_non_empty(std::env::var("FLAPJACK_AI_BASE_URL").ok());
    let env_api_key = normalized_non_empty(std::env::var("FLAPJACK_AI_API_KEY").ok());
    let env_model = normalized_non_empty(std::env::var("FLAPJACK_AI_MODEL").ok());

    let index = parse_index_provider_settings(settings);

    let base_url = index
        .as_ref()
        .and_then(|cfg| normalized_non_empty(cfg.base_url.clone()))
        .or(env_base_url);
    let api_key = index
        .as_ref()
        .and_then(|cfg| normalized_non_empty(cfg.api_key.clone()))
        .or(env_api_key);
    let model = req
        .model
        .clone()
        .and_then(|value| normalized_non_empty(Some(value)))
        .or_else(|| {
            index
                .as_ref()
                .and_then(|cfg| normalized_non_empty(cfg.model.clone()))
        })
        .or(env_model)
        .unwrap_or_else(|| "gpt-4o-mini".to_string());

    if base_url.is_none() && api_key.is_none() {
        return Err("AI provider is not configured for this index".to_string());
    }
    if base_url.is_none() {
        return Err("Missing base URL for AI provider".to_string());
    }
    if api_key.is_none() {
        return Err("Missing API key for AI provider".to_string());
    }
    let resolved_base_url = base_url.expect("checked above");
    let vetted_target = validate_ai_base_url(&resolved_base_url)?;
    let pinned_connect_addrs = vetted_target.as_ref().map(|target| {
        let port = target.port.unwrap_or(443);
        target
            .resolved_ips
            .iter()
            .map(|ip| std::net::SocketAddr::new(*ip, port))
            .collect()
    });

    Ok(AiProviderConfig {
        base_url: resolved_base_url,
        api_key: api_key.expect("checked above"),
        model,
        pinned_connect_addrs,
    })
}

fn normalized_non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// TODO: Document validate_ai_base_url.
fn validate_ai_base_url(
    base_url: &str,
) -> Result<Option<flapjack::security::VettedOutboundUrlTarget>, String> {
    if base_url == "stub" {
        return Ok(None);
    }

    let allow_local = flapjack::security::allow_local_outbound_urls();
    // Close the validate->send TOCTOU window by returning the vetted resolution
    // result from this single policy-owner lookup, so the send path can pin it.
    flapjack::security::vet_outbound_url_target(base_url, allow_local)
        .map_err(|error| format!("Invalid base URL for AI provider: {error}"))
}

fn wants_sse(headers: &HeaderMap, stream_flag: bool) -> bool {
    if stream_flag {
        return true;
    }
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.to_ascii_lowercase().contains("text/event-stream"))
}

/// Send the full sequence of SSE events—one `chunk` per token, then `sources`, then `done`—over the given channel.
///
/// Stops early without error if the receiver is dropped.
///
/// # Returns
///
/// The number of events successfully sent.
async fn emit_sse_events(
    tx: mpsc::Sender<Result<Event, Infallible>>,
    chunks: Vec<String>,
    query_id: String,
    conversation_id: String,
    sources: Vec<serde_json::Value>,
) -> usize {
    let mut sent_count = 0usize;

    // Emit one `chunk` event per token.
    for chunk in chunks {
        let payload = serde_json::json!({
            "type": "chunk",
            "content": chunk,
            "queryID": query_id,
        })
        .to_string();

        if tx.send(Ok(Event::default().data(payload))).await.is_err() {
            return sent_count;
        }
        sent_count += 1;
    }

    // Emit the `sources` event after all chunks (before `done`).
    let sources_payload = serde_json::json!({
        "type": "sources",
        "sources": sources,
        "queryID": query_id,
    })
    .to_string();
    if tx
        .send(Ok(Event::default().data(sources_payload)))
        .await
        .is_err()
    {
        return sent_count;
    }
    sent_count += 1;

    // Emit the terminal `done` event.
    let done_payload = serde_json::json!({
        "type": "done",
        "queryID": query_id,
        "conversationId": conversation_id,
    })
    .to_string();
    if tx
        .send(Ok(Event::default().data(done_payload)))
        .await
        .is_ok()
    {
        sent_count += 1;
    }

    sent_count
}

fn sse_from_chunks(
    chunks: Vec<String>,
    query_id: String,
    conversation_id: String,
    sources: Vec<serde_json::Value>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        let _ = emit_sse_events(tx, chunks, query_id, conversation_id, sources).await;
    });

    Sse::new(ReceiverStream::new(rx))
}

fn provider_error_response(err: AiProviderError) -> Response {
    match err {
        AiProviderError::Configuration(message) => json_error(StatusCode::BAD_REQUEST, message),
        AiProviderError::Upstream(message) => json_error(StatusCode::BAD_GATEWAY, message),
    }
}

/// Generate a canonical UUID v4 string for chat conversations.
fn uuid_v4_simple() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
