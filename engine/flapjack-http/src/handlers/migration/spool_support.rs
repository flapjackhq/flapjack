use super::*;

pub(super) fn public_view(manifest: &SpoolManifest) -> PublicExportView {
    let completed = manifest.counters.total();
    let total = manifest.denominators.total();
    let ratio = if total == 0 {
        1.0
    } else {
        completed as f64 / total as f64
    };
    PublicExportView {
        job_uuid: manifest.job_uuid,
        public_handle: manifest.public_handle.clone(),
        checkpoint_handle: manifest.checkpoint_handle.clone(),
        state: format!("{:?}", manifest.lifecycle),
        progress: SpoolProgress {
            completed,
            total,
            ratio,
        },
    }
}

pub(super) fn visible_artifacts(
    manifest: &SpoolManifest,
) -> impl Iterator<Item = &ArtifactManifest> {
    manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.state == ArtifactState::Visible)
}

pub(super) fn staged_count(manifest: &SpoolManifest) -> u64 {
    manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.state == ArtifactState::Staged)
        .count() as u64
}

pub(super) fn staged_bytes(manifest: &SpoolManifest) -> u64 {
    manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.state == ArtifactState::Staged)
        .map(|artifact| artifact.compressed_bytes)
        .sum()
}

pub(super) fn resource_count(counters: ResourceCounters, kind: ArtifactKind) -> u64 {
    match kind {
        ArtifactKind::Settings => counters.settings,
        ArtifactKind::DocumentPage => counters.documents,
        ArtifactKind::RulesPage => counters.rules,
        ArtifactKind::SynonymsPage => counters.synonyms,
        ArtifactKind::Config => counters.config,
    }
}

pub(super) fn lock_file(path: &Path) -> SpoolResult<LockGuard> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    file.lock_exclusive()?;
    Ok(LockGuard { file })
}

pub(super) fn write_atomic(dir: &Path, file_name: &str, bytes: &[u8]) -> SpoolResult<()> {
    let temp_name = format!("{TEMP_PREFIX}{file_name}-{}", Uuid::new_v4());
    let temp_path = dir.join(&temp_name);
    let final_path = dir.join(file_name);
    let mut file = File::create(&temp_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp_path, final_path)?;
    sync_dir(dir)
}

pub(super) fn sync_dir(path: &Path) -> SpoolResult<()> {
    File::open(path)?.sync_all().map_err(SpoolError::from)
}

pub(super) fn create_private_dir(path: &Path) -> SpoolResult<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(super) fn validate_relative(path: &str) -> SpoolResult<()> {
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(SpoolError::new(SpoolErrorKind::InvalidRelativePath));
    }
    Ok(())
}

pub(super) fn validate_source_identity_digest(digest: &str) -> SpoolResult<()> {
    if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(SpoolError::new(SpoolErrorKind::InvalidSourceIdentityDigest))
}

pub(super) fn new_handle() -> String {
    hex_digest(Uuid::new_v4().as_bytes())
}

pub(super) fn hex_digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

impl SpoolStore {
    pub(super) fn validate_artifact_limits(
        &self,
        manifest: &SpoolManifest,
        kind: ArtifactKind,
        compressed: u64,
        decompressed: u64,
        items: u64,
    ) -> SpoolResult<()> {
        let limits = manifest.limits;
        if compressed > limits.max_compressed_page_bytes {
            return Err(SpoolError::new(SpoolErrorKind::CompressedPageBytesExceeded));
        }
        if decompressed > limits.max_decompressed_page_bytes {
            return Err(SpoolError::new(
                SpoolErrorKind::DecompressedPageBytesExceeded,
            ));
        }
        if resource_count(manifest.counters, kind) + items > limits.max_items_per_resource {
            return Err(SpoolError::new(SpoolErrorKind::ResourceItemCountExceeded));
        }
        if manifest.bytes_committed + compressed > limits.max_bytes_per_job {
            return Err(SpoolError::new(SpoolErrorKind::JobBytesExceeded));
        }
        if self.global_committed_bytes()? + compressed > limits.max_global_bytes {
            return Err(SpoolError::new(SpoolErrorKind::GlobalBytesExceeded));
        }
        if self.available_bytes()? <= limits.minimum_free_bytes + compressed {
            return Err(SpoolError::new(SpoolErrorKind::FreeSpaceFloor));
        }
        if staged_count(manifest) >= limits.max_staged_artifacts {
            return Err(SpoolError::new(SpoolErrorKind::StagedArtifactCountExceeded));
        }
        if staged_bytes(manifest) + compressed > limits.max_staged_artifact_bytes {
            return Err(SpoolError::new(SpoolErrorKind::StagedArtifactBytesExceeded));
        }
        Ok(())
    }

    pub(super) fn write_and_publish_artifact(
        &self,
        job_uuid: Uuid,
        artifact: &ArtifactManifest,
        bytes: &[u8],
    ) -> SpoolResult<()> {
        let job_dir = self.job_dir(job_uuid);
        let temp_path = job_dir.join(&artifact.temp_path);
        let final_path = job_dir.join(&artifact.final_path);
        let mut file = File::create(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, &final_path)?;
        sync_dir(&job_dir)
    }

