use super::publication::{
    publication_scan_targets, scan_and_repair_publication_target,
    scan_and_repair_publication_target_while_fenced, PublicationRepairReport, PublicationTarget,
};
use super::{IndexManager, TenantQuiesce};
use crate::{FlapjackError, Result};
use std::sync::Arc;

impl IndexManager {
    /// Hand a committed native snapshot to the runtime before releasing quiesce.
    /// The journal stays immutable; subsequent runtime writes need not retain its digest.
    pub fn adopt_snapshot_publication(
        &self,
        journal: &super::publication::PublicationJournal,
    ) -> Result<()> {
        super::publication::adopt_snapshot_publication(&self.base_path, journal)
    }

    /// Repair and report a single node-local publication target.
    pub fn repair_publication_target(&self, tenant: &str) -> Result<PublicationRepairReport> {
        let target = PublicationTarget::new(tenant)?;
        let tenant_id = tenant.to_string();

        // Let the canonical scanner own the normal path, including its
        // non-mutating vacant fast path. Fencing an absent target here first
        // would create epoch.lock and turn our own fence into unresolved
        // publication evidence, preventing a first snapshot from being staged.
        // A pending analytics deletion is the one IndexManager-only lifecycle
        // concern; recheck after the scan so a concurrently published marker is
        // reconciled below rather than returned as clean.
        if !super::publication::analytics_purge_is_pending(&self.base_path, &target)? {
            let report = scan_and_repair_publication_target(
                &self.base_path,
                &self.publication_analytics_config(),
                target.clone(),
            )?;
            if !super::publication::analytics_purge_is_pending(&self.base_path, &target)? {
                if report.live_target_mutated {
                    self.unload(&tenant_id)?;
                }
                return Ok(report);
            }
        }

        let _target_fence =
            super::publication::fence_publication_admission(&self.base_path, &target).map_err(
                |error| {
                    crate::error::FlapjackError::Io(format!(
                        "publication repair target fence failed: {error}"
                    ))
                },
            )?;
        self.repair_publication_target_while_fenced(target)
    }

    /// Quiesce one tenant and repair its publication state under that same
    /// admission fence.
    ///
    /// Snapshot replacement needs the fence to remain held from repair through
    /// staging and activation. Keeping the pair in this owner prevents callers
    /// from nesting `repair_publication_target`'s target lock inside an existing
    /// quiesce lock.
    pub async fn quiesce_and_repair_publication_target(
        self: &Arc<Self>,
        tenant: &str,
    ) -> Result<(TenantQuiesce, PublicationRepairReport)> {
        let target = PublicationTarget::new(tenant)?;
        let quiesce = self.quiesce_tenant(&tenant.to_string()).await?;
        let manager = Arc::clone(self);
        let tenant_id = tenant.to_string();
        tokio::task::spawn_blocking(move || {
            manager
                .repair_publication_target_while_fenced(target)
                .map(|report| (quiesce, report))
        })
        .await
        .map_err(|error| {
            FlapjackError::Io(format!(
                "publication repair task failed for {tenant_id}: {error}"
            ))
        })?
    }

    fn repair_publication_target_while_fenced(
        &self,
        target: PublicationTarget,
    ) -> Result<PublicationRepairReport> {
        let tenant_id = target.as_str().to_string();
        if super::publication::analytics_purge_is_pending(&self.base_path, &target)? {
            self.unload(&tenant_id)?;
        }
        self.reconcile_pending_analytics_deletion_while_fenced(&target)?;
        let report = scan_and_repair_publication_target_while_fenced(
            &self.base_path,
            &self.publication_analytics_config(),
            target,
        )?;
        if report.live_target_mutated {
            self.unload(&tenant_id)?;
        }
        Ok(report)
    }

    /// Repair node-local publication transactions before any affected tenant is served.
    pub fn repair_publications_before_serve(&self) -> Result<Vec<PublicationRepairReport>> {
        let mut targets = publication_scan_targets(&self.base_path)?;
        // Legacy indexes predate the publication namespace. Admit only readable
        // durable indexes with a checked watermark, through the existing fenced
        // repair owner. Never reconstruct missing/corrupt evidence as zero.
        let mut legacy = std::collections::HashSet::new();
        let entries = match std::fs::read_dir(&self.base_path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !entry.file_type()?.is_dir()
                || crate::validate_index_name(&name).is_err()
                || targets.iter().any(|target| target.as_str() == name)
            {
                continue;
            }
            if matches!(
                crate::index::oplog::read_checked_committed_seq(&entry.path()),
                Ok(Some(_))
            ) && self.tenant_durable_doc_count(&name).is_ok()
            {
                legacy.insert(name.clone());
                targets.push(PublicationTarget::new(name)?);
            }
        }
        targets.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let mut reports = Vec::new();
        for target in targets {
            if !legacy.contains(target.as_str())
                && !super::publication::publication_target_has_repair_candidate(
                    &self.base_path,
                    &target,
                )?
            {
                continue;
            }
            self.unload(&target.as_str().to_string())?;
            reports.push(self.repair_publication_target(target.as_str())?);
        }
        Ok(reports)
    }
}
