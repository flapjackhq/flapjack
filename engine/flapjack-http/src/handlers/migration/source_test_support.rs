use super::algolia_client::{AlgoliaClientError, AlgoliaErrorKind, AlgoliaIndexRecord};
use super::meilisearch_client::{
    MeilisearchClientError, MeilisearchErrorKind, MeilisearchSourceObservation,
};
use super::source_identity_partitions::{
    SourceIdentityConfig, SourceIdentityError, CERTIFIED_MAX_ITEMS, DEFAULT_IDENTITY_BUDGET_BYTES,
    IDENTITY_V2_DOMAIN,
};
use super::source_reader::{
    MeilisearchExportSource, MeilisearchPageConsumer, MeilisearchSourceFuture,
    MigrationSourceReader, PageConsumer, SourceExportSink, SourceFuture, TypesenseExportSource,
    TypesensePageConsumer, TypesenseSourceFuture,
};
use super::source_snapshot::{source_item_hash, update_source_item_hash_digest};
use super::typesense_client::{
    TypesenseClientError, TypesenseErrorKind, TypesenseSourceObservation,
};
use crate::dto::SearchRequest;
use axum::{extract::State, Json};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, sync::Arc};
use tempfile::TempDir;

pub(super) fn identity_config_for_test(
) -> Result<(TempDir, SourceIdentityConfig), SourceIdentityError> {
    let spool_root = TempDir::new().map_err(SourceIdentityError::Io)?;
    let config = SourceIdentityConfig::for_test(
        spool_root.path(),
        DEFAULT_IDENTITY_BUDGET_BYTES,
        CERTIFIED_MAX_ITEMS,
    );
    Ok((spool_root, config))
}

#[derive(Default)]
pub(super) struct ScriptedMeilisearchSource {
    observations: VecDeque<MeilisearchSourceObservation>,
    settings: VecDeque<Value>,
    document_passes: VecDeque<Vec<Vec<Value>>>,
    access_error: Option<MeilisearchClientError>,
}

impl ScriptedMeilisearchSource {
    pub(super) fn with_passes(
        observation: MeilisearchSourceObservation,
        settings: Value,
        document_passes: Vec<Vec<Vec<Value>>>,
    ) -> Self {
        Self {
            observations: VecDeque::from(vec![observation.clone(); document_passes.len() + 1]),
            settings: VecDeque::from(vec![settings; document_passes.len()]),
            document_passes: VecDeque::from(document_passes),
            access_error: None,
        }
    }

    pub(super) fn with_access_error(mut self, error: MeilisearchClientError) -> Self {
        self.access_error = Some(error);
        self
    }

    pub(super) fn with_observations(
        mut self,
        observations: Vec<MeilisearchSourceObservation>,
    ) -> Self {
        self.observations = VecDeque::from(observations);
        self
    }
}

impl MeilisearchExportSource for ScriptedMeilisearchSource {
    fn observe_source(&mut self) -> MeilisearchSourceFuture<'_, MeilisearchSourceObservation> {
        Box::pin(async move {
            self.observations
                .pop_front()
                .ok_or_else(meilisearch_test_script_error)
        })
    }

    fn read_settings(&mut self) -> MeilisearchSourceFuture<'_, Value> {
        Box::pin(async move {
            self.settings
                .pop_front()
                .ok_or_else(meilisearch_test_script_error)
        })
    }

    fn require_read_access(&mut self) -> MeilisearchSourceFuture<'_, ()> {
        let error = self.access_error.clone();
        Box::pin(async move { error.map_or(Ok(()), Err) })
    }

    fn read_document_pages<'a>(
        &'a mut self,
        consume_page: &'a mut MeilisearchPageConsumer<'a>,
    ) -> MeilisearchSourceFuture<'a, MeilisearchSourceObservation> {
        Box::pin(async move {
            let pages = self
                .document_passes
                .pop_front()
                .ok_or_else(meilisearch_test_script_error)?;
            for page in pages {
                consume_page(page)?;
            }
            self.observations
                .front()
                .cloned()
                .ok_or_else(meilisearch_test_script_error)
        })
    }
}

pub(super) fn meilisearch_observation(
    source_name: &str,
    primary_key: &str,
    document_count: u64,
) -> MeilisearchSourceObservation {
    MeilisearchSourceObservation {
        source_name: source_name.to_string(),
        primary_key: primary_key.to_string(),
        updated_at: "2026-07-26T19:20:26Z".to_string(),
        document_count,
    }
}

