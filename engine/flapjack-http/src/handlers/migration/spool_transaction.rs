use super::*;
use std::collections::HashSet;

impl SpoolStore {
    pub(crate) fn commit_document_page_with_ids(
        &self,
        job_uuid: Uuid,
        bytes: &[u8],
        object_ids: &[&str],
    ) -> SpoolResult<()> {
        self.commit_object_page(job_uuid, ObjectResource::Documents, bytes, object_ids)
    }

    pub(crate) fn commit_rule_page_with_ids(
        &self,
        job_uuid: Uuid,
        bytes: &[u8],
        object_ids: &[&str],
    ) -> SpoolResult<()> {
        self.commit_object_page(job_uuid, ObjectResource::Rules, bytes, object_ids)
    }

    pub(crate) fn commit_synonym_page_with_ids(
        &self,
        job_uuid: Uuid,
        bytes: &[u8],
        object_ids: &[&str],
    ) -> SpoolResult<()> {
        self.commit_object_page(job_uuid, ObjectResource::Synonyms, bytes, object_ids)
    }

    pub(crate) fn completed_document_ids(&self, job_uuid: Uuid) -> SpoolResult<Vec<String>> {
        self.completed_resource_ids(job_uuid, ObjectResource::Documents)
    }

    pub(crate) fn completed_rule_ids(&self, job_uuid: Uuid) -> SpoolResult<Vec<String>> {
        self.completed_resource_ids(job_uuid, ObjectResource::Rules)
    }

    pub(crate) fn completed_synonym_ids(&self, job_uuid: Uuid) -> SpoolResult<Vec<String>> {
        self.completed_resource_ids(job_uuid, ObjectResource::Synonyms)
    }

    fn commit_object_page(
        &self,
        job_uuid: Uuid,
        resource: ObjectResource,
        bytes: &[u8],
        object_ids: &[&str],
    ) -> SpoolResult<()> {
        validate_page_ids(object_ids)?;
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        self.ensure_writable(&manifest)?;
        let existing = self.completed_resource_ids_from_manifest(job_uuid, &manifest, resource)?;
        if page_is_already_complete(&existing, object_ids)? {
            return Ok(());
        }

        let kind = resource.artifact_kind();
        ensure_resource_incomplete(&manifest, kind)?;
        self.validate_artifact_limits(
            &manifest,
            kind,
            bytes.len() as u64,
            bytes.len() as u64,
            object_ids.len() as u64,
        )?;
        let artifact = new_staged_artifact(kind, bytes, object_ids.len() as u64);
        manifest.artifacts.push(artifact.clone());
        self.commit_manifest(&manifest)?;

        let sidecar_bytes = append_ids(existing, object_ids);
        if let Err(error) =
            self.stage_transaction_files(job_uuid, resource, &artifact, bytes, &sidecar_bytes)
        {
            let _ = self.remove_artifact_paths(job_uuid, &artifact);
            let _ = self.recover_resource_sidecar(job_uuid, &manifest, resource);
            let _ = self.remove_manifest_artifact(job_uuid, &artifact.final_path);
            return Err(error);
        }

        artifact_committed(&mut manifest, &artifact);
        let generation = resource_sidecar(&manifest, resource).generation + 1;
        *resource_sidecar_mut(&mut manifest, resource) = SidecarManifest {
            generation,
            length: sidecar_bytes.len() as u64,
            digest: hex_digest(&sidecar_bytes),
            count: sidecar_bytes.iter().filter(|byte| **byte == b'\n').count() as u64,
        };
        self.commit_manifest(&manifest)
    }

