use super::*;
use crate::handlers::migration::AsyncMigrationSourceProvider;

impl SpoolStore {
    pub(crate) fn seal_bulk_replace_export(
        &self,
        job_uuid: Uuid,
        source_identity_digest: &str,
        document_count: u64,
    ) -> SpoolResult<()> {
        validate_source_identity_digest(source_identity_digest)?;
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        self.ensure_writable(&manifest)?;
        if manifest.counters.settings != 1
            || manifest.counters.documents != document_count
            || manifest.counters.rules != 0
            || manifest.counters.synonyms != 0
        {
            return Err(SpoolError::new(SpoolErrorKind::ResourceVerificationFailed));
        }
        manifest.source_identity_digest = source_identity_digest.to_string();
        manifest.denominators = ResourceDenominators {
            settings: 1,
            documents: document_count,
            rules: 0,
            synonyms: 0,
            config: 0,
        };
        self.commit_manifest(&manifest)
    }

    pub(crate) fn commit_settings_once(
        &self,
        job_uuid: Uuid,
        bytes: &[u8],
        resource_hash: &str,
    ) -> SpoolResult<()> {
        validate_source_identity_digest(resource_hash)?;
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        self.ensure_writable(&manifest)?;
        let expected = ResourceCompletion {
            complete: true,
            count: 1,
            hash: resource_hash.to_string(),
        };
        if manifest.resource_completions.settings.complete {
            return completion_matches(&manifest.resource_completions.settings, &expected);
        }
        if manifest.denominators.settings != 1 || manifest.counters.settings != 0 {
            return Err(SpoolError::new(SpoolErrorKind::ResourceVerificationFailed));
        }

        self.validate_artifact_limits(
            &manifest,
            ArtifactKind::Settings,
            bytes.len() as u64,
            bytes.len() as u64,
            1,
        )?;
        let artifact = new_staged_artifact(ArtifactKind::Settings, bytes, 1);
        manifest.artifacts.push(artifact.clone());
        self.commit_manifest(&manifest)?;
        if let Err(error) = self.write_and_publish_artifact(job_uuid, &artifact, bytes) {
            let _ = self.remove_artifact_paths(job_uuid, &artifact);
            let _ = self.remove_manifest_artifact(job_uuid, &artifact.final_path);
            return Err(error);
        }

        artifact_committed(&mut manifest, &artifact);
        manifest.resource_completions.settings = expected;
        self.commit_manifest(&manifest)
    }

    pub(crate) fn complete_documents(
        &self,
        job_uuid: Uuid,
        count: u64,
        hash: &str,
    ) -> SpoolResult<()> {
        self.complete_object_resource(job_uuid, ObjectResource::Documents, count, hash)
    }

    pub(crate) fn complete_rules(&self, job_uuid: Uuid, count: u64, hash: &str) -> SpoolResult<()> {
        self.complete_object_resource(job_uuid, ObjectResource::Rules, count, hash)
    }

    pub(crate) fn complete_synonyms(
        &self,
        job_uuid: Uuid,
        count: u64,
        hash: &str,
    ) -> SpoolResult<()> {
        self.complete_object_resource(job_uuid, ObjectResource::Synonyms, count, hash)
    }

    #[cfg(test)]
    pub(crate) fn checkpoint(
        &self,
        checkpoint_handle: &str,
        expected_source_identity_digest: &str,
    ) -> SpoolResult<ExportCheckpoint> {
        validate_source_identity_digest(expected_source_identity_digest)?;
        let _root_lock = self.lock_root()?;
        for job_uuid in self.job_uuids()? {
            let Some(manifest) = self.read_manifest_if_exists(job_uuid)? else {
                continue;
            };
            if manifest.checkpoint_handle != checkpoint_handle {
                continue;
            }
            if manifest.source_identity_digest != expected_source_identity_digest {
                return Err(SpoolError::new(SpoolErrorKind::SourceIdentityMismatch));
            }
            return Ok(checkpoint_view(&manifest));
        }
        Err(SpoolError::new(SpoolErrorKind::CheckpointHandleNotFound))
    }

    pub(crate) fn interrupt_export(
        &self,
        job_uuid: Uuid,
        expected_source_identity_digest: &str,
    ) -> SpoolResult<()> {
        validate_source_identity_digest(expected_source_identity_digest)?;
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        if manifest.source_identity_digest != expected_source_identity_digest {
            return Err(SpoolError::new(SpoolErrorKind::SourceIdentityMismatch));
        }
        if manifest.lifecycle == LifecycleState::Interrupted {
            return Ok(());
        }
        self.ensure_writable(&manifest)?;
        self.ensure_export_can_be_interrupted(job_uuid)?;
        manifest.lifecycle = LifecycleState::Interrupted;
        self.commit_manifest(&manifest)?;
        self.refresh_migration_export_progress(&manifest)
    }

