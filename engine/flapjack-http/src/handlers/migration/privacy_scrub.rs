//! Durable privacy-scrub admission, execution, replay, and exact-absence acknowledgement.

use super::{
    authenticated_owner_identity, import, spool, spool_error, AppState, MigrateError,
    MigrationDisposition, MigrationPhase, SpoolErrorKind,
};
use crate::auth::AuthenticatedAppId;
use crate::error_response::{json_error_parts, json_error_parts_with_code};
use crate::handlers::index_resource_store::{delete_resource_item, load_existing_store};
use axum::{
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use flapjack::index::manager::publication::{
    verify_current_generation_evidence, PublicationGenerationEvidence, PublicationTarget,
};
use flapjack::index::rules::RuleStore;
use flapjack::index::synonyms::SynonymStore;
use flapjack::validate_index_name;
use serde::{Deserialize, Serialize};
use spool::{PrivacyScrubAdmission, PrivacyScrubIntent, PrivacyScrubIntentFields};
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

const PRIVACY_SCRUB_UNKNOWN_TARGET_CODE: &str = "privacy_scrub_unknown_target";
const PRIVACY_SCRUB_STALE_GENERATION_CODE: &str = "privacy_scrub_stale_generation";
const PRIVACY_SCRUB_MISMATCHED_INTENT_CODE: &str = "privacy_scrub_mismatched_intent";
const PRIVACY_SCRUB_INTERRUPTED_RETRYABLE_CODE: &str = "privacy_scrub_interrupted_retryable";

#[derive(Debug, Deserialize, ToSchema)]
pub struct PrivacyScrubRequest {
    #[serde(rename = "scrubId")]
    pub scrub_id: String,
    pub tenant: String,
    #[serde(rename = "expectedGeneration")]
    pub expected_generation: String,
    #[serde(default, rename = "objectIDs")]
    pub object_ids: Vec<String>,
    #[serde(default, rename = "synonymIDs")]
    pub synonym_ids: Vec<String>,
    #[serde(default, rename = "ruleIDs")]
    pub rule_ids: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PrivacyScrubAck {
    #[serde(rename = "scrubId")]
    pub scrub_id: String,
    pub disposition: String,
}

#[utoipa::path(
    post,
    path = "/1/migrations/privacy-scrub",
    tag = "migration",
    request_body = PrivacyScrubRequest,
    responses(
        (status = 202, description = "Privacy scrub exact-absence ACK", body = PrivacyScrubAck),
        (status = 400, description = "Invalid privacy scrub request"),
        (status = 403, description = "Private migration credential required"),
        (status = 409, description = "Privacy scrub refused or retryable")
    ),
    security(("private_migration" = []))
)]
pub async fn submit_privacy_scrub(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    #[cfg(test)] hooks: Option<Extension<Arc<import::PrivacyScrubTestHooks>>>,
    Json(payload): Json<PrivacyScrubRequest>,
) -> Result<(StatusCode, Json<PrivacyScrubAck>), MigrateError> {
    #[cfg(test)]
    let hooks = hooks.map(|Extension(hooks)| hooks);
    submit_privacy_scrub_impl(
        state,
        authenticated_app_id,
        payload,
        #[cfg(test)]
        hooks,
    )
    .await
}

pub(crate) async fn submit_privacy_scrub_http(
    State(state): State<Arc<AppState>>,
    Extension(AuthenticatedAppId(authenticated_app_id)): Extension<AuthenticatedAppId>,
    headers: HeaderMap,
    #[cfg(test)] hooks: Option<Extension<Arc<import::PrivacyScrubTestHooks>>>,
    Json(payload): Json<PrivacyScrubRequest>,
) -> Result<(StatusCode, Json<PrivacyScrubAck>), MigrateError> {
    #[cfg(test)]
    let hooks = hooks.map(|Extension(hooks)| hooks);
    submit_privacy_scrub_impl(
        state,
        authenticated_owner_identity(authenticated_app_id, &headers),
        payload,
        #[cfg(test)]
        hooks,
    )
    .await
}

