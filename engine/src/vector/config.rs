//! Stub summary for config.rs.
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::VectorError;

/// Source type for an embedder configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EmbedderSource {
    OpenAi,
    Rest,
    #[default]
    UserProvided,
    FastEmbed,
}

/// Configuration for creating an embedder.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct EmbedderConfig {
    pub source: EmbedderSource,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub dimensions: Option<usize>,
    pub url: Option<String>,
    pub request: Option<serde_json::Value>,
    pub response: Option<serde_json::Value>,
    pub headers: Option<HashMap<String, String>>,
    pub document_template: Option<String>,
    pub document_template_max_bytes: Option<usize>,
}

impl EmbedderConfig {
    /// Build a DocumentTemplate from this embedder's template configuration.
    pub fn document_template(&self) -> DocumentTemplate {
        DocumentTemplate::new(
            self.document_template.clone(),
            self.document_template_max_bytes,
        )
    }

    /// Full validation = required-fields check + outbound-URL safety check.
    ///
    /// The composite reads the SSOT env-var opt-in via
    /// `crate::security::allow_local_outbound_urls()`. Used by the intake
    /// gate at `settings.rs::validate_embedders_inner` and by the
    /// `IndexSettings::load` disk-load defense.
    ///
    /// Constructors call `validate_required_fields()` plus a targeted
    /// `vet_outbound_url_target` pass to pin connect addresses for long-lived
    /// clients, but they intentionally do not call this composite method.
    pub fn validate(&self) -> Result<(), VectorError> {
        self.validate_required_fields()?;
        self.validate_url_for_outbound(crate::security::allow_local_outbound_urls())?;
        Ok(())
    }

    /// Required-field validation only — no env access, no I/O, no network
    /// policy. This is the precondition embedder constructors enforce so
    /// they fail fast on a malformed `EmbedderConfig` (missing `apiKey`,
    /// missing `url`/`request`/`response`, etc.) before constructor-level
    /// address pinning runs.
    ///
    /// SSRF policy lives at the trust boundary (intake + disk-load); see
    /// `validate_url_for_outbound` and the doc on `validate()`.
    pub fn validate_required_fields(&self) -> Result<(), VectorError> {
        match self.source {
            EmbedderSource::OpenAi => {
                if self.api_key.is_none() {
                    return Err(VectorError::EmbeddingError(
                        "openAi embedder requires `apiKey`".into(),
                    ));
                }
            }
            EmbedderSource::Rest => {
                let mut missing = Vec::new();
                if self.url.is_none() {
                    missing.push("`url`");
                }
                if self.request.is_none() {
                    missing.push("`request`");
                }
                if self.response.is_none() {
                    missing.push("`response`");
                }
                if !missing.is_empty() {
                    return Err(VectorError::EmbeddingError(format!(
                        "rest embedder requires {}",
                        missing.join(", ")
                    )));
                }
            }
            EmbedderSource::UserProvided => {
                let Some(dimensions) = self.dimensions else {
                    return Err(VectorError::EmbeddingError(
                        "userProvided embedder requires `dimensions`".into(),
                    ));
                };
                if dimensions == 0 {
                    return Err(VectorError::EmbeddingError(
                        "userProvided embedder requires `dimensions` greater than 0".into(),
                    ));
                }
            }
            EmbedderSource::FastEmbed => {
                // No mandatory fields — model defaults to bge-small-en-v1.5.
                // Dimension validation happens in FastEmbedEmbedder::new() where
                // the model info is available.
            }
        }
        Ok(())
    }

