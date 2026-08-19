//! Stub summary for engine/flapjack-http/src/handlers/migration/translation_session.rs.

use super::translation_bundle::{
    translate_and_apply_primary_replicas, translate_document, translate_serde_value,
    translate_settings_for_provider, ReplicaSettingsTranslation, SettingsSourceProvider,
    TranslationBundle, TypedTranslationFailure,
};
use super::translation_report::{
    contains_hard_rejection, finalize_report, hard_entry, non_portable_product_entries,
    source_snapshot_violation_entry, ReportCode, ReportResource, TranslationReport,
    TranslationReportEntry,
};
use super::translation_schema::{validate_rule_page, validate_synonym_page};
use super::{push_typed_failure, validate_settings_payload};
use crate::handlers::migration::meilisearch_synonyms::parse_meilisearch_synonym_payload;
use crate::handlers::migration::source_identity_partitions::{
    SourceIdentityConfig, SourceIdentityError, SourceIdentityValidation,
};
use crate::handlers::migration::source_snapshot::{
    document_violation_from_identity_error, SourceSnapshotBuilder,
};
#[cfg(test)]
use crate::handlers::migration::source_test_support::identity_config_for_test;
use crate::handlers::migration::spool::{
    AcceptedSpoolPage, AcceptedSpoolReader, SpoolError, SpoolErrorKind,
};
use crate::handlers::migration::AsyncMigrationSourceProvider;
use flapjack::index::settings::IndexSettings;
use flapjack::types::Document;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;
use std::convert::Infallible;

const MAX_DOCUMENT_BATCH_SIZE: usize =
    flapjack::index::DEFAULT_BULK_BUILD_DOCUMENT_CHECKPOINT_INTERVAL;

/// The settings inputs a translation session opens with: the primary source
/// settings it translates, plus the transient replica-owned settings it observes
/// (counted, never applied in this stage). Bundled so the page-streaming entry
/// points stay within the parameter budget.
struct TranslationSettingsInput {
    source_index_name: String,
    target_index_name: String,
    source_provider: AsyncMigrationSourceProvider,
    settings: Value,
    replica_settings: BTreeMap<String, Value>,
}

type SpoolResult<T> = Result<T, SpoolError>;
type TranslationStreamResult<T, E> = Result<T, TranslationStreamError<E>>;

#[derive(Debug, Clone, PartialEq)]
pub(in crate::handlers::migration) struct SpoolTranslationInput {
    pub(in crate::handlers::migration) source_index_name: String,
    pub(in crate::handlers::migration) target_index_name: String,
    pub(in crate::handlers::migration) source_provider: AsyncMigrationSourceProvider,
    pub(in crate::handlers::migration) settings: Value,
    pub(in crate::handlers::migration) document_pages: Vec<Vec<Value>>,
    pub(in crate::handlers::migration) rule_pages: Vec<Vec<Value>>,
    pub(in crate::handlers::migration) rule_stable_id_pages: Vec<Vec<String>>,
    pub(in crate::handlers::migration) synonym_pages: Vec<Vec<Value>>,
    pub(in crate::handlers::migration) synonym_stable_id_pages: Vec<Vec<String>>,
    /// Replica-owned source settings carried to the translation entry point.
    /// Observation-only in Stage 1: counted, never applied to settings.
    pub(in crate::handlers::migration) replica_settings: BTreeMap<String, Value>,
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
    Identity(SourceIdentityError),
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
    pub(in crate::handlers::migration) source_identity_validation: SourceIdentityValidation,
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
        Err(TranslationStreamError::Identity(error)) => {
            panic!("in-memory translation identity infrastructure failed: {error:?}")
        }
    }
}

/// Runs the in-memory translation owner and returns its finalized advisory
/// report without exposing translated resources to a caller that cannot publish.
pub(in crate::handlers::migration) fn translate_spool_report(
    input: SpoolTranslationInput,
) -> Result<TranslationReport, SourceIdentityError> {
    let mut instrumentation = TranslationSessionInstrumentation::default();
    let outcome =
        match translate_spool_input(input, &mut instrumentation, |_| Ok::<(), Infallible>(())) {
            Ok(outcome) => outcome,
            Err(TranslationStreamError::Identity(error)) => return Err(error),
            Err(TranslationStreamError::Emit(never)) => match never {},
            Err(TranslationStreamError::Spool(error)) => {
                unreachable!("in-memory translation pages cannot fail: {error}")
            }
            Err(TranslationStreamError::Cancelled) => {
                unreachable!("in-memory translation never requests cancellation")
            }
        };
    Ok(match outcome {
        TranslationOutcome::Translated(translated) => translated.report,
        TranslationOutcome::Rejected(report) => report,
    })
}