fn meilisearch_test_script_error() -> MeilisearchClientError {
    MeilisearchClientError::new(
        MeilisearchErrorKind::Progress,
        "Meilisearch test source script exhausted",
    )
}

#[derive(Default)]
pub(super) struct ScriptedTypesenseSource {
    observations: VecDeque<TypesenseSourceObservation>,
    settings: VecDeque<Value>,
    document_passes: VecDeque<Vec<Vec<Value>>>,
    access_error: Option<TypesenseClientError>,
}

impl ScriptedTypesenseSource {
    pub(super) fn with_passes(
        observation: TypesenseSourceObservation,
        settings: Value,
        document_passes: Vec<Vec<Vec<Value>>>,
    ) -> Self {
        Self {
            observations: VecDeque::from(vec![observation.clone(); document_passes.len() + 1]),
            settings: VecDeque::from(vec![settings; document_passes.len()]),
            document_passes: VecDeque::from(document_passes),
            access_error: None,
        }
    }

    pub(super) fn with_access_error(mut self, error: TypesenseClientError) -> Self {
        self.access_error = Some(error);
        self
    }

    pub(super) fn with_observations(
        mut self,
        observations: Vec<TypesenseSourceObservation>,
    ) -> Self {
        self.observations = VecDeque::from(observations);
        self
    }
}

impl TypesenseExportSource for ScriptedTypesenseSource {
    fn observe_source(&mut self) -> TypesenseSourceFuture<'_, TypesenseSourceObservation> {
        Box::pin(async move {
            self.observations
                .pop_front()
                .ok_or_else(typesense_test_script_error)
        })
    }

    fn read_settings(&mut self) -> TypesenseSourceFuture<'_, Value> {
        Box::pin(async move {
            self.settings
                .pop_front()
                .ok_or_else(typesense_test_script_error)
        })
    }

    fn require_read_access(&mut self) -> TypesenseSourceFuture<'_, ()> {
        let error = self.access_error.clone();
        Box::pin(async move { error.map_or(Ok(()), Err) })
    }

    fn read_document_pages<'a>(
        &'a mut self,
        consume_page: &'a mut TypesensePageConsumer<'a>,
    ) -> TypesenseSourceFuture<'a, TypesenseSourceObservation> {
        Box::pin(async move {
            let pages = self
                .document_passes
                .pop_front()
                .ok_or_else(typesense_test_script_error)?;
            for page in pages {
                consume_page(page)?;
            }
            self.observations
                .front()
                .cloned()
                .ok_or_else(typesense_test_script_error)
        })
    }
}

pub(super) fn typesense_observation(
    source_name: &str,
    document_count: u64,
) -> TypesenseSourceObservation {
    TypesenseSourceObservation {
        source_name: source_name.to_string(),
        updated_at: "1785020400".to_string(),
        document_count,
        schema_hash: "typesense-test-schema-hash".to_string(),
    }
}

fn typesense_test_script_error() -> TypesenseClientError {
    TypesenseClientError::new(
        TypesenseErrorKind::Progress,
        "Typesense test source script exhausted",
    )
}

pub(super) struct ScriptedSourceReader {
    pub(super) app_id: String,
    pub(super) source_name: String,
    pub(super) quiescent_records: VecDeque<AlgoliaIndexRecord>,
    pub(super) settings_reads: VecDeque<Value>,
    pub(super) index_settings_reads: VecDeque<(String, Result<Value, AlgoliaClientError>)>,
    pub(super) document_reads: VecDeque<Vec<Vec<Value>>>,
    document_failures: VecDeque<Option<PageFailure>>,
    pub(super) rule_reads: VecDeque<Vec<Vec<Value>>>,
    pub(super) synonym_reads: VecDeque<Vec<Vec<Value>>>,
    pub(super) acl_checks: usize,
}

#[derive(Clone)]
struct PageFailure {
    completed_pages_before_failure: usize,
    error: AlgoliaClientError,
}

impl ScriptedSourceReader {
    pub(super) fn new(app_id: &str, source_name: &str) -> Self {
        Self {
            app_id: app_id.to_string(),
            source_name: source_name.to_string(),
            quiescent_records: VecDeque::new(),
            settings_reads: VecDeque::new(),
            index_settings_reads: VecDeque::new(),
            document_reads: VecDeque::new(),
            document_failures: VecDeque::new(),
            rule_reads: VecDeque::new(),
            synonym_reads: VecDeque::new(),
            acl_checks: 0,
        }
    }

