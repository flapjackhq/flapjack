use crate::dto::SearchRequest;
use crate::extractors::ValidatedIndexName;
use crate::handlers::internal_ops::{
    apply_clear_index_op, apply_clear_rules_op, apply_clear_synonyms_op, apply_copy_index_op,
    apply_delete_op, apply_delete_rule_op, apply_delete_synonym_op, apply_move_index_op,
    apply_save_rule_op, apply_save_rules_op, apply_save_synonym_op, apply_save_synonyms_op,
    apply_settings_op, apply_upsert_op, flush_document_batch, preflight_document_op,
    preflight_index_op, preflight_resource_op, preflight_settings_op, ReplicatedDocumentBatch,
};
use crate::handlers::AppState;
use crate::security_audit::{self, Action, Actor, Outcome, Target};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use flapjack::index::manager::publication::{ContentDigest, PublicationTransactionId};
use flapjack::index::oplog::{OpLog, OpLogEntry};
use flapjack::{validate_index_name, IndexManager};
use flapjack_replication::config::{NodeConfig, PeerConfig};
use flapjack_replication::manager::{
    AddPeerError, AutohealLifecycleProjection, AutohealPeerLifecycle,
};
use flapjack_replication::types::{
    GetOpsQuery, GetOpsResponse, ListTenantsResponse, ReplicateOpsRequest, ReplicateOpsResponse,
    RELEASE_TRANSFER_AFTER_SEQ_HEADER, RELEASE_TRANSFER_CONTRACT_HEADER,
    RELEASE_TRANSFER_CONTRACT_V1, RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER,
    RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER, RELEASE_TRANSFER_STATUS_ACKNOWLEDGED,
    RELEASE_TRANSFER_STATUS_CONTIGUOUS, RELEASE_TRANSFER_STATUS_HEADER,
    RELEASE_TRANSFER_STATUS_RESNAPSHOT_REQUIRED, RELEASE_TRANSFER_TENANT_HEADER,
    RELEASE_TRANSFER_THROUGH_SEQ_HEADER, RELEASE_TRANSFER_TRANSACTION_HEADER,
};
use flapjack_ssl::manager::RenewalStatus;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;
use utoipa::ToSchema;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseWriteFenceRequest {
    transaction_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseWriteFenceResponse {
    active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    transaction_id: Option<String>,
}

fn mutation_fence_error(error: crate::pause_registry::MutationFenceError) -> Response {
    use crate::pause_registry::MutationFenceError;
    match error {
        MutationFenceError::InvalidTransaction => crate::error_response::json_error(
            StatusCode::BAD_REQUEST,
            "Invalid release transaction identifier",
        ),
        MutationFenceError::Conflict => crate::error_response::json_error(
            StatusCode::CONFLICT,
            "Release mutation fence transaction conflict",
        ),
        MutationFenceError::Storage(error) => {
            tracing::error!("release mutation fence storage failed: {error}");
            crate::error_response::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error",
            )
        }
    }
}

pub async fn release_write_fence_status(
    State(state): State<Arc<AppState>>,
) -> Json<ReleaseWriteFenceResponse> {
    let status = state.global_mutation_fence.status().await;
    Json(ReleaseWriteFenceResponse {
        active: status.is_some(),
        transaction_id: status.map(|status| status.transaction_id),
    })
}

pub async fn acquire_release_write_fence(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ReleaseWriteFenceRequest>,
) -> Response {
    match state
        .global_mutation_fence
        .acquire(&request.transaction_id)
        .await
    {
        Ok(status) => Json(ReleaseWriteFenceResponse {
            active: true,
            transaction_id: Some(status.transaction_id),
        })
        .into_response(),
        Err(error) => mutation_fence_error(error),
    }
}

pub async fn release_release_write_fence(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ReleaseWriteFenceRequest>,
) -> Response {
    match state
        .global_mutation_fence
        .release(&request.transaction_id)
        .await
    {
        Ok(()) => Json(ReleaseWriteFenceResponse {
            active: false,
            transaction_id: Some(request.transaction_id),
        })
        .into_response(),
        Err(error) => mutation_fence_error(error),
    }
}

/// Canonical document inventory used by both the protected runtime probe and
/// the post-drain shutdown receipt. IndexManager is tenant-per-index, so one
/// stable identifier names both ownership scopes without duplicating fields.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReleaseInventoryEntry {
    pub index_id: String,
    pub document_count: u64,
}

/// Load every durable tenant after the listener closes and before the final
/// queue drain. This prevents a cold, currently-unloaded tenant from silently
/// disappearing from the post-drain release inventory.
pub(crate) fn prepare_release_inventory(manager: &IndexManager) -> Result<(), String> {
    let index_ids =
        crate::tenant_dirs::visible_tenant_dir_names(&manager.base_path).map_err(|error| {
            format!("release inventory could not enumerate durable tenants: {error}")
        })?;
    for index_id in index_ids {
        manager
            .get_or_load(&index_id)
            .map_err(|error| format!("release inventory could not load {index_id}: {error}"))?;
    }
    Ok(())
}

