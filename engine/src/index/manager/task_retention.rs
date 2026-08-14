use crate::types::{TaskInfo, TaskStatus};
use dashmap::DashMap;
use std::sync::Mutex;

/// Serializes task publication with retention so concurrent admissions cannot
/// all observe the same free slot and race past the per-tenant bound.
pub(crate) struct TaskRetention {
    admission: Mutex<()>,
}

impl TaskRetention {
    pub(crate) fn new() -> Self {
        Self {
            admission: Mutex::new(()),
        }
    }

    pub(crate) fn insert(
        &self,
        tasks: &DashMap<String, TaskInfo>,
        tenant_id: &str,
        task: TaskInfo,
        max_tasks: usize,
    ) {
        let _guard = self
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // Treat this as an upsert. Removing the old aliases first means a
        // terminal-status refresh reserves exactly one slot, not two.
        tasks.remove(&task.id);
        tasks.remove(&task.numeric_id.to_string());
        Self::evict_locked(tasks, tenant_id, max_tasks, 1);
        tasks.insert(task.id.clone(), task.clone());
        tasks.insert(task.numeric_id.to_string(), task);
    }

    pub(crate) fn trim(
        &self,
        tasks: &DashMap<String, TaskInfo>,
        tenant_id: &str,
        max_tasks: usize,
    ) {
        let _guard = self
            .admission
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::evict_locked(tasks, tenant_id, max_tasks, 0);
    }

    fn evict_locked(
        tasks: &DashMap<String, TaskInfo>,
        tenant_id: &str,
        max_tasks: usize,
        reserved_slots: usize,
    ) {
        let write_prefix = format!("task_{tenant_id}_");
        let export_prefix = format!("export_{tenant_id}_");
        let tenant_tasks: Vec<_> = tasks
            .iter()
            .filter(|entry| {
                entry.key().starts_with(&write_prefix) || entry.key().starts_with(&export_prefix)
            })
            .map(|entry| {
                (
                    entry.key().clone(),
                    entry.value().numeric_id,
                    entry.value().created_at,
                    entry.value().status.clone(),
                )
            })
            .collect();

        let target_removals = tenant_tasks
            .len()
            .saturating_add(reserved_slots)
            .saturating_sub(max_tasks);
        if target_removals == 0 {
            return;
        }

        // In-flight tasks stay visible for wait loops. If there are not enough
        // terminal tasks, the registry may temporarily exceed the cap until a
        // later terminal transition invokes retention again.
        let mut terminal_tasks: Vec<_> = tenant_tasks
            .into_iter()
            .filter(|(_, _, _, status)| {
                !matches!(status, TaskStatus::Enqueued | TaskStatus::Processing)
            })
            .map(|(task_id, numeric_id, created_at, _)| (task_id, numeric_id, created_at))
            .collect();
        terminal_tasks.sort_by_key(|(_, _, created_at)| *created_at);
        for (task_id, numeric_id, _) in terminal_tasks.iter().take(target_removals) {
            tasks.remove(task_id);
            tasks.remove(&numeric_id.to_string());
        }
    }
}