/// TODO: Document translate_accepted_spool_payload.
pub(in crate::handlers::migration) fn translate_accepted_spool_payload<E>(
    reader: AcceptedSpoolReader,
    source_index_name: String,
    target_index_name: String,
    instrumentation: &mut TranslationSessionInstrumentation,
    should_cancel: impl FnMut() -> Result<bool, SpoolError>,
    emit_documents: impl FnMut(Vec<Document>) -> Result<(), E>,
) -> TranslationStreamResult<TranslationOutcome, E> {
    let source_provider = reader.source_provider()?;
    let settings = reader.settings()?;
    // Replica-owned settings are durable spool artifacts, so the accepted reader
    // is their single source here — nothing is carried in from the caller.
    let replica_settings = reader.replica_settings()?;
    translate_pages(
        TranslationSettingsInput {
            source_index_name,
            target_index_name,
            source_provider,
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
    let initial = translate_initial_settings(reader.settings()?, reader.source_provider()?);
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

/// TODO: Document translate_spool_input.
pub(in crate::handlers::migration) fn translate_spool_input<E>(
    input: SpoolTranslationInput,
    instrumentation: &mut TranslationSessionInstrumentation,
    emit_documents: impl FnMut(Vec<Document>) -> Result<(), E>,
) -> TranslationStreamResult<TranslationOutcome, E> {
    let configuration_stable_id_pages = |stable_id_pages| match input.source_provider {
        AsyncMigrationSourceProvider::Algolia => None,
        AsyncMigrationSourceProvider::Meilisearch | AsyncMigrationSourceProvider::Typesense => {
            Some(stable_id_pages)
        }
    };
    translate_pages(
        TranslationSettingsInput {
            source_index_name: input.source_index_name,
            target_index_name: input.target_index_name,
            source_provider: input.source_provider,
            settings: input.settings,
            replica_settings: input.replica_settings,
        },
        TranslationPageStreams {
            documents: pages_from_values(input.document_pages, None),
            rules: pages_from_values(
                input.rule_pages,
                configuration_stable_id_pages(input.rule_stable_id_pages),
            ),
            synonyms: pages_from_values(
                input.synonym_pages,
                configuration_stable_id_pages(input.synonym_stable_id_pages),
            ),
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

struct TranslationSessionOptions {
    identity_config: SourceIdentityConfig,
    retain_document_batches: bool,
    document_batch_size: usize,
}

/// TODO: Document translate_initial_settings.
fn translate_initial_settings(
    settings: Value,
    source_provider: AsyncMigrationSourceProvider,
) -> InitialSettingsTranslation {
    let mut entries = non_portable_product_entries();
    if source_provider == AsyncMigrationSourceProvider::Algolia {
        validate_settings_payload(&settings, &mut entries);
    }

    let mut failures = Vec::new();
    let translated_settings = translate_settings_for_provider(
        &settings,
        settings_source_provider(source_provider),
        &mut failures,
        &mut entries,
    );
    push_typed_failures(&mut entries, failures);

    InitialSettingsTranslation {
        entries,
        settings: translated_settings,
    }
}

/// TODO: Document translate_pages.
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
    #[cfg(not(test))]
    let identity_config =
        SourceIdentityConfig::from_env().map_err(TranslationStreamError::Identity)?;
    #[cfg(test)]
    let (_identity_spool_root, identity_config) =
        identity_config_for_test().map_err(TranslationStreamError::Identity)?;
    let mut session = TranslationSession::new(
        settings_input,
        TranslationSessionOptions {
            identity_config,
            retain_document_batches,
            document_batch_size: flapjack::index::BulkBuildWriterConfig::from_env()
                .document_checkpoint_interval,
        },
        instrumentation,
        should_cancel,
        &mut emit_documents,
    )?;
    session.consume_document_pages(page_streams.documents)?;
    session.consume_rule_pages(page_streams.rules)?;
    session.consume_synonym_pages(page_streams.synonyms)?;
    session.finish()
}

/// An explicit stable-ID sidecar carries identity that provider-native payloads
/// cannot recover, so it must stay exactly one-to-one with the value pages at
/// both the page and the item level. A mismatch — including a sidecar with no
/// value pages to align against — is corrupt manifest state, never a reason to
/// fall back to deriving IDs from payloads.
fn explicit_sidecar_is_aligned(pages: &[Vec<Value>], stable_id_pages: &[Vec<String>]) -> bool {
    stable_id_pages.len() == pages.len()
        && stable_id_pages
            .iter()
            .zip(pages)
            .all(|(stable_ids, items)| stable_ids.len() == items.len())
}

fn pages_from_values(
    pages: Vec<Vec<Value>>,
    stable_id_pages: Option<Vec<Vec<String>>>,
) -> impl Iterator<Item = SpoolResult<AcceptedSpoolPage>> {
    let alignment_error = stable_id_pages
        .as_ref()
        .is_some_and(|explicit| !explicit_sidecar_is_aligned(&pages, explicit))
        .then(|| SpoolError::new(SpoolErrorKind::ManifestCorrupt));
    // A misaligned sidecar fails the whole resource, so no page is translated
    // from identity state that could not be verified.
    let pages = if alignment_error.is_some() {
        Vec::new()
    } else {
        pages
    };
    let mut stable_id_pages = stable_id_pages.map(Vec::into_iter);
    alignment_error
        .map(Err)
        .into_iter()
        .chain(
            pages
                .into_iter()
                .enumerate()
                .map(move |(page_index, items)| {
                    let stable_ids = stable_id_pages
                        .as_mut()
                        .and_then(Iterator::next)
                        .unwrap_or_else(|| page_stable_ids(&items));
                    Ok(AcceptedSpoolPage {
                        page_index,
                        manifest_count: items.len() as u64,
                        stable_ids,
                        items,
                    })
                }),
        )
}

/// TODO: Document TranslationSession.
struct TranslationSession<'a, F, E>
where
    F: FnMut(Vec<Document>) -> Result<(), E>,
{
    entries: Vec<TranslationReportEntry>,
    source_provider: AsyncMigrationSourceProvider,
    snapshot_builder: Option<SourceSnapshotBuilder>,
    settings: Option<flapjack::index::settings::IndexSettings>,
    replica_settings: Vec<ReplicaSettingsTranslation>,
    rules: Vec<flapjack::index::rules::Rule>,
    synonyms: Vec<flapjack::index::synonyms::Synonym>,
    document_batch: Vec<Document>,
    document_batches: Vec<Vec<Document>>,
    retain_document_batches: bool,
    document_batch_size: usize,
    instrumentation: &'a mut TranslationSessionInstrumentation,
    should_cancel: Box<dyn FnMut() -> Result<bool, SpoolError> + 'a>,
    emit_documents: &'a mut F,
}

impl<'a, F, E> TranslationSession<'a, F, E>
where
    F: FnMut(Vec<Document>) -> Result<(), E>,
{
    /// TODO: Document TranslationSession.new.
    fn new(
        settings_input: TranslationSettingsInput,
        options: TranslationSessionOptions,
        instrumentation: &'a mut TranslationSessionInstrumentation,
        should_cancel: impl FnMut() -> Result<bool, SpoolError> + 'a,
        emit_documents: &'a mut F,
    ) -> TranslationStreamResult<Self, E> {
        let TranslationSettingsInput {
            source_index_name,
            target_index_name,
            source_provider,
            settings,
            replica_settings,
        } = settings_input;
        // Observation only: record that the replica-owned settings map reached
        // translation. Stage 1 does not apply it to settings or persist it.
        instrumentation.replica_settings_count = replica_settings.len();

        let mut entries = non_portable_product_entries();
        let mut snapshot_builder = SourceSnapshotBuilder::new(options.identity_config)
            .map_err(TranslationStreamError::Identity)?;
        snapshot_builder.record_settings(&settings);
        if source_provider == AsyncMigrationSourceProvider::Algolia {
            validate_settings_payload(&settings, &mut entries);
        }

        let mut failures = Vec::new();
        let mut translated_settings = translate_settings_for_provider(
            &settings,
            settings_source_provider(source_provider),
            &mut failures,
            &mut entries,
        );
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

        Ok(Self {
            entries,
            source_provider,
            snapshot_builder: Some(snapshot_builder),
            settings: translated_settings,
            replica_settings: translated_replica_settings,
            rules: Vec::new(),
            synonyms: Vec::new(),
            document_batch: Vec::with_capacity(MAX_DOCUMENT_BATCH_SIZE),
            document_batches: Vec::new(),
            retain_document_batches: options.retain_document_batches,
            document_batch_size: options.document_batch_size,
            instrumentation,
            should_cancel: Box::new(should_cancel),
            emit_documents,
        })
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

    /// TODO: Document TranslationSession.consume_document_page.
    fn consume_document_page(&mut self, page: AcceptedSpoolPage) -> TranslationStreamResult<(), E> {
        self.instrumentation.enter_document_page();
        if let Err(error) = self
            .snapshot_builder
            .as_mut()
            .expect("snapshot builder exists until finish")
            .record_documents_page(page.page_index, &page.items)
        {
            self.push_document_identity_error(error)?;
        }
        for (item_index, document) in page.items.iter().enumerate() {
            let mut failures = Vec::new();
            if let Some(document) =
                translate_document(document, page.page_index, item_index, &mut failures)
            {
                self.document_batch.push(document);
                self.instrumentation
                    .observe_pending_documents(self.document_batch.len());
                if self.document_batch.len() == self.document_batch_size {
                    self.flush_documents()?;
                }
            }
            push_typed_failures(&mut self.entries, failures);
        }
        self.instrumentation.leave_artifact_page();
        Ok(())
    }

    /// TODO: Document TranslationSession.consume_rule_pages.
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
                .as_mut()
                .expect("snapshot builder exists until finish")
                .record_rules_page_with_stable_ids(page.page_index, &page.items, &page.stable_ids)
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

    /// TODO: Document TranslationSession.consume_synonym_pages.
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
                .as_mut()
                .expect("snapshot builder exists until finish")
                .record_synonyms_page_with_stable_ids(
                    page.page_index,
                    &page.items,
                    &page.stable_ids,
                )
            {
                self.entries
                    .push(source_snapshot_violation_entry(violation));
            }
            let synonym_items = self.translate_synonym_page(&page);
            validate_synonym_page(page.page_index, &synonym_items, &mut self.entries);
            self.translate_serde_page(
                page.page_index,
                &synonym_items,
                ReportCode::MalformedSynonymPayload,
                ReportResource::Synonym,
                |session, value| session.synonyms.push(value),
            );
            self.instrumentation.leave_artifact_page();
        }
        Ok(())
    }

    fn translate_synonym_page(&mut self, page: &AcceptedSpoolPage) -> Vec<Value> {
        match self.source_provider {
            AsyncMigrationSourceProvider::Meilisearch => {
                translate_meilisearch_synonym_page(page, &mut self.entries)
            }
            AsyncMigrationSourceProvider::Algolia | AsyncMigrationSourceProvider::Typesense => {
                page.items.clone()
            }
        }
    }

    /// TODO: Document TranslationSession.translate_serde_page.
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

    /// TODO: Document TranslationSession.finish.
    fn finish(mut self) -> TranslationStreamResult<TranslationOutcome, E> {
        self.flush_documents()?;
        let snapshot_builder = self
            .snapshot_builder
            .take()
            .expect("snapshot builder exists until finish");
        let source_identity_validation = match snapshot_builder.finish() {
            Ok(_) => SourceIdentityValidation::Unique,
            Err(error) => match SourceIdentityValidation::from_duplicate_error(&error) {
                Some(validation) => validation,
                None => {
                    self.push_document_identity_error(error)?;
                    SourceIdentityValidation::Unique
                }
            },
        };
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
                source_identity_validation,
            },
        )))
    }

    fn push_document_identity_error(
        &mut self,
        error: SourceIdentityError,
    ) -> TranslationStreamResult<(), E> {
        if error.is_infrastructure() {
            return Err(TranslationStreamError::Identity(error));
        }
        if let Some(violation) = document_violation_from_identity_error(&error) {
            self.entries
                .push(source_snapshot_violation_entry(violation));
        }
        Ok(())
    }
}

