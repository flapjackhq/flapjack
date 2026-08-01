use super::translation_report::{
    warning_entry, ReportCode, ReportResource, TranslationReportEntry,
};
use serde_json::{Map, Value};

const UNMIGRATED_SETTINGS: [&str; 6] = [
    "dictionary",
    "facetSearch",
    "nonSeparatorTokens",
    "proximityPrecision",
    "sortableAttributes",
    "stopWords",
];

pub(super) struct NormalizedMeilisearchSettings {
    pub(super) value: Value,
    pub(super) warnings: Vec<TranslationReportEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MeilisearchSettingsError {
    pub(super) json_path: String,
}

pub(super) fn normalize_meilisearch_settings(
    raw: &Value,
) -> Result<NormalizedMeilisearchSettings, MeilisearchSettingsError> {
    let source = raw.as_object().ok_or_else(|| settings_error("$"))?;
    reject_unknown_fields(source)?;

    let mut normalized = Map::new();
    copy_string_array(
        source,
        &mut normalized,
        "displayedAttributes",
        "attributesToRetrieve",
    )?;
    copy_string_array(
        source,
        &mut normalized,
        "searchableAttributes",
        "searchableAttributes",
    )?;
    copy_string_array(
        source,
        &mut normalized,
        "filterableAttributes",
        "attributesForFaceting",
    )?;
    copy_ranking_rules(source, &mut normalized)?;
    copy_pagination(source, &mut normalized)?;
    copy_faceting(source, &mut normalized)?;
    copy_typo_tolerance(source, &mut normalized)?;
    copy_optional_string(
        source,
        &mut normalized,
        "distinctAttribute",
        "attributeForDistinct",
    )?;
    copy_separator_tokens(source, &mut normalized)?;
    validate_no_embedder(source)?;
    validate_known_noop_fields(source)?;

    let mut warnings = vec![
        warning(
            ReportCode::MeilisearchDocumentOrderNotContractual,
            "$.documents",
        ),
        warning(
            ReportCode::MeilisearchSearchPaginationNotExportBound,
            "$.pagination",
        ),
    ];
    warnings.extend(
        UNMIGRATED_SETTINGS
            .into_iter()
            .filter(|field| source.get(*field).is_some_and(has_semantic_value))
            .map(|field| {
                warning(
                    ReportCode::MeilisearchSettingNotMigrated,
                    &format!("$.{field}"),
                )
            }),
    );
    append_normalization_warnings(source, &mut warnings)?;

    Ok(NormalizedMeilisearchSettings {
        value: Value::Object(normalized),
        warnings,
    })
}

fn reject_unknown_fields(source: &Map<String, Value>) -> Result<(), MeilisearchSettingsError> {
    const KNOWN_FIELDS: [&str; 18] = [
        "dictionary",
        "displayedAttributes",
        "distinctAttribute",
        "embedders",
        "faceting",
        "facetSearch",
        "filterableAttributes",
        "localizedAttributes",
        "nonSeparatorTokens",
        "pagination",
        "proximityPrecision",
        "rankingRules",
        "searchableAttributes",
        "separatorTokens",
        "sortableAttributes",
        "stopWords",
        "synonyms",
        "typoTolerance",
    ];
    if let Some(unknown) = source
        .keys()
        .find(|field| !KNOWN_FIELDS.contains(&field.as_str()))
    {
        return Err(settings_error(&format!("$.{unknown}")));
    }
    Ok(())
}

fn copy_string_array(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    source_field: &str,
    target_field: &str,
) -> Result<(), MeilisearchSettingsError> {
    let Some(value) = source.get(source_field) else {
        return Ok(());
    };
    target.insert(
        target_field.to_string(),
        Value::Array(string_array(value, &format!("$.{source_field}"))?),
    );
    Ok(())
}

fn copy_ranking_rules(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
) -> Result<(), MeilisearchSettingsError> {
    let Some(value) = source.get("rankingRules") else {
        return Ok(());
    };
    let rules = string_array(value, "$.rankingRules")?;
    let mapped = rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            let rule = rule.as_str().expect("string_array returns strings");
            let target_rule = match rule {
                "words" | "typo" | "proximity" | "attribute" => rule,
                "sort" => "custom",
                "exactness" => "exact",
                _ => return Err(settings_error(&format!("$.rankingRules[{index}]"))),
            };
            Ok(Value::String(target_rule.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    target.insert("ranking".to_string(), Value::Array(mapped));
    Ok(())
}

fn copy_pagination(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
) -> Result<(), MeilisearchSettingsError> {
    let Some(value) = source.get("pagination") else {
        return Ok(());
    };
    let pagination = exact_object(value, "$.pagination", &["maxTotalHits"])?;
    if let Some(limit) = pagination.get("maxTotalHits") {
        target.insert(
            "paginationLimitedTo".to_string(),
            positive_u32(limit, "$.pagination.maxTotalHits")?,
        );
    }
    Ok(())
}

fn copy_faceting(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
) -> Result<(), MeilisearchSettingsError> {
    let Some(value) = source.get("faceting") else {
        return Ok(());
    };
    let faceting = exact_object(
        value,
        "$.faceting",
        &["maxValuesPerFacet", "sortFacetValuesBy"],
    )?;
    if let Some(limit) = faceting.get("maxValuesPerFacet") {
        target.insert(
            "maxValuesPerFacet".to_string(),
            positive_u32(limit, "$.faceting.maxValuesPerFacet")?,
        );
    }
    if let Some(sort) = faceting.get("sortFacetValuesBy") {
        let sort = exact_object(sort, "$.faceting.sortFacetValuesBy", &["*"])?;
        if sort.get("*").and_then(Value::as_str) != Some("alpha") {
            return Err(settings_error("$.faceting.sortFacetValuesBy.*"));
        }
        target.insert(
            "sortFacetValuesBy".to_string(),
            Value::String("alpha".to_string()),
        );
    }
    Ok(())
}

fn copy_typo_tolerance(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
) -> Result<(), MeilisearchSettingsError> {
    let Some(value) = source.get("typoTolerance") else {
        return Ok(());
    };
    let typo = exact_object(
        value,
        "$.typoTolerance",
        &[
            "disableOnAttributes",
            "disableOnNumbers",
            "disableOnWords",
            "enabled",
            "minWordSizeForTypos",
        ],
    )?;
    if typo
        .get("enabled")
        .is_some_and(|value| value != &Value::Bool(true))
    {
        return Err(settings_error("$.typoTolerance.enabled"));
    }
    if typo.get("disableOnNumbers") == Some(&Value::Bool(true)) {
        return Err(settings_error("$.typoTolerance.disableOnNumbers"));
    }
    if let Some(value) = typo.get("disableOnNumbers") {
        value
            .as_bool()
            .ok_or_else(|| settings_error("$.typoTolerance.disableOnNumbers"))?;
    }
    copy_typo_thresholds(typo, target)?;
    copy_lowercase_string_array(
        typo,
        target,
        "disableOnWords",
        "disableTypoToleranceOnWords",
    )?;
    copy_string_array(
        typo,
        target,
        "disableOnAttributes",
        "disableTypoToleranceOnAttributes",
    )?;
    Ok(())
}

fn copy_typo_thresholds(
    typo: &Map<String, Value>,
    target: &mut Map<String, Value>,
) -> Result<(), MeilisearchSettingsError> {
    let Some(value) = typo.get("minWordSizeForTypos") else {
        return Ok(());
    };
    let thresholds = exact_object(
        value,
        "$.typoTolerance.minWordSizeForTypos",
        &["oneTypo", "twoTypos"],
    )?;
    for (source_field, target_field) in [
        ("oneTypo", "minWordSizefor1Typo"),
        ("twoTypos", "minWordSizefor2Typos"),
    ] {
        if let Some(value) = thresholds.get(source_field) {
            target.insert(
                target_field.to_string(),
                positive_u32(
                    value,
                    &format!("$.typoTolerance.minWordSizeForTypos.{source_field}"),
                )?,
            );
        }
    }
    Ok(())
}

fn copy_optional_string(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    source_field: &str,
    target_field: &str,
) -> Result<(), MeilisearchSettingsError> {
    let Some(value) = source.get(source_field) else {
        return Ok(());
    };
    let value = value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| settings_error(&format!("$.{source_field}")))?;
    target.insert(target_field.to_string(), Value::String(value.to_string()));
    Ok(())
}

fn copy_separator_tokens(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
) -> Result<(), MeilisearchSettingsError> {
    let Some(value) = source.get("separatorTokens") else {
        return Ok(());
    };
    let tokens = string_array(value, "$.separatorTokens")?;
    if tokens.iter().any(|token| {
        token
            .as_str()
            .expect("string_array returns strings")
            .chars()
            .count()
            != 1
    }) {
        return Err(settings_error("$.separatorTokens"));
    }
    let separators = tokens.iter().filter_map(Value::as_str).collect::<String>();
    target.insert("separatorsToIndex".to_string(), Value::String(separators));
    Ok(())
}

fn copy_lowercase_string_array(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    source_field: &str,
    target_field: &str,
) -> Result<(), MeilisearchSettingsError> {
    let values = source
        .get(source_field)
        .map(|value| string_array(value, &format!("$.typoTolerance.{source_field}")))
        .transpose()?;
    if let Some(values) = values {
        target.insert(
            target_field.to_string(),
            Value::Array(
                values
                    .into_iter()
                    .map(|value| Value::String(value.as_str().unwrap().to_lowercase()))
                    .collect(),
            ),
        );
    }
    Ok(())
}

fn validate_no_embedder(source: &Map<String, Value>) -> Result<(), MeilisearchSettingsError> {
    if source
        .get("embedders")
        .is_some_and(|value| value.as_object().is_none_or(|value| !value.is_empty()))
    {
        return Err(settings_error("$.embedders"));
    }
    Ok(())
}

fn validate_known_noop_fields(source: &Map<String, Value>) -> Result<(), MeilisearchSettingsError> {
    if source
        .get("localizedAttributes")
        .is_some_and(|value| !value.is_null())
    {
        return Err(settings_error("$.localizedAttributes"));
    }
    if let Some(value) = source.get("facetSearch") {
        value
            .as_bool()
            .ok_or_else(|| settings_error("$.facetSearch"))?;
    }
    if let Some(value) = source.get("synonyms") {
        value
            .as_object()
            .ok_or_else(|| settings_error("$.synonyms"))?;
    }
    Ok(())
}

fn append_normalization_warnings(
    source: &Map<String, Value>,
    warnings: &mut Vec<TranslationReportEntry>,
) -> Result<(), MeilisearchSettingsError> {
    let Some(words) = source
        .get("typoTolerance")
        .and_then(Value::as_object)
        .and_then(|typo| typo.get("disableOnWords"))
    else {
        return Ok(());
    };
    for (index, word) in string_array(words, "$.typoTolerance.disableOnWords")?
        .iter()
        .enumerate()
    {
        let word = word.as_str().expect("string_array returns strings");
        if word != word.to_lowercase() {
            warnings.push(warning(
                ReportCode::MeilisearchSettingValueNormalized,
                &format!("$.typoTolerance.disableOnWords[{index}]"),
            ));
        }
    }
    Ok(())
}

fn string_array(value: &Value, path: &str) -> Result<Vec<Value>, MeilisearchSettingsError> {
    value
        .as_array()
        .ok_or_else(|| settings_error(path))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(|value| Value::String(value.to_string()))
                .ok_or_else(|| settings_error(path))
        })
        .collect()
}

fn exact_object<'a>(
    value: &'a Value,
    path: &str,
    fields: &[&str],
) -> Result<&'a Map<String, Value>, MeilisearchSettingsError> {
    let object = value.as_object().ok_or_else(|| settings_error(path))?;
    if let Some(unknown) = object
        .keys()
        .find(|field| !fields.contains(&field.as_str()))
    {
        return Err(settings_error(&format!("{path}.{unknown}")));
    }
    Ok(object)
}

fn positive_u32(value: &Value, path: &str) -> Result<Value, MeilisearchSettingsError> {
    let value = value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| settings_error(path))?;
    Ok(Value::from(value))
}

fn has_semantic_value(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
        _ => true,
    }
}

fn warning(code: ReportCode, path: &str) -> TranslationReportEntry {
    warning_entry(code, ReportResource::Settings, None, None, path)
}

fn settings_error(path: &str) -> MeilisearchSettingsError {
    MeilisearchSettingsError {
        json_path: path.to_string(),
    }
}
