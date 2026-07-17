//! Shared in-memory `MigrationSourceReader` used by source-reader and export
//! orchestration tests, so crash/drift behavior is exercised without DNS or
//! vendor credentials. Each queue front is one full traversal pass; the two-pass
//! acceptance contract pops a pre-pass, an export-pass, and final quiescence.

use super::algolia_client::{AlgoliaClientError, AlgoliaErrorKind, AlgoliaIndexRecord};
use super::source_reader::{MigrationSourceReader, PageConsumer, SourceExportSink, SourceFuture};
use serde_json::Value;
use std::collections::VecDeque;

pub(super) struct ScriptedSourceReader {
    pub(super) app_id: String,
    pub(super) source_name: String,
    pub(super) quiescent_records: VecDeque<AlgoliaIndexRecord>,
    pub(super) settings_reads: VecDeque<Value>,
    pub(super) document_reads: VecDeque<Vec<Vec<Value>>>,
    pub(super) rule_reads: VecDeque<Vec<Vec<Value>>>,
    pub(super) synonym_reads: VecDeque<Vec<Vec<Value>>>,
    pub(super) acl_checks: usize,
}

impl ScriptedSourceReader {
    pub(super) fn new(app_id: &str, source_name: &str) -> Self {
        Self {
            app_id: app_id.to_string(),
            source_name: source_name.to_string(),
            quiescent_records: VecDeque::new(),
            settings_reads: VecDeque::new(),
            document_reads: VecDeque::new(),
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
        self.rule_reads.push_back(rules);
        self.synonym_reads.push_back(synonyms);
    }

    pub(super) fn push_quiescent(&mut self, record: AlgoliaIndexRecord) {
        self.quiescent_records.push_back(record);
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
        Box::pin(async move {
            let pages = queue.pop_front().ok_or_else(|| {
                AlgoliaClientError::new(AlgoliaErrorKind::Progress, "test source script exhausted")
            })?;
            for page in pages {
                consume_page(page)?;
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
        Self::stream_pages(&mut self.document_reads, consume_page)
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
