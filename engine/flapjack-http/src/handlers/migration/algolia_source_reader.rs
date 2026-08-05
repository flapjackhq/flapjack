use super::algolia_client::{AlgoliaClient, AlgoliaClientError, AlgoliaIndexRecord, BrowseError};
use super::source_reader::{
    algolia_consumer_error, algolia_document_records, capture_neutral_page,
    finish_neutral_page_capture, missing_captured_primary_settings, replica_entry_validation_error,
    AlgoliaReplicaSource, MigrationSourceReader, PageCaptureAborted, SourceConfigurationArtifact,
    SourceConfigurationConsumer, SourceDocumentPageConsumer, SourceExportError, SourceFuture,
    SourceObservation,
};
use super::AsyncMigrationSourceProvider;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;

pub(super) struct AlgoliaSourceReader {
    app_id: String,
    source_name: String,
    client: AlgoliaClient,
    /// The primary settings captured during this pass, retained so the replica
    /// list is resolved from the value already emitted rather than a second read.
    captured_primary_settings: Option<Value>,
}

impl AlgoliaSourceReader {
    pub(super) fn new(
        app_id: &str,
        api_key: &str,
        source_name: &str,
    ) -> Result<Self, AlgoliaClientError> {
        let client = AlgoliaClient::for_source(app_id, api_key, source_name)?;
        Ok(Self {
            app_id: app_id.to_string(),
            source_name: source_name.to_string(),
            client,
            captured_primary_settings: None,
        })
    }
}

impl fmt::Debug for AlgoliaSourceReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AlgoliaSourceReader")
            .field("app_id", &"<scrubbed>")
            .field("source_name", &"<scrubbed>")
            .finish_non_exhaustive()
    }
}

impl AlgoliaReplicaSource for AlgoliaSourceReader {
    fn read_index_settings<'a>(&'a mut self, index_name: &'a str) -> SourceFuture<'a, Value> {
        Box::pin(async move {
            self.client
                .index_settings(index_name)
                .await
                .map_err(Into::into)
        })
    }
}

impl MigrationSourceReader for AlgoliaSourceReader {
    fn source_provider(&self) -> AsyncMigrationSourceProvider {
        AsyncMigrationSourceProvider::Algolia
    }

    fn source_namespace(&self) -> Option<&str> {
        Some(&self.app_id)
    }

    fn source_name(&self) -> &str {
        &self.source_name
    }

    fn observe_quiescent_source(&mut self) -> SourceFuture<'_, SourceObservation> {
        Box::pin(async move {
            let record = self.client.wait_for_quiescent_source().await?;
            Ok(algolia_observation(&record))
        })
    }

    fn read_configuration<'a>(
        &'a mut self,
        consume: &'a mut SourceConfigurationConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            let settings = self.client.settings().await?;
            self.client.require_unretrievable_access(&settings).await?;
            let artifact = SourceConfigurationArtifact::settings(&settings);
            if let SourceConfigurationArtifact::Settings { payload } = &artifact {
                self.captured_primary_settings = Some(payload.clone());
            }
            consume(artifact)?;
            read_algolia_hit_configuration(
                &mut self.client,
                "rules/search",
                SourceConfigurationArtifact::rules,
                consume,
            )
            .await?;
            read_algolia_hit_configuration(
                &mut self.client,
                "synonyms/search",
                SourceConfigurationArtifact::synonyms,
                consume,
            )
            .await
        })
    }

    fn read_derived_configuration<'a>(
        &'a mut self,
        consume: &'a mut SourceConfigurationConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            let Some(primary_settings) = self.captured_primary_settings.clone() else {
                return Err(missing_captured_primary_settings());
            };
            collect_replica_settings(self, &primary_settings, consume).await
        })
    }

    fn read_document_records<'a>(
        &'a mut self,
        consume_page: &'a mut SourceDocumentPageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            let mut capture_error = None;
            let mut raw_consumer = |page: Vec<Value>| {
                capture_neutral_page(
                    &mut capture_error,
                    algolia_document_records(&page),
                    consume_page,
                )
                .map_err(|PageCaptureAborted| algolia_consumer_error())
            };
            let outcome = self.client.browse_documents(&mut raw_consumer).await;
            finish_neutral_page_capture(capture_error, outcome.map_err(flatten_browse_error))
        })
    }
}

/// Stream one Algolia hit-paginated configuration resource into the shared
/// tagged stream. Rules and synonyms differ only by endpoint and artifact tag,
/// so they share this one traversal rather than two near-identical bodies.
async fn read_algolia_hit_configuration(
    client: &mut AlgoliaClient,
    path: &str,
    into_artifact: fn(&[Value]) -> Result<SourceConfigurationArtifact, SourceExportError>,
    mut consume: &mut SourceConfigurationConsumer<'_>,
) -> Result<(), SourceExportError> {
    let mut capture_error = None;
    let mut raw_consumer = |page: Vec<Value>| match into_artifact(&page).and_then(&mut consume) {
        Ok(()) => Ok(()),
        Err(error) => {
            capture_error = Some(error);
            Err(algolia_consumer_error())
        }
    };
    let outcome = client.paginated_hits(path, &mut raw_consumer).await;
    finish_neutral_page_capture(capture_error, outcome.map_err(flatten_browse_error))
}

/// Collect the complete source settings for every replica named in the primary
/// settings' `replicas` list, emitting each through the shared tagged
/// configuration stream. Each string entry is parsed through the single
/// canonical replica parser and its settings fetched exactly once.
///
/// Absent `replicas` performs zero index-specific reads. Malformed primary
/// `replicas` *shapes* (non-array, non-string entries) are left to the existing
/// translation validation owner, so non-string entries are skipped here rather
/// than rejected. A string entry that fails the canonical parser is a fail-closed
/// validation error with a single static, scrubbed message.
pub(super) async fn collect_replica_settings<R>(
    reader: &mut R,
    primary_settings: &Value,
    consume: &mut SourceConfigurationConsumer<'_>,
) -> Result<(), SourceExportError>
where
    R: AlgoliaReplicaSource + Send + ?Sized,
{
    let Some(entries) = primary_settings.get("replicas").and_then(Value::as_array) else {
        return Ok(());
    };

    let mut collected = BTreeSet::new();
    for entry in entries {
        let Some(raw) = entry.as_str() else {
            continue;
        };
        let parsed = flapjack::index::replica::parse_replica_entry(raw)
            .map_err(|_| replica_entry_validation_error())?;
        let name = parsed.name().to_string();
        if !collected.insert(name.clone()) {
            continue;
        }
        let settings = reader.read_index_settings(&name).await?;
        consume(SourceConfigurationArtifact::replica_settings(
            &name, &settings,
        ))?;
    }

    Ok(())
}

fn algolia_observation(record: &AlgoliaIndexRecord) -> SourceObservation {
    SourceObservation {
        source_name: record.name.clone(),
        accepted_revision: record.updated_at.clone(),
        identity_revision: record.updated_at.clone(),
        document_count: record.entries,
        quiescent: !record.pending_task,
    }
}

fn flatten_browse_error(error: BrowseError<AlgoliaClientError>) -> AlgoliaClientError {
    match error {
        BrowseError::Client(error) | BrowseError::Consumer(error) => error,
    }
}
