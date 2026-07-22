use super::translation_bundle::{
    translate_and_apply_primary_replicas, translate_document, translate_serde_value,
    translate_settings, ReplicaSettingsTranslation, TranslationBundle, TypedTranslationFailure,
};
use super::translation_report::{
    contains_hard_rejection, finalize_report, non_portable_product_entries,
    source_snapshot_violation_entry, ReportCode, ReportResource, TranslationReport,
    TranslationReportEntry,
};
use super::translation_schema::{validate_rule_page, validate_synonym_page};
use super::{push_typed_failure, validate_settings_payload};
use crate::handlers::migration::source_snapshot::SourceSnapshotBuilder;
use crate::handlers::migration::spool::{AcceptedSpoolPage, AcceptedSpoolReader, SpoolError};
use flapjack::index::settings::IndexSettings;
use flapjack::types::Document;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;
use std::convert::Infallible;

const MAX_DOCUMENT_BATCH_SIZE: usize = 1_000;

/// The settings inputs a translation session opens with: the primary source
/// settings it translates, plus the transient replica-owned settings it observes
/// (counted, never applied in this stage). Bundled so the page-streaming entry
/// points stay within the parameter budget.
struct TranslationSettingsInput {
    source_index_name: String,
    target_index_name: String,
    settings: Value,
    replica_settings: BTreeMap<String, Value>,
}

type SpoolResult<T> = Result<T, SpoolError>;
type TranslationStreamResult<T, E> = Result<T, TranslationStreamError<E>>;

#[derive(Debug, Clone, PartialEq)]
pub(in crate::handlers::migration) struct SpoolTranslationInput {
    pub(super) source_index_name: String,
    pub(super) target_index_name: String,
    pub(super) settings: Value,
    pub(super) document_pages: Vec<Vec<Value>>,
    pub(super) rule_pages: Vec<Vec<Value>>,
    pub(super) synonym_pages: Vec<Vec<Value>>,
    /// Replica-owned source settings carried to the translation entry point.
    /// Observation-only in Stage 1: counted, never applied to settings.
    pub(super) replica_settings: BTreeMap<String, Value>,
}

#[derive(Debug, Clone)]
pub(in crate::handlers::migration) enum TranslationOutcome {
    Translated(Box<TranslatedSpoolPayload>),
    Rejected(TranslationReport),
}

#[derive(Debug, Clone)]
pub(in crate::handlers::migration) enum SettingsTranslationOutcome {
    Translated(Box<IndexSettings>),
    Rejected(TranslationReport),
}

#[derive(Debug)]
pub(in crate::handlers::migration) enum TranslationStreamError<E> {
    Spool(SpoolError),
    Emit(E),
    Cancelled,
}

impl<E> From<SpoolError> for TranslationStreamError<E> {
    fn from(error: SpoolError) -> Self {
        Self::Spool(error)
    }
}

#[derive(Debug, Clone)]
pub(in crate::handlers::migration) struct TranslatedSpoolPayload {
    pub(in crate::handlers::migration) bundle: TranslationBundle,
    pub(in crate::handlers::migration) document_batches: Vec<Vec<Document>>,
    pub(in crate::handlers::migration) report: TranslationReport,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::handlers::migration) struct TranslationSessionInstrumentation {
    pub(super) document_pages_seen: usize,
    pub(super) max_live_decoded_pages: usize,
    pub(super) max_pending_documents: usize,
    pub(super) document_batches_emitted: Vec<usize>,
    /// Number of replica-owned settings maps that reached the translation entry
    /// point. Observation only — proves the carried map arrived without making
    /// translation a second settings owner.
    pub(super) replica_settings_count: usize,
    live_decoded_pages: usize,
}

pub(in crate::handlers::migration) fn translate_spool_payload(
    input: SpoolTranslationInput,
) -> TranslationOutcome {
    let mut instrumentation = TranslationSessionInstrumentation::default();
    match translate_spool_input(input, &mut instrumentation, |_| Ok::<(), Infallible>(())) {
        Ok(outcome) => outcome,
        Err(TranslationStreamError::Spool(error)) => {
            panic!("in-memory translation pages cannot fail: {error}")
        }
        Err(TranslationStreamError::Emit(_)) => unreachable!(),
        Err(TranslationStreamError::Cancelled) => {
            panic!("in-memory translation cannot observe migration cancellation")
        }
    }
}

