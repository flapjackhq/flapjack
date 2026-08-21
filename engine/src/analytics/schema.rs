//! Analytics event types (search and insight), their Arrow/Parquet schemas, and Algolia-spec validation logic.
use arrow::datatypes::{DataType, Field, Schema};
use std::sync::Arc;

/// Recorded automatically on every search request.
#[derive(Debug, Clone)]
pub struct SearchEvent {
    pub timestamp_ms: i64,
    pub query: String,
    pub query_id: Option<String>,
    pub index_name: String,
    pub nb_hits: u32,
    pub processing_time_ms: u32,
    pub user_token: Option<String>,
    pub user_ip: Option<String>,
    pub filters: Option<String>,
    pub facets: Option<String>,
    pub analytics_tags: Option<String>,
    pub page: u32,
    pub hits_per_page: u32,
    pub has_results: bool,
    pub country: Option<String>,
    pub region: Option<String>,
    pub experiment_id: Option<String>,
    pub variant_id: Option<String>,
    pub assignment_method: Option<String>,
}

/// Sent by client via Insights API (click, conversion, view events).
#[derive(Debug, Clone)]
pub struct InsightEvent {
    pub event_type: String,
    pub event_subtype: Option<String>,
    pub event_name: String,
    pub index: String,
    pub user_token: String,
    pub authenticated_user_token: Option<String>,
    pub query_id: Option<String>,
    pub object_ids: Vec<String>,
    pub object_ids_alt: Vec<String>,
    pub positions: Option<Vec<u32>>,
    pub timestamp: Option<i64>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub interleaving_team: Option<String>,
}

#[derive(serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
/// Wire representation used for both deserialization and generated API schema.
struct InsightEventWire {
    event_type: String,
    #[serde(default)]
    event_subtype: Option<String>,
    event_name: String,
    index: String,
    user_token: String,
    #[serde(default)]
    authenticated_user_token: Option<String>,
    #[serde(default, rename = "queryID", alias = "queryId")]
    query_id: Option<String>,
    #[serde(default, rename = "objectIDs", alias = "objectIds")]
    object_ids: Vec<String>,
    #[serde(default)]
    positions: Option<Vec<u32>>,
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    value: Option<f64>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    interleaving_team: Option<String>,
    #[serde(default)]
    object_data: Vec<InsightEventObjectData>,
}

#[derive(serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
struct InsightEventObjectData {
    #[serde(default, rename = "queryID", alias = "queryId")]
    query_id: Option<String>,
    #[serde(default)]
    price: Option<f64>,
    #[serde(default)]
    quantity: Option<u32>,
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for InsightEvent {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        <InsightEventWire as utoipa::PartialSchema>::schema()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for InsightEvent {
    fn schemas(
        schemas: &mut Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) {
        <InsightEventWire as utoipa::ToSchema>::schemas(schemas);
    }
}

impl<'de> serde::Deserialize<'de> for InsightEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let wire = InsightEventWire::deserialize(deserializer)?;
        let mut query_id = wire.query_id;
        if wire.event_subtype.as_deref() == Some("purchase") {
            if wire.object_ids.len() != 1 || wire.object_data.len() != 1 {
                return Err(D::Error::custom(
                    "purchase requires exactly one objectID and one matching objectData entry",
                ));
            }
            let object_data = &wire.object_data[0];
            let object_query_id = object_data
                .query_id
                .as_deref()
                .ok_or_else(|| D::Error::custom("purchase objectData.queryID is required"))?;
            if query_id
                .as_deref()
                .is_some_and(|top_level| top_level != object_query_id)
            {
                return Err(D::Error::custom(
                    "purchase queryID must match objectData.queryID",
                ));
            }
            object_data
                .price
                .filter(|price| price.is_finite() && *price > 0.0)
                .ok_or_else(|| D::Error::custom("purchase objectData.price must be positive"))?;
            object_data
                .quantity
                .filter(|quantity| *quantity > 0)
                .ok_or_else(|| D::Error::custom("purchase objectData.quantity must be positive"))?;
            wire.value
                .filter(|value| value.is_finite() && *value > 0.0)
                .ok_or_else(|| D::Error::custom("purchase value must be positive"))?;
            let currency = wire.currency.as_deref().ok_or_else(|| {
                D::Error::custom("purchase currency must be an ISO-4217 alphabetic code")
            })?;
            if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
                return Err(D::Error::custom(
                    "purchase currency must be an ISO-4217 alphabetic code",
                ));
            }
            query_id = Some(object_query_id.to_string());
        } else if !wire.object_data.is_empty() {
            return Err(D::Error::custom(
                "objectData is supported only for purchase conversion events",
            ));
        }

