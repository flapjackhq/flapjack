use super::*;

impl SpoolStore {
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

    pub(crate) fn checkpoint(
        &self,
        checkpoint_handle: &str,
        expected_source_identity_digest: &str,
    ) -> SpoolResult<ExportCheckpoint> {
        validate_source_identity_digest(expected_source_identity_digest)?;
        let _root_lock = self.lock_root()?;
        for job_uuid in self.job_uuids()? {
            let manifest = self.read_manifest(job_uuid)?;
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
        ArtifactKind::Config => false,
    };
    if complete {
        Err(SpoolError::new(SpoolErrorKind::ResourceComplete))
    } else {
        Ok(())
    }
}
