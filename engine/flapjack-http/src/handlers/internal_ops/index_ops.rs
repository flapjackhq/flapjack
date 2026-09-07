use flapjack::index::oplog::OpLogEntry;
use flapjack::validate_index_name;
use flapjack::IndexManager;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Replication position coordinate `(timestamp_ms, source_seq)` taken from an
/// operation's own oplog row.
pub(crate) type Position = (u64, u64);

const POSITIONS_SCHEMA_VERSION: u64 = 1;
const POSITIONS_DIRECTORY: &str = ".replication-positions";

/// Highest applied replicated index-operation coordinates for one origin node.
///
/// Both fields are nullable but required on disk: an omitted field is treated
/// as corrupt evidence rather than silently defaulted, so a truncated or
/// hand-edited record cannot widen what the replica believes it has applied.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NodePositions {
    #[serde(deserialize_with = "required_nullable_position")]
    pub(crate) index_op: Option<Position>,
    #[serde(deserialize_with = "required_nullable_position")]
    pub(crate) clear: Option<Position>,
}

/// Durable per-outer-tenant replay evidence, one record per origin node, stored
/// at `<data_root>/.replication-positions/<tenant>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TenantPositions {
    pub(crate) schema_version: u64,
    pub(crate) nodes: BTreeMap<String, NodePositions>,
}

impl Default for TenantPositions {
    fn default() -> Self {
        Self {
            schema_version: POSITIONS_SCHEMA_VERSION,
            nodes: BTreeMap::new(),
        }
    }
}

impl TenantPositions {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let positions: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if positions.schema_version != POSITIONS_SCHEMA_VERSION {
            return Err(format!(
                "unsupported schema_version {} (expected {POSITIONS_SCHEMA_VERSION})",
                positions.schema_version
            ));
        }
        if positions.nodes.keys().any(String::is_empty) {
            return Err("node id must not be empty".to_string());
        }
        Ok(positions)
    }

    fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|error| error.to_string())
    }
}

/// Deserialize a nullable `[timestamp_ms, source_seq]` pair that must be present
/// in the record, rejecting signed, fractional, string, or wrongly sized tuples.
fn required_nullable_position<'de, D>(deserializer: D) -> Result<Option<Position>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<[u64; 2]>::deserialize(deserializer)
        .map(|position| position.map(|[timestamp_ms, seq]| (timestamp_ms, seq)))
}

fn positions_root(base_path: &Path) -> PathBuf {
    base_path.join(POSITIONS_DIRECTORY)
}

fn positions_path(base_path: &Path, tenant_id: &str) -> PathBuf {
    positions_root(base_path).join(format!("{tenant_id}.json"))
}

fn is_real_directory(metadata: &std::fs::Metadata) -> bool {
    metadata.is_dir() && !metadata.file_type().is_symlink()
}

fn is_real_file(metadata: &std::fs::Metadata) -> bool {
    metadata.is_file() && !metadata.file_type().is_symlink()
}

fn positions_error(tenant_id: &str, detail: impl std::fmt::Display) -> String {
    format!("[REPL {tenant_id}] replication position evidence unavailable: {detail}")
}