        Ok(Self {
            event_type: wire.event_type,
            event_subtype: wire.event_subtype,
            event_name: wire.event_name,
            index: wire.index,
            user_token: wire.user_token,
            authenticated_user_token: wire.authenticated_user_token,
            query_id,
            object_ids: wire.object_ids,
            object_ids_alt: Vec::new(),
            positions: wire.positions,
            timestamp: wire.timestamp,
            value: wire.value,
            currency: wire.currency,
            interleaving_team: wire.interleaving_team,
        })
    }
}

impl InsightEvent {
    /// Get the effective objectIDs (handles both camelCase variants from Algolia SDK).
    pub fn effective_object_ids(&self) -> &[String] {
        if !self.object_ids.is_empty() {
            &self.object_ids
        } else {
            &self.object_ids_alt
        }
    }

    /// Validate per Algolia spec.
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.event_type.as_str(), "click" | "conversion" | "view") {
            return Err(format!("Invalid eventType: {}", self.event_type));
        }
        if self.event_name.is_empty() || self.event_name.len() > 64 {
            return Err("eventName must be 1-64 characters".to_string());
        }
        validate_user_token(&self.user_token)?;
        let oids = self.effective_object_ids();
        if oids.is_empty() || oids.len() > 20 {
            return Err("objectIDs must have 1-20 items".to_string());
        }
        // For click events, positions are required and must match objectIDs length.
        if self.event_type == "click" {
            match &self.positions {
                None => return Err("positions required for click events".to_string()),
                Some(pos) if pos.len() != oids.len() => {
                    return Err("positions length must match objectIDs length".to_string());
                }
                Some(pos) if pos.contains(&0) => {
                    return Err("positions must be one-based positive integers".to_string());
                }
                _ => {}
            }
        }
        if let Some(ref subtype) = self.event_subtype {
            if self.event_type != "conversion" {
                return Err("eventSubtype is only valid for conversion events".to_string());
            }
            if subtype != "addToCart" && subtype != "purchase" {
                return Err("eventSubtype must be addToCart or purchase".to_string());
            }
        }
        if let Some(ref qid) = self.query_id {
            if qid.len() != 32 || !qid.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err("queryID must be 32-char hex string".to_string());
            }
        }
        if (self.event_type == "click" && self.query_id.is_some())
            || (self.event_type == "conversion"
                && self.event_subtype.as_deref() == Some("purchase"))
        {
            let parsed = uuid::Uuid::parse_str(&self.user_token)
                .map_err(|_| "selected after-search events require a UUID userToken".to_string())?;
            if !parsed
                .hyphenated()
                .to_string()
                .eq_ignore_ascii_case(&self.user_token)
            {
                return Err(
                    "selected after-search events require a hyphenated UUID userToken".to_string(),
                );
            }
        }
        // Reject events older than 4 days
        if let Some(ts) = self.timestamp {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let four_days_ms = 4 * 24 * 60 * 60 * 1000_i64;
            if ts < now_ms - four_days_ms {
                return Err("timestamp must be within the last 4 days".to_string());
            }
        }
        // Validate interleaving team label matches search response values
        if let Some(ref team) = self.interleaving_team {
            if team != "control" && team != "variant" {
                return Err(format!(
                    "interleavingTeam must be \"control\" or \"variant\", got \"{}\"",
                    team
                ));
            }
        }
        Ok(())
    }
}

pub fn validate_user_token(user_token: &str) -> Result<(), String> {
    if user_token.is_empty() || user_token.len() > 129 {
        return Err("userToken must be 1-129 characters".to_string());
    }
    if !user_token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("userToken must contain only [a-zA-Z0-9\\-_]".to_string());
    }
    Ok(())
}

pub const ROLLUP_SCHEMA_VERSION: &str = "1";

/// Rollup schema version as a numeric value for JSON-serialized manifest fields.
pub fn rollup_schema_version_u32() -> u32 {
    ROLLUP_SCHEMA_VERSION
        .parse::<u32>()
        .expect("ROLLUP_SCHEMA_VERSION must be a valid u32 literal")
}