async fn submit_privacy_scrub_impl(
    state: Arc<AppState>,
    authenticated_app_id: String,
    payload: PrivacyScrubRequest,
    #[cfg(test)] hooks: Option<Arc<import::PrivacyScrubTestHooks>>,
) -> Result<(StatusCode, Json<PrivacyScrubAck>), MigrateError> {
    validate_privacy_scrub_request(&payload)?;
    verify_privacy_scrub_generation(&state, &payload)?;
    let spool = import::spool_for_manager(&state.manager)?;
    if privacy_scrub_terminal_replay(&spool, &authenticated_app_id, &payload)? {
        #[cfg(test)]
        wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::AckReplay).await;
        return Ok(privacy_scrub_ack(&payload.scrub_id));
    }
    #[cfg(test)]
    wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::PreIntent).await;
    let requested_intent = PrivacyScrubIntent::from_fields(PrivacyScrubIntentFields {
        scrub_id: payload.scrub_id.clone(),
        tenant: payload.tenant.clone(),
        expected_generation: payload.expected_generation.clone(),
        object_ids: payload.object_ids.clone(),
        synonym_ids: payload.synonym_ids.clone(),
        rule_ids: payload.rule_ids.clone(),
        authenticated_app_id,
        created_at: chrono::Utc::now(),
    });
    let admission = spool
        .admit_privacy_scrub_intent(Uuid::new_v4(), requested_intent)
        .map_err(privacy_scrub_spool_error)?;
    let (job_uuid, phase, intent, duplicate) = match admission {
        PrivacyScrubAdmission::Created {
            job_uuid,
            phase,
            intent,
        } => (job_uuid, phase, intent, false),
        PrivacyScrubAdmission::Duplicate {
            job_uuid,
            phase,
            intent,
        } => (job_uuid, phase, intent, true),
    };

    if duplicate
        && phase.disposition == MigrationDisposition::Succeeded
        && phase.terminal_at.is_some()
    {
        #[cfg(test)]
        wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::AckReplay).await;
        return Ok(privacy_scrub_ack(&intent.scrub_id));
    }
    if duplicate {
        if settle_privacy_scrub_success_if_exact_absence(&state, &spool, job_uuid, &intent)? {
            #[cfg(test)]
            wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::AckReplay).await;
            return Ok(privacy_scrub_ack(&intent.scrub_id));
        }
        return Err(privacy_scrub_conflict(
            PRIVACY_SCRUB_INTERRUPTED_RETRYABLE_CODE,
            "Privacy scrub is interrupted and retryable",
        ));
    }

    #[cfg(test)]
    wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::PostIntent).await;
    run_privacy_scrub(&state, &spool, job_uuid, &intent).await?;
    #[cfg(test)]
    wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::EngineCommit).await;
    settle_privacy_scrub_success(&spool, job_uuid)?;
    #[cfg(test)]
    wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::PreAck).await;
    #[cfg(test)]
    wait_privacy_scrub_boundary(&hooks, import::PrivacyScrubBoundary::ResponseLoss).await;
    Ok(privacy_scrub_ack(&intent.scrub_id))
}

fn validate_privacy_scrub_request(payload: &PrivacyScrubRequest) -> Result<(), MigrateError> {
    validate_component("scrubId", &payload.scrub_id)?;
    validate_index_name(&payload.tenant)
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
    PublicationGenerationEvidence::new(payload.expected_generation.clone())
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
    for (label, values) in [
        ("objectIDs", &payload.object_ids),
        ("synonymIDs", &payload.synonym_ids),
        ("ruleIDs", &payload.rule_ids),
    ] {
        for value in values {
            validate_component(label, value)?;
        }
    }
    Ok(())
}