pub(crate) fn canonical_release_inventory(
    manager: &IndexManager,
) -> Result<Vec<ReleaseInventoryEntry>, String> {
    let mut durable_index_ids = crate::tenant_dirs::visible_tenant_dir_names(&manager.base_path)
        .map_err(|error| {
            format!("release inventory could not enumerate durable tenants: {error}")
        })?;
    durable_index_ids.sort();
    let mut index_ids = manager.loaded_tenant_ids();
    index_ids.sort();
    if index_ids != durable_index_ids {
        return Err(format!(
            "release inventory loaded/durable tenant mismatch: loaded={index_ids:?} durable={durable_index_ids:?}"
        ));
    }
    index_ids
        .into_iter()
        .map(|index_id| {
            let document_count = manager.tenant_doc_count(&index_id).ok_or_else(|| {
                format!("loaded index {index_id} has no authoritative document count")
            })?;
            Ok(ReleaseInventoryEntry {
                index_id,
                document_count,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalCountOnlySearchRequest {
    #[serde(default)]
    pub query: String,
    pub hits_per_page: Option<usize>,
}

fn validate_count_only_hits_per_page(
    hits_per_page: Option<usize>,
) -> Result<(), (StatusCode, String)> {
    if hits_per_page == Some(0) {
        return Ok(());
    }
    Err((
        StatusCode::BAD_REQUEST,
        "internal count-only search requires hitsPerPage: 0".to_string(),
    ))
}

/// Core apply logic: parse ops and write to IndexManager.
/// Returns the highest sequence number applied, or an error string.
///
/// Conflict admission compares `(timestamp_ms, node_id, source_seq)` against
/// durable origin proof. Equal proven tuples succeed only for the same logical
/// effect digest; legacy or contradictory equal evidence fails closed.
pub async fn apply_ops_to_manager(
    manager: &IndexManager,
    tenant_id: &str,
    ops: &[OpLogEntry],
) -> Result<u64, String> {
    apply_ops(manager, None, tenant_id, ops).await
}

pub(crate) async fn apply_ops_to_state(
    state: &AppState,
    tenant_id: &str,
    ops: &[OpLogEntry],
) -> Result<u64, String> {
    apply_ops(&state.manager, Some(state), tenant_id, ops).await
}

async fn apply_ops(
    manager: &IndexManager,
    state: Option<&AppState>,
    tenant_id: &str,
    ops: &[OpLogEntry],
) -> Result<u64, String> {
    validate_index_name(tenant_id).map_err(|e| e.to_string())?;
    #[cfg(test)]
    if let Some(first) = ops.first() {
        crate::handlers::internal_ops::run_after_document_proof_accepted_hook_for_test(first.seq);
    }
    let _replication_guard = manager.lock_replication_apply(tenant_id).await;
    preflight_replication_batch(tenant_id, ops)?;
    if state.is_none() && ops.iter().any(|entry| entry.op_type == "settings") {
        return Err(format!(
            "[REPL {}] settings replication requires application state",
            tenant_id
        ));
    }

    bootstrap_document_version_state(manager, tenant_id, ops)?;

    let mut max_seq = 0u64;
    let mut document_batch = ReplicatedDocumentBatch::default();

    for op_entry in ops {
        if !matches!(op_entry.op_type.as_str(), "upsert" | "delete") {
            flush_document_batch(manager, tenant_id, std::mem::take(&mut document_batch)).await?;
        }
        let incoming = (op_entry.timestamp_ms, op_entry.node_id.clone());
        apply_replication_op(
            manager,
            state,
            tenant_id,
            op_entry,
            incoming,
            &mut document_batch,
        )
        .await?;
        max_seq = max_seq.max(op_entry.seq);
    }

    flush_document_batch(manager, tenant_id, document_batch).await?;
    Ok(max_seq)
}

fn preflight_replication_batch(tenant_id: &str, ops: &[OpLogEntry]) -> Result<(), String> {
    for op_entry in ops {
        if op_entry.tenant_id != tenant_id {
            return Err(format!(
                "[REPL {}] inner tenant {} does not match outer tenant {} at seq {}",
                tenant_id, op_entry.tenant_id, tenant_id, op_entry.seq
            ));
        }
    }

    for op_entry in ops {
        preflight_replication_op(tenant_id, op_entry)?;
    }
    Ok(())
}

fn validate_single_sender_sequence(tenant_id: &str, ops: &[OpLogEntry]) -> Result<(), String> {
    for entries in ops.windows(2) {
        let previous = &entries[0];
        let current = &entries[1];
        let expected = previous.seq.checked_add(1);
        if expected != Some(current.seq) {
            return Err(format!(
                "[REPL {}] non-adjacent replication sequence: {} followed by {}",
                tenant_id, previous.seq, current.seq
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReleaseTailStatus {
    Contiguous,
    ResnapshotRequired,
}

impl ReleaseTailStatus {
    fn as_header(self) -> &'static str {
        match self {
            Self::Contiguous => RELEASE_TRANSFER_STATUS_CONTIGUOUS,
            Self::ResnapshotRequired => RELEASE_TRANSFER_STATUS_RESNAPSHOT_REQUIRED,
        }
    }
}

#[derive(Debug)]
struct ReleaseTailProjection {
    status: ReleaseTailStatus,
    through_seq: u64,
    ops: Vec<OpLogEntry>,
}

/// Turn the oplog's authoritative sequence coordinates into a release-only
/// contiguous tail. A retention gap never returns a usable suffix: callers
/// must take another one-UID snapshot and replace their receipt watermark.
fn release_tail_projection(
    requested_after_seq: u64,
    current_seq: u64,
    oldest_retained_seq: Option<u64>,
    ops: Vec<OpLogEntry>,
) -> Result<ReleaseTailProjection, String> {
    if ops.iter().any(|entry| entry.seq == 0) {
        return Err("release tail contains invalid sequence 0".to_string());
    }

    let next_required = requested_after_seq.checked_add(1);
    if current_seq < requested_after_seq
        || oldest_retained_seq.is_some_and(|oldest| next_required.is_none_or(|next| oldest > next))
    {
        return Ok(ReleaseTailProjection {
            status: ReleaseTailStatus::ResnapshotRequired,
            through_seq: current_seq,
            ops: Vec::new(),
        });
    }

    if current_seq == requested_after_seq {
        if ops.is_empty() {
            return Ok(ReleaseTailProjection {
                status: ReleaseTailStatus::Contiguous,
                through_seq: current_seq,
                ops,
            });
        }
        return Err(
            "release tail returned operations beyond the authoritative current sequence"
                .to_string(),
        );
    }

    if ops.is_empty() {
        return Err(format!(
            "release tail omitted operations for authoritative interval {}..={current_seq}",
            next_required.unwrap_or(u64::MAX)
        ));
    }
    for (offset, entry) in ops.iter().enumerate() {
        let expected = next_required
            .and_then(|first| first.checked_add(offset as u64))
            .ok_or_else(|| "release tail sequence interval overflowed".to_string())?;
        if entry.seq != expected {
            return Err(format!(
                "release tail is noncontiguous: expected sequence {expected}, observed {}",
                entry.seq
            ));
        }
    }
    if ops.last().map(|entry| entry.seq) != Some(current_seq) {
        return Err(format!(
            "release tail omitted operations before authoritative sequence {current_seq}"
        ));
    }

    Ok(ReleaseTailProjection {
        status: ReleaseTailStatus::Contiguous,
        through_seq: current_seq,
        ops,
    })
}

#[derive(Clone, Copy)]
enum ReleaseRequestMode {
    Snapshot,
    Tail(u64),
    Apply,
}

#[derive(Debug)]
struct ReleaseRequest {
    transaction_id: String,
    payload_sha256: Option<String>,
    window: Option<(u64, u64)>,
}

fn exact_release_request_header<'a>(
    headers: &'a HeaderMap,
    name: &'static str,
) -> Result<Option<&'a str>, String> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(format!(
            "release transfer duplicated protected header {name}"
        ));
    }
    value
        .to_str()
        .map(Some)
        .map_err(|_| format!("release transfer {name} is not visible ASCII"))
}

fn strict_release_sequence_header(headers: &HeaderMap, name: &'static str) -> Result<u64, String> {
    let value = exact_release_request_header(headers, name)?
        .ok_or_else(|| format!("release transfer missing {name}"))?;
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("release transfer {name} is not an unsigned integer"))?;
    if parsed.to_string() != value {
        return Err(format!("release transfer {name} is not canonical"));
    }
    Ok(parsed)
}

fn strict_release_request(
    headers: &HeaderMap,
    expected_tenant_id: &str,
    mode: ReleaseRequestMode,
) -> Result<Option<ReleaseRequest>, String> {
    let contract = exact_release_request_header(headers, RELEASE_TRANSFER_CONTRACT_HEADER)?;
    let protected_without_contract = [
        RELEASE_TRANSFER_TENANT_HEADER,
        RELEASE_TRANSFER_TRANSACTION_HEADER,
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        RELEASE_TRANSFER_THROUGH_SEQ_HEADER,
        RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER,
        RELEASE_TRANSFER_STATUS_HEADER,
        RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER,
    ]
    .iter()
    .any(|name| headers.contains_key(*name));
    let Some(contract) = contract else {
        return if protected_without_contract {
            Err("release transfer protected headers require the exact contract header".to_string())
        } else {
            Ok(None)
        };
    };
    if contract != RELEASE_TRANSFER_CONTRACT_V1 {
        return Err("unknown release transfer contract".to_string());
    }

    let tenant_id = exact_release_request_header(headers, RELEASE_TRANSFER_TENANT_HEADER)?
        .ok_or_else(|| "release transfer missing exact tenant header".to_string())?;
    if tenant_id != expected_tenant_id {
        return Err("release transfer tenant does not match the requested tenant".to_string());
    }
    let transaction_id =
        exact_release_request_header(headers, RELEASE_TRANSFER_TRANSACTION_HEADER)?
            .ok_or_else(|| "release transfer missing exact transaction header".to_string())?;
    PublicationTransactionId::new(transaction_id).map_err(|_| {
        "release transfer transaction header is not a canonical identifier".to_string()
    })?;
    for response_only in [
        RELEASE_TRANSFER_STATUS_HEADER,
        RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER,
    ] {
        if exact_release_request_header(headers, response_only)?.is_some() {
            return Err(format!(
                "release transfer request supplied response-only header {response_only}"
            ));
        }
    }

    let (window, payload_sha256) = match mode {
        ReleaseRequestMode::Snapshot => {
            if exact_release_request_header(headers, RELEASE_TRANSFER_AFTER_SEQ_HEADER)?.is_some()
                || exact_release_request_header(headers, RELEASE_TRANSFER_THROUGH_SEQ_HEADER)?
                    .is_some()
                || exact_release_request_header(headers, RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER)?
                    .is_some()
            {
                return Err("release snapshot request supplied tail/apply coordinates".to_string());
            }
            (None, None)
        }
        ReleaseRequestMode::Tail(expected_after_seq) => {
            let after = strict_release_sequence_header(headers, RELEASE_TRANSFER_AFTER_SEQ_HEADER)?;
            if after != expected_after_seq {
                return Err(
                    "release tail header does not match the requested source interval".to_string(),
                );
            }
            if exact_release_request_header(headers, RELEASE_TRANSFER_THROUGH_SEQ_HEADER)?.is_some()
                || exact_release_request_header(headers, RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER)?
                    .is_some()
            {
                return Err("release tail request supplied response/apply coordinates".to_string());
            }
            (Some((after, after)), None)
        }
        ReleaseRequestMode::Apply => {
            let after = strict_release_sequence_header(headers, RELEASE_TRANSFER_AFTER_SEQ_HEADER)?;
            let through =
                strict_release_sequence_header(headers, RELEASE_TRANSFER_THROUGH_SEQ_HEADER)?;
            if through < after {
                return Err("release transfer through sequence precedes after sequence".to_string());
            }
            let payload_sha256 =
                exact_release_request_header(headers, RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER)?
                    .ok_or_else(|| {
                        "release apply request is missing the operations digest".to_string()
                    })?;
            ContentDigest::new(format!("sha256:{payload_sha256}"))
                .map_err(|_| "release apply payload digest is not canonical SHA-256".to_string())?;
            if !payload_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            {
                return Err(
                    "release apply payload digest is not canonical lowercase SHA-256".to_string(),
                );
            }
            (Some((after, through)), Some(payload_sha256.to_string()))
        }
    };
    Ok(Some(ReleaseRequest {
        transaction_id: transaction_id.to_string(),
        payload_sha256,
        window,
    }))
}

fn validate_release_apply_window(
    tenant_id: &str,
    after_seq: u64,
    through_seq: u64,
    ops: &[OpLogEntry],
) -> Result<(), String> {
    if through_seq == after_seq && ops.is_empty() {
        return Err(format!(
            "[REPL {tenant_id}] empty release tail cannot prove the destination watermark"
        ));
    }
    if ops.iter().any(|entry| entry.seq == 0) {
        return Err(format!(
            "[REPL {tenant_id}] release tail contains invalid sequence 0"
        ));
    }
    let expected_count = through_seq
        .checked_sub(after_seq)
        .ok_or_else(|| format!("[REPL {tenant_id}] invalid release tail interval"))?;
    if usize::try_from(expected_count).ok() != Some(ops.len()) {
        return Err(format!(
            "[REPL {tenant_id}] release tail count does not match exact interval"
        ));
    }
    for (offset, entry) in ops.iter().enumerate() {
        let expected = after_seq
            .checked_add(1)
            .and_then(|first| first.checked_add(offset as u64))
            .ok_or_else(|| format!("[REPL {tenant_id}] release tail interval overflowed"))?;
        if entry.seq != expected {
            return Err(format!(
                "[REPL {tenant_id}] release tail is noncontiguous: expected {expected}, observed {}",
                entry.seq
            ));
        }
    }
    Ok(())
}

fn release_transfer_response_headers(
    request: &ReleaseRequest,
    tenant_id: &str,
    after_seq: u64,
    through_seq: u64,
    status: &'static str,
    payload_sha256: &str,
) -> Result<HeaderMap, crate::error_response::HandlerError> {
    let mut headers = HeaderMap::new();
    for (name, value) in [
        (
            RELEASE_TRANSFER_CONTRACT_HEADER,
            RELEASE_TRANSFER_CONTRACT_V1.to_string(),
        ),
        (RELEASE_TRANSFER_TENANT_HEADER, tenant_id.to_string()),
        (
            RELEASE_TRANSFER_TRANSACTION_HEADER,
            request.transaction_id.clone(),
        ),
        (RELEASE_TRANSFER_AFTER_SEQ_HEADER, after_seq.to_string()),
        (RELEASE_TRANSFER_THROUGH_SEQ_HEADER, through_seq.to_string()),
        (RELEASE_TRANSFER_STATUS_HEADER, status.to_string()),
        (
            RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER,
            payload_sha256.to_string(),
        ),
    ] {
        headers.insert(
            name,
            HeaderValue::from_str(&value).map_err(|_| {
                crate::error_response::HandlerError::internal(
                    "release transfer response header was invalid",
                )
            })?,
        );
    }
    Ok(headers)
}

const RELEASE_APPLY_RECEIPT_KIND: &str = "flapjack_release_apply_interval";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReleaseApplyPhase {
    Prepared,
    Committed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseApplyReceipt {
    schema_version: u8,
    kind: String,
    tenant_id: String,
    after_seq: u64,
    through_seq: u64,
    transaction_id: String,
    payload_sha256: String,
    phase: ReleaseApplyPhase,
    acked_seq: Option<u64>,
}

#[derive(Debug)]
enum ReleaseApplyReceiptError {
    Conflict(String),
    Storage(String),
}

impl std::fmt::Display for ReleaseApplyReceiptError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict(message) | Self::Storage(message) => formatter.write_str(message),
        }
    }
}

impl ReleaseApplyReceiptError {
    #[cfg(test)]
    fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }
}