pub(in crate::handlers::migration) fn translate_accepted_spool_payload<E>(
    reader: AcceptedSpoolReader,
    source_index_name: String,
    target_index_name: String,
    replica_settings: BTreeMap<String, Value>,
    instrumentation: &mut TranslationSessionInstrumentation,
    should_cancel: impl FnMut() -> Result<bool, SpoolError>,
    emit_documents: impl FnMut(Vec<Document>) -> Result<(), E>,
) -> TranslationStreamResult<TranslationOutcome, E> {
    let settings = reader.settings()?;
    translate_pages(
        TranslationSettingsInput {
            source_index_name,
            target_index_name,
            settings,
            replica_settings,
        },
        TranslationPageStreams {
            documents: reader.document_pages(),
            rules: reader.rule_pages(),
            synonyms: reader.synonym_pages(),
        },
        false,
        instrumentation,
        should_cancel,
        emit_documents,
    )
}

pub(in crate::handlers::migration) fn translate_accepted_spool_settings(
    reader: &AcceptedSpoolReader,
) -> Result<SettingsTranslationOutcome, SpoolError> {
    let initial = translate_initial_settings(reader.settings()?);
    if contains_hard_rejection(&initial.entries) {
        return Ok(SettingsTranslationOutcome::Rejected(finalize_report(
            initial.entries,
        )));
    }
    Ok(SettingsTranslationOutcome::Translated(Box::new(
        initial
            .settings
            .expect("settings exist when translation has no failures"),
    )))
}

pub(in crate::handlers::migration) fn translate_spool_input<E>(
    input: SpoolTranslationInput,
    instrumentation: &mut TranslationSessionInstrumentation,
    emit_documents: impl FnMut(Vec<Document>) -> Result<(), E>,
) -> TranslationStreamResult<TranslationOutcome, E> {
    translate_pages(
        TranslationSettingsInput {
            source_index_name: input.source_index_name,
            target_index_name: input.target_index_name,
            settings: input.settings,
            replica_settings: input.replica_settings,
        },
        TranslationPageStreams {
            documents: pages_from_values(input.document_pages),
            rules: pages_from_values(input.rule_pages),
            synonyms: pages_from_values(input.synonym_pages),
        },
        true,
        instrumentation,
        || Ok(false),
        emit_documents,
    )
}

struct InitialSettingsTranslation {
    entries: Vec<TranslationReportEntry>,
    settings: Option<IndexSettings>,
}

struct TranslationPageStreams<DocumentPages, RulePages, SynonymPages> {
    documents: DocumentPages,
    rules: RulePages,
    synonyms: SynonymPages,
}

fn translate_initial_settings(settings: Value) -> InitialSettingsTranslation {
    let mut entries = non_portable_product_entries();
    validate_settings_payload(&settings, &mut entries);

    let mut failures = Vec::new();
    let translated_settings = translate_settings(&settings, &mut failures);
    push_typed_failures(&mut entries, failures);

    InitialSettingsTranslation {
        entries,
        settings: translated_settings,
    }
}

fn translate_pages<DocumentPages, RulePages, SynonymPages, E>(
    settings_input: TranslationSettingsInput,
    page_streams: TranslationPageStreams<DocumentPages, RulePages, SynonymPages>,
    retain_document_batches: bool,
    instrumentation: &mut TranslationSessionInstrumentation,
    should_cancel: impl FnMut() -> Result<bool, SpoolError>,
    mut emit_documents: impl FnMut(Vec<Document>) -> Result<(), E>,
) -> TranslationStreamResult<TranslationOutcome, E>
where
    DocumentPages: IntoIterator<Item = Result<AcceptedSpoolPage, SpoolError>>,
    RulePages: IntoIterator<Item = Result<AcceptedSpoolPage, SpoolError>>,
    SynonymPages: IntoIterator<Item = Result<AcceptedSpoolPage, SpoolError>>,
{
    let mut session = TranslationSession::new(
        settings_input,
        retain_document_batches,
        instrumentation,
        should_cancel,
        &mut emit_documents,
    );
    session.consume_document_pages(page_streams.documents)?;
    session.consume_rule_pages(page_streams.rules)?;
    session.consume_synonym_pages(page_streams.synonyms)?;
    session.finish()
}

fn pages_from_values(
    pages: Vec<Vec<Value>>,
) -> impl Iterator<Item = SpoolResult<AcceptedSpoolPage>> {
    pages.into_iter().enumerate().map(|(page_index, items)| {
        Ok(AcceptedSpoolPage {
            page_index,
            manifest_count: items.len() as u64,
            items,
        })
    })
}