    /// SSRF outbound-URL safety check, with explicit policy.
    ///
    /// `allow_local = false` is the production-default policy: loopback and
    /// RFC1918/ULA private destinations are rejected. `allow_local = true`
    /// is the operator opt-in that permits loopback/private targets for
    /// legitimate local-AI deployments (Ollama, vLLM, llama.cpp).
    ///
    /// The link-local / metadata / unspecified / broadcast class is rejected
    /// unconditionally — even under `allow_local = true`. The cloud
    /// metadata endpoint `169.254.169.254` is a pure SSRF target with no
    /// legitimate AI-provider use, and the opt-in must not silently widen
    /// to it. See `is_always_blocked_ip` vs. `is_local_network_ip` below.
    ///
    /// Sources that have no outbound URL (`UserProvided`, `FastEmbed`)
    /// are a no-op here — there is no SSRF surface to validate.
    pub fn validate_url_for_outbound(&self, allow_local: bool) -> Result<(), VectorError> {
        match self.source {
            EmbedderSource::OpenAi => {
                // OpenAi has a default base URL (https://api.openai.com); only
                // validate when the operator has supplied an override.
                if let Some(url) = self.url.as_deref() {
                    validate_outbound_url(url, "openAi", allow_local)?;
                }
            }
            EmbedderSource::Rest => {
                // Required-fields check guarantees self.url is Some by the
                // time the composite reaches us, but we tolerate a missing
                // URL here so callers can invoke the two methods in any
                // order without panicking on unwrap.
                if let Some(url) = self.url.as_deref() {
                    validate_outbound_url(url, "rest", allow_local)?;
                }
            }
            EmbedderSource::UserProvided | EmbedderSource::FastEmbed => {
                // No outbound URL — no SSRF surface.
            }
        }
        Ok(())
    }
}

/// Outbound-URL SSRF guard with explicit policy.
///
/// `allow_local`:
///   - `false` (production default) — rejects loopback, RFC1918/ULA private,
///     link-local, broadcast, and unspecified destinations.
///   - `true` (operator opt-in via `FLAPJACK_AI_ALLOW_LOCAL_URLS`) — permits
///     loopback and RFC1918/ULA private destinations for local-AI deployments
///     (Ollama, vLLM, llama.cpp). Link-local / metadata / unspecified /
///     broadcast remain rejected unconditionally — those are pure SSRF
///     targets with no legitimate AI-provider use, regardless of opt-in.
///
/// Mirrors the chat-side three-tier policy at
/// `flapjack-http::handlers::chat::validate_ai_base_url`, both via shared
/// `crate::security` helpers to keep classification behavior aligned.
fn validate_outbound_url(
    raw_url: &str,
    source: &str,
    allow_local: bool,
) -> Result<(), VectorError> {
    crate::security::vet_outbound_url_target(raw_url, allow_local)
        .map(|_| ())
        .map_err(|error| VectorError::EmbeddingError(format!("{source} embedder URL {error}")))
}

/// A single entry in the embedder fingerprint, capturing the semantic-relevant
/// fields of one embedder configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EmbedderFingerprintEntry {
    pub name: String,
    pub source: EmbedderSource,
    pub model: Option<String>,
    pub dimensions: usize,
    pub document_template: Option<String>,
    pub document_template_max_bytes: Option<usize>,
}

/// Fingerprint capturing all embedder configurations for a tenant.
/// Used to detect when embedder settings change, invalidating stored vectors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedderFingerprint {
    pub version: u32,
    pub embedders: Vec<EmbedderFingerprintEntry>,
}

impl EmbedderFingerprint {
    /// Build a fingerprint from the current embedder configs and actual dimensions
    /// from the VectorIndex.
    pub fn from_configs(configs: &[(String, EmbedderConfig)], actual_dimensions: usize) -> Self {
        let mut entries: Vec<EmbedderFingerprintEntry> = configs
            .iter()
            .map(|(name, config)| EmbedderFingerprintEntry {
                name: name.clone(),
                source: config.source,
                model: config.model.clone(),
                dimensions: actual_dimensions,
                document_template: config.document_template.clone(),
                document_template_max_bytes: config.document_template_max_bytes,
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            version: 1,
            embedders: entries,
        }
    }

    /// Check whether the current embedder configs match this fingerprint.
    /// Returns true if all semantic fields match (name, source, model, template).
    /// Dimensions: if config.dimensions is Some(n), checks n == entry.dimensions.
    /// If config.dimensions is None (auto-detect), skips dimension check.
    pub fn matches_configs(&self, configs: &[(String, EmbedderConfig)]) -> bool {
        let mut sorted: Vec<(String, &EmbedderConfig)> =
            configs.iter().map(|(n, c)| (n.clone(), c)).collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));

