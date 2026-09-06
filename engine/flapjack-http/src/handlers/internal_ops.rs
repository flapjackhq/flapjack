mod document_ops;
mod index_ops;
mod resource_ops;

use super::AppState;
use flapjack::index::oplog::OpLogEntry;
use flapjack::index::settings::IndexSettings;
use flapjack::types::TaskStatus;
use flapjack::IndexManager;

pub(crate) use document_ops::{
    apply_delete_op, apply_upsert_op, flush_document_batch, preflight_document_op,
    ReplicatedDocumentBatch,
};
#[cfg(test)]
pub(crate) use document_ops::{
    run_after_document_proof_accepted_hook_for_test,
    set_after_document_proof_accepted_hook_for_test,
};
pub(crate) use index_ops::{
    apply_clear_index_op, apply_copy_index_op, apply_move_index_op, preflight_index_op,
};
pub(crate) use resource_ops::{
    apply_clear_rules_op, apply_clear_synonyms_op, apply_delete_rule_op, apply_delete_synonym_op,
    apply_save_rule_op, apply_save_rules_op, apply_save_synonym_op, apply_save_synonyms_op,
    preflight_resource_op,
};

pub(in crate::handlers) async fn wait_for_durable_replication_task(
    manager: &IndexManager,
    tenant_id: &str,
    operation: &str,
    task_id: &str,
) -> Result<(), String> {
    manager
        .wait_for_write_durable(task_id)
        .await
        .map_err(|error| format!("{operation} failed: {error}"))?;
    let task = manager
        .get_task(task_id)
        .map_err(|error| format!("{operation} failed to read terminal task: {error}"))?;
    if task.rejected_count > 0 {
        return Err(format!(
            "[REPL {}] {} task {} rejected {} document(s)",
            tenant_id, operation, task.id, task.rejected_count
        ));
    }
    if task.status != TaskStatus::Succeeded {
        return Err(format!(
            "[REPL {}] {} task {} did not complete successfully: status={:?}, rejected_count={}",
            tenant_id, operation, task.id, task.status, task.rejected_count
        ));
    }
    Ok(())
}

fn parse_settings_op(tenant_id: &str, op_entry: &OpLogEntry) -> Result<IndexSettings, String> {
    serde_json::from_value::<IndexSettings>(op_entry.payload.clone()).map_err(|error| {
        format!(
            "[REPL {}] settings seq {} invalid payload: {}",
            tenant_id, op_entry.seq, error
        )
    })
}

pub(crate) fn preflight_settings_op(tenant_id: &str, op_entry: &OpLogEntry) -> Result<(), String> {
    let settings = parse_settings_op(tenant_id, op_entry)?;
    super::settings::validate_exact_index_settings(tenant_id, &settings)
        .map(|_| ())
        .map_err(|error| {
            format!(
                "[REPL {}] settings seq {} invalid payload: {}",
                tenant_id, op_entry.seq, error
            )
        })
}

pub(crate) async fn apply_settings_op(
    state: &AppState,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let settings = parse_settings_op(tenant_id, op_entry)?;
    super::settings::apply_exact_index_settings_no_oplog(state, tenant_id, &settings, true)
        .await
        .map(|_| ())
        .map_err(|error| {
            let detail = match error {
                crate::error_response::HandlerError::Core(error) => error.to_string(),
                crate::error_response::HandlerError::Custom { message, .. }
                | crate::error_response::HandlerError::Coded { message, .. } => message,
            };
            format!(
                "[REPL {}] settings seq {} failed to apply settings: {}",
                tenant_id, op_entry.seq, detail
            )
        })
}
