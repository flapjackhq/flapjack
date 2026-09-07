use super::{publication, IndexManager};
use crate::error::{FlapjackError, Result};
use crate::index::oplog::{AppendDurability, OpLog, OpLogEntry, OpLogFinalizationPermit};
use std::sync::Arc;

pub(super) fn is_immediately_committed_op(op_type: &str) -> bool {
    matches!(
        op_type,
        "settings"
            | "save_synonym"
            | "save_synonyms"
            | "delete_synonym"
            | "clear_synonyms"
            | "save_rule"
            | "save_rules"
            | "delete_rule"
            | "clear_rules"
            | "clear_index"
            | "move_index"
            | "copy_index"
    )
}

/// An appended sequence with exclusive finalization and shared tenant lifecycle ownership.
/// Drop the receipt after readback (and before another append or tenant replacement).
pub struct OpLogAppendReceipt {
    oplog: Arc<OpLog>,
    sequence: u64,
    expected: AppendedOperationIdentity,
    taskless_recovery_sequence: Option<u64>,
    finalization: OpLogFinalizationPermit,
    _admission: publication::PublicationEpochAdmissionGuard,
}

struct AppendedOperationIdentity {
    tenant_id: String,
    op_type: String,
    payload: serde_json::Value,
}

impl AppendedOperationIdentity {
    fn matches(&self, entry: &OpLogEntry) -> bool {
        entry.tenant_id == self.tenant_id
            && entry.op_type == self.op_type
            && entry.payload == self.payload
    }
}

impl OpLogAppendReceipt {
    /// Read exactly the appended row while lifecycle replacement and retraction are excluded.
    ///
    /// An absent or corrupt row disproves the durability this append already certified,
    /// so the failure withdraws that certification and leaves the oplog fail-closed until
    /// the existing recovery owner reconciles the tail with the durable watermark.
    pub fn read_entry(&mut self) -> Result<OpLogEntry> {
        let entry = self
            .oplog
            .read_since(self.sequence - 1)
            .and_then(|entries| {
                entries
                    .into_iter()
                    .find(|entry| entry.seq == self.sequence)
                    .ok_or_else(|| {
                        FlapjackError::Io(format!(
                            "appended oplog sequence {} is absent during readback",
                            self.sequence
                        ))
                    })
            })
            .and_then(|entry| {
                if self.expected.matches(&entry) {
                    Ok(entry)
                } else {
                    Err(FlapjackError::Io(format!(
                        "oplog sequence {} does not match its append receipt",
                        self.sequence
                    )))
                }
            });
        match entry {
            Ok(entry) => Ok(entry),
            Err(readback_error) => {
                if let Err(recovery_error) = self
                    .finalization
                    .require_recovery(self.taskless_recovery_sequence)
                {
                    return Err(FlapjackError::Io(format!(
                        "{readback_error}; failed to preserve durable readback recovery: {recovery_error}"
                    )));
                }
                Err(readback_error)
            }
        }
    }
}

impl IndexManager {
    /// Append with the operation's durability policy and retain ownership for exact-row readback.
    pub async fn append_oplog(
        &self,
        tenant_id: &str,
        op_type: &str,
        payload: serde_json::Value,
    ) -> Result<OpLogAppendReceipt> {
        let target = publication::PublicationTarget::new(tenant_id)?;
        let observed = publication::capture_publication_epoch(&self.base_path, &target)
            .map_err(|error| Self::admission_epoch_error(tenant_id, error))?;
        let oplog = {
            let _admission = self.admit_oplog_generation(&target, observed)?;
            self.get_or_create_oplog_result(tenant_id)?
        };
        // Lifecycle fences can block an executor thread. Release admission before
        // awaiting finalization, then validate both the epoch and cached owner again.
        let mut permit = oplog.acquire_finalization().await?;
        let admission = self
            .admit_oplog_generation(&target, observed)
            .and_then(|guard| {
                if self
                    .get_oplog(tenant_id)
                    .is_some_and(|current| Arc::ptr_eq(&current, &oplog))
                {
                    Ok(guard)
                } else {
                    Err(FlapjackError::Io(format!(
                        "oplog owner changed before append for index {tenant_id}"
                    )))
                }
            });
        let admission = match admission {
            Ok(guard) => guard,
            Err(error) => {
                permit.mark_safe();
                return Err(error);
            }
        };
        let immediately_committed = is_immediately_committed_op(op_type);
        let durability = if immediately_committed {
            AppendDurability::Synced
        } else {
            AppendDurability::Buffered
        };
        let expected = AppendedOperationIdentity {
            tenant_id: tenant_id.to_string(),
            op_type: op_type.to_string(),
            payload: payload.clone(),
        };
        let sequence = oplog.append(op_type, payload, durability)?;
        if immediately_committed {
            permit.advance_committed_seq(sequence)?;
        }
        permit.mark_safe();
        Ok(OpLogAppendReceipt {
            oplog,
            sequence,
            expected,
            taskless_recovery_sequence: immediately_committed.then_some(sequence),
            finalization: permit,
            _admission: admission,
        })
    }

