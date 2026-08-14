use flapjack::index::oplog::OpLogEntry;
use flapjack::validate_index_name;
use flapjack::IndexManager;
use serde_json::Value;

/// Apply a move-index replication op.
pub(crate) async fn apply_move_index_op(
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
pub(crate) async fn apply_copy_index_op(
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
        );
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
        );
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
        );
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
) where
    F: FnOnce(&IndexManager, &str),
{
    let Some(payload) = op_entry
        .payload
        .get(payload_key)
        .filter(|value| !value.is_null())
    else {
        return;
    };

    if let Err(error) = copy_scoped_json_file(ScopedJsonFileCopy {
        manager,
        tenant_id,
        seq: op_entry.seq,
        destination,
        payload,
        payload_key,
        filename,
        invalidate_cache,
    }) {
        tracing::warn!("{}", error);
    }
}

/// Apply a clear-index replication op.
pub(crate) async fn apply_clear_index_op(
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
        std::fs::read(&settings_path).ok()
    } else {
        None
    };
    let relevance = if relevance_path.exists() {
        std::fs::read(&relevance_path).ok()
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
        if let Err(error) =
            flapjack::index::atomic_write_file(&settings_path, &data).map_err(|error| {
                format!(
                    "[REPL {}] clear_index seq {} failed to restore settings for {}: {}",
                    tenant_id, op_entry.seq, index_name, error
                )
            })
        {
            tracing::warn!("{}", error);
        }
    }

    if let Some(data) = relevance {
        if let Err(error) =
            flapjack::index::atomic_write_file(&relevance_path, &data).map_err(|error| {
                format!(
                    "[REPL {}] clear_index seq {} failed to restore relevance for {}: {}",
                    tenant_id, op_entry.seq, index_name, error
                )
            })
        {
            tracing::warn!("{}", error);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_index_entry(payload: serde_json::Value) -> OpLogEntry {
        OpLogEntry {
            seq: 9,
            timestamp_ms: 42,
            node_id: "node-a".to_string(),
            tenant_id: "tenant-a".to_string(),
            op_type: "copy_index".to_string(),
            payload,
        }
    }

    #[test]
    fn parse_copy_scope_defaults_to_none_when_missing_or_null() {
        let missing_scope = copy_index_entry(serde_json::json!({
            "source": "src",
            "destination": "dst"
        }));
        let null_scope = copy_index_entry(serde_json::json!({
            "source": "src",
            "destination": "dst",
            "scope": null
        }));

        assert_eq!(parse_copy_scope("tenant-a", &missing_scope).unwrap(), None);
        assert_eq!(parse_copy_scope("tenant-a", &null_scope).unwrap(), None);
    }

    #[test]
    fn parse_copy_scope_parses_string_list() {
        let scoped_entry = copy_index_entry(serde_json::json!({
            "source": "src",
            "destination": "dst",
            "scope": ["settings", "rules"]
        }));

        assert_eq!(
            parse_copy_scope("tenant-a", &scoped_entry).unwrap(),
            Some(vec!["settings".to_string(), "rules".to_string()])
        );
    }
}