/// Load the tenant's durable replay evidence, initializing it when genuinely
/// absent.
///
/// Only true absence (no evidence root, or no tenant file inside a real root)
/// initializes an empty record, and that record is durably written before it
/// is returned so the first destructive dispatch already has readable
/// evidence. A missing root is created through the shared private-directory
/// facade; an existing root is validated as a real directory but never
/// repaired, so an unwritable root fails here rather than after an effect.
/// Malformed JSON, unsupported schema, unknown or missing fields, unsafe file
/// or root types, symlinks, and read errors fail closed without touching the
/// existing bytes.
pub(crate) fn load_tenant_positions(
    manager: &IndexManager,
    tenant_id: &str,
) -> Result<TenantPositions, String> {
    let root = positions_root(&manager.base_path);
    match std::fs::symlink_metadata(&root) {
        Ok(metadata) if is_real_directory(&metadata) => {}
        Ok(_) => {
            return Err(positions_error(
                tenant_id,
                "evidence root is not a real directory",
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            flapjack::index::ensure_private_directory(&root)
                .map_err(|error| positions_error(tenant_id, error))?;
            return initialize_tenant_positions(manager, tenant_id);
        }
        Err(error) => return Err(positions_error(tenant_id, error)),
    }

    let path = positions_path(&manager.base_path, tenant_id);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if is_real_file(&metadata) => {
            let bytes = std::fs::read(&path).map_err(|error| positions_error(tenant_id, error))?;
            TenantPositions::parse(&bytes).map_err(|error| positions_error(tenant_id, error))
        }
        Ok(_) => Err(positions_error(
            tenant_id,
            "evidence file is not a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            initialize_tenant_positions(manager, tenant_id)
        }
        Err(error) => Err(positions_error(tenant_id, error)),
    }
}

fn initialize_tenant_positions(
    manager: &IndexManager,
    tenant_id: &str,
) -> Result<TenantPositions, String> {
    let positions = TenantPositions::default();
    persist_tenant_positions(manager, tenant_id, &positions)?;
    Ok(positions)
}

/// Durably replace the tenant's evidence file (mode `0600`, payload sync,
/// rename, parent sync). A failure before the rename leaves the prior file
/// untouched.
fn persist_tenant_positions(
    manager: &IndexManager,
    tenant_id: &str,
    positions: &TenantPositions,
) -> Result<(), String> {
    let path = positions_path(&manager.base_path, tenant_id);
    let bytes = positions
        .to_bytes()
        .map_err(|error| positions_error(tenant_id, error))?;
    flapjack::index::atomic_write_private_file(&path, &bytes)
        .map_err(|error| positions_error(tenant_id, error))
}

/// Decide whether an admitted index row is a replay of evidence already
/// applied for its origin node.
///
/// Positions are compared timestamp-first: `(timestamp_ms, seq)` is ordered
/// lexicographically, so a newer origin wall-clock wins even when the origin's
/// sequence restarted at 0 after a clear or when a move/copy made the
/// destination inherit the source oplog; only equal timestamps fall back to the
/// sequence as the tie-breaker. This deliberately differs from document LWW,
/// which breaks ties on node id: here both coordinates come from one origin
/// node's own row, so node identity is never part of the comparison.
///
/// Accepted hazards (see `refined_input.md`, section "Accepted risks"):
/// - origin wall-clock rollback makes a genuinely later row compare older and
///   be skipped;
/// - a post-clear row in the same millisecond as the clear, with a reset
///   sequence at or below the clear's, is skipped permanently;
/// - restoring an origin snapshot behind a recorded position requires a
///   replica rebuild rather than replay.
fn is_replayed_position(applied: Option<Position>, candidate: Position) -> bool {
    applied.is_some_and(|applied| candidate <= applied)
}

/// Resolve the origin node a row's position evidence is keyed by.
///
/// Evidence keys are exact non-empty `OpLogEntry.node_id` values, so a row
/// without one cannot be positioned at all. Admission and the apply owner both
/// refuse it through this one check.
fn positioned_origin_node<'a>(
    tenant_id: &str,
    op_entry: &'a OpLogEntry,
) -> Result<&'a str, String> {
    if op_entry.node_id.is_empty() {
        return Err(format!(
            "[REPL {tenant_id}] {} seq {} has an empty origin node id",
            op_entry.op_type, op_entry.seq
        ));
    }
    Ok(&op_entry.node_id)
}

/// Whether a tenant-local mutation is covered by its own source node's durable
/// clear boundary and must therefore be skipped.
///
/// The boundary is compared with the same timestamp-first ordering as
/// [`is_replayed_position`], so a stale pre-clear row that a restart re-delivers
/// cannot resurrect state the clear removed, while a row from any other origin
/// node keeps its own independent history.
pub(crate) fn is_covered_by_clear_boundary(
    positions: &TenantPositions,
    node_id: &str,
    candidate: Position,
) -> bool {
    is_replayed_position(
        positions.nodes.get(node_id).and_then(|node| node.clear),
        candidate,
    )
}

