use super::super::index_resource_store::{
    clear_resource_store, delete_resource_item, save_resource_batch, save_resource_item,
};
use flapjack::index::oplog::OpLogEntry;
use flapjack::index::rules::{Rule, RuleStore};
use flapjack::index::synonyms::{Synonym, SynonymStore};
use flapjack::IndexManager;

pub(crate) fn preflight_resource_op(tenant_id: &str, op_entry: &OpLogEntry) -> Result<(), String> {
    match op_entry.op_type.as_str() {
        "save_synonym" => serde_json::from_value::<Synonym>(op_entry.payload.clone())
            .map(|_| ())
            .map_err(|error| {
                format!(
                    "[REPL {}] save_synonym seq {} invalid payload: {}",
                    tenant_id, op_entry.seq, error
                )
            }),
        "save_synonyms" => {
            if let Some(value) = op_entry.payload.get("replace") {
                if !value.is_boolean() {
                    return Err(format!(
                        "[REPL {}] save_synonyms seq {} invalid replace field",
                        tenant_id, op_entry.seq
                    ));
                }
            }
            let synonyms = op_entry.payload.get("synonyms").ok_or_else(|| {
                format!(
                    "[REPL {}] save_synonyms seq {} missing synonyms field",
                    tenant_id, op_entry.seq
                )
            })?;
            serde_json::from_value::<Vec<Synonym>>(synonyms.clone())
                .map(|_| ())
                .map_err(|error| {
                    format!(
                        "[REPL {}] save_synonyms seq {} invalid payload: {}",
                        tenant_id, op_entry.seq, error
                    )
                })
        }
        "delete_synonym" => preflight_resource_object_id(tenant_id, op_entry, "delete_synonym"),
        "clear_synonyms" | "clear_rules" => Ok(()),
        "save_rule" => serde_json::from_value::<Rule>(op_entry.payload.clone())
            .map(|_| ())
            .map_err(|error| {
                format!(
                    "[REPL {}] save_rule seq {} invalid payload: {}",
                    tenant_id, op_entry.seq, error
                )
            }),
        "save_rules" => {
            if let Some(value) = op_entry.payload.get("clearExisting") {
                if !value.is_boolean() {
                    return Err(format!(
                        "[REPL {}] save_rules seq {} invalid clearExisting field",
                        tenant_id, op_entry.seq
                    ));
                }
            }
            let rules = op_entry.payload.get("rules").ok_or_else(|| {
                format!(
                    "[REPL {}] save_rules seq {} missing rules field",
                    tenant_id, op_entry.seq
                )
            })?;
            serde_json::from_value::<Vec<Rule>>(rules.clone())
                .map(|_| ())
                .map_err(|error| {
                    format!(
                        "[REPL {}] save_rules seq {} invalid payload: {}",
                        tenant_id, op_entry.seq, error
                    )
                })
        }
        "delete_rule" => preflight_resource_object_id(tenant_id, op_entry, "delete_rule"),
        _ => unreachable!("resource preflight only receives resource operations"),
    }
}

fn preflight_resource_object_id(
    tenant_id: &str,
    op_entry: &OpLogEntry,
    operation: &str,
) -> Result<(), String> {
    op_entry
        .payload
        .get("objectID")
        .and_then(|value| value.as_str())
        .map(|_| ())
        .ok_or_else(|| {
            format!(
                "[REPL {}] {} seq {} missing objectID field",
                tenant_id, operation, op_entry.seq
            )
        })
}

fn synonym_save(manager: &IndexManager, tenant_id: &str, synonym: Synonym) -> Result<(), String> {
    save_resource_item::<SynonymStore>(manager, tenant_id, synonym).map_err(|e| e.to_string())
}

/// Applies a batch of synonym operations (add or replace-all) to a tenant index.
fn synonyms_batch(
    manager: &IndexManager,
    tenant_id: &str,
    synonyms: Vec<Synonym>,
    replace: bool,
) -> Result<(), String> {
    save_resource_batch::<SynonymStore, _>(manager, tenant_id, synonyms, replace)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn synonym_delete(manager: &IndexManager, tenant_id: &str, object_id: &str) -> Result<(), String> {
    delete_resource_item::<SynonymStore>(manager, tenant_id, object_id)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn synonyms_clear(manager: &IndexManager, tenant_id: &str) -> Result<(), String> {
    clear_resource_store::<SynonymStore>(manager, tenant_id).map_err(|e| e.to_string())
}

/// Dispatcher wrapper: parse payload and apply a single synonym save.
pub(crate) fn apply_save_synonym_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let synonym = serde_json::from_value::<Synonym>(op_entry.payload.clone()).map_err(|error| {
        format!(
            "[REPL {}] save_synonym seq {} invalid payload: {}",
            tenant_id, op_entry.seq, error
        )
    })?;
    synonym_save(manager, tenant_id, synonym).map_err(|error| {
        format!(
            "[REPL {}] save_synonym seq {} failed: {}",
            tenant_id, op_entry.seq, error
        )
    })
}

/// Dispatcher wrapper: parse payload and apply a batch synonym save.
pub(crate) fn apply_save_synonyms_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let replace = match op_entry.payload.get("replace") {
        Some(value) => value.as_bool().ok_or_else(|| {
            format!(
                "[REPL {}] save_synonyms seq {} invalid replace field",
                tenant_id, op_entry.seq
            )
        })?,
        None => false,
    };
    let synonyms_value = op_entry.payload.get("synonyms").ok_or_else(|| {
        format!(
            "[REPL {}] save_synonyms seq {} missing synonyms field",
            tenant_id, op_entry.seq
        )
    })?;
    let synonyms =
        serde_json::from_value::<Vec<Synonym>>(synonyms_value.clone()).map_err(|error| {
            format!(
                "[REPL {}] save_synonyms seq {} invalid payload: {}",
                tenant_id, op_entry.seq, error
            )
        })?;
    synonyms_batch(manager, tenant_id, synonyms, replace).map_err(|error| {
        format!(
            "[REPL {}] save_synonyms seq {} failed: {}",
            tenant_id, op_entry.seq, error
        )
    })
}