#[derive(Debug)]
struct ReleaseApplyGuard {
    _lock: File,
    receipt_path: PathBuf,
    receipt: ReleaseApplyReceipt,
}

impl ReleaseApplyGuard {
    fn commit(mut self, acked_seq: u64) -> Result<(), ReleaseApplyReceiptError> {
        if acked_seq != self.receipt.through_seq {
            return Err(ReleaseApplyReceiptError::Storage(
                "release apply acknowledgement did not match the prepared interval".to_string(),
            ));
        }
        self.receipt.phase = ReleaseApplyPhase::Committed;
        self.receipt.acked_seq = Some(acked_seq);
        persist_release_apply_receipt(&self.receipt_path, &self.receipt)
    }
}

#[derive(Debug)]
enum ReleaseApplyDisposition {
    Apply(ReleaseApplyGuard),
    Replay(u64),
}

fn release_apply_receipt_root(data_root: &StdPath) -> Result<PathBuf, ReleaseApplyReceiptError> {
    let canonical_data_root = data_root.canonicalize().map_err(|error| {
        ReleaseApplyReceiptError::Storage(format!(
            "release apply data root could not be canonicalized: {error}"
        ))
    })?;
    let parent = canonical_data_root.parent().ok_or_else(|| {
        ReleaseApplyReceiptError::Storage(
            "release apply data root has no receipt-state parent".to_string(),
        )
    })?;
    let data_name = canonical_data_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ReleaseApplyReceiptError::Storage(
                "release apply data root name is not valid UTF-8".to_string(),
            )
        })?;
    let root = parent.join(format!(".{data_name}.release-apply"));
    match std::fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(ReleaseApplyReceiptError::Storage(
                "release apply receipt root must be a real directory".to_string(),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&root).map_err(|error| {
                ReleaseApplyReceiptError::Storage(format!(
                    "release apply receipt root could not be created: {error}"
                ))
            })?;
        }
        Err(error) => {
            return Err(ReleaseApplyReceiptError::Storage(format!(
                "release apply receipt root could not be inspected: {error}"
            )))
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                ReleaseApplyReceiptError::Storage(format!(
                    "release apply receipt root could not be made private: {error}"
                ))
            },
        )?;
    }
    Ok(root)
}

fn release_apply_receipt_path(
    data_root: &StdPath,
    tenant_id: &str,
    after_seq: u64,
    through_seq: u64,
) -> Result<PathBuf, ReleaseApplyReceiptError> {
    let root = release_apply_receipt_root(data_root)?;
    let coordinate = format!("{tenant_id}\n{after_seq}\n{through_seq}\n");
    Ok(root.join(format!("{:x}.json", Sha256::digest(coordinate.as_bytes()))))
}

fn open_release_apply_lock(path: &StdPath) -> Result<File, ReleaseApplyReceiptError> {
    let lock_path = path.with_extension("lock");
    if let Ok(metadata) = std::fs::symlink_metadata(&lock_path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ReleaseApplyReceiptError::Storage(
                "release apply interval lock must be a regular non-symlink file".to_string(),
            ));
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(&lock_path).map_err(|error| {
        ReleaseApplyReceiptError::Storage(format!(
            "release apply interval lock could not be opened: {error}"
        ))
    })?;
    lock.lock_exclusive().map_err(|error| {
        ReleaseApplyReceiptError::Storage(format!(
            "release apply interval lock could not be acquired: {error}"
        ))
    })?;
    Ok(lock)
}

fn validate_release_apply_receipt(
    receipt: &ReleaseApplyReceipt,
) -> Result<(), ReleaseApplyReceiptError> {
    if receipt.schema_version != 1 || receipt.kind != RELEASE_APPLY_RECEIPT_KIND {
        return Err(ReleaseApplyReceiptError::Storage(
            "release apply receipt schema or kind is invalid".to_string(),
        ));
    }
    PublicationTransactionId::new(&receipt.transaction_id).map_err(|_| {
        ReleaseApplyReceiptError::Storage(
            "release apply receipt transaction is invalid".to_string(),
        )
    })?;
    ContentDigest::new(format!("sha256:{}", receipt.payload_sha256)).map_err(|_| {
        ReleaseApplyReceiptError::Storage(
            "release apply receipt payload digest is invalid".to_string(),
        )
    })?;
    match (receipt.phase, receipt.acked_seq) {
        (ReleaseApplyPhase::Prepared, None) => Ok(()),
        (ReleaseApplyPhase::Committed, Some(acked)) if acked == receipt.through_seq => Ok(()),
        _ => Err(ReleaseApplyReceiptError::Storage(
            "release apply receipt phase and acknowledgement disagree".to_string(),
        )),
    }
}

