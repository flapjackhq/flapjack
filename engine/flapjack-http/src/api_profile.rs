use std::collections::BTreeMap;
use std::fmt;

use flapjack::error::FlapjackError;

use crate::auth::{invalid_api_credentials_flapjack_error, ApiKey};
use crate::dto::BatchSearchRequest;

pub const FLAPJACK_API_PROFILE_ENV: &str = "FLAPJACK_API_PROFILE";
pub const PAID_BETA_V1_DIRECT_SEARCH_PATH: &str = "/1/indexes/*/queries";
pub const PAID_BETA_V1_APPLICATION_ID: &str = "flapjack";
pub const SUPPORTED_API_PROFILES: [&str; 2] = ["full", "paid_beta_v1"];
pub const PAID_BETA_V1_SEARCH_PARAMS: [&str; 6] = [
    "query",
    "page",
    "hitsPerPage",
    "facets",
    "facetFilters",
    "filters",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ApiProfile {
    #[default]
    Full,
    PaidBetaV1,
}

impl ApiProfile {
    pub fn from_optional_value(value: Option<&str>) -> Result<Self, ApiProfileConfigError> {
        match value {
            None | Some("full") => Ok(Self::Full),
            Some("paid_beta_v1") => Ok(Self::PaidBetaV1),
            Some(value) => Err(ApiProfileConfigError::UnknownValue(value.to_string())),
        }
    }

    pub fn from_env() -> Result<Self, ApiProfileConfigError> {
        match std::env::var(FLAPJACK_API_PROFILE_ENV) {
            Ok(value) => Self::from_optional_value(Some(&value)),
            Err(std::env::VarError::NotPresent) => Self::from_optional_value(None),
            Err(std::env::VarError::NotUnicode(value)) => Err(ApiProfileConfigError::UnknownValue(
                value.to_string_lossy().into_owned(),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::PaidBetaV1 => "paid_beta_v1",
        }
    }

    pub fn validate_auth_enabled(self, auth_enabled: bool) -> Result<(), ApiProfileConfigError> {
        if self == Self::PaidBetaV1 && !auth_enabled {
            Err(ApiProfileConfigError::AuthenticationRequired)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiProfileConfigError {
    UnknownValue(String),
    AuthenticationRequired,
}

impl fmt::Display for ApiProfileConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownValue(value) => write!(
                formatter,
                "{FLAPJACK_API_PROFILE_ENV} has unsupported value {value:?}; supported values are full and paid_beta_v1"
            ),
            Self::AuthenticationRequired => write!(
                formatter,
                "{FLAPJACK_API_PROFILE_ENV}=paid_beta_v1 requires authentication"
            ),
        }
    }
}

impl std::error::Error for ApiProfileConfigError {}

/// Marker inserted only after the PBV1 customer credential boundary succeeds.
/// Admin, dashboard-session, and replication-peer traffic never receives it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PaidBetaV1CustomerRequest;

fn invalid_batch(message: impl Into<String>) -> FlapjackError {
    FlapjackError::InvalidQuery(format!("Invalid paid_beta_v1 batch: {}", message.into()))
}

fn valid_nonnegative_integer(value: &serde_json::Value) -> bool {
    value
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
        .is_some()
}

/// Validate the closed PBV1 body before any query begins executing, then flatten
/// its object-valued `params` into the legacy internal batch representation.
pub(crate) fn prepare_paid_beta_v1_batch(
    body: serde_json::Value,
    api_key: Option<&ApiKey>,
) -> Result<BatchSearchRequest, FlapjackError> {
    let top = body
        .as_object()
        .ok_or_else(|| invalid_batch("body must be an object"))?;
    if top.len() != 1 || !top.contains_key("requests") {
        return Err(invalid_batch("body permits only requests"));
    }
    let requests = top["requests"]
        .as_array()
        .ok_or_else(|| invalid_batch("requests must be an array"))?;
    if requests.is_empty() {
        return Err(invalid_batch("requests must not be empty"));
    }

    let mut physical_index: Option<&str> = None;
    let mut normalized_requests = Vec::with_capacity(requests.len());
    for entry in requests {
        let entry = entry
            .as_object()
            .ok_or_else(|| invalid_batch("each request must be an object"))?;
        if entry.len() != 2 || !entry.contains_key("indexName") || !entry.contains_key("params") {
            return Err(invalid_batch(
                "each request permits only indexName and params",
            ));
        }
        let index_name = entry["indexName"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid_batch("indexName must be a non-empty string"))?;
        if physical_index.is_some_and(|expected| expected != index_name) {
            return Err(invalid_batch("all requests must use one indexName"));
        }
        physical_index = Some(index_name);

        let params = entry["params"]
            .as_object()
            .ok_or_else(|| invalid_batch("params must be an object"))?;
        for (name, value) in params {
            if !PAID_BETA_V1_SEARCH_PARAMS.contains(&name.as_str()) {
                return Err(invalid_batch(format!("unsupported parameter {name:?}")));
            }
            let valid = match name.as_str() {
                "query" | "filters" => value.is_string(),
                "page" => valid_nonnegative_integer(value),
                "hitsPerPage" => valid_nonnegative_integer(value) && value.as_u64() != Some(0),
                "facets" => value
                    .as_array()
                    .is_some_and(|values| values.iter().all(serde_json::Value::is_string)),
                "facetFilters" => value.is_array(),
                _ => unreachable!("the closed parameter inventory was checked above"),
            };
            if !valid {
                return Err(invalid_batch(format!(
                    "unsupported or invalid parameter {name:?}"
                )));
            }
        }

        let mut normalized = BTreeMap::new();
        normalized.insert(
            "indexName".to_string(),
            serde_json::Value::String(index_name.to_string()),
        );
        normalized.extend(
            params
                .iter()
                .map(|(name, value)| (name.clone(), value.clone())),
        );
        normalized_requests.push(normalized);
    }

    let physical_index = physical_index.expect("non-empty requests established an index");
    let api_key = api_key.ok_or_else(invalid_api_credentials_flapjack_error)?;
    let has_exact_acls = api_key.acl.len() == 2
        && api_key.acl.iter().any(|acl| acl == "search")
        && api_key.acl.iter().any(|acl| acl == "browse");
    let has_exact_index_scope = api_key.indexes.len() == 1 && api_key.indexes[0] == physical_index;
    if !has_exact_acls || !has_exact_index_scope {
        return Err(invalid_api_credentials_flapjack_error());
    }

    serde_json::from_value(serde_json::json!({"requests": normalized_requests}))
        .map_err(|error| invalid_batch(format!("could not normalize request: {error}")))
}