/// Arrow schema for pre-aggregated search rollup data (per query, per time window).
pub fn search_rollup_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("window_start_ms", DataType::Int64, false),
        Field::new("window_end_ms", DataType::Int64, false),
        Field::new("query", DataType::Utf8, false),
        Field::new("count", DataType::Int64, false),
        Field::new("nb_hits_sum", DataType::Int64, false),
        Field::new("nb_hits_count", DataType::Int64, false),
        Field::new("no_results_count", DataType::Int64, false),
        Field::new("has_results_count", DataType::Int64, false),
        Field::new("unique_users_hll", DataType::Binary, true),
    ]))
}

/// Arrow schema for search events stored in Parquet.
pub fn search_event_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("timestamp_ms", DataType::Int64, false),
        Field::new("query", DataType::Utf8, false),
        Field::new("query_id", DataType::Utf8, true),
        Field::new("index_name", DataType::Utf8, false),
        Field::new("nb_hits", DataType::UInt32, false),
        Field::new("processing_time_ms", DataType::UInt32, false),
        Field::new("user_token", DataType::Utf8, true),
        Field::new("user_ip", DataType::Utf8, true),
        Field::new("filters", DataType::Utf8, true),
        Field::new("facets", DataType::Utf8, true),
        Field::new("analytics_tags", DataType::Utf8, true),
        Field::new("page", DataType::UInt32, false),
        Field::new("hits_per_page", DataType::UInt32, false),
        Field::new("has_results", DataType::Boolean, false),
        Field::new("country", DataType::Utf8, true),
        Field::new("region", DataType::Utf8, true),
        Field::new("experiment_id", DataType::Utf8, true),
        Field::new("variant_id", DataType::Utf8, true),
        Field::new("assignment_method", DataType::Utf8, true),
    ]))
}

