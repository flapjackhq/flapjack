use super::publication::{
    publication_scan_targets, scan_and_repair_publication_target, scan_and_repair_publications,
    PublicationRepairReport, PublicationTarget,
};
use super::IndexManager;
use crate::Result;

impl IndexManager {
    /// Repair and report a single node-local publication target.
    pub fn repair_publication_target(&self, tenant: &str) -> Result<PublicationRepairReport> {
        let target = PublicationTarget::new(tenant)?;
        let tenant_id = tenant.to_string();
        let report = scan_and_repair_publication_target(
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
        let targets = publication_scan_targets(&self.base_path)?;
        for target in &targets {
            self.unload(&target.as_str().to_string())?;
        }
        let reports =
            scan_and_repair_publications(&self.base_path, &self.publication_analytics_config())?;
        for target in targets {
            self.unload(&target.as_str().to_string())?;
        }
        Ok(reports)
    }
}