    fn stage_transaction_files(
        &self,
        job_uuid: Uuid,
        resource: ObjectResource,
        artifact: &ArtifactManifest,
        payload: &[u8],
        sidecar_bytes: &[u8],
    ) -> SpoolResult<()> {
        let sidecar_path = self.resource_sidecar_path(job_uuid, resource);
        let file_name = sidecar_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| SpoolError::new(SpoolErrorKind::InvalidRelativePath))?;
        write_atomic(&self.job_dir(job_uuid), file_name, sidecar_bytes)?;
        self.write_and_publish_artifact(job_uuid, artifact, payload)
    }

    fn completed_resource_ids(
        &self,
        job_uuid: Uuid,
        resource: ObjectResource,
    ) -> SpoolResult<Vec<String>> {
        let manifest = self.read_manifest(job_uuid)?;
        self.completed_resource_ids_from_manifest(job_uuid, &manifest, resource)
    }

    pub(super) fn completed_resource_ids_from_manifest(
        &self,
        job_uuid: Uuid,
        manifest: &SpoolManifest,
        resource: ObjectResource,
    ) -> SpoolResult<Vec<String>> {
        let sidecar = resource_sidecar(manifest, resource);
        let path = self.resource_sidecar_path(job_uuid, resource);
        if !path.exists() {
            return if sidecar.length == 0 && sidecar.count == 0 {
                Ok(Vec::new())
            } else {
                Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt))
            };
        }
        let bytes = fs::read(path)?;
        if bytes.len() < sidecar.length as usize {
            return Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt));
        }
        let committed = &bytes[..sidecar.length as usize];
        if sidecar.length > 0 && hex_digest(committed) != sidecar.digest {
            return Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt));
        }
        let ids = String::from_utf8_lossy(committed)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if ids.len() as u64 != sidecar.count {
            return Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt));
        }
        Ok(ids)
    }

    pub(super) fn resource_sidecar_path(
        &self,
        job_uuid: Uuid,
        resource: ObjectResource,
    ) -> PathBuf {
        let file_name = match resource {
            ObjectResource::Documents => COMPLETED_DOCUMENTS_FILE,
            ObjectResource::Rules => COMPLETED_RULES_FILE,
            ObjectResource::Synonyms => COMPLETED_SYNONYMS_FILE,
        };
        self.job_dir(job_uuid).join(file_name)
    }
}

pub(super) fn resource_sidecar(
    manifest: &SpoolManifest,
    resource: ObjectResource,
) -> &SidecarManifest {
    match resource {
        ObjectResource::Documents => &manifest.completed_objects,
        ObjectResource::Rules => &manifest.completed_rules,
        ObjectResource::Synonyms => &manifest.completed_synonyms,
    }
}

fn resource_sidecar_mut(
    manifest: &mut SpoolManifest,
    resource: ObjectResource,
) -> &mut SidecarManifest {
    match resource {
        ObjectResource::Documents => &mut manifest.completed_objects,
        ObjectResource::Rules => &mut manifest.completed_rules,
        ObjectResource::Synonyms => &mut manifest.completed_synonyms,
    }
}

fn validate_page_ids(object_ids: &[&str]) -> SpoolResult<()> {
    let mut unique = HashSet::with_capacity(object_ids.len());
    if object_ids.is_empty()
        || object_ids
            .iter()
            .any(|id| id.is_empty() || id.contains(['\n', '\r']) || !unique.insert(*id))
    {
        return Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt));
    }
    Ok(())
}

fn page_is_already_complete(existing: &[String], object_ids: &[&str]) -> SpoolResult<bool> {
    let completed = existing.iter().map(String::as_str).collect::<HashSet<_>>();
    let completed_count = object_ids
        .iter()
        .filter(|id| completed.contains(**id))
        .count();
    if completed_count == 0 {
        Ok(false)
    } else if completed_count == object_ids.len() {
        Ok(true)
    } else {
        Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt))
    }
}

fn append_ids(existing: Vec<String>, object_ids: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for id in existing
        .iter()
        .map(String::as_str)
        .chain(object_ids.iter().copied())
    {
        bytes.extend_from_slice(id.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

pub(super) fn new_staged_artifact(
    kind: ArtifactKind,
    bytes: &[u8],
    item_count: u64,
) -> ArtifactManifest {
    ArtifactManifest {
        kind,
        state: ArtifactState::Staged,
        temp_path: format!("{TEMP_PREFIX}{}-{}.tmp", kind.prefix(), Uuid::new_v4()),
        final_path: format!("{}-{}.bin", kind.prefix(), Uuid::new_v4()),
        compressed_bytes: bytes.len() as u64,
        decompressed_bytes: bytes.len() as u64,
        item_count,
        digest: hex_digest(bytes),
    }
}

pub(super) fn artifact_committed(manifest: &mut SpoolManifest, artifact: &ArtifactManifest) {
    manifest
        .artifacts
        .iter_mut()
        .find(|entry| entry.final_path == artifact.final_path)
        .expect("newly staged artifact must remain registered")
        .state = ArtifactState::Visible;
    manifest.bytes_committed += artifact.compressed_bytes;
    manifest.counters.add(artifact.kind, artifact.item_count);
}