fn persist_release_apply_receipt(
    path: &StdPath,
    receipt: &ReleaseApplyReceipt,
) -> Result<(), ReleaseApplyReceiptError> {
    validate_release_apply_receipt(receipt)?;
    let mut payload = serde_json::to_vec(receipt).map_err(|error| {
        ReleaseApplyReceiptError::Storage(format!(
            "release apply receipt could not be serialized: {error}"
        ))
    })?;
    payload.push(b'\n');
    flapjack::index::atomic_write_private_file(path, &payload).map_err(|error| {
        ReleaseApplyReceiptError::Storage(format!(
            "release apply receipt could not be persisted: {error}"
        ))
    })
}

fn prepare_release_apply_receipt(
    data_root: &StdPath,
    tenant_id: &str,
    after_seq: u64,
    through_seq: u64,
    transaction_id: &str,
    payload_sha256: &str,
) -> Result<ReleaseApplyDisposition, ReleaseApplyReceiptError> {
    let receipt_path = release_apply_receipt_path(data_root, tenant_id, after_seq, through_seq)?;
    let lock = open_release_apply_lock(&receipt_path)?;
    let expected = ReleaseApplyReceipt {
        schema_version: 1,
        kind: RELEASE_APPLY_RECEIPT_KIND.to_string(),
        tenant_id: tenant_id.to_string(),
        after_seq,
        through_seq,
        transaction_id: transaction_id.to_string(),
        payload_sha256: payload_sha256.to_string(),
        phase: ReleaseApplyPhase::Prepared,
        acked_seq: None,
    };
    validate_release_apply_receipt(&expected)?;

    let receipt = match std::fs::symlink_metadata(&receipt_path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(ReleaseApplyReceiptError::Storage(
                    "release apply receipt must be a regular non-symlink file".to_string(),
                ));
            }
            let payload = std::fs::read(&receipt_path).map_err(|error| {
                ReleaseApplyReceiptError::Storage(format!(
                    "release apply receipt could not be read: {error}"
                ))
            })?;
            let receipt: ReleaseApplyReceipt =
                serde_json::from_slice(&payload).map_err(|error| {
                    ReleaseApplyReceiptError::Storage(format!(
                        "release apply receipt is invalid: {error}"
                    ))
                })?;
            validate_release_apply_receipt(&receipt)?;
            receipt
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            persist_release_apply_receipt(&receipt_path, &expected)?;
            expected
        }
        Err(error) => {
            return Err(ReleaseApplyReceiptError::Storage(format!(
                "release apply receipt could not be inspected: {error}"
            )))
        }
    };

    if receipt.tenant_id != tenant_id
        || receipt.after_seq != after_seq
        || receipt.through_seq != through_seq
        || receipt.transaction_id != transaction_id
        || receipt.payload_sha256 != payload_sha256
    {
        return Err(ReleaseApplyReceiptError::Conflict(
            "release apply interval does not match its durable receipt".to_string(),
        ));
    }

    match receipt.phase {
        ReleaseApplyPhase::Prepared => Ok(ReleaseApplyDisposition::Apply(ReleaseApplyGuard {
            _lock: lock,
            receipt_path,
            receipt,
        })),
        ReleaseApplyPhase::Committed => Ok(ReleaseApplyDisposition::Replay(
            receipt
                .acked_seq
                .expect("validated committed receipt has an ACK"),
        )),
    }
}

/// Encode one JSON value using release canonical-value encoding v1.
///
/// Containers carry an element count, strings carry a UTF-8 byte length,
/// object keys use Unicode scalar order, integers use exact decimal magnitude,
/// and floats use big-endian IEEE-754 bits with floating negative zero
/// normalized to zero. Type tags and terminators make the encoding injective.
fn write_canonical_release_json(
    value: &serde_json::Value,
    output: &mut Vec<u8>,
) -> Result<(), String> {
    match value {
        serde_json::Value::Null => output.push(b'n'),
        serde_json::Value::Bool(true) => output.push(b't'),
        serde_json::Value::Bool(false) => output.push(b'f'),
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_u64() {
                output.push(b'u');
                output.extend_from_slice(value.to_string().as_bytes());
                output.push(b';');
            } else if let Some(value) = number.as_i64() {
                output.push(b'i');
                output.extend_from_slice(value.to_string().as_bytes());
                output.push(b';');
            } else {
                let value = number
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| "release JSON float must be finite".to_string())?;
                let normalized = if value == 0.0 { 0.0 } else { value };
                output.push(b'd');
                output.extend_from_slice(format!("{:016x}", normalized.to_bits()).as_bytes());
            }
        }
        serde_json::Value::String(value) => {
            output.push(b's');
            output.extend_from_slice(value.len().to_string().as_bytes());
            output.push(b':');
            output.extend_from_slice(value.as_bytes());
        }
        serde_json::Value::Array(values) => {
            output.push(b'a');
            output.extend_from_slice(values.len().to_string().as_bytes());
            output.push(b':');
            for value in values {
                write_canonical_release_json(value, output)?;
            }
        }
        serde_json::Value::Object(object) => {
            output.push(b'o');
            output.extend_from_slice(object.len().to_string().as_bytes());
            output.push(b':');
            let sorted: BTreeMap<_, _> = object.iter().collect();
            for (key, value) in sorted {
                write_canonical_release_json(&serde_json::Value::String(key.clone()), output)?;
                write_canonical_release_json(value, output)?;
            }
        }
    }
    Ok(())
}

fn canonical_release_json_bytes(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    write_canonical_release_json(value, &mut output)?;
    Ok(output)
}

fn canonical_release_operations_sha256(ops: &[OpLogEntry]) -> Result<String, String> {
    let value = serde_json::to_value(ops)
        .map_err(|error| format!("release operations could not be canonicalized: {error}"))?;
    let canonical = canonical_release_json_bytes(&value)?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}

fn release_apply_receipt_handler_error(
    error: ReleaseApplyReceiptError,
) -> crate::error_response::HandlerError {
    match error {
        ReleaseApplyReceiptError::Conflict(message) => {
            crate::error_response::HandlerError::bad_request(message)
        }
        ReleaseApplyReceiptError::Storage(message) => {
            tracing::error!("release apply receipt failed: {message}");
            crate::error_response::HandlerError::internal("release apply receipt is unavailable")
        }
    }
}

fn release_apply_ack_response(
    request: &ReleaseRequest,
    tenant_id: &str,
    after_seq: u64,
    through_seq: u64,
    payload_sha256: &str,
) -> Result<Response, crate::error_response::HandlerError> {
    let response = Json(ReplicateOpsResponse {
        tenant_id: tenant_id.to_string(),
        acked_seq: through_seq,
    });
    let response_headers = release_transfer_response_headers(
        request,
        tenant_id,
        after_seq,
        through_seq,
        RELEASE_TRANSFER_STATUS_ACKNOWLEDGED,
        payload_sha256,
    )?;
    Ok((response_headers, response).into_response())
}

fn preflight_replication_op(tenant_id: &str, op_entry: &OpLogEntry) -> Result<(), String> {
    match op_entry.op_type.as_str() {
        "upsert" | "delete" => preflight_document_op(tenant_id, op_entry),
        "move_index" | "copy_index" | "clear_index" => preflight_index_op(tenant_id, op_entry),
        "settings" => preflight_settings_op(tenant_id, op_entry),
        "save_synonym" | "save_synonyms" | "delete_synonym" | "clear_synonyms" | "save_rule"
        | "save_rules" | "delete_rule" | "clear_rules" => {
            preflight_resource_op(tenant_id, op_entry)
        }
        _ => Err(format!(
            "[REPL {}] unknown op_type {} at seq {}",
            tenant_id, op_entry.op_type, op_entry.seq
        )),
    }
}

fn bootstrap_document_version_state(
    manager: &IndexManager,
    tenant_id: &str,
    ops: &[OpLogEntry],
) -> Result<(), String> {
    if !contains_document_replication_ops(ops) {
        return Ok(());
    }
    if manager.get_or_load(tenant_id).is_ok() {
        return Ok(());
    }
    manager
        .create_tenant(tenant_id)
        .map(|_| ())
        .map_err(|error| format!("failed to initialize replication tenant: {error}"))
}

fn contains_document_replication_ops(ops: &[OpLogEntry]) -> bool {
    ops.iter()
        .any(|op| matches!(op.op_type.as_str(), "upsert" | "delete"))
}