    fn admit_oplog_generation(
        &self,
        target: &publication::PublicationTarget,
        observed: publication::PublicationEpoch,
    ) -> Result<publication::PublicationEpochAdmissionGuard> {
        publication::try_validate_publication_epoch_admission(&self.base_path, target, observed)
            .map_err(|error| Self::admission_epoch_error(target.as_str(), error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn metadata_oplog_waiting_append_rejects_quiesced_owner_without_pinning_lifecycle() {
        let tmp = tempfile::TempDir::new().unwrap();
        let manager = IndexManager::new(tmp.path());
        let tenant_id = "metadata_waiting_append_quiesce".to_string();
        manager.create_tenant(&tenant_id).unwrap();
        let oplog = manager.get_or_create_oplog_result(&tenant_id).unwrap();
        let mut permit = oplog.acquire_finalization().await.unwrap();
        let append = manager.append_oplog(&tenant_id, "settings", serde_json::json!({}));
        tokio::pin!(append);
        assert!(futures::poll!(append.as_mut()).is_pending());
        let quiesce = timeout(Duration::from_secs(1), manager.quiesce_tenant(&tenant_id))
            .await
            .expect("waiting for finalization must not pin lifecycle admission")
            .unwrap();
        drop(quiesce);
        permit.mark_safe();
        drop(permit);
        assert!(
            append.await.is_err(),
            "a retired cached owner cannot append"
        );
        assert_eq!(oplog.current_seq(), 0);
        let mut old_owner_permit = oplog.acquire_finalization().await.unwrap();
        old_owner_permit.mark_safe();
        drop(old_owner_permit);
        let mut receipt = manager
            .append_oplog(&tenant_id, "settings", serde_json::json!({"retry": true}))
            .await
            .unwrap();
        let row = receipt.read_entry().unwrap();
        assert_eq!(row.seq, 1);
        assert_eq!(row.payload, serde_json::json!({"retry": true}));
    }

    fn active_segment_path(tenant_root: &std::path::Path) -> std::path::PathBuf {
        std::fs::read_dir(tenant_root.join("oplog"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .unwrap()
    }

    #[tokio::test]
    async fn metadata_oplog_failed_readback_blocks_appends_in_process_and_after_reopen() {
        let tmp = tempfile::TempDir::new().unwrap();
        let tenant_id = "metadata_readback_failure_fails_closed".to_string();
        let manager = IndexManager::new(tmp.path());
        manager.create_tenant(&tenant_id).unwrap();
        let mut receipt = manager
            .append_oplog(&tenant_id, "settings", serde_json::json!({"seed": true}))
            .await
            .unwrap();
        let oplog = manager.get_oplog(&tenant_id).unwrap();
        assert_eq!(oplog.committed_seq().unwrap(), Some(1));

        let segment_path = active_segment_path(&tmp.path().join(&tenant_id));
        let durable_row = std::fs::read(&segment_path).unwrap();
        std::fs::write(&segment_path, b"corrupt\n").unwrap();
        assert!(
            receipt.read_entry().is_err(),
            "a corrupt committed row must fail readback"
        );
        std::fs::write(&segment_path, durable_row).unwrap();
        assert_eq!(
            oplog.read_since(0).unwrap().len(),
            1,
            "the transient read failure must be repaired before restart"
        );

        let held_receipt_append = timeout(
            Duration::from_secs(5),
            manager.append_oplog(&tenant_id, "settings", serde_json::json!({"after": true})),
        )
        .await
        .expect("a failed readback must poison finalization instead of blocking on the permit");
        assert!(
            held_receipt_append.is_err(),
            "the live oplog must not stay writable after a failed readback"
        );
        drop(receipt);
        assert!(manager
            .append_oplog(
                &tenant_id,
                "settings",
                serde_json::json!({"after_drop": true})
            )
            .await
            .is_err());
        assert!(oplog.acquire_finalization().await.is_err());
        assert_eq!(oplog.committed_seq().unwrap(), Some(1));
        assert_eq!(oplog.current_seq(), 1);

        drop(oplog);
        drop(manager);
        let reopened = IndexManager::new(tmp.path());
        assert!(
            reopened
                .append_oplog(
                    &tenant_id,
                    "settings",
                    serde_json::json!({"reopened": true})
                )
                .await
                .is_err(),
            "the failed readback must keep the oplog fail-closed across restart"
        );
        let reopened_oplog = reopened.get_or_create_oplog_result(&tenant_id).unwrap();
        assert_eq!(
            reopened_oplog.current_seq(),
            0,
            "durable recovery must retract the receipt whose readback failed"
        );
        assert!(reopened_oplog.acquire_finalization().await.is_err());
        assert_eq!(
            crate::index::oplog::read_committed_seq(&tmp.path().join(&tenant_id)),
            1,
            "a failed publication must not advance the durable watermark"
        );
    }
}