    /// Queue one full traversal pass: a settings read plus document, rule, and
    /// synonym page groups consumed in order.
    pub(super) fn push_pass(
        &mut self,
        settings: Value,
        documents: Vec<Vec<Value>>,
        rules: Vec<Vec<Value>>,
        synonyms: Vec<Vec<Value>>,
    ) {
        self.settings_reads.push_back(settings);
        self.document_reads.push_back(documents);
        self.document_failures.push_back(None);
        self.rule_reads.push_back(rules);
        self.synonym_reads.push_back(synonyms);
    }

    pub(super) fn push_document_pass_failing_after_page(
        &mut self,
        settings: Value,
        documents: Vec<Vec<Value>>,
        completed_pages_before_failure: usize,
        error: AlgoliaClientError,
    ) {
        self.settings_reads.push_back(settings);
        self.document_reads.push_back(documents);
        self.document_failures.push_back(Some(PageFailure {
            completed_pages_before_failure,
            error,
        }));
        self.rule_reads.push_back(vec![]);
        self.synonym_reads.push_back(vec![]);
    }

    pub(super) fn push_quiescent(&mut self, record: AlgoliaIndexRecord) {
        self.quiescent_records.push_back(record);
    }

    /// Queue one expected replica settings read. The reader fails closed if the
    /// collector requests a name out of order or a name that was never queued.
    pub(super) fn push_index_settings(
        &mut self,
        expected_index_name: &str,
        result: Result<Value, AlgoliaClientError>,
    ) {
        self.index_settings_reads
            .push_back((expected_index_name.to_string(), result));
    }

    fn pop_value(queue: &mut VecDeque<Value>) -> SourceFuture<'_, Value> {
        Box::pin(async move {
            queue.pop_front().ok_or_else(|| {
                AlgoliaClientError::new(AlgoliaErrorKind::Progress, "test source script exhausted")
            })
        })
    }

    fn stream_pages<'a>(
        queue: &'a mut VecDeque<Vec<Vec<Value>>>,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Self::stream_pages_with_failure(queue, None, consume_page)
    }

    fn stream_pages_with_failure<'a>(
        queue: &'a mut VecDeque<Vec<Vec<Value>>>,
        failure: Option<PageFailure>,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            let pages = queue.pop_front().ok_or_else(|| {
                AlgoliaClientError::new(AlgoliaErrorKind::Progress, "test source script exhausted")
            })?;
            for (page_index, page) in pages.into_iter().enumerate() {
                consume_page(page)?;
                if let Some(failure) = &failure {
                    if page_index + 1 == failure.completed_pages_before_failure {
                        return Err(failure.error.clone());
                    }
                }
            }
            Ok(())
        })
    }
}

impl MigrationSourceReader for ScriptedSourceReader {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn source_name(&self) -> &str {
        &self.source_name
    }

    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord> {
        Box::pin(async move {
            self.quiescent_records.pop_front().ok_or_else(|| {
                AlgoliaClientError::new(AlgoliaErrorKind::Progress, "test source script exhausted")
            })
        })
    }

    fn read_settings(&mut self) -> SourceFuture<'_, Value> {
        Self::pop_value(&mut self.settings_reads)
    }

    fn read_index_settings<'a>(&'a mut self, index_name: &'a str) -> SourceFuture<'a, Value> {
        Box::pin(async move {
            let (expected, result) = self.index_settings_reads.pop_front().ok_or_else(|| {
                AlgoliaClientError::new(
                    AlgoliaErrorKind::Progress,
                    "test source index settings script exhausted",
                )
            })?;
            if expected != index_name {
                return Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Progress,
                    "test source index settings requested out of order",
                ));
            }
            result
        })
    }

    fn require_unretrievable_access<'a>(
        &'a mut self,
        _settings: &'a Value,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            self.acl_checks += 1;
            Ok(())
        })
    }

    fn read_documents<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        let failure = self.document_failures.pop_front().unwrap_or(None);
        Self::stream_pages_with_failure(&mut self.document_reads, failure, consume_page)
    }

    fn read_rules<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Self::stream_pages(&mut self.rule_reads, consume_page)
    }

    fn read_synonyms<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Self::stream_pages(&mut self.synonym_reads, consume_page)
    }
}

#[derive(Default)]
pub(super) struct RecordingSink {
    pub(super) settings: Vec<Value>,
    pub(super) document_pages: Vec<Vec<String>>,
    pub(super) raw_document_pages: Vec<Vec<Value>>,
    pub(super) rule_pages: Vec<Vec<String>>,
    pub(super) raw_rule_pages: Vec<Vec<Value>>,
    pub(super) synonym_pages: Vec<Vec<String>>,
    pub(super) raw_synonym_pages: Vec<Vec<Value>>,
}