/// Applies a single replicated oplog entry (upsert, delete, settings change, copy/move, etc.) to the local index, accumulating batch upserts and deletes.
async fn apply_replication_op(
    manager: &IndexManager,
    state: Option<&AppState>,
    tenant_id: &str,
    op_entry: &OpLogEntry,
    incoming: (u64, String),
    document_batch: &mut ReplicatedDocumentBatch,
) -> Result<(), String> {
    match op_entry.op_type.as_str() {
        "upsert" => {
            apply_upsert_op(manager, tenant_id, op_entry, incoming, document_batch)?;
        }
        "delete" => {
            apply_delete_op(manager, tenant_id, op_entry, incoming, document_batch)?;
        }
        "move_index" => apply_move_index_op(manager, tenant_id, op_entry).await?,
        "copy_index" => apply_copy_index_op(manager, tenant_id, op_entry).await?,
        "clear_index" => apply_clear_index_op(manager, tenant_id, op_entry).await?,
        "settings" => {
            let state = state.ok_or_else(|| {
                format!(
                    "[REPL {}] settings replication requires application state",
                    tenant_id
                )
            })?;
            apply_settings_op(state, tenant_id, op_entry).await?;
        }
        "save_synonym" => apply_save_synonym_op(manager, tenant_id, op_entry)?,
        "save_synonyms" => apply_save_synonyms_op(manager, tenant_id, op_entry)?,
        "delete_synonym" => apply_delete_synonym_op(manager, tenant_id, op_entry)?,
        "clear_synonyms" => apply_clear_synonyms_op(manager, tenant_id, op_entry)?,
        "save_rule" => apply_save_rule_op(manager, tenant_id, op_entry)?,
        "save_rules" => apply_save_rules_op(manager, tenant_id, op_entry)?,
        "delete_rule" => apply_delete_rule_op(manager, tenant_id, op_entry)?,
        "clear_rules" => apply_clear_rules_op(manager, tenant_id, op_entry)?,
        _ => {
            return Err(format!(
                "[REPL {}] unknown op_type {} at seq {}",
                tenant_id, op_entry.op_type, op_entry.seq
            ));
        }
    }
    Ok(())
}

fn local_node_current_seq_map(state: &AppState, current_seq: u64) -> BTreeMap<String, u64> {
    let mut node_current_seqs = BTreeMap::new();
    if let Some(repl_mgr) = state.replication_manager.as_ref() {
        node_current_seqs.insert(repl_mgr.node_id().to_string(), current_seq);
    }
    node_current_seqs
}

/// POST /internal/replicate
/// Receive operations from a peer and apply them to local index.
pub async fn replicate_ops(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ReplicateOpsRequest>,
) -> Result<Response, crate::error_response::HandlerError> {
    replicate_ops_with_headers(State(state), HeaderMap::new(), Json(req)).await
}

/// HTTP entrypoint that layers exact release-transfer coordinates over the
/// legacy replication body. Startup catch-up continues to call
/// [`replicate_ops`] directly and therefore cannot accidentally claim a
/// release-scoped acknowledgement.
pub async fn replicate_ops_with_headers(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ReplicateOpsRequest>,
) -> Result<Response, crate::error_response::HandlerError> {
    let tenant_id = req.tenant_id.clone();

    // Preserve 400 semantics for malformed peer input before apply_ops_to_manager
    // erases validation failures into a plain String for shared non-HTTP callers.
    validate_index_name(&tenant_id).map_err(crate::error_response::HandlerError::from)?;
    validate_single_sender_sequence(&tenant_id, &req.ops)?;
    let release_request = strict_release_request(&headers, &tenant_id, ReleaseRequestMode::Apply)
        .map_err(crate::error_response::HandlerError::bad_request)?;
    let release_window = release_request.as_ref().and_then(|request| request.window);
    let mut release_guard = None;
    if let Some((after_seq, through_seq)) = release_window {
        validate_release_apply_window(&tenant_id, after_seq, through_seq, &req.ops)
            .map_err(crate::error_response::HandlerError::bad_request)?;
        let payload_sha256 = canonical_release_operations_sha256(&req.ops)
            .map_err(crate::error_response::HandlerError::bad_request)?;
        let request = release_request
            .as_ref()
            .expect("release interval is present only for a release request");
        if request.payload_sha256.as_deref() != Some(payload_sha256.as_str()) {
            return Err(crate::error_response::HandlerError::bad_request(
                "release apply body does not match the operations digest",
            ));
        }
        match prepare_release_apply_receipt(
            &state.manager.base_path,
            &tenant_id,
            after_seq,
            through_seq,
            &request.transaction_id,
            &payload_sha256,
        )
        .map_err(release_apply_receipt_handler_error)?
        {
            ReleaseApplyDisposition::Apply(guard) => release_guard = Some(guard),
            ReleaseApplyDisposition::Replay(acked_seq) => {
                if acked_seq != through_seq {
                    return Err(crate::error_response::HandlerError::internal(
                        "release apply replay acknowledgement is invalid",
                    ));
                }
                return release_apply_ack_response(
                    request,
                    &tenant_id,
                    after_seq,
                    through_seq,
                    &payload_sha256,
                );
            }
        }
    }

    let max_seq = apply_ops_to_state(&state, &tenant_id, &req.ops).await?;

    tracing::info!(
        "[REPL {}] applied {} ops (max_seq={})",
        tenant_id,
        req.ops.len(),
        max_seq
    );

    let acked_seq = release_window.map_or(max_seq, |(_, through_seq)| through_seq);
    if let Some(guard) = release_guard {
        guard
            .commit(acked_seq)
            .map_err(release_apply_receipt_handler_error)?;
    }
    let response = Json(ReplicateOpsResponse {
        tenant_id,
        acked_seq,
    });
    if let Some((after_seq, through_seq)) = release_window {
        let request = release_request
            .as_ref()
            .expect("release interval is present only for a release request");
        return release_apply_ack_response(
            request,
            &response.0.tenant_id,
            after_seq,
            through_seq,
            request
                .payload_sha256
                .as_deref()
                .expect("release apply requests require a payload digest"),
        );
    }
    Ok(response.into_response())
}

/// GET /internal/ops?tenant_id=X&since_seq=N
/// Fetch operations since a given sequence number for catch-up
pub async fn get_ops(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<GetOpsQuery>,
) -> Result<Response, crate::error_response::HandlerError> {
    use crate::error_response::HandlerError;

    let tenant_id = query.tenant_id.clone();
    validate_index_name(&tenant_id).map_err(HandlerError::from)?;
    let release_transfer = strict_release_request(
        &headers,
        &tenant_id,
        ReleaseRequestMode::Tail(query.since_seq),
    )
    .map_err(HandlerError::bad_request)?;

    // Get oplog for tenant
    let oplog = match state.manager.get_oplog(&tenant_id) {
        Some(ol) => ol,
        None => {
            // move_index writes are logged under the destination stream after the move,
            // which means the source tenant oplog path no longer exists. For anti-entropy
            // catch-up, when source oplog is missing, search existing oplogs for a matching
            // move_index(source=tenant_id) and return only ops up to that move boundary.
            if let Some((ops, current_seq, moved_to)) =
                find_moved_source_ops(&state, &tenant_id, query.since_seq)
            {
                tracing::info!(
                    "[REPL {}] source oplog missing; serving {} moved-source ops from destination stream {} (since_seq={}, current_seq={})",
                    tenant_id,
                    ops.len(),
                    moved_to,
                    query.since_seq,
                    current_seq
                );
                let payload = GetOpsResponse {
                    tenant_id,
                    ops,
                    current_seq,
                    oldest_retained_seq: None,
                    node_current_seqs: local_node_current_seq_map(&state, current_seq),
                };
                return release_ops_response(release_transfer.as_ref(), query.since_seq, payload);
            }

            tracing::warn!("[REPL {}] oplog not found", tenant_id);
            return Err(HandlerError::not_found("Tenant not found"));
        }
    };

    // Read ops since requested sequence
    let ops = oplog.read_since(query.since_seq).map_err(|e| {
        tracing::error!("[REPL {}] failed to read oplog: {}", tenant_id, e);
        HandlerError::from(e)
    })?;

    let current_seq = oplog.current_seq();
    let oldest_retained_seq = oplog.oldest_seq();

    tracing::info!(
        "[REPL {}] serving {} ops (since_seq={}, current_seq={})",
        tenant_id,
        ops.len(),
        query.since_seq,
        current_seq
    );

    release_ops_response(
        release_transfer.as_ref(),
        query.since_seq,
        GetOpsResponse {
            tenant_id,
            ops,
            current_seq,
            oldest_retained_seq,
            node_current_seqs: local_node_current_seq_map(&state, current_seq),
        },
    )
}