/// Apply one admitted replicated index operation at most once per origin node.
///
/// Replays at or below the node's durable `index_op` position acknowledge
/// without effect. Otherwise the existing effect owner runs first; only after
/// it succeeds is the candidate evidence built from a clone, advanced (also
/// recording the clear boundary for `clear_index`), persisted, and finally
/// published into the caller's in-memory owner. A failed effect or a failed
/// persistence therefore leaves both disk and memory at the prior evidence, so
/// a retry through the same owner re-dispatches instead of skipping.
pub(crate) async fn apply_replicated_index_op(
    manager: &IndexManager,
    tenant_id: &str,
    positions: &mut TenantPositions,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let node_id = positioned_origin_node(tenant_id, op_entry)?;
    let candidate: Position = (op_entry.timestamp_ms, op_entry.seq);
    let applied = positions.nodes.get(node_id).and_then(|node| node.index_op);
    if is_replayed_position(applied, candidate) {
        return Ok(());
    }

    match op_entry.op_type.as_str() {
        "move_index" => apply_move_index_op(manager, tenant_id, op_entry).await?,
        "copy_index" => apply_copy_index_op(manager, tenant_id, op_entry).await?,
        "clear_index" => apply_clear_index_op(manager, tenant_id, op_entry).await?,
        other => {
            return Err(format!(
                "[REPL {tenant_id}] {other} seq {} is not a replicated index operation",
                op_entry.seq
            ))
        }
    }

    let mut next = positions.clone();
    let node = next.nodes.entry(op_entry.node_id.clone()).or_default();
    node.index_op = Some(candidate);
    if op_entry.op_type == "clear_index" {
        node.clear = Some(candidate);
    }
    persist_tenant_positions(manager, tenant_id, &next)?;
    *positions = next;
    Ok(())
}

pub(crate) fn preflight_index_op(tenant_id: &str, op_entry: &OpLogEntry) -> Result<(), String> {
    positioned_origin_node(tenant_id, op_entry)?;
    let validate_endpoint = |operation: &str, field_name: &str| {
        let value = op_entry
            .payload
            .get(field_name)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!(
                    "[REPL {}] {} seq {} missing {} field",
                    tenant_id, operation, op_entry.seq, field_name
                )
            })?;
        validate_index_name(value).map_err(|error| {
            format!(
                "[REPL {}] {} seq {} invalid {} '{}': {}",
                tenant_id, operation, op_entry.seq, field_name, value, error
            )
        })?;
        Ok::<&str, String>(value)
    };
    let validate_distinct_endpoints = |operation: &str, source: &str, destination: &str| {
        if source == destination {
            return Err(format!(
                "[REPL {}] {} seq {} source and destination must differ",
                tenant_id, operation, op_entry.seq
            ));
        }
        Ok(())
    };
    // The origin logs each index operation on exactly one index, so only the
    // stream whose oplog carries the row may replicate it: clear on the cleared
    // index, copy on its source, and move on its destination. Any other
    // relationship is an unauthorized cross-stream row.
    let validate_admitted_origin_stream =
        |operation: &str, field_name: &str, origin_index: &str| {
            if origin_index != tenant_id {
                return Err(format!(
                    "[REPL {}] {} seq {} {} '{}' must be the replicated tenant",
                    tenant_id, operation, op_entry.seq, field_name, origin_index
                ));
            }
            Ok(())
        };

    match op_entry.op_type.as_str() {
        "move_index" => {
            let source = validate_endpoint("move_index", "source")?;
            let destination = validate_endpoint("move_index", "destination")?;
            validate_distinct_endpoints("move_index", source, destination)?;
            validate_admitted_origin_stream("move_index", "destination", destination)
        }
        "copy_index" => {
            let source = validate_endpoint("copy_index", "source")?;
            let destination = validate_endpoint("copy_index", "destination")?;
            validate_distinct_endpoints("copy_index", source, destination)?;
            parse_copy_scope(tenant_id, op_entry)?;
            validate_admitted_origin_stream("copy_index", "source", source)
        }
        "clear_index" => {
            let index_name = validate_endpoint("clear_index", "index_name")?;
            validate_admitted_origin_stream("clear_index", "index_name", index_name)
        }
        _ => unreachable!("index preflight only receives index operations"),
    }
}

