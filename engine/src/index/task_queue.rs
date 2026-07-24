//! Background task queue that serializes long-running index operations (currently export) via a bounded mpsc channel, tracking each task's lifecycle in a shared `DashMap`.
use crate::error::Result;
use crate::types::{TaskInfo, TaskStatus, TenantId};
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tokio::sync::mpsc;

pub struct TaskQueue {
    pub(crate) sender: mpsc::Sender<TaskCommand>,
}

pub enum TaskCommand {
    Export {
        task_id: String,
        tenant_id: TenantId,
        dest_path: PathBuf,
    },
}

impl TaskQueue {
    pub fn new(manager: Weak<crate::IndexManager>, tasks: Arc<DashMap<String, TaskInfo>>) -> Self {
        let (tx, rx) = mpsc::channel(100);

        tokio::spawn(process_tasks(rx, tasks.clone(), manager));

        TaskQueue { sender: tx }
    }

    pub async fn enqueue_export(
        &self,
        task_id: String,
        tenant_id: TenantId,
        dest_path: PathBuf,
    ) -> Result<()> {
        self.sender
            .send(TaskCommand::Export {
                task_id,
                tenant_id,
                dest_path,
            })
            .await
            .map_err(|_| crate::FlapjackError::QueueFull)
    }
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
                tasks.alter(&task_id, |_, mut t| {
                    t.status = TaskStatus::Failed("Manager dropped".to_string());
                    t
                });
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
/// Flushes in-flight writes by draining the tenant's write queue and awaiting
/// its write-task handle, then copies the on-disk index directory to
/// `dest_path` using a blocking filesystem copy. The tenant's writer and
/// loaded-index entries are removed from the manager regardless of outcome.
/// Task status in `tasks` is updated to `Processing`, then to `Succeeded` or
/// `Failed` at each stage.
///
/// # Arguments
///
/// * `task_id` — Unique identifier used to update status in the shared task map.
/// * `tenant_id` — Tenant whose index is being exported.
/// * `dest_path` — Target directory for the recursive copy.
/// * `manager` — Shared `IndexManager` used to flush writers and locate source data.
/// * `tasks` — Shared map of task metadata, mutated with progress and outcome.
async fn process_export(
    _task_id: String,
    _tenant_id: TenantId,
    _dest_path: PathBuf,
    _manager: Arc<crate::IndexManager>,
    _tasks: Arc<DashMap<String, TaskInfo>>,
) {
    _tasks.alter(&_task_id, |_, mut t| {
        t.status = TaskStatus::Processing;
        t
    });

    _manager.write_queues.remove(&_tenant_id);

    if let Some(handle) = _manager
        .write_task_handles
        .get(&_tenant_id)
        .map(|entry| entry.value().clone())
    {
        if let Err(error) = handle.drain(_tenant_id.clone()).await {
            _tasks.alter(&_task_id, |_, mut t| {
                t.status = TaskStatus::Failed(format!("Commit failed: {}", error));
                t
            });
            return;
        }
        _manager
            .write_task_handles
            .remove_if(&_tenant_id, |_, current| current.same_handle(&handle));
    }

    let src = _manager.base_path.join(&_tenant_id);
    let dest = _dest_path.clone();

    let copy_result = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&dest)?;
        crate::index::utils::copy_dir_recursive(&src, &dest)
    })
    .await;

    _manager.writers.remove(&_tenant_id);
    _manager.loaded.remove(&_tenant_id);

    match copy_result {
        Ok(Ok(())) => {
            _tasks.alter(&_task_id, |_, mut t| {
                t.status = TaskStatus::Succeeded;
                t
            });
        }
        Ok(Err(e)) => {
            _manager.writers.remove(&_tenant_id);
            _manager.loaded.remove(&_tenant_id);
            _tasks.alter(&_task_id, |_, mut t| {
                t.status = TaskStatus::Failed(format!("Copy failed: {}", e));
                t
            });
        }
        Err(e) => {
            _manager.writers.remove(&_tenant_id);
            _manager.loaded.remove(&_tenant_id);
            _tasks.alter(&_task_id, |_, mut t| {
                t.status = TaskStatus::Failed(format!("Spawn blocking failed: {:?}", e));
                t
            });
        }
    }
}