fn settings_source_provider(
    source_provider: AsyncMigrationSourceProvider,
) -> SettingsSourceProvider {
    match source_provider {
        AsyncMigrationSourceProvider::Algolia => SettingsSourceProvider::Algolia,
        AsyncMigrationSourceProvider::Meilisearch => SettingsSourceProvider::Meilisearch,
        AsyncMigrationSourceProvider::Typesense => SettingsSourceProvider::Typesense,
    }
}

pub(in crate::handlers::migration) fn page_stable_ids(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| {
            item.get("objectID")
                .or_else(|| item.get("stableId"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn translate_meilisearch_synonym_page(
    page: &AcceptedSpoolPage,
    entries: &mut Vec<TranslationReportEntry>,
) -> Vec<Value> {
    page.items
        .iter()
        .enumerate()
        .filter_map(|(item_index, item)| {
            translate_meilisearch_synonym(
                item,
                page.stable_ids.get(item_index),
                page.page_index,
                item_index,
                entries,
            )
        })
        .collect()
}

fn translate_meilisearch_synonym(
    item: &Value,
    stable_id: Option<&String>,
    page_index: usize,
    item_index: usize,
    entries: &mut Vec<TranslationReportEntry>,
) -> Option<Value> {
    let Some(stable_id) = stable_id else {
        entries.push(hard_entry(
            ReportCode::MalformedSynonymPayload,
            ReportResource::Synonym,
            Some(page_index),
            Some(item_index),
            "$",
        ));
        return None;
    };
    let synonym = match parse_meilisearch_synonym_payload(item) {
        Ok(synonym) => synonym,
        Err(error) => {
            entries.push(hard_entry(
                ReportCode::MalformedSynonymPayload,
                ReportResource::Synonym,
                Some(page_index),
                Some(item_index),
                error.json_path(),
            ));
            return None;
        }
    };
    let mut words = Vec::with_capacity(synonym.alternatives.len() + 1);
    words.push(Value::String(synonym.input));
    words.extend(synonym.alternatives.into_iter().map(Value::String));
    Some(serde_json::json!({
        "objectID": stable_id,
        "type": "synonym",
        "synonyms": words,
    }))
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