/// Apply a move-index replication op.
async fn apply_move_index_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let Some(source) = op_entry.payload.get("source").and_then(|v| v.as_str()) else {
        return Err(format!(
            "[REPL {}] move_index seq {} missing source field",
            tenant_id, op_entry.seq
        ));
    };
    let Some(destination) = op_entry.payload.get("destination").and_then(|v| v.as_str()) else {
        return Err(format!(
            "[REPL {}] move_index seq {} missing destination field",
            tenant_id, op_entry.seq
        ));
    };

    manager
        .move_index(source, destination)
        .await
        .map(|_| ())
        .map_err(|error| {
            format!(
                "[REPL {}] move_index seq {} failed ({} -> {}): {}",
                tenant_id, op_entry.seq, source, destination, error
            )
        })
}

pub(crate) struct ScopedJsonFileCopy<'a, F>
where
    F: FnOnce(&IndexManager, &str),
{
    pub manager: &'a IndexManager,
    pub tenant_id: &'a str,
    pub seq: u64,
    pub destination: &'a str,
    pub payload: &'a Value,
    pub payload_key: &'a str,
    pub filename: &'a str,
    pub invalidate_cache: F,
}

/// Copy JSON file payload to a destination tenant index file and invalidate cache.
pub(crate) fn copy_scoped_json_file<F>(copy: ScopedJsonFileCopy<'_, F>) -> Result<(), String>
where
    F: FnOnce(&IndexManager, &str),
{
    let ScopedJsonFileCopy {
        manager,
        tenant_id,
        seq,
        destination,
        payload,
        payload_key,
        filename,
        invalidate_cache,
    } = copy;

    let destination_path = manager.base_path.join(destination).join(filename);

    match serde_json::to_vec(payload) {
        Ok(bytes) => {
            flapjack::index::atomic_write_file(&destination_path, &bytes).map_err(|error| {
                format!(
                    "[REPL {}] copy_index seq {} failed to write destination {} for {}: {}",
                    tenant_id, seq, filename, destination, error
                )
            })?;
            invalidate_cache(manager, destination);
            Ok(())
        }
        Err(error) => Err(format!(
            "[REPL {}] copy_index seq {} failed to serialize {} payload for {}: {}",
            tenant_id, seq, payload_key, destination, error
        )),
    }
}

/// Apply a copy-index replication op, including indexed scope payload handling.
async fn apply_copy_index_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let source = copy_index_endpoint(tenant_id, op_entry, "source")?;
    let destination = copy_index_endpoint(tenant_id, op_entry, "destination")?;
    let scope = parse_copy_scope(tenant_id, op_entry)?;

    manager
        .copy_index(source, destination, scope.as_deref())
        .await
        .map_err(|error| {
            format!(
                "[REPL {}] copy_index seq {} failed ({} -> {}): {}",
                tenant_id, op_entry.seq, source, destination, error
            )
        })?;

    if scope_includes(scope.as_deref(), "settings") {
        copy_scoped_payload_if_present(
            manager,
            tenant_id,
            op_entry,
            destination,
            "source_settings",
            "settings.json",
            |index_manager, index_name| index_manager.invalidate_settings_cache(index_name),
        )?;
    }

    if scope_includes(scope.as_deref(), "synonyms") {
        copy_scoped_payload_if_present(
            manager,
            tenant_id,
            op_entry,
            destination,
            "source_synonyms",
            "synonyms.json",
            |index_manager, index_name| index_manager.invalidate_synonyms_cache(index_name),
        )?;
    }

    if scope_includes(scope.as_deref(), "rules") {
        copy_scoped_payload_if_present(
            manager,
            tenant_id,
            op_entry,
            destination,
            "source_rules",
            "rules.json",
            |index_manager, index_name| index_manager.invalidate_rules_cache(index_name),
        )?;
    }

    Ok(())
}

/// Extracts a string field (source or destination index name) from a copy/move operation payload, returning an error if missing.
fn copy_index_endpoint<'a>(
    tenant_id: &str,
    op_entry: &'a OpLogEntry,
    field_name: &str,
) -> Result<&'a str, String> {
    op_entry
        .payload
        .get(field_name)
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            format!(
                "[REPL {}] copy_index seq {} missing {} field",
                tenant_id, op_entry.seq, field_name
            )
        })
}