/// Arrow schema for insight events (clicks, conversions, views) stored in Parquet.
pub fn insight_event_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("timestamp_ms", DataType::Int64, false),
        Field::new("event_type", DataType::Utf8, false),
        Field::new("event_subtype", DataType::Utf8, true),
        Field::new("event_name", DataType::Utf8, false),
        Field::new("index_name", DataType::Utf8, false),
        Field::new("user_token", DataType::Utf8, false),
        Field::new("authenticated_user_token", DataType::Utf8, true),
        Field::new("query_id", DataType::Utf8, true),
        Field::new("object_ids", DataType::Utf8, false), // JSON array string
        Field::new("positions", DataType::Utf8, true),   // JSON array string
        Field::new("value", DataType::Float64, true),
        Field::new("currency", DataType::Utf8, true),
        Field::new("interleaving_team", DataType::Utf8, true),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a minimal valid click `InsightEvent` for use as a test fixture.
    fn valid_event() -> InsightEvent {
        InsightEvent {
            event_type: "click".to_string(),
            event_subtype: None,
            event_name: "Product Clicked".to_string(),
            index: "products".to_string(),
            user_token: "018f6b5e-4d3c-7a21-8b9c-0123456789ab".to_string(),
            authenticated_user_token: None,
            query_id: None,
            object_ids: vec!["obj1".to_string()],
            object_ids_alt: vec![],
            positions: Some(vec![1]),
            timestamp: None,
            value: None,
            currency: None,
            interleaving_team: None,
        }
    }

    // ── effective_object_ids ────────────────────────────────────────────

    #[test]
    fn effective_oids_prefers_object_ids() {
        let mut e = valid_event();
        e.object_ids = vec!["a".to_string()];
        e.object_ids_alt = vec!["b".to_string()];
        assert_eq!(e.effective_object_ids(), &["a"]);
    }

    #[test]
    fn effective_oids_falls_back_to_alt() {
        let mut e = valid_event();
        e.object_ids = vec![];
        e.object_ids_alt = vec!["b".to_string()];
        assert_eq!(e.effective_object_ids(), &["b"]);
    }

    // ── validate: event_type ────────────────────────────────────────────

    #[test]
    fn validate_click_ok() {
        assert!(valid_event().validate().is_ok());
    }

    #[test]
    fn validate_conversion_ok() {
        let mut e = valid_event();
        e.event_type = "conversion".to_string();
        assert!(e.validate().is_ok());
    }

    #[test]
    fn validate_view_ok() {
        let mut e = valid_event();
        e.event_type = "view".to_string();
        assert!(e.validate().is_ok());
    }

    #[test]
    fn validate_invalid_event_type() {
        let mut e = valid_event();
        e.event_type = "hover".to_string();
        assert!(e.validate().is_err());
    }

    // ── validate: event_name ────────────────────────────────────────────

    #[test]
    fn validate_empty_event_name() {
        let mut e = valid_event();
        e.event_name = "".to_string();
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_event_name_too_long() {
        let mut e = valid_event();
        e.event_name = "x".repeat(65);
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_event_name_at_max_64() {
        let mut e = valid_event();
        e.event_name = "x".repeat(64);
        assert!(e.validate().is_ok());
    }

    // ── validate: user_token ────────────────────────────────────────────

    #[test]
    fn validate_empty_user_token() {
        let mut e = valid_event();
        e.user_token = "".to_string();
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_user_token_too_long() {
        let mut e = valid_event();
        e.user_token = "x".repeat(130);
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_user_token_invalid_chars() {
        let mut e = valid_event();
        e.user_token = "user@email.com".to_string();
        assert!(e.validate().is_err());
    }

    // ── validate: object_ids ────────────────────────────────────────────

    #[test]
    fn validate_no_object_ids() {
        let mut e = valid_event();
        e.object_ids = vec![];
        e.object_ids_alt = vec![];
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_too_many_object_ids() {
        let mut e = valid_event();
        e.object_ids = (0..21).map(|i| format!("obj{}", i)).collect();
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_20_object_ids_ok() {
        let mut e = valid_event();
        e.object_ids = (0..20).map(|i| format!("obj{}", i)).collect();
        e.positions = Some((1..=20).collect());
        assert!(e.validate().is_ok());
    }

    // ── validate: click-after-search positions ──────────────────────────

    #[test]
    fn validate_click_with_query_id_needs_positions() {
        let mut e = valid_event();
        e.event_type = "click".to_string();
        e.query_id = Some("a".repeat(32));
        e.positions = None;
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_click_without_query_id_needs_positions() {
        let mut e = valid_event();
        e.event_type = "click".to_string();
        e.query_id = None;
        e.positions = None;
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_click_with_query_id_positions_length_mismatch() {
        let mut e = valid_event();
        e.event_type = "click".to_string();
        e.query_id = Some("a".repeat(32));
        e.object_ids = vec!["obj1".to_string(), "obj2".to_string()];
        e.positions = Some(vec![1]); // mismatch: 2 objects, 1 position
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_click_with_query_id_positions_match() {
        let mut e = valid_event();
        e.event_type = "click".to_string();
        e.query_id = Some("a".repeat(32));
        e.positions = Some(vec![1]);
        assert!(e.validate().is_ok());
    }

    // ── validate: event_subtype ────────────────────────────────────────

    #[test]
    fn validate_event_subtype_rejected_for_non_conversion() {
        let mut e = valid_event();
        e.event_type = "click".to_string();
        e.event_subtype = Some("addToCart".to_string());
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_event_subtype_rejects_invalid_conversion_value() {
        let mut e = valid_event();
        e.event_type = "conversion".to_string();
        e.event_subtype = Some("invalid".to_string());
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_event_subtype_accepts_purchase_on_conversion() {
        let mut e = valid_event();
        e.event_type = "conversion".to_string();
        e.event_subtype = Some("purchase".to_string());
        assert!(e.validate().is_ok());
    }

    #[test]
    fn validate_event_subtype_accepts_add_to_cart_on_conversion() {
        let mut e = valid_event();
        e.event_type = "conversion".to_string();
        e.event_subtype = Some("addToCart".to_string());
        assert!(e.validate().is_ok());
    }

    // ── validate: query_id format ───────────────────────────────────────

    #[test]
    fn validate_query_id_not_32_chars() {
        let mut e = valid_event();
        e.query_id = Some("abc".to_string());
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_query_id_non_hex() {
        let mut e = valid_event();
        e.query_id = Some("g".repeat(32));
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_query_id_valid_hex() {
        let mut e = valid_event();
        // Don't need positions since this is not a click-after-search
        e.event_type = "view".to_string();
        e.query_id = Some("abcdef0123456789abcdef0123456789".to_string());
        assert!(e.validate().is_ok());
    }

    // ── validate: timestamp ─────────────────────────────────────────────

    #[test]
    fn validate_recent_timestamp_ok() {
        let mut e = valid_event();
        e.timestamp = Some(chrono::Utc::now().timestamp_millis() - 1000);
        assert!(e.validate().is_ok());
    }

    #[test]
    fn validate_old_timestamp_rejected() {
        let mut e = valid_event();
        // 5 days ago
        let five_days_ms = 5 * 24 * 60 * 60 * 1000_i64;
        e.timestamp = Some(chrono::Utc::now().timestamp_millis() - five_days_ms);
        assert!(e.validate().is_err());
    }

    // ── Arrow schemas ───────────────────────────────────────────────────

    #[test]
    fn search_event_schema_has_19_fields() {
        let schema = search_event_schema();
        assert_eq!(schema.fields().len(), 19);
    }

    #[test]
    fn search_event_schema_has_experiment_id_field() {
        let schema = search_event_schema();
        let field = schema.field_with_name("experiment_id").unwrap();
        assert!(field.is_nullable());
        assert_eq!(*field.data_type(), DataType::Utf8);
    }

    #[test]
    fn search_event_schema_has_variant_id_field() {
        let schema = search_event_schema();
        let field = schema.field_with_name("variant_id").unwrap();
        assert!(field.is_nullable());
        assert_eq!(*field.data_type(), DataType::Utf8);
    }

    #[test]
    fn search_event_schema_has_assignment_method_field() {
        let schema = search_event_schema();
        let field = schema.field_with_name("assignment_method").unwrap();
        assert!(field.is_nullable());
        assert_eq!(*field.data_type(), DataType::Utf8);
    }

    #[test]
    fn insight_event_schema_has_13_fields() {
        let schema = insight_event_schema();
        assert_eq!(schema.fields().len(), 13);
    }

    #[test]
    fn insight_event_schema_has_interleaving_team_field() {
        let schema = insight_event_schema();
        let field = schema.field_with_name("interleaving_team").unwrap();
        assert!(field.is_nullable());
        assert_eq!(*field.data_type(), DataType::Utf8);
    }

    #[test]
    fn insight_event_deserializes_interleaving_team() {
        let json = r#"{"eventType":"click","eventName":"Clicked","index":"products","userToken":"user1","objectIDs":["obj1"],"interleavingTeam":"control"}"#;
        let event: InsightEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.interleaving_team.as_deref(), Some("control"));
    }

    #[test]
    fn insight_event_without_interleaving_team_defaults_to_none() {
        let json = r#"{"eventType":"click","eventName":"Clicked","index":"products","userToken":"user1","objectIDs":["obj1"]}"#;
        let event: InsightEvent = serde_json::from_str(json).unwrap();
        assert!(event.interleaving_team.is_none());
    }

    #[test]
    fn validate_interleaving_team_control_accepted() {
        let mut ev = valid_event();
        ev.interleaving_team = Some("control".to_string());
        assert!(ev.validate().is_ok());
    }

    #[test]
    fn validate_interleaving_team_variant_accepted() {
        let mut ev = valid_event();
        ev.interleaving_team = Some("variant".to_string());
        assert!(ev.validate().is_ok());
    }

    #[test]
    fn validate_interleaving_team_none_accepted() {
        let ev = valid_event();
        assert!(ev.interleaving_team.is_none());
        assert!(ev.validate().is_ok());
    }

    #[test]
    fn validate_interleaving_team_rejects_arbitrary_value() {
        let mut ev = valid_event();
        ev.interleaving_team = Some("A".to_string());
        let err = ev.validate().unwrap_err();
        assert!(
            err.contains("interleavingTeam"),
            "error should mention field name: {err}"
        );
    }

    #[test]
    fn search_event_schema_timestamp_is_i64() {
        let schema = search_event_schema();
        let field = schema.field_with_name("timestamp_ms").unwrap();
        assert_eq!(*field.data_type(), DataType::Int64);
    }

    // ── InsightEvent deserialization ─────────────────────────────────────

    #[test]
    fn insight_event_deserializes_from_json() {
        let json = r#"{"eventType":"click","eventName":"Clicked","index":"products","userToken":"user1","objectIDs":["obj1"]}"#;
        let event: InsightEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.event_type, "click");
        assert_eq!(event.effective_object_ids(), &["obj1"]);
    }

    #[test]
    fn insight_event_deserializes_alt_object_ids() {
        let json = r#"{"eventType":"click","eventName":"Clicked","index":"products","userToken":"user1","objectIDs":["obj1"]}"#;
        let event: InsightEvent = serde_json::from_str(json).unwrap();
        assert_eq!(event.effective_object_ids(), &["obj1"]);
    }

    #[test]
    fn insight_event_deserializes_query_id_from_queryid() {
        let json = r#"{"eventType":"click","eventName":"Clicked","index":"products","userToken":"018f6b5e-4d3c-7a21-8b9c-0123456789ab","queryID":"abcdef0123456789abcdef0123456789","objectIDs":["obj1"],"positions":[1]}"#;
        let event: InsightEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            event.query_id.as_deref(),
            Some("abcdef0123456789abcdef0123456789")
        );
    }

    #[test]
    fn insight_event_deserializes_query_id_from_queryid_alias() {
        let json = r#"{"eventType":"click","eventName":"Clicked","index":"products","userToken":"018f6b5e-4d3c-7a21-8b9c-0123456789ab","queryId":"abcdef0123456789abcdef0123456789","objectIDs":["obj1"],"positions":[1]}"#;
        let event: InsightEvent = serde_json::from_str(json).unwrap();
        assert_eq!(
            event.query_id.as_deref(),
            Some("abcdef0123456789abcdef0123456789")
        );
    }

    #[test]
    fn pbv3_official_purchase_shape_normalizes_object_data_query_id() {
        let json = r#"{
            "eventType":"conversion",
            "eventSubtype":"purchase",
            "eventName":"Purchased",
            "index":"products",
            "userToken":"018f6b5e-4d3c-7a21-8b9c-0123456789ab",
            "objectIDs":["sku-1"],
            "objectData":[{
                "queryID":"abcdef0123456789abcdef0123456789",
                "price":19.95,
                "quantity":2
            }],
            "value":39.9,
            "currency":"USD"
        }"#;
        let event: InsightEvent = serde_json::from_str(json).unwrap();

        assert!(event.validate().is_ok());
        assert_eq!(
            event.query_id.as_deref(),
            Some("abcdef0123456789abcdef0123456789")
        );
        assert_eq!(event.effective_object_ids(), &["sku-1"]);
        assert_eq!(event.value, Some(39.9));
        assert_eq!(event.currency.as_deref(), Some("USD"));
    }

    #[test]
    fn pbv3_purchase_rejects_invalid_commercial_fields() {
        let valid = serde_json::json!({
            "eventType": "conversion",
            "eventSubtype": "purchase",
            "eventName": "Purchased",
            "index": "products",
            "userToken": "018f6b5e-4d3c-7a21-8b9c-0123456789ab",
            "objectIDs": ["sku-1"],
            "objectData": [{
                "queryID": "abcdef0123456789abcdef0123456789",
                "price": 19.95,
                "quantity": 2
            }],
            "value": 39.9,
            "currency": "USD"
        });
        let mut invalid_cases = Vec::new();

        let mut missing_object_data = valid.clone();
        missing_object_data
            .as_object_mut()
            .unwrap()
            .remove("objectData");
        invalid_cases.push(missing_object_data);

        let mut zero_price = valid.clone();
        zero_price["objectData"][0]["price"] = serde_json::json!(0);
        invalid_cases.push(zero_price);

        let mut zero_quantity = valid.clone();
        zero_quantity["objectData"][0]["quantity"] = serde_json::json!(0);
        invalid_cases.push(zero_quantity);

        let mut zero_value = valid.clone();
        zero_value["value"] = serde_json::json!(0);
        invalid_cases.push(zero_value);

        let mut malformed_currency = valid;
        malformed_currency["currency"] = serde_json::json!("usd");
        invalid_cases.push(malformed_currency);

        for invalid in invalid_cases {
            assert!(serde_json::from_value::<InsightEvent>(invalid).is_err());
        }
    }

    #[test]
    fn pbv3_click_after_search_requires_positive_position_and_uuid() {
        let mut event = valid_event();
        event.query_id = Some("abcdef0123456789abcdef0123456789".to_string());
        event.positions = Some(vec![0]);
        assert!(event.validate().is_err());

        event.positions = Some(vec![1]);
        event.user_token = "not-a-uuid".to_string();
        assert!(event.validate().is_err());
    }

    #[cfg(feature = "openapi")]
    #[test]
    fn pbv3_openapi_schema_uses_official_wire_fields() {
        let schema = serde_json::to_value(<InsightEvent as utoipa::PartialSchema>::schema())
            .expect("InsightEvent schema must serialize");
        let properties = schema["properties"]
            .as_object()
            .expect("InsightEvent must remain an object schema");

        for field in [
            "eventType",
            "eventSubtype",
            "queryID",
            "objectIDs",
            "objectData",
        ] {
            assert!(properties.contains_key(field), "missing wire field {field}");
        }
        for forbidden in ["event_type", "query_id", "object_ids", "object_data"] {
            assert!(
                !properties.contains_key(forbidden),
                "internal field leaked into wire schema: {forbidden}"
            );
        }

        let mut dependencies = Vec::new();
        <InsightEvent as utoipa::ToSchema>::schemas(&mut dependencies);
        assert!(
            dependencies
                .iter()
                .any(|(name, _)| name == "InsightEventObjectData"),
            "objectData component must be registered"
        );
    }

    // ── Rollup schema ──────────────────────────────────────────────────

    #[test]
    fn rollup_schema_version_is_1() {
        assert_eq!(ROLLUP_SCHEMA_VERSION, "1");
    }

    #[test]
    fn rollup_schema_version_u32_is_1() {
        assert_eq!(rollup_schema_version_u32(), 1);
    }

    #[test]
    fn search_rollup_schema_has_9_fields() {
        let schema = search_rollup_schema();
        assert_eq!(schema.fields().len(), 9);
    }

    #[test]
    fn search_rollup_schema_window_start_ms() {
        let schema = search_rollup_schema();
        let field = schema.field_with_name("window_start_ms").unwrap();
        assert_eq!(*field.data_type(), DataType::Int64);
        assert!(!field.is_nullable());
    }

    #[test]
    fn search_rollup_schema_window_end_ms() {
        let schema = search_rollup_schema();
        let field = schema.field_with_name("window_end_ms").unwrap();
        assert_eq!(*field.data_type(), DataType::Int64);
        assert!(!field.is_nullable());
    }

    #[test]
    fn search_rollup_schema_query() {
        let schema = search_rollup_schema();
        let field = schema.field_with_name("query").unwrap();
        assert_eq!(*field.data_type(), DataType::Utf8);
        assert!(!field.is_nullable());
    }

    #[test]
    fn search_rollup_schema_count() {
        let schema = search_rollup_schema();
        let field = schema.field_with_name("count").unwrap();
        assert_eq!(*field.data_type(), DataType::Int64);
        assert!(!field.is_nullable());
    }

    #[test]
    fn search_rollup_schema_nb_hits_sum() {
        let schema = search_rollup_schema();
        let field = schema.field_with_name("nb_hits_sum").unwrap();
        assert_eq!(*field.data_type(), DataType::Int64);
        assert!(!field.is_nullable());
    }

    #[test]
    fn search_rollup_schema_nb_hits_count() {
        let schema = search_rollup_schema();
        let field = schema.field_with_name("nb_hits_count").unwrap();
        assert_eq!(*field.data_type(), DataType::Int64);
        assert!(!field.is_nullable());
    }

    #[test]
    fn search_rollup_schema_no_results_count() {
        let schema = search_rollup_schema();
        let field = schema.field_with_name("no_results_count").unwrap();
        assert_eq!(*field.data_type(), DataType::Int64);
        assert!(!field.is_nullable());
    }

    #[test]
    fn search_rollup_schema_has_results_count() {
        let schema = search_rollup_schema();
        let field = schema.field_with_name("has_results_count").unwrap();
        assert_eq!(*field.data_type(), DataType::Int64);
        assert!(!field.is_nullable());
    }

    #[test]
    fn search_rollup_schema_unique_users_hll() {
        let schema = search_rollup_schema();
        let field = schema.field_with_name("unique_users_hll").unwrap();
        assert_eq!(*field.data_type(), DataType::Binary);
        assert!(field.is_nullable());
    }
}