    pub(crate) fn claim_interrupted_export(
        &self,
        checkpoint_handle: &str,
        expected_source_identity_digest: &str,
    ) -> SpoolResult<ExportCheckpoint> {
        validate_source_identity_digest(expected_source_identity_digest)?;
        self.claim_interrupted_export_inner(checkpoint_handle, |manifest| {
            if manifest.source_identity_digest == expected_source_identity_digest {
                Ok(())
            } else {
                Err(SpoolError::new(SpoolErrorKind::SourceIdentityMismatch))
            }
        })
    }

    fn claim_interrupted_export_inner(
        &self,
        checkpoint_handle: &str,
        validate_identity: impl Fn(&SpoolManifest) -> SpoolResult<()>,
    ) -> SpoolResult<ExportCheckpoint> {
        let _root_lock = self.lock_root()?;
        for job_uuid in self.job_uuids()? {
            let _job_lock = self.lock_job(job_uuid)?;
            let Some(mut manifest) = self.read_manifest_if_exists(job_uuid)? else {
                continue;
            };
            if manifest.checkpoint_handle != checkpoint_handle {
                continue;
            }
            ensure_supported_spool_format(&manifest)?;
            validate_identity(&manifest)?;
            if manifest.lifecycle != LifecycleState::Interrupted {
                return Err(SpoolError::new(SpoolErrorKind::JobNotInterrupted));
            }
            self.ensure_export_can_be_interrupted(job_uuid)?;
            // ADR 0012 Fence A: a resume claim is exactly one durable manifest flip.
            manifest.lifecycle = LifecycleState::Running;
            self.commit_manifest(&manifest)?;
            self.refresh_migration_export_progress(&manifest)?;
            return Ok(checkpoint_view(&manifest));
        }
        Err(SpoolError::new(SpoolErrorKind::CheckpointHandleNotFound))
    }

    pub(crate) fn recover_export_interruption(&self, job_uuid: Uuid) -> SpoolResult<bool> {
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let Some(mut manifest) = self.read_manifest_if_exists(job_uuid)? else {
            return Ok(false);
        };
        if manifest.lifecycle == LifecycleState::Interrupted {
            return Ok(true);
        }
        if manifest.lifecycle != LifecycleState::Running {
            return Ok(false);
        }
        self.ensure_export_can_be_interrupted(job_uuid)?;
        manifest.lifecycle = LifecycleState::Interrupted;
        self.commit_manifest(&manifest)?;
        self.refresh_migration_export_progress(&manifest)?;
        Ok(true)
    }

    pub(crate) fn resumable_export_handle(&self, job_uuid: Uuid) -> SpoolResult<Option<String>> {
        let _root_lock = self.lock_root()?;
        if !self.job_dir(job_uuid).exists() {
            return Ok(None);
        }
        let _job_lock = self.lock_job(job_uuid)?;
        let Some(manifest) = self.read_manifest_if_exists(job_uuid)? else {
            return Ok(None);
        };
        if manifest.lifecycle != LifecycleState::Interrupted {
            return Ok(None);
        }
        ensure_supported_spool_format(&manifest)?;
        let record = self.read_migration_phase(job_uuid)?;
        if record.phase != MigrationPhase::Exporting
            || record.disposition != MigrationDisposition::Running
            || record.terminal_at.is_some()
            || record.cancel_requested
        {
            return Ok(None);
        }
        let Some(metadata) = self.read_async_migration_metadata_if_exists(job_uuid)? else {
            return Ok(None);
        };
        if !algolia_source_import_can_interrupt(&metadata) {
            return Ok(None);
        }
        Ok(Some(manifest.checkpoint_handle))
    }

    /// Answers the format gate without mutating anything, so restart recovery can
    /// decide an incompatible export's fate before touching its lifecycle. A job
    /// with no durable manifest has nothing to be incompatible with.
    pub(crate) fn export_spool_format_is_supported(&self, job_uuid: Uuid) -> SpoolResult<bool> {
        let _root_lock = self.lock_root()?;
        if !self.job_dir(job_uuid).exists() {
            return Ok(true);
        }
        let _job_lock = self.lock_job(job_uuid)?;
        let Some(manifest) = self.read_manifest_if_exists(job_uuid)? else {
            return Ok(true);
        };
        Ok(ensure_supported_spool_format(&manifest).is_ok())
    }

    pub(crate) fn export_lifecycle_is_running(&self, job_uuid: Uuid) -> SpoolResult<bool> {
        let _root_lock = self.lock_root()?;
        if !self.job_dir(job_uuid).exists() {
            return Ok(false);
        }
        let _job_lock = self.lock_job(job_uuid)?;
        let Some(manifest) = self.read_manifest_if_exists(job_uuid)? else {
            return Ok(false);
        };
        Ok(manifest.lifecycle == LifecycleState::Running)
    }