fn validate_component(label: &str, value: &str) -> Result<(), MigrateError> {
    if value.trim().is_empty()
        || value != value.trim()
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(json_error_parts(
            StatusCode::BAD_REQUEST,
            format!("{label} must contain stable non-empty IDs"),
        ));
    }
    Ok(())
}
fn verify_privacy_scrub_generation(
    state: &AppState,
    payload: &PrivacyScrubRequest,
) -> Result<(), MigrateError> {
    if !state.manager.base_path.join(&payload.tenant).exists() {
        return Err(privacy_scrub_conflict(
            PRIVACY_SCRUB_UNKNOWN_TARGET_CODE,
            "Privacy scrub target is unknown",
        ));
    }
    let target = PublicationTarget::new(payload.tenant.clone())
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
    let expected = PublicationGenerationEvidence::new(payload.expected_generation.clone())
        .map_err(|error| json_error_parts(StatusCode::BAD_REQUEST, error.to_string()))?;
    verify_current_generation_evidence(&state.manager.base_path, &target, &expected).map_err(|_| {
        privacy_scrub_conflict(
            PRIVACY_SCRUB_STALE_GENERATION_CODE,
            "Privacy scrub generation evidence is stale or unavailable",
        )
    })
}

fn privacy_scrub_terminal_replay(
    spool: &spool::SpoolStore,
    authenticated_app_id: &str,
    payload: &PrivacyScrubRequest,
) -> Result<bool, MigrateError> {
    for job_uuid in spool.job_uuids().map_err(privacy_scrub_spool_error)? {
        let Some(intent) = spool
            .read_privacy_scrub_intent_if_exists(job_uuid)
            .map_err(privacy_scrub_spool_error)?
        else {
            continue;
        };
        if !privacy_scrub_payload_matches_intent(&intent, authenticated_app_id, payload) {
            continue;
        }
        let phase = spool
            .read_migration_phase(job_uuid)
            .map_err(privacy_scrub_spool_error)?;
        return Ok(
            phase.disposition == MigrationDisposition::Succeeded && phase.terminal_at.is_some()
        );
    }
    Ok(false)
}

fn privacy_scrub_payload_matches_intent(
    intent: &PrivacyScrubIntent,
    authenticated_app_id: &str,
    payload: &PrivacyScrubRequest,
) -> bool {
    intent.authenticated_app_id == authenticated_app_id
        && intent.scrub_id == payload.scrub_id
        && intent.tenant == payload.tenant
        && intent.expected_generation == payload.expected_generation
        && intent.object_ids == payload.object_ids
        && intent.synonym_ids == payload.synonym_ids
        && intent.rule_ids == payload.rule_ids
}

fn settle_privacy_scrub_success_if_exact_absence(
    state: &Arc<AppState>,
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
    intent: &PrivacyScrubIntent,
) -> Result<bool, MigrateError> {
    if assert_privacy_scrub_absence(state, intent).is_err() {
        return Ok(false);
    }
    settle_privacy_scrub_success(spool, job_uuid)?;
    Ok(true)
}

fn settle_privacy_scrub_success(
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
) -> Result<(), MigrateError> {
    let phase = spool
        .read_migration_phase(job_uuid)
        .map_err(privacy_scrub_spool_error)?;
    if phase.disposition == MigrationDisposition::Succeeded && phase.terminal_at.is_some() {
        return Ok(());
    }
    if phase.disposition != MigrationDisposition::Running || phase.terminal_at.is_some() {
        return Err(privacy_scrub_conflict(
            PRIVACY_SCRUB_INTERRUPTED_RETRYABLE_CODE,
            "Privacy scrub is interrupted and retryable",
        ));
    }
    for next_phase in privacy_scrub_remaining_phases(phase.phase) {
        spool
            .transition_migration_phase(job_uuid, *next_phase)
            .map_err(privacy_scrub_spool_error)?;
    }
    spool
        .succeed_migration(job_uuid, None)
        .map_err(privacy_scrub_spool_error)?;
    Ok(())
}

fn privacy_scrub_remaining_phases(phase: MigrationPhase) -> &'static [MigrationPhase] {
    match phase {
        MigrationPhase::Submitted => &[
            MigrationPhase::Exporting,
            MigrationPhase::Preparing,
            MigrationPhase::Staging,
            MigrationPhase::Activating,
        ],
        MigrationPhase::Exporting => &[
            MigrationPhase::Preparing,
            MigrationPhase::Staging,
            MigrationPhase::Activating,
        ],
        MigrationPhase::Preparing => &[MigrationPhase::Staging, MigrationPhase::Activating],
        MigrationPhase::Staging => &[MigrationPhase::Activating],
        MigrationPhase::Activating => &[],
    }
}

