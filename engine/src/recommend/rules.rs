//! Storage layer for recommendation rules per index and model, supporting CRUD operations, batch upsert/delete, searching with pagination, and path traversal protection.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::index::manager::validate_index_name;

use super::VALID_MODELS;

pub(crate) const RECOMMEND_RULES_DIR: &str = "recommend_rules";

// ── DTOs ────────────────────────────────────────────────────────────────────

/// Represent a recommendation rule with optional conditions and consequences that customize results. Includes objectID (unique identifier), optional condition (filters and context), optional consequence (hide/promote actions and params), description, enabled flag, and operation for batch requests ('delete' removes instead of saving).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecommendRule {
    #[serde(rename = "objectID")]
    pub object_id: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<RecommendRuleCondition>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub consequence: Option<RecommendRuleConsequence>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(default = "default_true")]
    pub enabled: bool,

    /// When present in a batch request, indicates the operation to perform.
    /// `"delete"` removes the rule instead of saving it.
    #[serde(skip_serializing_if = "Option::is_none", rename = "_operation")]
    pub operation: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecommendRuleCondition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecommendRuleConsequence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide: Option<Vec<HideObject>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub promote: Option<Vec<PromoteObject>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<RecommendRuleParams>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HideObject {
    #[serde(rename = "objectID")]
    pub object_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PromoteObject {
    #[serde(rename = "objectID")]
    pub object_id: String,
    pub position: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecommendRuleParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automatic_facet_filters: Option<Vec<serde_json::Value>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_filters: Option<Vec<String>>,
}

// ── Storage ─────────────────────────────────────────────────────────────────

/// Validates that a model name is one of the valid recommend models.
pub fn validate_model(model: &str) -> Result<(), String> {
    if VALID_MODELS.contains(&model) {
        Ok(())
    } else {
        Err(format!(
            "Invalid model '{}'. Must be one of: {}",
            model,
            VALID_MODELS.join(", ")
        ))
    }
}

/// Returns the directory path for recommend rules storage for a given index and model.
/// Validates index_name to prevent path traversal attacks.
fn rules_dir(base_path: &Path, index_name: &str, model: &str) -> Result<PathBuf, String> {
    validate_index_name(index_name).map_err(|e| format!("Invalid index name: {e}"))?;
    validate_model(model)?;
    Ok(base_path
        .join(index_name)
        .join(RECOMMEND_RULES_DIR)
        .join(model))
}

/// Returns the file path for the rules JSON file.
fn rules_file(base_path: &Path, index_name: &str, model: &str) -> Result<PathBuf, String> {
    Ok(rules_dir(base_path, index_name, model)?.join("rules.json"))
}

/// Load all rules for a given index and model.
pub fn load_rules(
    base_path: &Path,
    index_name: &str,
    model: &str,
) -> Result<Vec<RecommendRule>, String> {
    let path = rules_file(base_path, index_name, model)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data =
        std::fs::read_to_string(&path).map_err(|e| format!("Failed to read rules file: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("Failed to parse rules file: {}", e))
}

/// Save all rules for a given index and model (overwrites existing).
fn save_rules(
    base_path: &Path,
    index_name: &str,
    model: &str,
    rules: &[RecommendRule],
) -> Result<(), String> {
    let dir = rules_dir(base_path, index_name, model)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create rules directory: {}", e))?;
    let path = dir.join("rules.json");
    let data = serde_json::to_string_pretty(rules)
        .map_err(|e| format!("Failed to serialize rules: {}", e))?;
    std::fs::write(&path, data).map_err(|e| format!("Failed to write rules file: {}", e))
}

/// Get a single rule by objectID.
pub fn get_rule(
    base_path: &Path,
    index_name: &str,
    model: &str,
    object_id: &str,
) -> Result<Option<RecommendRule>, String> {
    let rules = load_rules(base_path, index_name, model)?;
    Ok(rules.into_iter().find(|r| r.object_id == object_id))
}

/// Delete a single rule by objectID. Returns true if found and removed.
pub fn delete_rule(
    base_path: &Path,
    index_name: &str,
    model: &str,
    object_id: &str,
) -> Result<bool, String> {
    let mut rules = load_rules(base_path, index_name, model)?;
    let original_len = rules.len();
    rules.retain(|r| r.object_id != object_id);
    if rules.len() == original_len {
        return Ok(false);
    }
    save_rules(base_path, index_name, model, &rules)?;
    Ok(true)
}

/// Batch save rules. Supports `clear_existing` to wipe before saving.
/// Rules with `_operation: "delete"` are removed instead of saved.
pub fn save_rules_batch(
    base_path: &Path,
    index_name: &str,
    model: &str,
    incoming: Vec<RecommendRule>,
    clear_existing: bool,
) -> Result<(), String> {
    let mut rules = if clear_existing {
        Vec::new()
    } else {
        load_rules(base_path, index_name, model)?
    };

    for rule in incoming {
        if rule.operation.as_deref() == Some("delete") {
            rules.retain(|r| r.object_id != rule.object_id);
        } else {
            // Upsert: replace existing or append
            if let Some(pos) = rules.iter().position(|r| r.object_id == rule.object_id) {
                rules[pos] = rule;
            } else {
                rules.push(rule);
            }
        }
    }

    save_rules(base_path, index_name, model, &rules)
}

/// Search rules by substring match on objectID and description.
pub fn search_rules(
    base_path: &Path,
    index_name: &str,
    model: &str,
    query: &str,
    page: usize,
    hits_per_page: usize,
) -> Result<(Vec<RecommendRule>, usize), String> {
    let rules = load_rules(base_path, index_name, model)?;
    let query_lower = query.to_lowercase();

    let matched: Vec<RecommendRule> = if query.is_empty() {
        rules
    } else {
        rules
            .into_iter()
            .filter(|r| {
                r.object_id.to_lowercase().contains(&query_lower)
                    || r.description
                        .as_deref()
                        .map(|d| d.to_lowercase().contains(&query_lower))
                        .unwrap_or(false)
            })
            .collect()
    };

    let total = matched.len();
    let start = page * hits_per_page;
    let hits = if start >= total {
        Vec::new()
    } else {
        matched
            .into_iter()
            .skip(start)
            .take(hits_per_page)
            .collect()
    };

    Ok((hits, total))
}

/// Delete all rules for a given index and model.
pub fn clear_all_rules(base_path: &Path, index_name: &str, model: &str) -> Result<(), String> {
    save_rules(base_path, index_name, model, &[])
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a fully-populated RecommendRule with condition, consequence, and description for use in tests.
    fn sample_rule(id: &str) -> RecommendRule {
        RecommendRule {
            object_id: id.to_string(),
            condition: Some(RecommendRuleCondition {
                filters: Some("brand:Nike".to_string()),
                context: Some("homepage".to_string()),
            }),
            consequence: Some(RecommendRuleConsequence {
                hide: Some(vec![HideObject {
                    object_id: "prod-99".to_string(),
                }]),
                promote: Some(vec![PromoteObject {
                    object_id: "prod-1".to_string(),
                    position: 0,
                }]),
                params: Some(RecommendRuleParams {
                    automatic_facet_filters: None,
                    filters: Some("category:shoes".to_string()),
                    optional_filters: Some(vec!["color:red".to_string()]),
                }),
            }),
            description: Some("Pin Nike product for related-products".to_string()),
            enabled: true,
            operation: None,
        }
    }

    #[test]
    fn recommend_rule_serde_roundtrip() {
        let rule = sample_rule("rule-123");
        let json = serde_json::to_string_pretty(&rule).unwrap();
        let deserialized: RecommendRule = serde_json::from_str(&json).unwrap();
        assert_eq!(rule, deserialized);
    }

    #[test]
    fn recommend_rule_serde_minimal() {
        // Minimal rule — only objectID and enabled
        let json = r#"{"objectID": "minimal-rule"}"#;
        let rule: RecommendRule = serde_json::from_str(json).unwrap();
        assert_eq!(rule.object_id, "minimal-rule");
        assert!(rule.enabled); // defaults to true
        assert!(rule.condition.is_none());
        assert!(rule.consequence.is_none());
        assert!(rule.description.is_none());
    }

    #[test]
    fn recommend_rule_serde_with_operation() {
        let json = r#"{"objectID": "del-rule", "_operation": "delete"}"#;
        let rule: RecommendRule = serde_json::from_str(json).unwrap();
        assert_eq!(rule.operation.as_deref(), Some("delete"));
    }

    /// Verify that rules saved via batch can be retrieved individually by objectID, returning None if not found.
    #[test]
    fn save_and_get_rule() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        save_rules_batch(
            base,
            "my-index",
            "related-products",
            vec![sample_rule("rule-1"), sample_rule("rule-2")],
            false,
        )
        .unwrap();

        let found = get_rule(base, "my-index", "related-products", "rule-1").unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().object_id, "rule-1");

        let not_found = get_rule(base, "my-index", "related-products", "rule-999").unwrap();
        assert!(not_found.is_none());
    }

    /// Verify that delete_rule removes the specified rule, leaves others untouched, and returns false for non-existent rules.
    #[test]
    fn delete_rule_removes_it() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        save_rules_batch(
            base,
            "my-index",
            "related-products",
            vec![sample_rule("rule-1"), sample_rule("rule-2")],
            false,
        )
        .unwrap();

        let removed = delete_rule(base, "my-index", "related-products", "rule-1").unwrap();
        assert!(removed);

        let found = get_rule(base, "my-index", "related-products", "rule-1").unwrap();
        assert!(found.is_none());

        // rule-2 still there
        let found2 = get_rule(base, "my-index", "related-products", "rule-2").unwrap();
        assert!(found2.is_some());

        // deleting non-existent returns false
        let not_removed = delete_rule(base, "my-index", "related-products", "rule-999").unwrap();
        assert!(!not_removed);
    }

    /// Verify that batch saves support upserting existing rules, deleting via _operation='delete', and adding new rules in a single call.
    #[test]
    fn batch_upsert_and_delete_operations() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        // Initial batch
        save_rules_batch(
            base,
            "my-index",
            "related-products",
            vec![
                sample_rule("rule-1"),
                sample_rule("rule-2"),
                sample_rule("rule-3"),
            ],
            false,
        )
        .unwrap();

        // Second batch: update rule-1, delete rule-2, add rule-4
        let mut updated_rule = sample_rule("rule-1");
        updated_rule.description = Some("Updated description".to_string());

        let delete_rule_op = RecommendRule {
            object_id: "rule-2".to_string(),
            condition: None,
            consequence: None,
            description: None,
            enabled: true,
            operation: Some("delete".to_string()),
        };

        save_rules_batch(
            base,
            "my-index",
            "related-products",
            vec![updated_rule, delete_rule_op, sample_rule("rule-4")],
            false,
        )
        .unwrap();

        // Verify
        let r1 = get_rule(base, "my-index", "related-products", "rule-1")
            .unwrap()
            .unwrap();
        assert_eq!(r1.description.as_deref(), Some("Updated description"));

        let r2 = get_rule(base, "my-index", "related-products", "rule-2").unwrap();
        assert!(r2.is_none(), "rule-2 should be deleted");

        let r3 = get_rule(base, "my-index", "related-products", "rule-3").unwrap();
        assert!(r3.is_some(), "rule-3 untouched");

        let r4 = get_rule(base, "my-index", "related-products", "rule-4").unwrap();
        assert!(r4.is_some(), "rule-4 added");
    }

    /// Verify that batch saves with clear_existing=true remove all previous rules before applying new ones.
    #[test]
    fn batch_with_clear_existing() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        // Save initial rules
        save_rules_batch(
            base,
            "my-index",
            "related-products",
            vec![sample_rule("old-1"), sample_rule("old-2")],
            false,
        )
        .unwrap();

        // Batch with clear_existing = true
        save_rules_batch(
            base,
            "my-index",
            "related-products",
            vec![sample_rule("new-1")],
            true,
        )
        .unwrap();

        let old1 = get_rule(base, "my-index", "related-products", "old-1").unwrap();
        assert!(old1.is_none(), "old rules should be cleared");

        let new1 = get_rule(base, "my-index", "related-products", "new-1").unwrap();
        assert!(new1.is_some());
    }

    /// Verify that search_rules correctly matches rules by substring in both objectID and description fields.
    #[test]
    fn search_rules_by_query() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        let mut r1 = sample_rule("nike-rule");
        r1.description = Some("Nike product promotion".to_string());

        let mut r2 = sample_rule("adidas-rule");
        r2.description = Some("Adidas product promotion".to_string());

        let mut r3 = sample_rule("general-rule");
        r3.description = Some("General boost".to_string());

        save_rules_batch(
            base,
            "my-index",
            "related-products",
            vec![r1, r2, r3],
            false,
        )
        .unwrap();

        // Empty query returns all
        let (hits, total) = search_rules(base, "my-index", "related-products", "", 0, 10).unwrap();
        assert_eq!(total, 3);
        assert_eq!(hits.len(), 3);

        // Search by description
        let (hits, total) =
            search_rules(base, "my-index", "related-products", "Nike", 0, 10).unwrap();
        assert_eq!(total, 1);
        assert_eq!(hits[0].object_id, "nike-rule");

        // Search by objectID
        let (hits, total) =
            search_rules(base, "my-index", "related-products", "adidas", 0, 10).unwrap();
        assert_eq!(total, 1);
        assert_eq!(hits[0].object_id, "adidas-rule");
    }

    /// Verify that search_rules correctly returns paginated results with accurate total count and handles page boundary conditions.
    #[test]
    fn search_rules_pagination() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        let rules: Vec<RecommendRule> = (0..5)
            .map(|i| sample_rule(&format!("rule-{}", i)))
            .collect();

        save_rules_batch(base, "my-index", "related-products", rules, false).unwrap();

        // Page 0, 2 per page
        let (hits, total) = search_rules(base, "my-index", "related-products", "", 0, 2).unwrap();
        assert_eq!(total, 5);
        assert_eq!(hits.len(), 2);

        // Page 1
        let (hits, _) = search_rules(base, "my-index", "related-products", "", 1, 2).unwrap();
        assert_eq!(hits.len(), 2);

        // Page 2 (last page, 1 item)
        let (hits, _) = search_rules(base, "my-index", "related-products", "", 2, 2).unwrap();
        assert_eq!(hits.len(), 1);

        // Page 3 (beyond)
        let (hits, _) = search_rules(base, "my-index", "related-products", "", 3, 2).unwrap();
        assert!(hits.is_empty());
    }

    /// Verify that rules for different models (related-products, bought-together, etc.) remain isolated from each other.
    #[test]
    fn rules_are_model_scoped() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        save_rules_batch(
            base,
            "my-index",
            "related-products",
            vec![sample_rule("rp-rule")],
            false,
        )
        .unwrap();

        save_rules_batch(
            base,
            "my-index",
            "bought-together",
            vec![sample_rule("bt-rule")],
            false,
        )
        .unwrap();

        // Each model's rules are isolated
        let rp = get_rule(base, "my-index", "related-products", "rp-rule").unwrap();
        assert!(rp.is_some());
        let rp_bt = get_rule(base, "my-index", "related-products", "bt-rule").unwrap();
        assert!(rp_bt.is_none());

        let bt = get_rule(base, "my-index", "bought-together", "bt-rule").unwrap();
        assert!(bt.is_some());
        let bt_rp = get_rule(base, "my-index", "bought-together", "rp-rule").unwrap();
        assert!(bt_rp.is_none());
    }

    #[test]
    fn validate_model_accepts_valid_rejects_invalid() {
        assert!(validate_model("related-products").is_ok());
        assert!(validate_model("bought-together").is_ok());
        assert!(validate_model("trending-items").is_ok());
        assert!(validate_model("trending-facets").is_ok());
        assert!(validate_model("looking-similar").is_ok());
        assert!(validate_model("invalid-model").is_err());
        assert!(validate_model("").is_err());
    }

    #[test]
    fn get_rule_from_empty_index() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();
        let result = get_rule(base, "no-index", "related-products", "rule-1").unwrap();
        assert!(result.is_none());
    }

    /// Verify that storage functions reject path traversal attacks, including ../, backslashes, null bytes, and empty index names in all load/save/get/delete/search operations.
    #[test]
    fn path_traversal_rejected_by_storage() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path();

        // Path traversal in index_name must be rejected
        assert!(load_rules(base, "../etc", "related-products").is_err());
        assert!(load_rules(base, "foo/../../bar", "related-products").is_err());
        assert!(load_rules(base, "foo\\bar", "related-products").is_err());
        assert!(load_rules(base, "test\0name", "related-products").is_err());
        assert!(load_rules(base, "", "related-products").is_err());

        assert!(save_rules_batch(base, "../etc", "related-products", vec![], false).is_err());
        assert!(get_rule(base, "../etc", "related-products", "r1").is_err());
        assert!(delete_rule(base, "../etc", "related-products", "r1").is_err());
        assert!(search_rules(base, "../etc", "related-products", "", 0, 10).is_err());
    }
}