    pub(crate) fn source_error_can_interrupt_export(&self, job_uuid: Uuid) -> SpoolResult<bool> {
        match self.ensure_export_can_be_interrupted(job_uuid) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == SpoolErrorKind::JobTerminal => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn accept_export(&self, job_uuid: Uuid) -> SpoolResult<()> {
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        if manifest.lifecycle == LifecycleState::Accepted {
            return Ok(());
        }
        self.ensure_writable(&manifest)?;
        if !manifest.resource_completions.all_complete() {
            return Err(SpoolError::new(SpoolErrorKind::ResourcesIncomplete));
        }
        manifest.lifecycle = LifecycleState::Accepted;
        self.commit_manifest(&manifest)
    }

    pub(crate) fn fail_export(&self, job_uuid: Uuid) -> SpoolResult<()> {
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        if manifest.lifecycle == LifecycleState::Failed {
            return Ok(());
        }
        self.ensure_writable(&manifest)?;
        manifest.lifecycle = LifecycleState::Failed;
        self.commit_manifest(&manifest)
    }

    fn complete_object_resource(
        &self,
        job_uuid: Uuid,
        resource: ObjectResource,
        count: u64,
        hash: &str,
    ) -> SpoolResult<()> {
        validate_source_identity_digest(hash)?;
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        self.ensure_writable(&manifest)?;
        let expected = ResourceCompletion {
            complete: true,
            count,
            hash: hash.to_string(),
        };
        let current = object_completion(&manifest, resource);
        if current.complete {
            return completion_matches(current, &expected);
        }
        if object_denominator(&manifest, resource) != count
            || resource_count(manifest.counters, resource.artifact_kind()) != count
            || resource_sidecar(&manifest, resource).count != count
        {
            return Err(SpoolError::new(SpoolErrorKind::ResourceVerificationFailed));
        }
        *object_completion_mut(&mut manifest, resource) = expected;
        self.commit_manifest(&manifest)
    }
}

impl SpoolStore {
    fn ensure_export_can_be_interrupted(&self, job_uuid: Uuid) -> SpoolResult<()> {
        let record = self.read_migration_phase(job_uuid)?;
        if record.phase != MigrationPhase::Exporting
            || record.disposition != MigrationDisposition::Running
            || record.terminal_at.is_some()
            || record.cancel_requested
        {
            return Err(SpoolError::new(SpoolErrorKind::JobTerminal));
        }
        let Some(metadata) = self.read_async_migration_metadata_if_exists(job_uuid)? else {
            return Err(SpoolError::new(SpoolErrorKind::JobTerminal));
        };
        if !algolia_source_import_can_interrupt(&metadata) {
            return Err(SpoolError::new(SpoolErrorKind::JobTerminal));
        }
        Ok(())
    }
}

fn algolia_source_import_can_interrupt(metadata: &AsyncMigrationMetadata) -> bool {
    metadata.source_provider == AsyncMigrationSourceProvider::Algolia
        && metadata.operation_kind == AsyncMigrationOperationKind::SourceImport
        && metadata.publication_transaction_id.is_none()
}

fn completion_matches(
    actual: &ResourceCompletion,
    expected: &ResourceCompletion,
) -> SpoolResult<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(SpoolError::new(SpoolErrorKind::ResourceVerificationFailed))
    }
}

fn object_completion(manifest: &SpoolManifest, resource: ObjectResource) -> &ResourceCompletion {
    match resource {
        ObjectResource::Documents => &manifest.resource_completions.documents,
        ObjectResource::Rules => &manifest.resource_completions.rules,
        ObjectResource::Synonyms => &manifest.resource_completions.synonyms,
    }
}

fn object_completion_mut(
    manifest: &mut SpoolManifest,
    resource: ObjectResource,
) -> &mut ResourceCompletion {
    match resource {
        ObjectResource::Documents => &mut manifest.resource_completions.documents,
        ObjectResource::Rules => &mut manifest.resource_completions.rules,
        ObjectResource::Synonyms => &mut manifest.resource_completions.synonyms,
    }
}

fn object_denominator(manifest: &SpoolManifest, resource: ObjectResource) -> u64 {
    match resource {
        ObjectResource::Documents => manifest.denominators.documents,
        ObjectResource::Rules => manifest.denominators.rules,
        ObjectResource::Synonyms => manifest.denominators.synonyms,
    }
}

fn checkpoint_view(manifest: &SpoolManifest) -> ExportCheckpoint {
    let public = public_view(manifest);
    ExportCheckpoint {
        job_uuid: manifest.job_uuid,
        source_identity_digest: manifest.source_identity_digest.clone(),
        state: public.state,
        progress: public.progress,
        resources: manifest.resource_completions.clone(),
    }
}

pub(super) fn ensure_resource_incomplete(
    manifest: &SpoolManifest,
    kind: ArtifactKind,
) -> SpoolResult<()> {
    let complete = match kind {
        ArtifactKind::Settings => manifest.resource_completions.settings.complete,
        ArtifactKind::DocumentPage => manifest.resource_completions.documents.complete,
        ArtifactKind::RulesPage => manifest.resource_completions.rules.complete,
        ArtifactKind::SynonymsPage => manifest.resource_completions.synonyms.complete,
        // Derived-source configuration has no resource completion of its own —
        // it is captured inside the accepted-state window and bracketed by the
        // export's drift proof. Writability past acceptance is already fenced by
        // `ensure_writable`, so there is no completion flag to consult here.
        ArtifactKind::Config => false,
    };
    if complete {
        Err(SpoolError::new(SpoolErrorKind::ResourceComplete))
    } else {
        Ok(())
    }
}
