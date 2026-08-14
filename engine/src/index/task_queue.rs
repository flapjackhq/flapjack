//! Background task queue that serializes long-running index operations (currently export) via a bounded mpsc channel, tracking each task's lifecycle in a shared `DashMap`.
use crate::error::Result;
use crate::index::manager::task_retention::TaskRetention;
use crate::index::manager::MAX_TASKS_PER_TENANT;
use crate::types::{TaskInfo, TaskStatus, TenantId};
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tokio::sync::mpsc;

pub struct TaskQueue {
    sender: mpsc::Sender<TaskCommand>,
    tasks: Arc<DashMap<String, TaskInfo>>,
    task_retention: Arc<TaskRetention>,
}

pub enum TaskCommand {
    Export {
        task_id: String,
        tenant_id: TenantId,
        dest_path: PathBuf,
    },
}

impl TaskQueue {
    pub(crate) fn new(
        manager: Weak<crate::IndexManager>,
        tasks: Arc<DashMap<String, TaskInfo>>,
        task_retention: Arc<TaskRetention>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(100);

        tokio::spawn(process_tasks(rx, tasks.clone(), manager));

        TaskQueue {
            sender: tx,
            tasks,
            task_retention,
        }
    }

    #[cfg(test)]
    /// Build a deterministically closed queue so admission tests exercise the
    /// real sender failure without racing a background receiver shutdown.
    pub(crate) fn closed_for_test(
        tasks: Arc<DashMap<String, TaskInfo>>,
        task_retention: Arc<TaskRetention>,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);
        TaskQueue {
            sender,
            tasks,
            task_retention,
        }
    }

    pub fn enqueue_export(
        &self,
        task: TaskInfo,
        tenant_id: TenantId,
        dest_path: PathBuf,
    ) -> Result<()> {
        let permit = self
            .sender
            .try_reserve()
            .map_err(|_| crate::FlapjackError::QueueFull)?;
        let task_id = task.id.clone();
        self.task_retention
            .insert(&self.tasks, &tenant_id, task, MAX_TASKS_PER_TENANT);
        permit.send(TaskCommand::Export {
            task_id,
            tenant_id,
            dest_path,
        });
        Ok(())
    }
}

fn update_export_status(tasks: &DashMap<String, TaskInfo>, task_id: &str, status: TaskStatus) {
    let Some(current) = tasks.get(task_id) else {
        return;
    };
    let mut updated = current.clone();
    drop(current);
    updated.status = status;
    let numeric_alias = updated.numeric_id.to_string();
    tasks.insert(task_id.to_string(), updated.clone());
    tasks.insert(numeric_alias, updated);
}

/// Consume commands from the task-queue channel and dispatch each to its handler.
///
/// Runs as a long-lived tokio task. On each received `TaskCommand` the weak
/// `IndexManager` reference is upgraded; if the manager has been dropped the
/// task status is marked as failed and the loop exits. Currently all commands
/// are `Export` variants and are forwarded to `process_export`.
///
/// # Arguments
///
/// * `rx` — Receiving half of the bounded command channel.
/// * `tasks` — Shared map of in-flight task metadata, updated with status changes.
/// * `manager_weak` — Weak reference to the `IndexManager`; breaks the loop when dropped.
async fn process_tasks(
    mut rx: mpsc::Receiver<TaskCommand>,
    tasks: Arc<DashMap<String, TaskInfo>>,
    manager_weak: Weak<crate::IndexManager>,
) {
    while let Some(cmd) = rx.recv().await {
        let manager = match manager_weak.upgrade() {
            Some(m) => m,
            None => {
                let TaskCommand::Export { task_id, .. } = cmd;
                update_export_status(
                    &tasks,
                    &task_id,
                    TaskStatus::Failed("Manager dropped".to_string()),
                );
                break;
            }
        };

        let TaskCommand::Export {
            task_id,
            tenant_id,
            dest_path,
        } = cmd;
        process_export(task_id, tenant_id, dest_path, manager, tasks.clone()).await;
    }
}

