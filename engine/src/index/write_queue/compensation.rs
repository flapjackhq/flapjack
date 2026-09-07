//! Fail-closed cleanup for durable writes that did not reach Tantivy commit.

use std::sync::Arc;

use super::WriteFinalizationContext;

#[cfg(any(test, feature = "fault-injection"))]
static COMPENSATION_FAULTS: once_cell::sync::Lazy<dashmap::DashMap<String, usize>> =
    once_cell::sync::Lazy::new(dashmap::DashMap::new);
#[cfg(test)]
type BeforeOplogRetractionHook = Arc<dyn Fn(&str) + Send + Sync>;
#[cfg(test)]
static BEFORE_OPLOG_RETRACTION_HOOK: once_cell::sync::Lazy<
    std::sync::Mutex<Option<BeforeOplogRetractionHook>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(None));

/// Durable replay owners needed to retract an uncommitted write task.
pub(crate) struct DurableReplayState<'a> {
    #[cfg_attr(not(any(test, feature = "fault-injection")), allow(dead_code))]
    pub(crate) tenant_id: &'a str,
    pub(crate) admission_store: &'a super::admission::WriteAdmissionStore,
    pub(crate) oplog: Option<&'a crate::index::oplog::OpLog>,
}

/// Retract both durable replay routes before a failed verdict becomes public.
///
/// Task-scoped oplog retraction runs before admission removal. This ordering
/// keeps the admission record as a complete recovery route if an in-place oplog
/// neutralization changes a rejected row and then reports an I/O error.
pub(super) fn compensate_failed_commit_batch(
    context: &WriteFinalizationContext<'_>,
    pre_batch_oplog_seq: Option<u64>,
    task_ids: &[String],
) -> crate::error::Result<()> {
    compensate_uncommitted_tasks(
        DurableReplayState {
            tenant_id: context.tenant_id,
            admission_store: context.admission_store,
            oplog: context.oplog.map(Arc::as_ref),
        },
        pre_batch_oplog_seq.map_or(0, |floor| floor + 1),
        task_ids,
    )
}

/// Remove both durable replay routes for task-tagged writes that never committed.
///
/// The write worker calls this before publishing a terminal failure. A bounded
/// public waiter may retry it only after the worker has stopped, avoiding a race
/// with an active Tantivy commit.
pub(crate) fn compensate_uncommitted_tasks(
    replay_state: DurableReplayState<'_>,
    from_seq: u64,
    task_ids: &[String],
) -> crate::error::Result<()> {
    #[cfg(any(test, feature = "fault-injection"))]
    inject_compensation_fault(replay_state.tenant_id)?;

    #[cfg(test)]
    run_before_oplog_retraction_hook(replay_state.tenant_id);

    if let Some(oplog) = replay_state.oplog {
        oplog.retract_tasks_from(from_seq, task_ids.iter().map(String::as_str))?;
    }
    replay_state
        .admission_store
        .remove_tasks(task_ids.iter().map(String::as_str))
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) struct CompensationFaultGuard {
    tenant_id: String,
}

#[cfg(any(test, feature = "fault-injection"))]
impl Drop for CompensationFaultGuard {
    fn drop(&mut self) {
        COMPENSATION_FAULTS.remove(&self.tenant_id);
    }
}

#[cfg(test)]
pub(crate) fn fail_next_compensation_for_test(tenant_id: &str) -> CompensationFaultGuard {
    fail_compensation_attempts_for_test(tenant_id, 1)
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) fn fail_compensation_attempts_for_test(
    tenant_id: &str,
    attempts: usize,
) -> CompensationFaultGuard {
    assert!(attempts > 0, "compensation fault count must be non-zero");
    let tenant_id = tenant_id.to_string();
    assert!(
        COMPENSATION_FAULTS
            .insert(tenant_id.clone(), attempts)
            .is_none(),
        "a compensation failure is already armed for tenant {tenant_id}"
    );
    CompensationFaultGuard { tenant_id }
}

#[cfg(any(test, feature = "fault-injection"))]
fn inject_compensation_fault(tenant_id: &str) -> crate::error::Result<()> {
    let mut remove_fault = false;
    let should_fail = if let Some(mut remaining) = COMPENSATION_FAULTS.get_mut(tenant_id) {
        *remaining -= 1;
        remove_fault = *remaining == 0;
        true
    } else {
        false
    };
    if remove_fault {
        COMPENSATION_FAULTS.remove(tenant_id);
    }
    if should_fail {
        return Err(crate::error::FlapjackError::Io(
            "injected write-queue compensation failure".to_string(),
        ));
    }
    Ok(())
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) fn compensation_fault_attempts_remaining_for_test(tenant_id: &str) -> usize {
    COMPENSATION_FAULTS
        .get(tenant_id)
        .map_or(0, |remaining| *remaining)
}

#[cfg(test)]
pub(crate) struct BeforeOplogRetractionHookGuard {
    previous: Option<BeforeOplogRetractionHook>,
}

#[cfg(test)]
impl Drop for BeforeOplogRetractionHookGuard {
    fn drop(&mut self) {
        *BEFORE_OPLOG_RETRACTION_HOOK.lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
pub(crate) fn set_compensation_before_oplog_retraction_hook_for_test(
    hook: BeforeOplogRetractionHook,
) -> BeforeOplogRetractionHookGuard {
    let mut slot = BEFORE_OPLOG_RETRACTION_HOOK.lock().unwrap();
    BeforeOplogRetractionHookGuard {
        previous: slot.replace(hook),
    }
}

#[cfg(test)]
fn run_before_oplog_retraction_hook(tenant_id: &str) {
    let hook = BEFORE_OPLOG_RETRACTION_HOOK.lock().unwrap().clone();
    if let Some(hook) = hook {
        hook(tenant_id);
    }
}