    pub(super) fn recover_artifacts(
        &self,
        job_uuid: Uuid,
        manifest: &mut SpoolManifest,
    ) -> SpoolResult<()> {
        let job_dir = self.job_dir(job_uuid);
        let mut keep = Vec::new();
        for artifact in manifest.artifacts.drain(..) {
            let final_path = job_dir.join(&artifact.final_path);
            let temp_path = job_dir.join(&artifact.temp_path);
            match artifact.state {
                ArtifactState::Visible if final_path.exists() => keep.push(artifact),
                ArtifactState::Visible => {}
                ArtifactState::Staged => {
                    let _ = fs::remove_file(temp_path);
                    let _ = fs::remove_file(final_path);
                }
            }
        }
        manifest.artifacts = keep;
        manifest.counters = ResourceCounters::from_visible_artifacts(visible_artifacts(manifest));
        manifest.bytes_committed = visible_artifacts(manifest)
            .map(|artifact| artifact.compressed_bytes)
            .sum();
        Ok(())
    }

    pub(super) fn recover_resource_sidecar(
        &self,
        job_uuid: Uuid,
        manifest: &SpoolManifest,
        resource: ObjectResource,
    ) -> SpoolResult<()> {
        let sidecar = resource_sidecar(manifest, resource);
        let path = self.resource_sidecar_path(job_uuid, resource);
        if path.exists() {
            let file = OpenOptions::new().write(true).open(path)?;
            file.set_len(sidecar.length)?;
            file.sync_all()?;
        } else if sidecar.length > 0 || sidecar.count > 0 {
            return Err(SpoolError::new(SpoolErrorKind::ManifestCorrupt));
        }
        Ok(())
    }

    pub(super) fn remove_artifact_paths(
        &self,
        job_uuid: Uuid,
        artifact: &ArtifactManifest,
    ) -> SpoolResult<()> {
        let job_dir = self.job_dir(job_uuid);
        let _ = fs::remove_file(job_dir.join(&artifact.temp_path));
        let _ = fs::remove_file(job_dir.join(&artifact.final_path));
        sync_dir(&job_dir)
    }

    pub(super) fn remove_manifest_artifact(
        &self,
        job_uuid: Uuid,
        final_path: &str,
    ) -> SpoolResult<()> {
        let mut manifest = self.read_manifest(job_uuid)?;
        manifest
            .artifacts
            .retain(|artifact| artifact.final_path != final_path);
        self.commit_manifest(&manifest)
    }

    pub(super) fn ensure_writable(&self, manifest: &SpoolManifest) -> SpoolResult<()> {
        match manifest.lifecycle {
            LifecycleState::Running => Ok(()),
            LifecycleState::Deleting | LifecycleState::Deleted => {
                Err(SpoolError::new(SpoolErrorKind::JobDeleting))
            }
            LifecycleState::Accepted | LifecycleState::Failed => {
                Err(SpoolError::new(SpoolErrorKind::JobTerminal))
            }
        }
    }

    pub(super) fn global_committed_bytes(&self) -> SpoolResult<u64> {
        let mut total = 0;
        for job_uuid in self.job_uuids()? {
            total += self.read_manifest(job_uuid)?.bytes_committed;
        }
        Ok(total)
    }

    pub(super) fn available_bytes(&self) -> SpoolResult<u64> {
        match self.free_bytes {
            Some(bytes) => Ok(bytes),
            None => available_space(&self.root).map_err(SpoolError::from),
        }
    }

    pub(super) fn clean_store_temp_files(&self, job_uuid: Uuid) -> SpoolResult<()> {
        for entry in fs::read_dir(self.job_dir(job_uuid))? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(TEMP_PREFIX) {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(())
    }

    pub(super) fn write_tombstone(&self, manifest: &SpoolManifest) -> SpoolResult<()> {
        let tombstone = serde_json::json!({
            "job_uuid": manifest.job_uuid,
            "state": "deleted",
            "deleted_at": manifest.deleted_at,
            "expires_at": manifest.expires_at,
        });
        let bytes = serde_json::to_vec_pretty(&tombstone)
            .map_err(|_| SpoolError::new(SpoolErrorKind::ManifestCorrupt))?;
        write_atomic(&self.job_dir(manifest.job_uuid), "tombstone.json", &bytes)
    }

    pub(super) fn read_manifest(&self, job_uuid: Uuid) -> SpoolResult<SpoolManifest> {
        let bytes = fs::read(self.manifest_path(job_uuid))?;
        serde_json::from_slice(&bytes).map_err(|_| SpoolError::new(SpoolErrorKind::ManifestCorrupt))
    }

    pub(super) fn commit_manifest(&self, manifest: &SpoolManifest) -> SpoolResult<()> {
        let bytes = serde_json::to_vec_pretty(manifest)
            .map_err(|_| SpoolError::new(SpoolErrorKind::ManifestCorrupt))?;
        write_atomic(&self.job_dir(manifest.job_uuid), MANIFEST_FILE, &bytes)
    }

    pub(super) fn manifest_path(&self, job_uuid: Uuid) -> PathBuf {
        self.job_dir(job_uuid).join(MANIFEST_FILE)
    }

    pub(crate) fn job_uuids(&self) -> SpoolResult<Vec<Uuid>> {
        let jobs_dir = self.root.join(JOBS_DIR);
        let mut job_uuids = Vec::new();
        for entry in fs::read_dir(jobs_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let name = entry.file_name();
                if let Ok(uuid) = Uuid::parse_str(&name.to_string_lossy()) {
                    job_uuids.push(uuid);
                }
            }
        }
        Ok(job_uuids)
    }

    pub(super) fn lock_root(&self) -> SpoolResult<LockGuard> {
        lock_file(&self.root.join(ROOT_LOCK_FILE))
    }

    pub(super) fn lock_job(&self, job_uuid: Uuid) -> SpoolResult<LockGuard> {
        lock_file(&self.job_dir(job_uuid).join(JOB_LOCK_FILE))
    }
}
