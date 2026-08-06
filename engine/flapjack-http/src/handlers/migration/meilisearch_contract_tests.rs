use super::source_reader::{accept_source_export, MeilisearchSourceReader};
use super::source_test_support::{
    meilisearch_observation, RecordingSink, ScriptedMeilisearchSource,
};
use super::translation::{
    translate_settings_for_provider, warning_message, ReportCode, SettingsSourceProvider,
    TranslationReportEntry,
};
use super::*;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;

#[path = "meilisearch_contract_tests/ranking_rules.rs"]
mod ranking_rules;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/2026_07_26_m0a_meilisearch_source_contract")
}

fn receipt_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../docs2/4_EVIDENCE/2026_07_26_m0a_meilisearch_kat_receipt.md")
}

fn read_fixture_json(name: &str) -> Value {
    let path = fixture_dir().join(name);
    serde_json::from_slice(
        &std::fs::read(&path)
            .unwrap_or_else(|error| panic!("fixture {} must be readable: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("fixture {} must be JSON: {error}", path.display()))
}

fn expected_bundle() -> Value {
    read_fixture_json("expected_bundle.json")
}

fn string_array(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("oracle value must be an array")
        .iter()
        .map(|item| {
            item.as_str()
                .expect("oracle array item must be a string")
                .to_string()
        })
        .collect()
}

fn assert_provider_advisories(warnings: &[TranslationReportEntry]) {
    assert_eq!(
        warnings
            .iter()
            .map(|warning| (warning.code, warning.json_path.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                ReportCode::MeilisearchDocumentOrderNotContractual,
                "$.documents",
            ),
            (
                ReportCode::MeilisearchSearchPaginationNotExportBound,
                "$.pagination",
            ),
        ],
        "provider-wide advisories must survive an unrelated setting rejection"
    );
}

fn stable_ids(documents: &Value, primary_key: &str) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    for document in documents
        .as_array()
        .ok_or_else(|| "document fixture must be an array".to_string())?
    {
        let id = document
            .get(primary_key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("document is missing string primary key {primary_key}"))?;
        if !seen.insert(id.to_string()) {
            return Err(format!("duplicate stable ID {id}"));
        }
        ids.push(id.to_string());
    }
    Ok(ids)
}

fn assert_exact_stable_ids(documents: &Value, primary_key: &str, expected: &[&str]) {
    assert_eq!(
        stable_ids(documents, primary_key).unwrap(),
        expected,
        "{primary_key} stable IDs drifted"
    );
}

fn assert_source_hash_pair(bundle: &Value, before: &str, after: &str) {
    assert_eq!(bundle["documents"]["hashBefore"], before);
    assert_eq!(bundle["documents"]["hashAfter"], after);
    assert_ne!(
        bundle["documents"]["hashBefore"], bundle["documents"]["hashAfter"],
        "source mutation hash must distinguish before/after captures"
    );
}

fn receipt_bullets_after(heading: &str) -> Vec<String> {
    let receipt = std::fs::read_to_string(receipt_path()).expect("M0AR receipt must be readable");
    let mut lines = receipt
        .lines()
        .skip_while(|line| line.trim() != heading)
        .skip(1);
    let mut bullets = Vec::new();
    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed.is_empty() && bullets.is_empty() {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("## ") {
            break;
        }
        if let Some(bullet) = trimmed.strip_prefix("- ") {
            bullets.push(bullet.trim_end_matches('.').to_string());
        } else if let Some(last) = bullets.last_mut() {
            last.push(' ');
            last.push_str(trimmed.trim_end_matches('.'));
        }
    }
    bullets
}

fn assert_serialized_omits_source_canaries(value: &Value) {
    let serialized = serde_json::to_string(value).unwrap();
    assert!(!serialized.contains("http://127.0.0.1:17747"));
    assert!(!serialized.contains("meili-master-key"));
    assert!(!serialized.contains("meili-source-key-canary"));
}

fn reject_meilisearch_admission_error(status: StatusCode, body: Value) -> ! {
    assert_eq!(body["code"], SOURCE_PROVIDER_UNSUPPORTED_CODE);
    assert_eq!(body["message"], SOURCE_PROVIDER_UNSUPPORTED_MESSAGE);
    assert_serialized_omits_source_canaries(&body);
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "current unsupported-provider diagnostic status drifted"
    );
    panic!(
        "semantic RED: valid Meilisearch fixture is still refused before source factory; \
         status={status}, body={body}"
    );
}

#[test]
fn m0ar_fixture_pins_exact_primary_key_identity_and_hash_guards() {
    let bundle = expected_bundle();
    let configured = read_fixture_json("configured_primary_key_documents.json");
    let inferred = read_fixture_json("inferred_primary_key_documents.json");
    let ambiguous = read_fixture_json("ambiguous_primary_key_documents.json");

    assert_eq!(bundle["indexes"]["configured"]["uid"], "configured_pk");
    assert_eq!(bundle["indexes"]["configured"]["primaryKey"], "sku");
    assert_eq!(bundle["expectedPrimaryKeys"]["configured_pk"], "sku");
    assert_eq!(bundle["documents"]["countBefore"], 3);
    assert_exact_stable_ids(&configured, "sku", &["SKU-001", "SKU-002", "SKU-003"]);

    assert_eq!(bundle["indexes"]["inferred"]["uid"], "inferred_pk");
    assert_eq!(bundle["indexes"]["inferred"]["primaryKey"], "book_id");
    assert_eq!(bundle["expectedPrimaryKeys"]["inferred_pk"], "book_id");
    assert_eq!(inferred.as_array().unwrap().len(), 2);
    assert_exact_stable_ids(&inferred, "book_id", &["B-001", "B-002"]);

    assert_eq!(bundle["indexes"]["ambiguous"]["uid"], "ambiguous_pk");
    assert!(bundle["indexes"]["ambiguous"]["primaryKey"].is_null());
    assert!(bundle["expectedPrimaryKeys"]["ambiguous_pk"].is_null());
    assert_eq!(ambiguous.as_array().unwrap().len(), 1);
    assert!(
        stable_ids(&ambiguous, "objectID").is_err(),
        "ambiguous_pk must be refused instead of inventing a stable ID"
    );

    let mut duplicate = configured.clone();
    duplicate[1]["sku"] = json!("SKU-001");
    assert!(
        stable_ids(&duplicate, "sku").is_err(),
        "duplicate stable IDs must fail the oracle guard"
    );

    let mut dropped = configured.clone();
    dropped.as_array_mut().unwrap().pop();
    assert_ne!(
        stable_ids(&dropped, "sku").unwrap(),
        vec!["SKU-001", "SKU-002", "SKU-003"],
        "dropped IDs must change the exact stable ID contract"
    );

    let mut wrong_id = configured.clone();
    wrong_id[2]["sku"] = json!("SKU-999");
    assert_ne!(
        stable_ids(&wrong_id, "sku").unwrap(),
        vec!["SKU-001", "SKU-002", "SKU-003"],
        "wrong IDs must change the exact stable ID contract"
    );

    assert_source_hash_pair(
        &bundle,
        "130f0e4b01a3029917f5bbc0ce5930fafd352c8943cd0ffbe4119c386fd62b3e",
        "1a4dce2fa85fabbf33e398065faee733df2f3d0afae9f401d4170d3df9614c52",
    );
    let mut stale_after_hash = bundle.clone();
    let before_hash = stale_after_hash["documents"]["hashBefore"].clone();
    stale_after_hash["documents"]["hashAfter"] = before_hash;
    assert_eq!(
        stale_after_hash["documents"]["hashBefore"], stale_after_hash["documents"]["hashAfter"],
        "mutation specimen proves the source-hash guard would fail if inverted"
    );
}

#[test]
fn m0ar_receipt_pins_exact_classification_and_warning_contracts() {
    let bundle = expected_bundle();

    assert_eq!(
        receipt_bullets_after("Supported exact rows:"),
        vec![
            "Index discovery",
            "Configured primary key",
            "Inferred primary key",
            "Single document fetch",
            "Settings inventory",
            "Display/search/filter/sort/ranking settings",
            "Synonyms",
            "Vector/semantic settings when no embedder is configured",
            "Task success/failure/quiescence",
            "Least-privilege action probes",
            "Version identity",
            "Health",
            "Dumps",
            "Snapshots",
            "Public mutation markers",
        ]
    );
    assert_eq!(
        string_array(&bundle["warningIdentifiers"]),
        vec![
            "meili_primary_key_ambiguous_candidates",
            "meili_document_order_not_contractual",
            "meili_search_pagination_bound_not_document_export_bound",
            "meili_setting_value_normalized",
            "meili_trailing_slash_redirect_unknown",
        ]
    );
    assert_eq!(
        receipt_bullets_after("Unsupported by this M0A gate:"),
        vec![
            "Experimental chat, prefix search, search cutoff, and localized-field migration semantics",
            "Large dump/snapshot staging disk sizing",
        ]
    );
    assert_eq!(bundle["tasks"]["dump"]["type"], "dumpCreation");
    assert_eq!(bundle["tasks"]["snapshot"]["type"], "snapshotCreation");
    assert_eq!(bundle["tasks"]["taskPollLimit"], 120);
    assert_eq!(
        string_array(&bundle["tasks"]["terminalStatuses"]),
        vec!["succeeded", "failed", "canceled"]
    );

    for code in string_array(&bundle["warningIdentifiers"]) {
        assert!(
            code.starts_with("meili_"),
            "{code} must preserve source-provider attribution"
        );
    }
}

#[test]
fn meilisearch_settings_map_only_proved_fields_with_applicable_warnings() {
    let raw = read_fixture_json("configured_primary_key_settings.json");
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    let translated = translate_settings_for_provider(
        &raw,
        SettingsSourceProvider::Meilisearch,
        &mut failures,
        &mut warnings,
    )
    .expect("the exact M0AR settings fixture should translate");

    assert!(failures.is_empty());
    assert_eq!(
        translated.attributes_to_retrieve,
        Some(vec![
            "sku".to_string(),
            "title".to_string(),
            "category".to_string(),
            "price".to_string(),
            "color".to_string(),
            "rank".to_string(),
        ])
    );
    assert_eq!(
        translated.searchable_attributes,
        Some(vec!["title".to_string(), "category".to_string()])
    );
    assert_eq!(
        translated.attributes_for_faceting,
        vec![
            "category".to_string(),
            "color".to_string(),
            "price".to_string()
        ]
    );
    assert_eq!(
        translated.ranking,
        Some(vec![
            "words".to_string(),
            "typo".to_string(),
            "proximity".to_string(),
            "attribute".to_string(),
            "custom".to_string(),
            "exact".to_string(),
        ])
    );
    assert_eq!(translated.pagination_limited_to, 50);
    assert_eq!(translated.max_values_per_facet, 25);
    assert_eq!(translated.sort_facet_values_by.as_deref(), Some("alpha"));
    assert_eq!(translated.min_word_size_for_1_typo, 5);
    assert_eq!(translated.min_word_size_for_2_typos, 9);
    assert_eq!(
        translated.disable_typo_tolerance_on_words,
        Some(vec!["sku".to_string()])
    );
    assert_eq!(
        translated.disable_typo_tolerance_on_attributes,
        Some(vec!["sku".to_string()])
    );
    assert_eq!(translated.attribute_for_distinct.as_deref(), Some("sku"));
    assert_eq!(translated.separators_to_index, "-");

    let warning_contract = warnings
        .iter()
        .map(|warning| {
            (
                warning.code,
                warning.json_path.as_str(),
                warning_message(warning.code),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        warning_contract,
        vec![
            (
                ReportCode::MeilisearchDocumentOrderNotContractual,
                "$.documents",
                Some("Meilisearch document order is not contractual; stable IDs preserve source identity.")
            ),
            (
                ReportCode::MeilisearchSearchPaginationNotExportBound,
                "$.pagination",
                Some("Meilisearch search pagination limits do not bound document export traversal.")
            ),
            (
                ReportCode::MeilisearchSettingNotMigrated,
                "$.dictionary",
                Some("Meilisearch setting has no proven Flapjack equivalent and was not migrated.")
            ),
            (
                ReportCode::MeilisearchSettingNotMigrated,
                "$.nonSeparatorTokens",
                Some("Meilisearch setting has no proven Flapjack equivalent and was not migrated.")
            ),
            (
                ReportCode::MeilisearchSettingNotMigrated,
                "$.proximityPrecision",
                Some("Meilisearch setting has no proven Flapjack equivalent and was not migrated.")
            ),
            (
                ReportCode::MeilisearchSettingNotMigrated,
                "$.sortableAttributes",
                Some("Meilisearch setting has no proven Flapjack equivalent and was not migrated.")
            ),
            (
                ReportCode::MeilisearchSettingNotMigrated,
                "$.stopWords",
                Some("Meilisearch setting has no proven Flapjack equivalent and was not migrated.")
            ),
            (
                ReportCode::MeilisearchSettingValueNormalized,
                "$.typoTolerance.disableOnWords[0]",
                Some("Meilisearch setting value was normalized to the proven Flapjack representation.")
            ),
        ]
    );
}

// Live specimen captured directly from the pinned image
// `getmeili/meilisearch@sha256:9694a59d...` (pkgVersion 1.50.0) by GETting
// `/indexes/configured_pk/settings` after applying the M0A fixture PATCH. The
// GET response, unlike the PATCH body in the fixture file, always carries the
// search-runtime defaults `facetSearch`, `prefixSearch`, and `searchCutoffMs`
// and lowercases `typoTolerance.disableOnWords`. Preview translation receives
// exactly this payload, so this is the known answer the live KAT proves.
#[test]
fn meilisearch_live_default_settings_specimen_accepts_prefix_search_and_cutoff() {
    let live_specimen = json!({
        "displayedAttributes": ["sku", "title", "category", "price", "color", "rank"],
        "searchableAttributes": ["title", "category"],
        "filterableAttributes": ["category", "color", "price"],
        "sortableAttributes": ["price", "rank"],
        "rankingRules": ["words", "typo", "proximity", "attributeRank", "sort", "wordPosition", "exactness"],
        "stopWords": ["the"],
        "nonSeparatorTokens": ["_"],
        "separatorTokens": ["-"],
        "dictionary": ["flapjack"],
        "synonyms": {"saw": ["cutter"], "wrench": ["spanner"]},
        "distinctAttribute": "sku",
        "proximityPrecision": "byWord",
        "typoTolerance": {
            "enabled": true,
            "minWordSizeForTypos": {"oneTypo": 5, "twoTypos": 9},
            "disableOnWords": ["sku"],
            "disableOnAttributes": ["sku"],
            "disableOnNumbers": false
        },
        "faceting": {"maxValuesPerFacet": 25, "sortFacetValuesBy": {"*": "alpha"}},
        "pagination": {"maxTotalHits": 50},
        "embedders": {},
        "searchCutoffMs": Value::Null,
        "localizedAttributes": Value::Null,
        "facetSearch": true,
        "prefixSearch": "indexingTime"
    });

    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let translated = translate_settings_for_provider(
        &live_specimen,
        SettingsSourceProvider::Meilisearch,
        &mut failures,
        &mut warnings,
    )
    .expect("the live Meilisearch default settings specimen must translate");

    assert!(failures.is_empty());
    // The proven fields still map exactly as before; admitting the new defaults
    // must not perturb the migrated output.
    assert_eq!(translated.pagination_limited_to, 50);
    assert_eq!(
        translated.disable_typo_tolerance_on_words,
        Some(vec!["sku".to_string()])
    );

    // Exact warning contract for the live GET response: `searchCutoffMs` is null
    // (no semantic value, so no warning) and `disableOnWords` is already
    // lowercase (no normalization warning), while `prefixSearch`/`facetSearch`
    // and the lossy default `wordPosition` fold are surfaced as unmigrated.
    // This is the settings portion the live KAT asserts inside the full preview
    // code vector.
    let warning_contract = warnings
        .iter()
        .map(|warning| (warning.code, warning.json_path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        warning_contract,
        vec![
            (
                ReportCode::MeilisearchDocumentOrderNotContractual,
                "$.documents"
            ),
            (
                ReportCode::MeilisearchSearchPaginationNotExportBound,
                "$.pagination"
            ),
            (
                ReportCode::MeilisearchSettingNotMigrated,
                "$.rankingRules[5]"
            ),
            (ReportCode::MeilisearchSettingNotMigrated, "$.dictionary"),
            (ReportCode::MeilisearchSettingNotMigrated, "$.facetSearch"),
            (
                ReportCode::MeilisearchSettingNotMigrated,
                "$.nonSeparatorTokens"
            ),
            (ReportCode::MeilisearchSettingNotMigrated, "$.prefixSearch"),
            (
                ReportCode::MeilisearchSettingNotMigrated,
                "$.proximityPrecision"
            ),
            (
                ReportCode::MeilisearchSettingNotMigrated,
                "$.sortableAttributes"
            ),
            (ReportCode::MeilisearchSettingNotMigrated, "$.stopWords"),
        ]
    );
}

// The pinned probe container (Meilisearch 1.50.0) returns a full default
// settings object for a fresh index, with explicit `null` for unset scalars and
// the 1.50 split ranking rules. This is the exact `/settings` GET body the live
// probe migrates; it must translate with zero hard rejections or the migration
// lands no data. Byte-copied from the probe's staged settings artifact so it
// stays a real specimen, not a hand-approximated one.
#[test]
fn meilisearch_1_50_live_default_settings_translate_without_hard_rejection() {
    let live_defaults = json!({
        "dictionary": [],
        "displayedAttributes": ["*"],
        "distinctAttribute": Value::Null,
        "embedders": {},
        "facetSearch": true,
        "faceting": {"maxValuesPerFacet": 100, "sortFacetValuesBy": {"*": "alpha"}},
        "filterableAttributes": [],
        "localizedAttributes": Value::Null,
        "nonSeparatorTokens": [],
        "pagination": {"maxTotalHits": 1000},
        "prefixSearch": "indexingTime",
        "proximityPrecision": "byWord",
        "rankingRules": [
            "words", "typo", "proximity", "attributeRank", "sort", "wordPosition", "exactness"
        ],
        "searchCutoffMs": Value::Null,
        "searchableAttributes": ["*"],
        "separatorTokens": [],
        "sortableAttributes": [],
        "stopWords": [],
        "typoTolerance": {
            "disableOnAttributes": [],
            "disableOnNumbers": false,
            "disableOnWords": [],
            "enabled": true,
            "minWordSizeForTypos": {"oneTypo": 5, "twoTypos": 9}
        }
    });

    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let translated = translate_settings_for_provider(
        &live_defaults,
        SettingsSourceProvider::Meilisearch,
        &mut failures,
        &mut warnings,
    )
    .expect("live Meilisearch 1.50 default settings must translate without a hard rejection");

    assert!(
        failures.is_empty(),
        "unexpected hard rejections: {failures:?}"
    );
    assert_eq!(
        translated.ranking,
        Some(vec![
            "words".to_string(),
            "typo".to_string(),
            "proximity".to_string(),
            "attribute".to_string(),
            "custom".to_string(),
            "exact".to_string(),
        ]),
    );
    // A null `distinctAttribute` means "no distinct attribute", so it must not
    // surface as `attributeForDistinct` in the translated payload.
    assert_eq!(translated.attribute_for_distinct, None);
    assert_eq!(translated.pagination_limited_to, 1000);
}

// Meilisearch >=1.50 (the pinned probe container is 1.50.0) splits the pre-1.50
// `attribute` ranking rule into `attributeRank` (which attribute matched) and
// `wordPosition` (position of matched words within the attribute). Flapjack can
// preserve `attributeRank` under its `attribute` criterion but has no separate
// `wordPosition` criterion, so the known vendor default remains admissible only
// with an explicit lossy warning at the folded source rule.
#[test]
fn meilisearch_1_50_default_ranking_rules_fold_attribute_split_to_algolia_attribute() {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let translated = translate_settings_for_provider(
        &json!({
            "rankingRules": [
                "words", "typo", "proximity", "attributeRank", "sort", "wordPosition", "exactness"
            ]
        }),
        SettingsSourceProvider::Meilisearch,
        &mut failures,
        &mut warnings,
    )
    .expect("Meilisearch 1.50 default ranking rules must translate");

    assert!(failures.is_empty(), "unexpected failures: {failures:?}");
    assert_eq!(
        translated.ranking,
        Some(vec![
            "words".to_string(),
            "typo".to_string(),
            "proximity".to_string(),
            "attribute".to_string(),
            "custom".to_string(),
            "exact".to_string(),
        ]),
    );
    assert!(
        warnings.iter().any(|warning| {
            warning.code == ReportCode::MeilisearchSettingNotMigrated
                && warning.json_path == "$.rankingRules[5]"
        }),
        "default wordPosition fold must be explicit: {warnings:?}"
    );
}

// Admitting `prefixSearch`/`searchCutoffMs` must not weaken fail-closed
// rejection: a value outside the vendor enum/type is still a MalformedSettings
// failure at the offending path, not a silently accepted unknown field.
#[test]
fn meilisearch_prefix_search_and_cutoff_reject_out_of_contract_values() {
    for (payload, expected_path) in [
        (json!({"prefixSearch": "onDemand"}), "$.prefixSearch"),
        (json!({"prefixSearch": true}), "$.prefixSearch"),
        (json!({"searchCutoffMs": "150"}), "$.searchCutoffMs"),
        (json!({"searchCutoffMs": -5}), "$.searchCutoffMs"),
    ] {
        let mut failures = Vec::new();
        let mut warnings = Vec::new();
        assert!(
            translate_settings_for_provider(
                &payload,
                SettingsSourceProvider::Meilisearch,
                &mut failures,
                &mut warnings,
            )
            .is_none(),
            "out-of-contract {expected_path} must fail closed"
        );
        assert_eq!(failures.len(), 1);
        let failure = format!("{:?}", failures[0]);
        assert!(failure.contains("MalformedSettingsPayload"));
        assert!(
            failure.contains(expected_path),
            "failure {failure} must name {expected_path}"
        );
        assert_provider_advisories(&warnings);
    }
}

#[test]
fn meilisearch_hard_rejection_preserves_provider_advisory_warnings() {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    assert!(
        translate_settings_for_provider(
            &json!({"typoTolerance": {"disableOnNumbers": true}}),
            SettingsSourceProvider::Meilisearch,
            &mut failures,
            &mut warnings,
        )
        .is_none(),
        "disableOnNumbers=true must remain a hard rejection"
    );
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].json_path, "$.typoTolerance.disableOnNumbers");
    assert_provider_advisories(&warnings);
}

#[test]
fn meilisearch_hard_rejection_preserves_known_unmigrated_setting_warnings() {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();

    assert!(
        translate_settings_for_provider(
            &json!({
                "dictionary": ["flapjack"],
                "embedders": {"default": {"source": "userProvided"}}
            }),
            SettingsSourceProvider::Meilisearch,
            &mut failures,
            &mut warnings,
        )
        .is_none(),
        "an unrelated hard rejection must not discard already-known warnings"
    );
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].json_path, "$.embedders");
    assert_eq!(
        warnings
            .iter()
            .map(|warning| (warning.code, warning.json_path.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                ReportCode::MeilisearchDocumentOrderNotContractual,
                "$.documents",
            ),
            (
                ReportCode::MeilisearchSearchPaginationNotExportBound,
                "$.pagination",
            ),
            (ReportCode::MeilisearchSettingNotMigrated, "$.dictionary"),
        ]
    );
}

