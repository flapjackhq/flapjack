use super::*;
#[cfg(test)]
use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{Seek, Write};

const SIDECAR_DIGEST_PREFIX: &str = "fnv1a64:";
const FNV64_OFFSET: u64 = 0xcbf29ce484222325;
const FNV64_PRIME: u64 = 0x100000001b3;

#[cfg(test)]
thread_local! {
    static COMPLETED_ID_CHECKPOINT_WRITES: RefCell<Vec<CompletedIdCheckpointWrite>> =
        const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CompletedIdCheckpointWrite {
    pub resource: ObjectResource,
    pub byte_len: usize,
    pub serialized_id_count: usize,
    pub sidecar_read_bytes: usize,
    pub digest_input_bytes: usize,
    pub counted_id_count: usize,
}

#[cfg(test)]
pub(super) fn reset_completed_id_checkpoint_writes_for_tests() {
    COMPLETED_ID_CHECKPOINT_WRITES.with(|writes| writes.borrow_mut().clear());
}

#[cfg(test)]
pub(super) fn completed_id_checkpoint_writes_for_tests() -> Vec<CompletedIdCheckpointWrite> {
    COMPLETED_ID_CHECKPOINT_WRITES.with(|writes| writes.borrow().clone())
}

#[cfg(test)]
fn record_completed_id_checkpoint_write(write: CompletedIdCheckpointWrite) {
    COMPLETED_ID_CHECKPOINT_WRITES.with(|writes| writes.borrow_mut().push(write));
}

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
        let completion_check =
            self.completed_resource_page_check(job_uuid, &manifest, resource, object_ids)?;
        if completion_check.already_complete {
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

        let sidecar_bytes = id_lines(object_ids);
        #[cfg(test)]
        record_completed_id_checkpoint_write(CompletedIdCheckpointWrite {
            resource,
            byte_len: sidecar_bytes.len(),
            serialized_id_count: object_ids.len(),
            sidecar_read_bytes: completion_check.sidecar_read_bytes,
            digest_input_bytes: sidecar_bytes.len(),
            counted_id_count: object_ids.len(),
        });
        if let Err(error) = self.stage_transaction_files(
            job_uuid,
            resource,
            &artifact,
            bytes,
            &completion_check.sidecar,
            &sidecar_bytes,
        ) {
            let _ = self.remove_artifact_paths(job_uuid, &artifact);
            let _ = self.recover_resource_sidecar(job_uuid, &manifest, resource);
            let _ = self.remove_manifest_artifact(job_uuid, &artifact.final_path);
            return Err(error);
        }

        artifact_committed(&mut manifest, &artifact);
        let next_sidecar = extend_sidecar_manifest(
            &completion_check.sidecar,
            &sidecar_bytes,
            object_ids.len() as u64,
            completion_check.digest_state,
        );
        *resource_sidecar_mut(&mut manifest, resource) = next_sidecar.clone();
        self.commit_manifest(&manifest)?;
        self.remember_completed_resource_append(
            job_uuid,
            CompletedSidecarAppend {
                resource,
                previous: &completion_check.sidecar,
                next: next_sidecar,
                object_ids,
                previous_digest_state: completion_check.digest_state,
                sidecar_delta: &sidecar_bytes,
            },
        );
        Ok(())
    }

    fn stage_transaction_files(
        &self,
        job_uuid: Uuid,
        resource: ObjectResource,
        artifact: &ArtifactManifest,
        payload: &[u8],
        sidecar: &SidecarManifest,
        sidecar_delta: &[u8],
    ) -> SpoolResult<()> {
        let sidecar_path = self.resource_sidecar_path(job_uuid, resource);
        append_completed_id_lines(&sidecar_path, sidecar, sidecar_delta)?;
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
        self.read_completed_sidecar_snapshot(job_uuid, sidecar, resource)
            .map(|snapshot| snapshot.ids)
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

struct CompletedSidecarSnapshot {
    ids: Vec<String>,
    digest_state: u64,
}

struct CompletedPageCheck {
    already_complete: bool,
    sidecar: SidecarManifest,
    digest_state: u64,
    sidecar_read_bytes: usize,
}

struct CompletedSidecarAppend<'a> {
    resource: ObjectResource,
    previous: &'a SidecarManifest,
    next: SidecarManifest,
    object_ids: &'a [&'a str],
    previous_digest_state: u64,
    sidecar_delta: &'a [u8],
}

