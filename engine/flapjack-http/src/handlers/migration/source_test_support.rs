use super::algolia_client::{AlgoliaClientError, AlgoliaErrorKind, AlgoliaIndexRecord};
use super::source_reader::{MigrationSourceReader, PageConsumer, SourceExportSink, SourceFuture};
use serde_json::Value;
use std::collections::VecDeque;

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
    pub(super) rule_pages: Vec<Vec<String>>,
    pub(super) synonym_pages: Vec<Vec<String>>,
}

impl SourceExportSink for RecordingSink {
    fn commit_settings(&mut self, settings: &Value) -> Result<(), AlgoliaClientError> {
        self.settings.push(settings.clone());
        Ok(())
    }

    fn commit_document_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.document_pages.push(page_object_ids(page));
        Ok(())
    }

    fn commit_rule_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.rule_pages.push(page_object_ids(page));
        Ok(())
    }

    fn commit_synonym_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.synonym_pages.push(page_object_ids(page));
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