#[test]
fn meilisearch_unmapped_and_malformed_settings_are_explicit() {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let translated = translate_settings_for_provider(
        &json!({"facetSearch": true}),
        SettingsSourceProvider::Meilisearch,
        &mut failures,
        &mut warnings,
    )
    .expect("a valid unmapped setting should produce a warning");

    assert!(failures.is_empty());
    assert!(translated.searchable_attributes.is_none());
    assert!(translated.attributes_for_faceting.is_empty());
    assert!(warnings.iter().any(|warning| {
        warning.code == ReportCode::MeilisearchSettingNotMigrated
            && warning.json_path == "$.facetSearch"
    }));

    for (payload, expected_path) in [
        (
            json!({"embedders": {"default": {"source": "userProvided"}}}),
            "$.embedders",
        ),
        (json!({"rankingRules": ["unproved"]}), "$.rankingRules[0]"),
        (json!({"paginationLimitedTo": 50}), "$.paginationLimitedTo"),
        (
            json!({"typoTolerance": {"enabled": false}}),
            "$.typoTolerance.enabled",
        ),
    ] {
        failures.clear();
        warnings.clear();
        assert!(
            translate_settings_for_provider(
                &payload,
                SettingsSourceProvider::Meilisearch,
                &mut failures,
                &mut warnings,
            )
            .is_none(),
            "unsupported semantic settings must fail closed"
        );
        assert_eq!(failures.len(), 1);
        let failure = format!("{:?}", failures[0]);
        assert!(failure.contains("MalformedSettingsPayload"));
        assert!(failure.contains(expected_path));
        assert_provider_advisories(&warnings);
    }
}

