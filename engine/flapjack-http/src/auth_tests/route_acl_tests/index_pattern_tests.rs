//! Stub summary for index_pattern_tests.rs.
use super::super::*;

// ── index_pattern_matches ──

#[test]
fn index_pattern_empty_matches_all() {
    assert!(index_pattern_matches(&[], "anything"));
}

#[test]
fn index_pattern_exact_match() {
    let patterns = vec!["products".to_string()];
    assert!(index_pattern_matches(&patterns, "products"));
    assert!(!index_pattern_matches(&patterns, "users"));
}

#[test]
fn index_pattern_star_matches_all() {
    let patterns = vec!["*".to_string()];
    assert!(index_pattern_matches(&patterns, "anything"));
}

#[test]
fn index_pattern_prefix_wildcard() {
    let patterns = vec!["prod_*".to_string()];
    assert!(index_pattern_matches(&patterns, "prod_us"));
    assert!(index_pattern_matches(&patterns, "prod_eu"));
    assert!(!index_pattern_matches(&patterns, "dev_us"));
}

#[test]
fn index_pattern_suffix_wildcard() {
    let patterns = vec!["*_prod".to_string()];
    assert!(index_pattern_matches(&patterns, "us_prod"));
    assert!(!index_pattern_matches(&patterns, "us_dev"));
}

#[test]
fn index_pattern_contains_wildcard() {
    let patterns = vec!["*prod*".to_string()];
    assert!(index_pattern_matches(&patterns, "my_prod_index"));
    assert!(index_pattern_matches(&patterns, "production"));
    assert!(!index_pattern_matches(&patterns, "development"));
}

#[test]
fn index_pattern_multiple_any_match() {
    let patterns = vec!["products".to_string(), "users".to_string()];
    assert!(index_pattern_matches(&patterns, "products"));
    assert!(index_pattern_matches(&patterns, "users"));
    assert!(!index_pattern_matches(&patterns, "orders"));
}

#[test]
fn key_allows_index_with_no_restrictions_matches_any_index() {
    let api_key = test_search_api_key("unrestricted key");
    assert!(key_allows_index(&api_key, None, "products"));
}
/// TODO: Document key_allows_index_requires_parent_and_secured_restrictions_to_match.
#[test]
fn key_allows_index_requires_parent_and_secured_restrictions_to_match() {
    let mut api_key = test_search_api_key("restricted key");
    api_key.indexes = vec!["tenant_*".to_string()];
    let secured_restrictions = SecuredKeyRestrictions {
        restrict_indices: Some(vec!["tenant_public".to_string()]),
        ..Default::default()
    };

    assert!(key_allows_index(
        &api_key,
        Some(&secured_restrictions),
        "tenant_public"
    ));
    assert!(!key_allows_index(
        &api_key,
        Some(&secured_restrictions),
        "tenant_private"
    ));
    assert!(!key_allows_index(
        &api_key,
        Some(&secured_restrictions),
        "other_tenant"
    ));
}