fn release_ops_response(
    release_transfer: Option<&ReleaseRequest>,
    requested_after_seq: u64,
    mut payload: GetOpsResponse,
) -> Result<Response, crate::error_response::HandlerError> {
    let Some(release_request) = release_transfer else {
        return Ok(Json(payload).into_response());
    };
    let projection = release_tail_projection(
        requested_after_seq,
        payload.current_seq,
        payload.oldest_retained_seq,
        std::mem::take(&mut payload.ops),
    )
    .map_err(crate::error_response::HandlerError::internal)?;
    payload.ops = projection.ops;
    let payload_sha256 = canonical_release_operations_sha256(&payload.ops)
        .map_err(crate::error_response::HandlerError::internal)?;
    let response_headers = release_transfer_response_headers(
        release_request,
        &payload.tenant_id,
        requested_after_seq,
        projection.through_seq,
        projection.status.as_header(),
        &payload_sha256,
    )?;
    Ok((response_headers, Json(payload)).into_response())
}

/// GET /internal/tenants
/// Return visible tenant directory names for startup catch-up discovery.
pub async fn list_tenants(
    State(state): State<Arc<AppState>>,
) -> Result<Json<ListTenantsResponse>, crate::error_response::HandlerError> {
    let mut tenants = crate::tenant_dirs::visible_tenant_dir_names(&state.manager.base_path)?;
    tenants.sort();
    Ok(Json(ListTenantsResponse { tenants }))
}

/// GET /internal/snapshot/:tenantId
/// Export a tenant directory as gzipped snapshot bytes for startup gap recovery.
pub async fn internal_snapshot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ValidatedIndexName(tenant_id): ValidatedIndexName,
) -> Result<impl IntoResponse, crate::error_response::HandlerError> {
    use crate::error_response::HandlerError;

    let release_transfer =
        strict_release_request(&headers, &tenant_id, ReleaseRequestMode::Snapshot)
            .map_err(HandlerError::bad_request)?;

    let tenant_path = state.manager.base_path.join(&tenant_id);
    if !tenant_path.exists() {
        return Err(HandlerError::not_found("Tenant not found"));
    }

    // A replica catching up from these bytes must not inherit a mid-commit
    // generation, so this takes the same quiesce and the same blocking byte seam as
    // every other snapshot producer. The guard is held across the read.
    let _quiesce = state
        .manager
        .quiesce_tenant(&tenant_id.to_string())
        .await
        .map_err(|error| {
            tracing::error!(
                "[REPL {}] failed to quiesce before internal snapshot: {}",
                tenant_id,
                error
            );
            HandlerError::from(error)
        })?;

    let through_seq = if release_transfer.is_some() {
        state
            .manager
            // Quiesce deliberately clears loaded runtime state, including the
            // cached oplog. Reopen the durable oplog while admission remains
            // fenced so this watermark names the exact bytes exported below.
            .get_or_create_oplog(&tenant_id)
            .ok_or_else(|| {
                HandlerError::internal("release snapshot could not open the durable oplog")
            })?
            .current_seq()
    } else {
        0
    };
    let export_tenant_id = tenant_id.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        crate::snapshot_byte_ops::export_snapshot_bytes(&tenant_path, &export_tenant_id)
    })
    .await
    .map_err(|join_error| {
        HandlerError::internal(format!(
            "[REPL {tenant_id}] internal snapshot export task failed: {join_error}"
        ))
    })?
    .map_err(|error| {
        tracing::error!(
            "[REPL {}] failed to export internal snapshot: {}",
            tenant_id,
            error
        );
        HandlerError::from(error)
    })?;

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/gzip"),
    );
    if let Some(release_request) = release_transfer.as_ref() {
        let digest = format!("{:x}", Sha256::digest(&bytes));
        response_headers = release_transfer_response_headers(
            release_request,
            &tenant_id,
            0,
            through_seq,
            RELEASE_TRANSFER_STATUS_CONTIGUOUS,
            &digest,
        )?;
        response_headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/gzip"),
        );
        response_headers.insert(
            RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER,
            HeaderValue::from_str(&digest)
                .map_err(|_| HandlerError::internal("snapshot digest header was invalid"))?,
        );
    }

    Ok((response_headers, bytes))
}

/// Search all tenant oplogs for a `move_index` entry whose source matches `source_tenant`.
///
/// Used as a fallback when a source tenant's oplog no longer exists because the
/// index was renamed. Returns ops from the destination stream up to (and including)
/// the move boundary, so the replica can catch up without missing the move event.
///
/// # Arguments
///
/// * `state` - Application state providing access to the index manager.
/// * `source_tenant` - Original tenant name before the move.
/// * `since_seq` - Sequence number to read ops from.
///
/// # Returns
///
/// `Some((ops, current_seq, destination_tenant))` if a matching move was found,
/// or `None` if no destination stream contains a relevant `move_index` entry.
fn find_moved_source_ops(
    state: &AppState,
    source_tenant: &str,
    since_seq: u64,
) -> Option<(Vec<OpLogEntry>, u64, String)> {
    let tenant_names =
        crate::tenant_dirs::valid_index_tenant_dir_names(&state.manager.base_path).ok()?;
    let node_id = std::env::var("FLAPJACK_NODE_ID").unwrap_or_else(|_| "unknown".to_string());

    for candidate_tenant in tenant_names {
        if candidate_tenant == source_tenant {
            continue;
        }

        let oplog_dir = state
            .manager
            .base_path
            .join(&candidate_tenant)
            .join("oplog");
        if !oplog_dir.exists() {
            continue;
        }

        let oplog = match OpLog::open(&oplog_dir, &candidate_tenant, &node_id) {
            Ok(oplog) => oplog,
            Err(e) => {
                tracing::debug!(
                    "[REPL {}] moved-source fallback skipping {}: failed to open oplog: {}",
                    source_tenant,
                    candidate_tenant,
                    e
                );
                continue;
            }
        };

        let mut ops = match oplog.read_since(since_seq) {
            Ok(ops) => ops,
            Err(e) => {
                tracing::debug!(
                    "[REPL {}] moved-source fallback skipping {}: failed to read oplog: {}",
                    source_tenant,
                    candidate_tenant,
                    e
                );
                continue;
            }
        };

        let Some(move_pos) = ops.iter().position(|op| {
            op.op_type == "move_index"
                && op
                    .payload
                    .get("source")
                    .and_then(|v| v.as_str())
                    .map(|src| src == source_tenant)
                    .unwrap_or(false)
        }) else {
            continue;
        };

        // Never return destination writes after the move boundary when serving
        // source stream catch-up.
        ops.truncate(move_pos + 1);
        let current_seq = ops.last().map(|op| op.seq).unwrap_or(since_seq);
        return Some((ops, current_seq, candidate_tenant));
    }

    None
}

/// GET /internal/status
/// Return basic replication status for monitoring
#[utoipa::path(
    get,
    path = "/internal/status",
    tag = "internal",
    responses(
        (status = 200, description = "Replication and storage status", body = ReplicationStatusResponse)
    ),
    security(("api_key" = []))
)]
pub async fn replication_status(
    State(state): State<Arc<AppState>>,
) -> Json<ReplicationStatusResponse> {
    let (node_id, replication_enabled, peer_count) = match &state.replication_manager {
        Some(repl_mgr) => (repl_mgr.node_id().to_string(), true, repl_mgr.peer_count()),
        None => (
            std::env::var("FLAPJACK_NODE_ID").unwrap_or_else(|_| "unknown".to_string()),
            false,
            0,
        ),
    };

    // Get SSL renewal status if available
    let ssl_renewal = if let Some(ref ssl_mgr) = state.ssl_manager {
        Some(ssl_mgr.get_status().await)
    } else {
        None
    };

    let storage_total_bytes: u64 = state
        .manager
        .all_tenant_storage()
        .iter()
        .map(|(_, b)| b)
        .sum();
    let tenant_count = state.manager.loaded_count();

    #[cfg(feature = "vector-search")]
    let vector_memory_bytes = state.manager.vector_memory_usage();
    #[cfg(not(feature = "vector-search"))]
    let vector_memory_bytes = 0usize;

    Json(ReplicationStatusResponse {
        node_id,
        replication_enabled,
        peer_count,
        ssl_renewal: ssl_renewal.map(SslRenewalStatus::from),
        storage_total_bytes,
        tenant_count,
        vector_memory_bytes,
    })
}