impl SpoolStore {
    fn completed_resource_page_check(
        &self,
        job_uuid: Uuid,
        manifest: &SpoolManifest,
        resource: ObjectResource,
        object_ids: &[&str],
    ) -> SpoolResult<CompletedPageCheck> {
        let key = CompletedResourceKey { job_uuid, resource };
        let sidecar = resource_sidecar(manifest, resource).clone();
        let mut cache = self
            .completed_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sidecar_read_bytes = 0;
        if !cache
            .entries
            .get(&key)
            .is_some_and(|entry| entry.matches(&sidecar))
        {
            let snapshot = self.read_completed_sidecar_snapshot(job_uuid, &sidecar, resource)?;
            sidecar_read_bytes = sidecar.length as usize;
            cache.entries.insert(
                key,
                CachedCompletedIds {
                    generation: sidecar.generation,
                    length: sidecar.length,
                    digest: sidecar.digest.clone(),
                    count: sidecar.count,
                    digest_state: snapshot.digest_state,
                    ids: snapshot.ids.into_iter().collect(),
                },
            );
        }
        let entry = cache
            .entries
            .get(&key)
            .expect("completed-ID cache entry must exist after hydration");
        Ok(CompletedPageCheck {
            already_complete: page_is_already_complete(&entry.ids, object_ids)?,
            sidecar,
            digest_state: entry.digest_state,
            sidecar_read_bytes,
        })
    }

    fn remember_completed_resource_append(
        &self,
        job_uuid: Uuid,
        append: CompletedSidecarAppend<'_>,
    ) {
        let key = CompletedResourceKey {
            job_uuid,
            resource: append.resource,
        };
        let mut cache = self
            .completed_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = cache
            .entries
            .entry(key)
            .or_insert_with(|| CachedCompletedIds {
                generation: append.previous.generation,
                length: append.previous.length,
                digest: append.previous.digest.clone(),
                count: append.previous.count,
                digest_state: append.previous_digest_state,
                ids: HashSet::new(),
            });
        for object_id in append.object_ids {
            entry.ids.insert((*object_id).to_string());
        }
        entry.generation = append.next.generation;
        entry.length = append.next.length;
        entry.digest = append.next.digest;
        entry.count = append.next.count;
        entry.digest_state =
            update_sidecar_digest_state(append.previous_digest_state, append.sidecar_delta);
    }

    fn read_completed_sidecar_snapshot(
        &self,
        job_uuid: Uuid,
        sidecar: &SidecarManifest,
        resource: ObjectResource,
    ) -> SpoolResult<CompletedSidecarSnapshot> {
        let path = self.resource_sidecar_path(job_uuid, resource);
        if !path.exists() {
            return if sidecar.length == 0 && sidecar.count == 0 {
                Ok(CompletedSidecarSnapshot {
                    ids: Vec::new(),
                    digest_state: FNV64_OFFSET,
                })
            } else {
                Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt))
            };
        }
        let bytes = fs::read(path)?;
        if bytes.len() < sidecar.length as usize {
            return Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt));
        }
        let committed = &bytes[..sidecar.length as usize];
        if !sidecar_digest_matches(sidecar, committed) {
            return Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt));
        }
        let ids = String::from_utf8_lossy(committed)
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if ids.len() as u64 != sidecar.count {
            return Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt));
        }
        Ok(CompletedSidecarSnapshot {
            ids,
            digest_state: update_sidecar_digest_state(FNV64_OFFSET, committed),
        })
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

fn page_is_already_complete(completed: &HashSet<String>, object_ids: &[&str]) -> SpoolResult<bool> {
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

fn id_lines(object_ids: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for id in object_ids {
        bytes.extend_from_slice(id.as_bytes());
        bytes.push(b'\n');
    }
    bytes
}

pub(super) fn append_completed_id_lines(
    path: &Path,
    sidecar: &SidecarManifest,
    sidecar_delta: &[u8],
) -> SpoolResult<u64> {
    if !path.exists() && (sidecar.length > 0 || sidecar.count > 0) {
        return Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt));
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.set_len(sidecar.length)?;
    file.seek(io::SeekFrom::Start(sidecar.length))?;
    file.write_all(sidecar_delta)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(sidecar.length + sidecar_delta.len() as u64)
}

pub(super) fn extend_sidecar_manifest(
    sidecar: &SidecarManifest,
    sidecar_delta: &[u8],
    delta_count: u64,
    previous_digest_state: u64,
) -> SidecarManifest {
    let next_digest_state = update_sidecar_digest_state(previous_digest_state, sidecar_delta);
    SidecarManifest {
        generation: sidecar.generation + 1,
        length: sidecar.length + sidecar_delta.len() as u64,
        digest: sidecar_digest(next_digest_state),
        count: sidecar.count + delta_count,
    }
}

fn sidecar_digest_matches(sidecar: &SidecarManifest, committed: &[u8]) -> bool {
    if sidecar.length == 0 && sidecar.count == 0 && sidecar.digest.is_empty() {
        return true;
    }
    if let Some(expected) = sidecar.digest.strip_prefix(SIDECAR_DIGEST_PREFIX) {
        return expected
            == format!(
                "{:016x}",
                update_sidecar_digest_state(FNV64_OFFSET, committed)
            );
    }
    hex_digest(committed) == sidecar.digest
}

fn sidecar_digest(state: u64) -> String {
    format!("{SIDECAR_DIGEST_PREFIX}{state:016x}")
}

fn update_sidecar_digest_state(mut state: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(FNV64_PRIME);
    }
    state
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