async fn run_privacy_scrub(
    state: &Arc<AppState>,
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
    intent: &PrivacyScrubIntent,
) -> Result<(), MigrateError> {
    if !intent.object_ids.is_empty() {
        state
            .manager
            .delete_documents_durable(&intent.tenant, intent.object_ids.clone())
            .await
            .map_err(|error| {
                json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
            })?;
    }
    for synonym_id in &intent.synonym_ids {
        delete_resource_item::<SynonymStore>(state.manager.as_ref(), &intent.tenant, synonym_id)
            .map_err(|error| {
                json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
            })?;
        state.manager.append_oplog(
            &intent.tenant,
            "delete_synonym",
            serde_json::json!({"objectID": synonym_id}),
        );
    }
    for rule_id in &intent.rule_ids {
        delete_resource_item::<RuleStore>(state.manager.as_ref(), &intent.tenant, rule_id)
            .map_err(|error| {
                json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
            })?;
        state.manager.append_oplog(
            &intent.tenant,
            "delete_rule",
            serde_json::json!({"objectID": rule_id}),
        );
    }
    assert_privacy_scrub_absence(state, intent).map_err(|error| {
        let _ = spool.fail_migration(job_uuid);
        json_error_parts(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    })
}

fn assert_privacy_scrub_absence(
    state: &Arc<AppState>,
    intent: &PrivacyScrubIntent,
) -> Result<(), flapjack::error::FlapjackError> {
    for object_id in &intent.object_ids {
        if state
            .manager
            .get_document(&intent.tenant, object_id)?
            .is_some()
        {
            return Err(flapjack::error::FlapjackError::InvalidQuery(format!(
                "privacy scrub object {object_id} is still present"
            )));
        }
    }
    let synonyms = load_existing_store::<SynonymStore>(state.manager.as_ref(), &intent.tenant)?;
    for synonym_id in &intent.synonym_ids {
        if synonyms
            .as_ref()
            .and_then(|store| store.get(synonym_id))
            .is_some()
        {
            return Err(flapjack::error::FlapjackError::InvalidQuery(format!(
                "privacy scrub synonym {synonym_id} is still present"
            )));
        }
    }
    let rules = load_existing_store::<RuleStore>(state.manager.as_ref(), &intent.tenant)?;
    for rule_id in &intent.rule_ids {
        if rules
            .as_ref()
            .and_then(|store| store.get(rule_id))
            .is_some()
        {
            return Err(flapjack::error::FlapjackError::InvalidQuery(format!(
                "privacy scrub rule {rule_id} is still present"
            )));
        }
    }
    Ok(())
}

fn privacy_scrub_ack(scrub_id: &str) -> (StatusCode, Json<PrivacyScrubAck>) {
    (
        StatusCode::ACCEPTED,
        Json(PrivacyScrubAck {
            scrub_id: scrub_id.to_string(),
            disposition: "acknowledged".to_string(),
        }),
    )
}

fn privacy_scrub_conflict(code: &'static str, message: &'static str) -> MigrateError {
    json_error_parts_with_code(StatusCode::CONFLICT, code, message)
}

fn privacy_scrub_spool_error(error: spool::SpoolError) -> MigrateError {
    if error.kind() == SpoolErrorKind::PrivacyScrubIntentCollision {
        return privacy_scrub_conflict(
            PRIVACY_SCRUB_MISMATCHED_INTENT_CODE,
            "Privacy scrub intent identity does not match",
        );
    }
    spool_error(error)
}

#[cfg(test)]
async fn wait_privacy_scrub_boundary(
    hooks: &Option<Arc<import::PrivacyScrubTestHooks>>,
    boundary: import::PrivacyScrubBoundary,
) {
    if let Some(hooks) = hooks {
        hooks.wait_at(boundary).await;
    }
}