#[test]
fn meilisearch_warning_only_settings_reject_malformed_values() {
    for (payload, expected_path) in [
        (json!({"dictionary": true}), "$.dictionary"),
        (
            json!({"nonSeparatorTokens": ["_",""]}),
            "$.nonSeparatorTokens",
        ),
        (json!({"proximityPrecision": 5}), "$.proximityPrecision"),
        (
            json!({"proximityPrecision": "unsupported"}),
            "$.proximityPrecision",
        ),
        (
            json!({"sortableAttributes": {"price": "asc"}}),
            "$.sortableAttributes",
        ),
        (json!({"stopWords": [true]}), "$.stopWords"),
    ] {
        let mut failures = Vec::new();
        let mut warnings = Vec::new();
        assert!(
            translate_settings_for_provider(
                &payload,
                SettingsSourceProvider::Meilisearch,
                &mut failures,
                &mut warnings,
            )
            .is_none(),
            "malformed warning-only field {expected_path} must fail closed"
        );
        assert_eq!(failures.len(), 1);
        let failure = format!("{:?}", failures[0]);
        assert!(failure.contains("MalformedSettingsPayload"));
        assert!(
            failure.contains(expected_path),
            "failure {failure} must name {expected_path}"
        );
        assert_provider_advisories(&warnings);
    }
}

