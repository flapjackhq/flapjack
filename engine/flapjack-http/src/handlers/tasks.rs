//! Axum handlers for querying indexing-task status with an Algolia-compatible response shape.
use axum::{
    extract::{Path, State},
    Json,
};
use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;

use super::AppState;
use flapjack::error::FlapjackError;
use flapjack::index::write_queue::{delete_term_observation, DeleteTermObservation};
use flapjack::types::{TaskInfo, TaskStatus};

#[derive(Debug, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AlgoliaTaskResponse {
    pub status: String,
    pub pending_task: bool,
}

/// Get task status by ID
#[utoipa::path(
    get,
    path = "/1/task/{task_id}",
    tag = "tasks",
    params(
        ("task_id" = String, Path, description = "Task ID")
    ),
    responses(
        (status = 200, description = "Task status and results", body = AlgoliaTaskResponse),
        (status = 404, description = "Task not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_task(
    State(state): State<Arc<AppState>>,
    Path(task_id): Path<String>,
) -> Result<Json<AlgoliaTaskResponse>, FlapjackError> {
    let task = state.manager.get_task(&task_id)?;
    trace_delete_term_observation(&task);
    Ok(Json(map_task_status_to_algolia(&task.status)))
}

/// Get task status for a specific index
#[utoipa::path(
    get,
    path = "/1/indexes/{indexName}/task/{task_id}",
    tag = "tasks",
    params(
        ("indexName" = String, Path, description = "Index name"),
        ("task_id" = String, Path, description = "Task ID")
    ),
    responses(
        (status = 200, description = "Task status and results", body = AlgoliaTaskResponse),
        (status = 404, description = "Task not found")
    ),
    security(
        ("api_key" = [])
    )
)]
pub async fn get_task_for_index(
    State(state): State<Arc<AppState>>,
    Path((index_name, task_id)): Path<(String, String)>,
) -> Result<Json<AlgoliaTaskResponse>, FlapjackError> {
    let full_task_id = if task_id.starts_with("task_") {
        task_id.clone()
    } else {
        format!("task_{}_{}", index_name, task_id)
    };

    let task = state
        .manager
        .get_task(&full_task_id)
        .or_else(|_| state.manager.get_task(&task_id))?;

    // Validate the task belongs to this index.
    // Task IDs have the format "task_{index_name}_{uuid}", so check that
    // the expected prefix matches. We can't naively split on '_' because
    // index names themselves may contain underscores.
    let expected_prefix = format!("task_{}_", index_name);
    if !task.id.starts_with(&expected_prefix) {
        return Err(FlapjackError::TaskNotFound(task_id));
    }

    trace_delete_term_observation(&task);
    Ok(Json(map_task_status_to_algolia(&task.status)))
}

fn trace_delete_term_observation(task: &TaskInfo) -> DeleteTermObservation {
    let observation = delete_term_observation(task);
    tracing::debug!(
        task_id = task.id,
        explicit_delete_actions = observation.explicit_delete_actions,
        document_write_delete_terms = observation.document_write_delete_terms,
        "task write-preparation observation"
    );
    observation
}

fn map_task_status_to_algolia(task_status: &TaskStatus) -> AlgoliaTaskResponse {
    match task_status {
        TaskStatus::Enqueued | TaskStatus::Processing => AlgoliaTaskResponse {
            status: "notPublished".to_string(),
            pending_task: true,
        },
        // Failed tasks are terminal and should not keep waitTask() polling forever.
        TaskStatus::Succeeded | TaskStatus::Failed(_) => AlgoliaTaskResponse {
            status: "published".to_string(),
            pending_task: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_enqueued_and_processing_to_pending_not_published() {
        let enqueued = map_task_status_to_algolia(&TaskStatus::Enqueued);
        assert_eq!(enqueued.status, "notPublished");
        assert!(enqueued.pending_task);

        let processing = map_task_status_to_algolia(&TaskStatus::Processing);
        assert_eq!(processing.status, "notPublished");
        assert!(processing.pending_task);
    }

    #[test]
    fn maps_terminal_states_to_published_and_not_pending() {
        let succeeded = map_task_status_to_algolia(&TaskStatus::Succeeded);
        assert_eq!(succeeded.status, "published");
        assert!(!succeeded.pending_task);

        let failed = map_task_status_to_algolia(&TaskStatus::Failed("boom".to_string()));
        assert_eq!(failed.status, "published");
        assert!(!failed.pending_task);
    }

    #[test]
    fn task_observation_consumer_accepts_public_typed_seam() {
        let task = TaskInfo::new("task_observation".to_string(), 1, 0);
        assert_eq!(
            trace_delete_term_observation(&task),
            DeleteTermObservation {
                explicit_delete_actions: 0,
                document_write_delete_terms: 0,
            }
        );
    }
}