/// GET /internal/cluster/status
/// Return health status of all peers based on last_success timestamps.
/// Provides quick cluster health overview without active probing.
#[utoipa::path(
    get,
    path = "/internal/cluster/status",
    tag = "cluster",
    responses(
        (status = 200, description = "Cluster membership and auto-heal lifecycle status", body = ClusterStatusResponse),
        (status = 403, description = "Admin API key is required")
    ),
    security(("api_key" = []))
)]
pub async fn cluster_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let repl_mgr = match &state.replication_manager {
        Some(r) => r,
        None => {
            return (
                StatusCode::OK,
                Json(ClusterStatusResponse::Standalone(
                    ClusterStatusStandaloneResponse {
                        node_id: std::env::var("FLAPJACK_NODE_ID")
                            .unwrap_or_else(|_| "unknown".to_string()),
                        replication_enabled: false,
                        peers: Vec::new(),
                        autoheal_enabled: false,
                        autoheal_peers: Vec::new(),
                    },
                )),
            )
                .into_response();
        }
    };

    let peers = repl_mgr
        .peer_statuses()
        .into_iter()
        .map(|peer| ClusterPeerStatus {
            peer_id: peer.peer_id,
            addr: peer.addr,
            status: peer.status,
            last_success_secs_ago: peer.last_success_secs_ago,
        })
        .collect::<Vec<_>>();

    let healthy_count = peers.iter().filter(|peer| peer.status == "healthy").count();
    let peers_total = peers.len();
    let autoheal = autoheal_lifecycle_response(repl_mgr.autoheal_lifecycle_projection());

    (
        StatusCode::OK,
        Json(ClusterStatusResponse::Ha(ClusterStatusHaResponse {
            node_id: repl_mgr.node_id().to_string(),
            replication_enabled: true,
            peers_total,
            peers_healthy: healthy_count,
            peers,
            autoheal_enabled: autoheal.autoheal_enabled,
            autoheal_peers: autoheal.peers,
        })),
    )
        .into_response()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClusterPeerHealthStatus {
    Healthy,
    Stale,
    Unhealthy,
    CircuitOpen,
    NeverContacted,
}