/// Parses the optional `scope` field from a copy operation payload.
pub(super) fn parse_copy_scope(
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<Option<Vec<String>>, String> {
    let Some(scope_value) = op_entry.payload.get("scope") else {
        return Ok(None);
    };
    if scope_value.is_null() {
        return Ok(None);
    }

    serde_json::from_value(scope_value.clone())
        .map(Some)
        .map_err(|error| {
            format!(
                "[REPL {}] copy_index seq {} has invalid scope payload: {}",
                tenant_id, op_entry.seq, error
            )
        })
}

fn scope_includes(scope: Option<&[String]>, field_name: &str) -> bool {
    match scope {
        Some(values) => values.iter().any(|value| value == field_name),
        None => true,
    }
}

/// Copies a scoped data file (rules, synonyms, etc.) from the operation payload to the destination index directory, invalidating the relevant cache.
fn copy_scoped_payload_if_present<F>(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
    destination: &str,
    payload_key: &str,
    filename: &str,
    invalidate_cache: F,
) -> Result<(), String>
where
    F: FnOnce(&IndexManager, &str),
{
    let Some(payload) = op_entry
        .payload
        .get(payload_key)
        .filter(|value| !value.is_null())
    else {
        return Ok(());
    };

    copy_scoped_json_file(ScopedJsonFileCopy {
        manager,
        tenant_id,
        seq: op_entry.seq,
        destination,
        payload,
        payload_key,
        filename,
        invalidate_cache,
    })
}

/// Apply a clear-index replication op.
async fn apply_clear_index_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let Some(index_name) = op_entry.payload.get("index_name").and_then(|v| v.as_str()) else {
        return Err(format!(
            "[REPL {}] clear_index seq {} missing index_name field",
            tenant_id, op_entry.seq
        ));
    };

    validate_index_name(index_name).map_err(|error| {
        format!(
            "[REPL {}] clear_index seq {} invalid index_name '{}': {}",
            tenant_id, op_entry.seq, index_name, error
        )
    })?;

    let index_path = manager.base_path.join(index_name);
    let settings_path = index_path.join("settings.json");
    let relevance_path = index_path.join("relevance.json");

    let settings = if settings_path.exists() {
        Some(std::fs::read(&settings_path).map_err(|error| {
            format!(
                "[REPL {}] clear_index seq {} failed to read settings for {}: {}",
                tenant_id, op_entry.seq, index_name, error
            )
        })?)
    } else {
        None
    };
    let relevance = if relevance_path.exists() {
        Some(std::fs::read(&relevance_path).map_err(|error| {
            format!(
                "[REPL {}] clear_index seq {} failed to read relevance for {}: {}",
                tenant_id, op_entry.seq, index_name, error
            )
        })?)
    } else {
        None
    };

    manager
        .delete_tenant(&index_name.to_string())
        .await
        .map_err(|error| {
            format!(
                "[REPL {}] clear_index seq {} delete_tenant failed for {}: {}",
                tenant_id, op_entry.seq, index_name, error
            )
        })?;

    manager.create_tenant(index_name).map_err(|error| {
        format!(
            "[REPL {}] clear_index seq {} create_tenant failed for {}: {}",
            tenant_id, op_entry.seq, index_name, error
        )
    })?;

    if let Some(data) = settings {
        flapjack::index::atomic_write_file(&settings_path, &data).map_err(|error| {
            format!(
                "[REPL {}] clear_index seq {} failed to restore settings for {}: {}",
                tenant_id, op_entry.seq, index_name, error
            )
        })?;
        manager.invalidate_settings_cache(index_name);
        manager.invalidate_facet_cache(index_name);
    }

    if let Some(data) = relevance {
        flapjack::index::atomic_write_file(&relevance_path, &data).map_err(|error| {
            format!(
                "[REPL {}] clear_index seq {} failed to restore relevance for {}: {}",
                tenant_id, op_entry.seq, index_name, error
            )
        })?;
    }

    Ok(())
}

#[cfg(test)]
#[path = "index_ops_tests.rs"]
mod tests;
