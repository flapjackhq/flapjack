use super::{hard_entry, ReportCode, ReportResource, TranslationReportEntry};
use super::{
    resolve_rule_schema, resolve_source_field, resolve_source_schema, Disposition, ResourceKind,
    RuleSchemaPath,
};
use serde_json::Value;

pub(super) fn validate_rule_page(
    page_index: usize,
    page: &[Value],
    entries: &mut Vec<TranslationReportEntry>,
) {
    for (item_index, rule) in page.iter().enumerate() {
        validate_rule_payload(rule, page_index, item_index, entries);
    }
}

fn validate_rule_payload(
    rule: &Value,
    page_index: usize,
    item_index: usize,
    entries: &mut Vec<TranslationReportEntry>,
) {
    let Some(rule_object) = rule.as_object() else {
        entries.push(hard_entry(
            ReportCode::MalformedRulePayload,
            ReportResource::Rule,
            Some(page_index),
            Some(item_index),
            "$",
        ));
        return;
    };

    for key in rule_object.keys() {
        let row = resolve_source_field(ResourceKind::Rule, key);
        if row.disposition == Disposition::Rejected {
            entries.push(hard_entry(
                ReportCode::UnsupportedSourceField,
                ReportResource::Rule,
                Some(page_index),
                Some(item_index),
                &field_path(key),
            ));
        }
    }

    if let Some(conditions) = rule_object.get("conditions").and_then(Value::as_array) {
        for (condition_index, condition) in conditions.iter().enumerate() {
            validate_rule_schema_value(
                RuleSchemaPath::Condition,
                condition,
                page_index,
                item_index,
                &format!("$.conditions[{condition_index}]"),
                entries,
            );
        }
    }
    if let Some(validity) = rule_object.get("validity").and_then(Value::as_array) {
        for (validity_index, time_range) in validity.iter().enumerate() {
            validate_rule_schema_value(
                RuleSchemaPath::TimeRange,
                time_range,
                page_index,
                item_index,
                &format!("$.validity[{validity_index}]"),
                entries,
            );
        }
    }
    if let Some(consequence) = rule_object.get("consequence") {
        validate_consequence_schema(consequence, page_index, item_index, entries);
    }
}

fn validate_consequence_schema(
    consequence: &Value,
    page_index: usize,
    item_index: usize,
    entries: &mut Vec<TranslationReportEntry>,
) {
    if !consequence.is_object() {
        return;
    }
    validate_rule_schema_value(
        RuleSchemaPath::Consequence,
        consequence,
        page_index,
        item_index,
        "$.consequence",
        entries,
    );

    let Some(consequence_object) = consequence.as_object() else {
        return;
    };
    validate_rule_schema_array(
        consequence_object.get("promote"),
        RuleSchemaPath::Promote,
        page_index,
        item_index,
        "$.consequence.promote",
        entries,
    );
    validate_rule_schema_array(
        consequence_object.get("hide"),
        RuleSchemaPath::Hide,
        page_index,
        item_index,
        "$.consequence.hide",
        entries,
    );
    if let Some(params) = consequence_object.get("params") {
        validate_consequence_params_schema(params, page_index, item_index, entries);
    }
}

fn validate_consequence_params_schema(
    params: &Value,
    page_index: usize,
    item_index: usize,
    entries: &mut Vec<TranslationReportEntry>,
) {
    if !params.is_object() {
        return;
    }
    validate_rule_schema_value(
        RuleSchemaPath::ConsequenceParams,
        params,
        page_index,
        item_index,
        "$.consequence.params",
        entries,
    );

    let Some(params_object) = params.as_object() else {
        return;
    };
    for (key, path) in [
        (
            "automaticFacetFilters",
            "$.consequence.params.automaticFacetFilters",
        ),
        (
            "automaticOptionalFacetFilters",
            "$.consequence.params.automaticOptionalFacetFilters",
        ),
    ] {
        validate_rule_schema_array(
            params_object.get(key),
            RuleSchemaPath::AutomaticFacetFilter,
            page_index,
            item_index,
            path,
            entries,
        );
    }
    if let Some(query) = params_object.get("query") {
        validate_rule_schema_value(
            RuleSchemaPath::ConsequenceQuery,
            query,
            page_index,
            item_index,
            "$.consequence.params.query",
            entries,
        );
        if let Some(edits) = query
            .as_object()
            .and_then(|query_object| query_object.get("edits"))
        {
            validate_rule_schema_array(
                Some(edits),
                RuleSchemaPath::QueryEdit,
                page_index,
                item_index,
                "$.consequence.params.query.edits",
                entries,
            );
        }
    }
}

fn validate_rule_schema_array(
    values: Option<&Value>,
    path: RuleSchemaPath,
    page_index: usize,
    item_index: usize,
    json_path: &str,
    entries: &mut Vec<TranslationReportEntry>,
) {
    let Some(values) = values.and_then(Value::as_array) else {
        return;
    };
    for (value_index, value) in values.iter().enumerate() {
        validate_rule_schema_value(
            path,
            value,
            page_index,
            item_index,
            &format!("{json_path}[{value_index}]"),
            entries,
        );
    }
}

fn validate_rule_schema_value(
    path: RuleSchemaPath,
    value: &Value,
    page_index: usize,
    item_index: usize,
    json_path: &str,
    entries: &mut Vec<TranslationReportEntry>,
) {
    let row = resolve_rule_schema(path, value);
    if row.disposition == Disposition::Rejected {
        entries.push(hard_entry(
            ReportCode::UnsupportedRuleSchema,
            ReportResource::Rule,
            Some(page_index),
            Some(item_index),
            json_path,
        ));
    }
}

pub(super) fn validate_synonym_page(
    page_index: usize,
    page: &[Value],
    entries: &mut Vec<TranslationReportEntry>,
) {
    for (item_index, synonym) in page.iter().enumerate() {
        let row = resolve_source_schema(ResourceKind::Synonym, synonym);
        if row.disposition == Disposition::Rejected {
            entries.push(hard_entry(
                ReportCode::UnsupportedSynonymSchema,
                ReportResource::Synonym,
                Some(page_index),
                Some(item_index),
                "$",
            ));
        }
    }
}

fn field_path(field: &str) -> String {
    format!("$.{field}")
}