impl ClusterPeerHealthStatus {
    pub const WIRE_TOKENS: [&'static str; 5] = [
        "healthy",
        "stale",
        "unhealthy",
        "circuit_open",
        "never_contacted",
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct ClusterPeerStatus {
    pub peer_id: String,
    pub addr: String,
    #[schema(value_type = ClusterPeerHealthStatus)]
    pub status: String,
    #[schema(required)]
    pub last_success_secs_ago: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClusterStatusStandaloneResponse {
    pub node_id: String,
    pub replication_enabled: bool,
    pub peers: Vec<ClusterPeerStatus>,
    #[serde(default)]
    pub autoheal_enabled: bool,
    #[serde(default)]
    pub autoheal_peers: Vec<AutohealPeerLifecycleResponse>,
}

struct AutohealLifecycleResponse {
    autoheal_enabled: bool,
    peers: Vec<AutohealPeerLifecycleResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct AutohealPeerLifecycleResponse {
    pub peer_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    pub observation_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Object)]
    pub decision: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<AutohealActionResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct AutohealActionResponse {
    pub phase: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn autoheal_lifecycle_response(
    projection: AutohealLifecycleProjection,
) -> AutohealLifecycleResponse {
    AutohealLifecycleResponse {
        autoheal_enabled: projection.autoheal_enabled,
        peers: projection
            .peers
            .into_iter()
            .map(autoheal_peer_lifecycle_response)
            .collect(),
    }
}

fn autoheal_peer_lifecycle_response(peer: AutohealPeerLifecycle) -> AutohealPeerLifecycleResponse {
    AutohealPeerLifecycleResponse {
        peer_id: peer.peer_id,
        addr: peer.addr,
        observation_count: peer.observation_count,
        decision: peer
            .last_decision
            .map(|decision| serde_json::to_value(decision).expect("decision should serialize")),
        action: peer.last_action.map(|action| AutohealActionResponse {
            phase: action.phase,
            outcome: action.outcome,
            error: action.error,
        }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClusterStatusHaResponse {
    pub node_id: String,
    pub replication_enabled: bool,
    pub peers_total: usize,
    pub peers_healthy: usize,
    pub peers: Vec<ClusterPeerStatus>,
    #[serde(default)]
    pub autoheal_enabled: bool,
    #[serde(default)]
    pub autoheal_peers: Vec<AutohealPeerLifecycleResponse>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, ToSchema)]
#[serde(untagged)]
pub enum ClusterStatusResponse {
    Standalone(ClusterStatusStandaloneResponse),
    Ha(ClusterStatusHaResponse),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClusterStatusResponseFields {
    node_id: String,
    replication_enabled: bool,
    peers_total: Option<usize>,
    peers_healthy: Option<usize>,
    peers: Vec<ClusterPeerStatus>,
    #[serde(default)]
    autoheal_enabled: bool,
    #[serde(default)]
    autoheal_peers: Vec<AutohealPeerLifecycleResponse>,
}

impl<'de> Deserialize<'de> for ClusterStatusResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = ClusterStatusResponseFields::deserialize(deserializer)?;
        if fields.replication_enabled {
            let peers_total = fields.peers_total.unwrap_or(fields.peers.len());
            let peers_healthy = fields.peers_healthy.unwrap_or_else(|| {
                fields
                    .peers
                    .iter()
                    .filter(|peer| peer.status == "healthy")
                    .count()
            });
            return Ok(Self::Ha(ClusterStatusHaResponse {
                node_id: fields.node_id,
                replication_enabled: true,
                peers_total,
                peers_healthy,
                peers: fields.peers,
                autoheal_enabled: fields.autoheal_enabled,
                autoheal_peers: fields.autoheal_peers,
            }));
        }

        if fields.peers_total.is_some() || fields.peers_healthy.is_some() {
            return Err(serde::de::Error::custom(
                "standalone cluster status cannot include peer counts",
            ));
        }
        if !fields.peers.is_empty() {
            return Err(serde::de::Error::custom(
                "standalone cluster status cannot include peer rows",
            ));
        }

        Ok(Self::Standalone(ClusterStatusStandaloneResponse {
            node_id: fields.node_id,
            replication_enabled: false,
            peers: fields.peers,
            autoheal_enabled: fields.autoheal_enabled,
            autoheal_peers: fields.autoheal_peers,
        }))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct SslRenewalStatus {
    pub enabled: bool,
    pub status: String,
    #[schema(required)]
    pub error: Option<String>,
    #[schema(required)]
    pub cert_expires_in_days: Option<i64>,
    #[schema(required)]
    pub next_check: Option<DateTime<Utc>>,
}

impl From<RenewalStatus> for SslRenewalStatus {
    fn from(status: RenewalStatus) -> Self {
        Self {
            enabled: status.enabled,
            status: status.status,
            error: status.error,
            cert_expires_in_days: status.cert_expires_in_days,
            next_check: status.next_check,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct ReplicationStatusResponse {
    pub node_id: String,
    pub replication_enabled: bool,
    pub peer_count: usize,
    #[schema(required)]
    pub ssl_renewal: Option<SslRenewalStatus>,
    pub storage_total_bytes: u64,
    pub tenant_count: usize,
    pub vector_memory_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct AddPeerRequest {
    pub node_id: String,
    pub addr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct AddPeerResponse {
    pub node_id: String,
    pub addr: String,
    pub peers_total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct RemovePeerResponse {
    pub node_id: String,
    pub peers_total: usize,
}

/// Add one peer to the live replication membership without changing bootstrap config.
#[utoipa::path(
    post,
    path = "/internal/cluster/peers",
    tag = "cluster",
    request_body = AddPeerRequest,
    responses(
        (status = 200, description = "Peer added to runtime membership", body = AddPeerResponse),
        (status = 400, description = "Invalid peer or replication is not configured"),
        (status = 403, description = "Admin API key is required"),
        (status = 409, description = "Peer conflicts with current runtime membership")
    ),
    security(("api_key" = []))
)]
pub async fn add_cluster_peer(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AddPeerRequest>,
) -> Response {
    let node_id = request.node_id.trim().to_string();
    if node_id.is_empty() {
        return crate::error_response::json_error(
            StatusCode::BAD_REQUEST,
            "node_id must not be blank",
        );
    }

    let Some(addr) = NodeConfig::normalize_peer_addr(&request.addr) else {
        return crate::error_response::json_error(
            StatusCode::BAD_REQUEST,
            "addr must be a safe HTTP or HTTPS peer URL",
        );
    };
    if let Err(message) =
        crate::analytics_cluster::validate_authenticated_query_peer_transport(&node_id, &addr)
    {
        return crate::error_response::json_error(StatusCode::BAD_REQUEST, message);
    }
    let Some(replication_manager) = state.replication_manager.as_ref() else {
        return crate::error_response::json_error(
            StatusCode::BAD_REQUEST,
            "replication manager is not configured",
        );
    };

    let receipt = match replication_manager.add_peer(PeerConfig { node_id, addr }) {
        Ok(receipt) => receipt,
        Err(error) => return add_peer_error_response(error),
    };

    (
        StatusCode::OK,
        Json(AddPeerResponse {
            node_id: receipt.node_id,
            addr: receipt.addr,
            peers_total: receipt.peers_total,
        }),
    )
        .into_response()
}

fn add_peer_error_response(error: AddPeerError) -> Response {
    match error {
        AddPeerError::Conflict(message) => {
            crate::error_response::json_error(StatusCode::CONFLICT, message)
        }
        AddPeerError::Persistence(error) => {
            tracing::error!(error = %error, "failed to persist replication peer membership");
            crate::error_response::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist replication peer membership",
            )
        }
    }
}

/// Remove one peer from the live replication membership without changing bootstrap config.
#[utoipa::path(
    delete,
    path = "/internal/cluster/peers/{node_id}",
    tag = "cluster",
    params(
        ("node_id" = String, Path, description = "Runtime peer node identifier")
    ),
    responses(
        (status = 200, description = "Peer removed from runtime membership", body = RemovePeerResponse),
        (status = 400, description = "Replication is not configured"),
        (status = 403, description = "Admin API key is required"),
        (status = 404, description = "Peer is not in runtime membership")
    ),
    security(("api_key" = []))
)]
pub async fn remove_cluster_peer(
    State(state): State<Arc<AppState>>,
    Path(node_id): Path<String>,
) -> Response {
    let Some(replication_manager) = state.replication_manager.as_ref() else {
        return crate::error_response::json_error(
            StatusCode::BAD_REQUEST,
            "replication manager is not configured",
        );
    };

    match replication_manager.remove_peer(&node_id) {
        Ok(Some(receipt)) => (
            StatusCode::OK,
            Json(RemovePeerResponse {
                node_id: receipt.node_id,
                peers_total: receipt.peers_total,
            }),
        )
            .into_response(),
        Ok(None) => crate::error_response::json_error(
            StatusCode::NOT_FOUND,
            format!("Peer '{node_id}' not found"),
        ),
        Err(error) => {
            tracing::error!(error = %error, "failed to remove replication peer membership");
            crate::error_response::json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist replication peer membership",
            )
        }
    }
}

/// POST /internal/rotate-admin-key
/// Generate a new admin key, update the in-memory KeyStore and persist to disk.
/// Returns the new plaintext key. Requires admin auth.
pub async fn rotate_admin_key(
    key_store: axum::Extension<Arc<crate::auth::KeyStore>>,
) -> impl IntoResponse {
    match key_store.rotate_admin_key() {
        Ok(new_key) => {
            security_audit::emit_admin_action(
                Actor::admin_api_key(),
                Action::RotateAdminKey,
                Target::admin_key(),
                Outcome::Success,
                None,
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({ "key": new_key, "message": "Admin key rotated" })),
            )
                .into_response()
        }
        Err(e) => rotate_admin_key_error_response(&e),
    }
}

fn rotate_admin_key_error_response(error: &dyn std::fmt::Display) -> Response {
    tracing::error!(error = %error, "admin key rotation failed");
    crate::error_response::json_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Failed to rotate admin key",
    )
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
#[path = "internal_tests.rs"]
mod tests;

/// POST /internal/analytics-rollup
///
/// Receive a pre-computed analytics rollup from a peer and store it in the
/// global rollup cache. Part of Phase 4 (HA Analytics Tier 2).
///
/// Protected by auth middleware in normal operation: `/internal/*` routes
/// require the admin key (see `required_acl_for_route`). In `--no-auth` local
/// dev mode, these routes are intentionally open.
pub async fn receive_analytics_rollup(
    Json(rollup): Json<crate::analytics_cluster::AnalyticsRollup>,
) -> impl IntoResponse {
    let cache = crate::analytics_cluster::get_global_rollup_cache();
    tracing::debug!(
        "[ROLLUP] received rollup from peer={} index={} generated_at={}",
        rollup.node_id,
        rollup.index,
        rollup.generated_at_secs
    );
    cache.store(rollup);
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
}

/// GET /internal/rollup-cache
///
/// Diagnostic endpoint: returns all entries currently stored in the global
/// rollup cache. Used by tests and operators to inspect the Tier 2 cache state.
///
/// Response: `{"count": N, "entries": [AnalyticsRollup, ...]}`
pub async fn rollup_cache_status() -> impl IntoResponse {
    let cache = crate::analytics_cluster::get_global_rollup_cache();
    let entries = cache.all_entries();
    let count = entries.len();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "count": count,
            "entries": entries
        })),
    )
        .into_response()
}

/// GET /internal/storage
/// Returns disk usage and doc count for every durable tenant.
pub async fn storage_all(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, crate::error_response::HandlerError> {
    prepare_release_inventory(&state.manager)?;
    let tenants = canonical_release_inventory(&state.manager)?
        .into_iter()
        .map(|entry| {
            let bytes = state.manager.tenant_storage_bytes(&entry.index_id);
            serde_json::json!({
                "id": entry.index_id,
                "bytes": bytes,
                "doc_count": entry.document_count,
            })
        })
        .collect::<Vec<_>>();

    Ok(Json(serde_json::json!({ "tenants": tenants })))
}

/// GET /internal/storage/:indexName
/// Returns disk usage and doc count for a specific tenant.
pub async fn storage_index(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
) -> impl IntoResponse {
    let bytes = state.manager.tenant_storage_bytes(&index_name);
    let doc_count = state.manager.tenant_doc_count(&index_name).unwrap_or(0);
    (
        StatusCode::OK,
        Json(serde_json::json!({ "index": index_name, "bytes": bytes, "doc_count": doc_count })),
    )
        .into_response()
}

/// POST /internal/indexes/:indexName/count
///
/// Executes the canonical search pipeline with a zero-sized hit window and
/// returns only readiness plus the matching count. This route is mounted only
/// in the authenticated internal-admin group and outside customer usage
/// middleware.
pub async fn count_only_search(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
    Json(request): Json<InternalCountOnlySearchRequest>,
) -> Result<Json<serde_json::Value>, crate::error_response::HandlerError> {
    validate_count_only_hits_per_page(request.hits_per_page)
        .map_err(crate::error_response::HandlerError::from)?;

    let search_request = SearchRequest {
        query: request.query,
        hits_per_page: Some(0),
        analytics: Some(false),
        click_analytics: Some(false),
        attributes_to_retrieve: Some(Vec::new()),
        ..Default::default()
    };
    let Json(result) =
        crate::handlers::search::search_single(State(state), index_name.clone(), search_request)
            .await
            .map_err(crate::error_response::HandlerError::from)?;
    let count = result
        .get("nbHits")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| crate::error_response::HandlerError::internal("search count missing"))?;

    Ok(Json(serde_json::json!({
        "index": index_name,
        "status": "ready",
        "nbHits": count,
    })))
}

/// GET /.well-known/acme-challenge/:token
/// ACME http-01 challenge handler for Let's Encrypt validation
pub async fn acme_challenge(
    Path(token): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    tracing::debug!("[SSL] ACME challenge request for token: {}", token);

    if let Some(ref ssl_mgr) = state.ssl_manager {
        if let Some(acme_client) = ssl_mgr.get_acme_client() {
            if let Some(response) = acme_client.get_challenge_response(&token) {
                tracing::info!("[SSL] Serving ACME challenge response for token: {}", token);
                return (StatusCode::OK, response).into_response();
            }
        }
    }

    tracing::warn!("[SSL] ACME challenge token not found: {}", token);
    (StatusCode::NOT_FOUND, "Challenge not found").into_response()
}

/// POST /internal/pause/:indexName
/// Mark an index as paused — writes will be rejected with 503.
pub async fn pause_index(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
) -> impl IntoResponse {
    state.paused_indexes.pause(&index_name);
    tracing::info!("[PAUSE] index '{}' paused", index_name);
    (
        StatusCode::OK,
        Json(serde_json::json!({"index": index_name, "paused": true})),
    )
        .into_response()
}

/// POST /internal/resume/:indexName
/// Clear the paused flag — writes resume normally.
pub async fn resume_index(
    State(state): State<Arc<AppState>>,
    ValidatedIndexName(index_name): ValidatedIndexName,
) -> impl IntoResponse {
    state.paused_indexes.resume(&index_name);
    tracing::info!("[PAUSE] index '{}' resumed", index_name);
    (
        StatusCode::OK,
        Json(serde_json::json!({"index": index_name, "paused": false})),
    )
        .into_response()
}