#[test]
fn provider_settings_seam_preserves_existing_algolia_translation() {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let translated = translate_settings_for_provider(
        &json!({"searchableAttributes": ["title"], "paginationLimitedTo": 75}),
        SettingsSourceProvider::Algolia,
        &mut failures,
        &mut warnings,
    )
    .expect("the provider seam must preserve Algolia translation");

    assert!(failures.is_empty());
    assert!(warnings.is_empty());
    assert_eq!(
        translated.searchable_attributes,
        Some(vec!["title".to_string()])
    );
    assert_eq!(translated.pagination_limited_to, 75);
}

#[test]
#[should_panic(expected = "current unsupported-provider diagnostic status drifted")]
fn accepted_unsupported_error_cannot_satisfy_meilisearch_admission_contract() {
    let (_, Json(body)) = source_provider_unsupported();
    reject_meilisearch_admission_error(StatusCode::ACCEPTED, body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_meilisearch_fixture_admission_reaches_provider_neutral_snapshot_contract() {
    let bundle = expected_bundle();
    let documents = bundle["documents"]["beforeMutation"]
        .as_array()
        .unwrap()
        .to_vec();
    let pages = vec![documents[..2].to_vec(), documents[2..].to_vec()];
    let source = ScriptedMeilisearchSource::with_passes(
        meilisearch_observation("configured_pk", "sku", 3),
        bundle["settings"].clone(),
        vec![pages.clone(), pages],
    );
    let mut reader = MeilisearchSourceReader::from_source("configured_pk", source);
    let mut sink = RecordingSink::default();

    let accepted = accept_source_export(
        AsyncMigrationSourceProvider::Meilisearch,
        &mut reader,
        &mut sink,
    )
    .await
    .expect("the production adapter must satisfy the shared snapshot contract");

    assert_eq!(accepted.identity().document_metadata_count(), 3);
    assert_eq!(accepted.identity().snapshot().documents.count, 3);
    assert_eq!(accepted.identity().snapshot().rules.count, 0);
    assert_eq!(accepted.identity().snapshot().synonyms.count, 2);
    assert_eq!(
        sink.document_pages,
        vec![
            vec!["SKU-001".to_string(), "SKU-002".to_string()],
            vec!["SKU-003".to_string()]
        ]
    );
    // The neutral capture keeps the source payload verbatim and carries the
    // normalized identity beside it as the stable ID.
    assert_eq!(sink.raw_document_pages[0][0]["stableId"], "SKU-001");
    assert_eq!(sink.raw_document_pages[0][0]["payload"]["sku"], "SKU-001");
    assert_eq!(
        sink.raw_document_pages[0][0]["payload"]["objectID"],
        Value::Null
    );
    assert_eq!(
        sink.synonym_pages,
        vec![vec![
            "meilisearch:synonym:saw".to_string(),
            "meilisearch:synonym:wrench".to_string()
        ]]
    );
    assert_eq!(
        sink.raw_synonym_pages[0][0],
        json!({"stableId": "meilisearch:synonym:saw", "payload": {"saw": ["cutter"]}})
    );
    assert_eq!(
        sink.raw_synonym_pages[0][1],
        json!({"stableId": "meilisearch:synonym:wrench", "payload": {"wrench": ["spanner"]}})
    );
    assert!(sink.raw_rule_pages.concat().is_empty());

    assert_serialized_omits_source_canaries(&serde_json::to_value(&sink.settings).unwrap());
    let debug = format!("{reader:?}\n{:?}", accepted.identity());
    assert!(!debug.contains("http://127.0.0.1:17747"));
    assert!(!debug.contains("meili-master-key"));

    ensure_source_provider_supported(AsyncMigrationSourceProvider::Meilisearch)
        .expect("Stage 3 admits Meilisearch through the shared source lifecycle");
}