impl SourceExportSink for RecordingSink {
    fn commit_settings(&mut self, settings: &Value) -> Result<(), AlgoliaClientError> {
        self.settings.push(settings.clone());
        Ok(())
    }

    fn commit_document_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.document_pages.push(page_object_ids(page));
        self.raw_document_pages.push(page.to_vec());
        Ok(())
    }

    fn commit_rule_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.rule_pages.push(page_object_ids(page));
        self.raw_rule_pages.push(page.to_vec());
        Ok(())
    }

    fn commit_synonym_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.synonym_pages.push(page_object_ids(page));
        self.raw_synonym_pages.push(page.to_vec());
        Ok(())
    }
}

pub(super) fn page_object_ids(page: &[Value]) -> Vec<String> {
    page.iter()
        .map(|item| {
            item.get("objectID")
                .and_then(Value::as_str)
                .expect("test fixtures should contain string objectID")
                .to_string()
        })
        .collect()
}

pub(super) fn expected_document_v2_digest(items: Vec<Value>, partition_count: u32) -> String {
    let mut partitions = vec![Vec::<(String, String)>::new(); partition_count as usize];
    for item in items {
        let object_id = item["objectID"].as_str().unwrap().to_string();
        let item_hash = source_item_hash(&item);
        let partition = expected_identity_partition(&object_id, partition_count);
        partitions[partition as usize].push((object_id, item_hash));
    }

    let mut identity = Sha256::new();
    identity.update(IDENTITY_V2_DOMAIN);
    identity.update(partition_count.to_string().as_bytes());
    identity.update(*b"\n");
    for (partition, tuples) in partitions.iter_mut().enumerate() {
        if tuples.is_empty() {
            continue;
        }
        tuples.sort_by(|left, right| left.0.cmp(&right.0));
        let mut partition_hasher = Sha256::new();
        for (object_id, item_hash) in tuples {
            update_source_item_hash_digest(&mut partition_hasher, object_id, item_hash);
        }
        identity.update(partition.to_string().as_bytes());
        identity.update([0]);
        identity.update(hex::encode(partition_hasher.finalize()).as_bytes());
        identity.update(*b"\n");
    }
    hex::encode(identity.finalize())
}

pub(super) fn duplicate_ids_in_different_identity_partitions(
    partition_count: u32,
) -> (String, u32, String, u32) {
    let mut seen = Vec::new();
    for index in 0..10_000 {
        let object_id = format!("partitioned-dup-{index}");
        let partition = expected_identity_partition(&object_id, partition_count);
        if let Some((other_id, other_partition)) = seen
            .iter()
            .find(|(_, other_partition)| *other_partition != partition)
            .cloned()
        {
            if other_partition < partition {
                return (other_id, other_partition, object_id, partition);
            }
            return (object_id, partition, other_id, other_partition);
        }
        seen.push((object_id, partition));
    }
    panic!("expected fixture search to find IDs in different partitions");
}

fn expected_identity_partition(object_id: &str, partition_count: u32) -> u32 {
    let digest = Sha256::digest(object_id.as_bytes());
    let first_eight = digest[..8].try_into().unwrap();
    (u64::from_be_bytes(first_eight) % u64::from(partition_count)) as u32
}

pub(super) async fn sorted_exact_hits_by_object_id<T>(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
    hits_per_page: usize,
    queryable_message: &str,
    extract_hit: impl FnMut(&Value) -> T,
) -> Vec<T> {
    let Json(search_response) = crate::handlers::search::search_single(
        State(Arc::clone(state)),
        target_index.to_string(),
        SearchRequest {
            query: String::new(),
            hits_per_page: Some(hits_per_page),
            ..Default::default()
        },
    )
    .await
    .expect(queryable_message);
    let hits = search_response["hits"]
        .as_array()
        .expect("search response should contain a hit array");
    assert_eq!(
        search_response["nbHits"],
        hits.len(),
        "reported hit count must equal the exact returned set"
    );

    let mut sorted_hits = hits.iter().collect::<Vec<_>>();
    sorted_hits.sort_by(|left, right| exact_hit_object_id(left).cmp(exact_hit_object_id(right)));
    sorted_hits.into_iter().map(extract_hit).collect()
}

fn exact_hit_object_id(hit: &Value) -> &str {
    hit["objectID"]
        .as_str()
        .expect("hit should contain string objectID")
}