struct TranslationSession<'a, F, E>
where
    F: FnMut(Vec<Document>) -> Result<(), E>,
{
    entries: Vec<TranslationReportEntry>,
    snapshot_builder: SourceSnapshotBuilder,
    settings: Option<flapjack::index::settings::IndexSettings>,
    replica_settings: Vec<ReplicaSettingsTranslation>,
    rules: Vec<flapjack::index::rules::Rule>,
    synonyms: Vec<flapjack::index::synonyms::Synonym>,
    document_batch: Vec<Document>,
    document_batches: Vec<Vec<Document>>,
    retain_document_batches: bool,
    instrumentation: &'a mut TranslationSessionInstrumentation,
    should_cancel: Box<dyn FnMut() -> Result<bool, SpoolError> + 'a>,
    emit_documents: &'a mut F,
}

impl<'a, F, E> TranslationSession<'a, F, E>
where
    F: FnMut(Vec<Document>) -> Result<(), E>,
{
    fn new(
        settings_input: TranslationSettingsInput,
        retain_document_batches: bool,
        instrumentation: &'a mut TranslationSessionInstrumentation,
        should_cancel: impl FnMut() -> Result<bool, SpoolError> + 'a,
        emit_documents: &'a mut F,
    ) -> Self {
        let TranslationSettingsInput {
            source_index_name,
            target_index_name,
            settings,
            replica_settings,
        } = settings_input;
        // Observation only: record that the replica-owned settings map reached
        // translation. Stage 1 does not apply it to settings or persist it.
        instrumentation.replica_settings_count = replica_settings.len();

        let mut entries = non_portable_product_entries();
        let mut snapshot_builder = SourceSnapshotBuilder::new();
        snapshot_builder.record_settings(&settings);
        validate_settings_payload(&settings, &mut entries);

        let mut failures = Vec::new();
        let mut translated_settings = translate_settings(&settings, &mut failures);
        push_typed_failures(&mut entries, failures);
        let mut translated_replica_settings = Vec::new();
        if let Some(translated_settings) = &mut translated_settings {
            let replica_application = translate_and_apply_primary_replicas(
                translated_settings,
                &settings,
                &replica_settings,
                &source_index_name,
                &target_index_name,
            );
            for entry in replica_application.report_entries {
                super::push_unique_entry(&mut entries, entry);
            }
            translated_replica_settings = replica_application.replica_settings;
        }

        Self {
            entries,
            snapshot_builder,
            settings: translated_settings,
            replica_settings: translated_replica_settings,
            rules: Vec::new(),
            synonyms: Vec::new(),
            document_batch: Vec::with_capacity(MAX_DOCUMENT_BATCH_SIZE),
            document_batches: Vec::new(),
            retain_document_batches,
            instrumentation,
            should_cancel: Box::new(should_cancel),
            emit_documents,
        }
    }

    fn consume_document_pages(
        &mut self,
        pages: impl IntoIterator<Item = SpoolResult<AcceptedSpoolPage>>,
    ) -> TranslationStreamResult<(), E> {
        for page in pages {
            self.check_cancelled()?;
            self.consume_document_page(page?)?;
        }
        self.flush_documents()?;
        Ok(())
    }

    fn consume_document_page(&mut self, page: AcceptedSpoolPage) -> TranslationStreamResult<(), E> {
        self.instrumentation.enter_document_page();
        if let Err(violation) = self
            .snapshot_builder
            .record_documents_page(page.page_index, &page.items)
        {
            self.entries
                .push(source_snapshot_violation_entry(violation));
        }
        for (item_index, document) in page.items.iter().enumerate() {
            let mut failures = Vec::new();
            if let Some(document) =
                translate_document(document, page.page_index, item_index, &mut failures)
            {
                self.document_batch.push(document);
                self.instrumentation
                    .observe_pending_documents(self.document_batch.len());
                if self.document_batch.len() == MAX_DOCUMENT_BATCH_SIZE {
                    self.flush_documents()?;
                }
            }
            push_typed_failures(&mut self.entries, failures);
        }
        self.instrumentation.leave_artifact_page();
        Ok(())
    }

    fn consume_rule_pages(
        &mut self,
        pages: impl IntoIterator<Item = SpoolResult<AcceptedSpoolPage>>,
    ) -> TranslationStreamResult<(), E> {
        for page in pages {
            self.check_cancelled()?;
            let page = page?;
            self.instrumentation.enter_artifact_page();
            if let Err(violation) = self
                .snapshot_builder
                .record_rules_page(page.page_index, &page.items)
            {
                self.entries
                    .push(source_snapshot_violation_entry(violation));
            }
            validate_rule_page(page.page_index, &page.items, &mut self.entries);
            self.translate_serde_page(
                page.page_index,
                &page.items,
                ReportCode::MalformedRulePayload,
                ReportResource::Rule,
                |session, value| session.rules.push(value),
            );
            self.instrumentation.leave_artifact_page();
        }
        Ok(())
    }

    fn consume_synonym_pages(
        &mut self,
        pages: impl IntoIterator<Item = SpoolResult<AcceptedSpoolPage>>,
    ) -> TranslationStreamResult<(), E> {
        for page in pages {
            self.check_cancelled()?;
            let page = page?;
            self.instrumentation.enter_artifact_page();
            if let Err(violation) = self
                .snapshot_builder
                .record_synonyms_page(page.page_index, &page.items)
            {
                self.entries
                    .push(source_snapshot_violation_entry(violation));
            }
            validate_synonym_page(page.page_index, &page.items, &mut self.entries);
            self.translate_serde_page(
                page.page_index,
                &page.items,
                ReportCode::MalformedSynonymPayload,
                ReportResource::Synonym,
                |session, value| session.synonyms.push(value),
            );
            self.instrumentation.leave_artifact_page();
        }
        Ok(())
    }

    fn translate_serde_page<T: DeserializeOwned>(
        &mut self,
        page_index: usize,
        items: &[Value],
        code: ReportCode,
        resource: ReportResource,
        mut push_value: impl FnMut(&mut Self, T),
    ) {
        for (item_index, item) in items.iter().enumerate() {
            let mut failures = Vec::new();
            if let Some(value) =
                translate_serde_value(item, page_index, item_index, code, resource, &mut failures)
            {
                push_value(self, value);
            }
            push_typed_failures(&mut self.entries, failures);
        }
    }

    fn flush_documents(&mut self) -> TranslationStreamResult<(), E> {
        if self.document_batch.is_empty() {
            return Ok(());
        }
        self.check_cancelled()?;
        let batch = std::mem::take(&mut self.document_batch);
        self.instrumentation.record_document_batch(batch.len());
        if self.retain_document_batches {
            self.document_batches.push(batch.clone());
        }
        (self.emit_documents)(batch).map_err(TranslationStreamError::Emit)?;
        Ok(())
    }

    fn check_cancelled(&mut self) -> TranslationStreamResult<(), E> {
        if (self.should_cancel)()? {
            return Err(TranslationStreamError::Cancelled);
        }
        Ok(())
    }

    fn finish(mut self) -> TranslationStreamResult<TranslationOutcome, E> {
        self.flush_documents()?;
        if contains_hard_rejection(&self.entries) {
            return Ok(TranslationOutcome::Rejected(finalize_report(self.entries)));
        }

        Ok(TranslationOutcome::Translated(Box::new(
            TranslatedSpoolPayload {
                bundle: TranslationBundle {
                    settings: self
                        .settings
                        .expect("settings exist when translation has no failures"),
                    replica_settings: self.replica_settings,
                    rules: self.rules,
                    synonyms: self.synonyms,
                },
                document_batches: self.document_batches,
                report: finalize_report(self.entries),
            },
        )))
    }
}

impl TranslationSessionInstrumentation {
    fn enter_document_page(&mut self) {
        self.document_pages_seen += 1;
        self.enter_artifact_page();
    }

    fn enter_artifact_page(&mut self) {
        self.live_decoded_pages += 1;
        self.max_live_decoded_pages = self.max_live_decoded_pages.max(self.live_decoded_pages);
    }

    fn leave_artifact_page(&mut self) {
        self.live_decoded_pages -= 1;
    }

    fn observe_pending_documents(&mut self, pending: usize) {
        self.max_pending_documents = self.max_pending_documents.max(pending);
    }

    fn record_document_batch(&mut self, len: usize) {
        self.document_batches_emitted.push(len);
    }
}

fn push_typed_failures(
    entries: &mut Vec<TranslationReportEntry>,
    failures: Vec<TypedTranslationFailure>,
) {
    for failure in failures {
        push_typed_failure(entries, failure);
    }
}
