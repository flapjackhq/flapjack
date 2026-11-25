use super::*;

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateIndexResponse {
    pub uid: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteIndexResponse {
    #[serde(rename = "taskID")]
    pub task_id: i64,
    pub deleted_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SaveObjectResponse {
    #[serde(rename = "taskID")]
    pub task_id: i64,
    #[serde(rename = "objectID")]
    pub object_id: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PutObjectResponse {
    #[serde(rename = "taskID")]
    pub task_id: i64,
    #[serde(rename = "objectID")]
    pub object_id: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeleteObjectResponse {
    #[serde(rename = "taskID")]
    pub task_id: i64,
    pub deleted_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PartialUpdateObjectResponse {
    #[serde(rename = "taskID")]
    pub task_id: i64,
    #[serde(rename = "objectID")]
    pub object_id: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchWriteResponse {
    #[serde(rename = "taskID")]
    pub task_id: i64,
    #[serde(rename = "objectIDs")]
    pub object_ids: Vec<String>,
}

/// Response variants for document ingestion endpoints.
///
/// - `Algolia` — single-index batch with a scalar `taskID`.
/// - `MultiIndexAlgolia` — multi-index batch with per-index `taskID` map.
/// - `Legacy` — Meilisearch-style response with `task_uid` and `status`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum AddDocumentsResponse {
    Algolia {
        #[serde(rename = "taskID")]
        task_id: i64,
        #[serde(rename = "objectIDs")]
        object_ids: Vec<String>,
    },
    MultiIndexAlgolia {
        #[serde(rename = "taskID")]
        task_id: HashMap<String, i64>,
        #[serde(rename = "objectIDs")]
        object_ids: Vec<String>,
    },
    Legacy {
        task_uid: String,
        status: String,
        received_documents: usize,
    },
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskResponse {
    pub task_uid: String,
    pub status: String,
    pub received_documents: usize,
    pub indexed_documents: usize,
    pub rejected_documents: Vec<DocFailureDto>,
    pub rejected_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocFailureDto {
    pub doc_id: String,
    pub error: String,
    pub message: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SearchHit {
    #[serde(flatten)]
    pub document: HashMap<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub _score: Option<f32>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchProcessingTimings {
    pub queue: u64,
    pub search: u64,
    pub highlight: u64,
    pub total: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchExhaustive {
    pub nb_hits: bool,
    pub typo: bool,
    pub facet_values: bool,
    pub rules_match: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facets_count: Option<bool>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SearchFacetStatsSummary {
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub sum: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SearchAppliedRule {
    #[serde(rename = "objectID")]
    pub object_id: String,
}

/// Algolia-compatible search response returned by query endpoints.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    #[serde(rename = "nbHits")]
    pub nb_hits: usize,
    pub page: usize,
    #[serde(rename = "nbPages")]
    pub nb_pages: usize,
    #[serde(rename = "hitsPerPage")]
    pub hits_per_page: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<usize>,
    #[serde(rename = "processingTimeMS")]
    pub processing_time_ms: u64,
    #[serde(rename = "serverTimeMS")]
    pub server_time_ms: u64,
    pub query: String,
    pub params: String,
    pub exhaustive: SearchExhaustive,
    #[serde(rename = "exhaustiveNbHits")]
    pub exhaustive_nb_hits: bool,
    #[serde(rename = "exhaustiveTypo")]
    pub exhaustive_typo: bool,
    pub index: String,
    #[schema(value_type = Object)]
    #[serde(rename = "renderingContent")]
    pub rendering_content: serde_json::Value,
    #[serde(rename = "serverUsed")]
    pub server_used: String,
    #[serde(rename = "_automaticInsights")]
    pub automatic_insights: bool,
    #[serde(rename = "processingTimingsMS")]
    pub processing_timings_ms: SearchProcessingTimings,
    #[serde(rename = "queryAfterRemoval", skip_serializing_if = "Option::is_none")]
    pub query_after_removal: Option<String>,
    #[serde(rename = "parsedQuery", skip_serializing_if = "Option::is_none")]
    pub parsed_query: Option<String>,
    #[serde(rename = "nbSortedHits", skip_serializing_if = "Option::is_none")]
    pub nb_sorted_hits: Option<usize>,
    #[serde(
        rename = "appliedRelevancyStrictness",
        skip_serializing_if = "Option::is_none"
    )]
    pub applied_relevancy_strictness: Option<u32>,
    #[serde(
        rename = "exhaustiveFacetsCount",
        skip_serializing_if = "Option::is_none"
    )]
    pub exhaustive_facets_count: Option<bool>,
    #[schema(value_type = Object)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facets: Option<HashMap<String, serde_json::Value>>,
    #[serde(rename = "facets_stats", skip_serializing_if = "Option::is_none")]
    pub facets_stats: Option<HashMap<String, SearchFacetStatsSummary>>,
    #[schema(value_type = Object)]
    #[serde(rename = "userData", skip_serializing_if = "Option::is_none")]
    pub user_data: Option<serde_json::Value>,
    #[serde(rename = "automaticRadius", skip_serializing_if = "Option::is_none")]
    pub automatic_radius: Option<String>,
    #[serde(rename = "appliedRules", skip_serializing_if = "Option::is_none")]
    pub applied_rules: Option<Vec<SearchAppliedRule>>,
    #[serde(rename = "queryID", skip_serializing_if = "Option::is_none")]
    pub query_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(rename = "abTestID", skip_serializing_if = "Option::is_none")]
    pub ab_test_id: Option<String>,
    #[serde(rename = "abTestVariantID", skip_serializing_if = "Option::is_none")]
    pub ab_test_variant_id: Option<String>,
    #[serde(rename = "interleavedTeams", skip_serializing_if = "Option::is_none")]
    pub interleaved_teams: Option<HashMap<String, String>>,
    #[serde(rename = "indexUsed", skip_serializing_if = "Option::is_none")]
    pub index_used: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GetObjectsResponse {
    pub results: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BatchSearchLegacyResponse {
    pub results: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum BatchSearchResponse {
    Legacy(BatchSearchLegacyResponse),
    Federated(crate::federation::FederatedResponse),
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchFacetValuesResponse {
    pub facet_hits: Vec<FacetHit>,
    pub exhaustive_facets_count: bool,
    #[serde(rename = "processingTimeMS")]
    pub processing_time_ms: u64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FacetHit {
    pub value: String,
    pub highlighted: String,
    pub count: u64,
}