/// Dispatcher wrapper: extract objectID and delete a synonym.
pub(crate) fn apply_delete_synonym_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let object_id = op_entry
        .payload
        .get("objectID")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            format!(
                "[REPL {}] delete_synonym seq {} missing objectID field",
                tenant_id, op_entry.seq
            )
        })?;
    synonym_delete(manager, tenant_id, object_id).map_err(|error| {
        format!(
            "[REPL {}] delete_synonym seq {} failed: {}",
            tenant_id, op_entry.seq, error
        )
    })?;
    Ok(())
}

/// Dispatcher wrapper: clear all synonyms for a tenant.
pub(crate) fn apply_clear_synonyms_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    synonyms_clear(manager, tenant_id).map_err(|error| {
        format!(
            "[REPL {}] clear_synonyms seq {} failed: {}",
            tenant_id, op_entry.seq, error
        )
    })
}

fn rule_save(manager: &IndexManager, tenant_id: &str, rule: Rule) -> Result<(), String> {
    save_resource_item::<RuleStore>(manager, tenant_id, rule).map_err(|e| e.to_string())
}

/// Applies a batch of rule operations (add or replace-all) to a tenant index.
fn rules_batch(
    manager: &IndexManager,
    tenant_id: &str,
    rules: Vec<Rule>,
    clear_existing: bool,
) -> Result<(), String> {
    save_resource_batch::<RuleStore, _>(manager, tenant_id, rules, clear_existing)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn rule_delete(manager: &IndexManager, tenant_id: &str, object_id: &str) -> Result<(), String> {
    delete_resource_item::<RuleStore>(manager, tenant_id, object_id)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn rules_clear(manager: &IndexManager, tenant_id: &str) -> Result<(), String> {
    clear_resource_store::<RuleStore>(manager, tenant_id).map_err(|e| e.to_string())
}

/// Dispatcher wrapper: parse payload and apply a single rule save.
pub(crate) fn apply_save_rule_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let rule = serde_json::from_value::<Rule>(op_entry.payload.clone()).map_err(|error| {
        format!(
            "[REPL {}] save_rule seq {} invalid payload: {}",
            tenant_id, op_entry.seq, error
        )
    })?;
    rule_save(manager, tenant_id, rule).map_err(|error| {
        format!(
            "[REPL {}] save_rule seq {} failed: {}",
            tenant_id, op_entry.seq, error
        )
    })
}

/// Dispatcher wrapper: parse payload and apply a batch rule save.
pub(crate) fn apply_save_rules_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let clear_existing = match op_entry.payload.get("clearExisting") {
        Some(value) => value.as_bool().ok_or_else(|| {
            format!(
                "[REPL {}] save_rules seq {} invalid clearExisting field",
                tenant_id, op_entry.seq
            )
        })?,
        None => false,
    };
    let rules_value = op_entry.payload.get("rules").ok_or_else(|| {
        format!(
            "[REPL {}] save_rules seq {} missing rules field",
            tenant_id, op_entry.seq
        )
    })?;
    let rules = serde_json::from_value::<Vec<Rule>>(rules_value.clone()).map_err(|error| {
        format!(
            "[REPL {}] save_rules seq {} invalid payload: {}",
            tenant_id, op_entry.seq, error
        )
    })?;
    rules_batch(manager, tenant_id, rules, clear_existing).map_err(|error| {
        format!(
            "[REPL {}] save_rules seq {} failed: {}",
            tenant_id, op_entry.seq, error
        )
    })
}

/// Dispatcher wrapper: extract objectID and delete a rule.
pub(crate) fn apply_delete_rule_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    let object_id = op_entry
        .payload
        .get("objectID")
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            format!(
                "[REPL {}] delete_rule seq {} missing objectID field",
                tenant_id, op_entry.seq
            )
        })?;
    rule_delete(manager, tenant_id, object_id).map_err(|error| {
        format!(
            "[REPL {}] delete_rule seq {} failed: {}",
            tenant_id, op_entry.seq, error
        )
    })?;
    Ok(())
}

/// Dispatcher wrapper: clear all rules for a tenant.
pub(crate) fn apply_clear_rules_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
) -> Result<(), String> {
    rules_clear(manager, tenant_id).map_err(|error| {
        format!(
            "[REPL {}] clear_rules seq {} failed: {}",
            tenant_id, op_entry.seq, error
        )
    })
}