        if sorted.len() != self.embedders.len() {
            return false;
        }

        for (entry, (name, config)) in self.embedders.iter().zip(sorted.iter()) {
            if entry.name != *name {
                return false;
            }
            if entry.source != config.source {
                return false;
            }
            if entry.model != config.model {
                return false;
            }
            if entry.document_template != config.document_template {
                return false;
            }
            if entry.document_template_max_bytes != config.document_template_max_bytes {
                return false;
            }
            // Dimensions: only check if config specifies them (Some).
            // None means auto-detect — matches any stored dimensions.
            if let Some(dim) = config.dimensions {
                if dim != entry.dimensions {
                    return false;
                }
            }
        }

        true
    }

    /// Save fingerprint to `{dir}/fingerprint.json`.
    pub fn save(&self, dir: &std::path::Path) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("fingerprint.json");
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        crate::index::atomic_write_file(&path, json.as_bytes())
    }

    /// Load fingerprint from `{dir}/fingerprint.json`.
    pub fn load(dir: &std::path::Path) -> Result<Self, std::io::Error> {
        let path = dir.join("fingerprint.json");
        let data = std::fs::read_to_string(&path)?;
        serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// Document template for rendering searchable text from JSON documents.
pub struct DocumentTemplate {
    pub template: Option<String>,
    pub max_bytes: usize,
}

impl Default for DocumentTemplate {
    fn default() -> Self {
        Self {
            template: None,
            max_bytes: 400,
        }
    }
}

impl DocumentTemplate {
    pub fn new(template: Option<String>, max_bytes: Option<usize>) -> Self {
        Self {
            template,
            max_bytes: max_bytes.unwrap_or(400),
        }
    }

    /// Render a JSON document into a searchable text string.
    ///
    /// If a template is set, substitute `{{doc.field_name}}` patterns.
    /// If no template, concatenate all string values separated by `. `.
    /// Truncate to `max_bytes` at a UTF-8 boundary.
    pub fn render(&self, document: &serde_json::Value) -> String {
        let result = match &self.template {
            Some(tmpl) => Self::render_template(tmpl, document),
            None => Self::render_default(document),
        };
        truncate_utf8(&result, self.max_bytes)
    }

    /// Substitute `{{doc.field.path}}` placeholders with values from the document.
    fn render_template(template: &str, document: &serde_json::Value) -> String {
        let mut result = String::new();
        let mut rest = template;
        while let Some(start) = rest.find("{{doc.") {
            result.push_str(&rest[..start]);
            let after_open = &rest[start + 6..]; // skip "{{doc."
            if let Some(end) = after_open.find("}}") {
                let field_path = &after_open[..end];
                let value = resolve_path(document, field_path);
                result.push_str(value);
                rest = &after_open[end + 2..];
            } else {
                // No closing }}, copy the remainder literally and stop
                result.push_str(&rest[start..]);
                return result;
            }
        }
        result.push_str(rest);
        result
    }

    /// Default: concatenate all top-level user string values separated by `. `.
    /// Skips internal fields (`_id`, `objectID`) which carry no semantic meaning.
    fn render_default(document: &serde_json::Value) -> String {
        let obj = match document.as_object() {
            Some(o) => o,
            None => return String::new(),
        };
        let parts: Vec<&str> = obj
            .iter()
            .filter(|(k, _)| *k != "_id" && *k != "objectID")
            .filter_map(|(_, v)| v.as_str())
            .collect();
        parts.join(". ")
    }
}

/// Navigate a dot-separated path into a JSON value, returning the string value or "".
fn resolve_path<'a>(value: &'a serde_json::Value, path: &str) -> &'a str {
    let mut current = value;
    for key in path.split('.') {
        match current.get(key) {
            Some(v) => current = v,
            None => return "",
        }
    }
    current.as_str().unwrap_or("")
}

/// Truncate a string to at most `max_bytes` at a UTF-8 char boundary.
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    // Find the last valid char boundary at or before max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