/// Execute a full index export for a single tenant.
///
/// Quiesces the tenant through the canonical [`IndexManager::quiesce_tenant`]
/// contract — draining and merge-quiescing the persistent writer and clearing
/// runtime caches — then copies the on-disk index directory to `dest_path`
/// using a blocking filesystem copy off the async runtime.
/// Task status in `tasks` is updated to `Processing`, then to `Succeeded` or
/// `Failed` at each stage.
///
/// # Arguments
///
/// * `task_id` — Unique identifier used to update status in the shared task map.
/// * `tenant_id` — Tenant whose index is being exported.
/// * `dest_path` — Target directory for the recursive copy.
/// * `manager` — Shared `IndexManager` used to drain writes and locate source data.
/// * `tasks` — Shared map of task metadata, mutated with progress and outcome.
async fn process_export(
    task_id: String,
    tenant_id: TenantId,
    dest_path: PathBuf,
    manager: Arc<crate::IndexManager>,
    tasks: Arc<DashMap<String, TaskInfo>>,
) {
    update_export_status(&tasks, &task_id, TaskStatus::Processing);

    // Stop admission, drain the persistent writer through merge quiescence, and
    // drop the cached generation before reading files off disk. This is the same
    // canonical quiesce contract used by delete/import/replace so the export
    // tarball reflects a merge-quiesced generation.
    // The guard is held across the blocking copy below: without it, admission
    // re-opens the moment quiesce returns and a replacement writer could commit
    // into the tree while it is being read.
    let _quiesce = match manager.quiesce_tenant(&tenant_id).await {
        Ok(quiesce) => quiesce,
        Err(error) => {
            update_export_status(
                &tasks,
                &task_id,
                TaskStatus::Failed(format!("Commit failed: {}", error)),
            );
            return;
        }
    };

    let src = manager.base_path.join(&tenant_id);
    let dest = dest_path.clone();
    #[cfg(any(debug_assertions, feature = "test-support"))]
    let checkpoint_tenant_id = tenant_id.clone();

    let copy_result = tokio::task::spawn_blocking(move || {
        #[cfg(any(debug_assertions, feature = "test-support"))]
        crate::index::write_queue::record_writer_lifecycle_publication_checkpoint(
            &checkpoint_tenant_id,
            "manager_export_publication",
        );
        std::fs::create_dir_all(&dest)?;
        crate::index::utils::copy_dir_recursive(&src, &dest)
    })
    .await;

    match copy_result {
        Ok(Ok(())) => {
            update_export_status(&tasks, &task_id, TaskStatus::Succeeded);
        }
        Ok(Err(e)) => {
            update_export_status(
                &tasks,
                &task_id,
                TaskStatus::Failed(format!("Copy failed: {}", e)),
            );
        }
        Err(e) => {
            update_export_status(
                &tasks,
                &task_id,
                TaskStatus::Failed(format!("Spawn blocking failed: {:?}", e)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn closed_admission_rejects_without_publishing_task_aliases() {
        let temp_dir = TempDir::new().unwrap();
        let tasks = Arc::new(DashMap::new());
        let queue = TaskQueue::closed_for_test(tasks.clone(), Arc::new(TaskRetention::new()));
        let task = TaskInfo::new("export_tenant_test".to_string(), 42, 0);

        let result = queue.enqueue_export(
            task.clone(),
            "tenant".to_string(),
            temp_dir.path().join("export"),
        );

        assert!(matches!(result, Err(crate::FlapjackError::QueueFull)));
        assert!(!tasks.contains_key(&task.id));
        assert!(!tasks.contains_key(&task.numeric_id.to_string()));
    }

    #[test]
    fn status_transition_updates_both_aliases_with_same_task_identity() {
        let tasks = Arc::new(DashMap::new());
        let task = TaskInfo::new("export_tenant_test".to_string(), 42, 0);
        tasks.insert(task.id.clone(), task.clone());
        tasks.insert(task.numeric_id.to_string(), task.clone());

        update_export_status(&tasks, &task.id, TaskStatus::Succeeded);

        let canonical = tasks.get(&task.id).unwrap();
        let numeric_alias = tasks.get(&task.numeric_id.to_string()).unwrap();
        assert_eq!(canonical.status, TaskStatus::Succeeded);
        assert_eq!(numeric_alias.status, TaskStatus::Succeeded);
        assert_eq!(numeric_alias.id, canonical.id);
        assert_eq!(numeric_alias.numeric_id, canonical.numeric_id);
    }
}
