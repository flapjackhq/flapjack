#![cfg_attr(not(test), allow(dead_code))]

#[path = "translation_bundle.rs"]
mod translation_bundle;
#[path = "translation_report.rs"]
mod translation_report;
#[path = "translation_schema.rs"]
mod translation_schema;
#[path = "translation_session.rs"]
mod translation_session;

pub(super) use self::translation_bundle::ReplicaSettingsTranslation;
use self::translation_bundle::TypedTranslationFailure;
use self::translation_report::{hard_entry, warning_entry, ReportCode, ReportResource};
pub(super) use self::translation_report::{
    warning_message, TranslationReport, TranslationReportEntry,
};
#[cfg(test)]
pub(super) use self::translation_session::translate_spool_input;
#[cfg_attr(not(test), allow(unused_imports))]
pub(super) use self::translation_session::{
    translate_accepted_spool_payload, translate_accepted_spool_settings, translate_spool_payload,
    SettingsTranslationOutcome, SpoolTranslationInput, TranslatedSpoolPayload, TranslationOutcome,
    TranslationSessionInstrumentation, TranslationStreamError,
};
use crate::handlers::settings::payload_merge::parse_distinct_value_strict;
use serde_json::Value;

#[cfg(test)]
use self::translation_report::ReportSeverity;

const FIELD_PRECEDENCE: u16 = 10;
const SCHEMA_PRECEDENCE: u16 = 20;
const FALLBACK_PRECEDENCE: u16 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ResourceKind {
    Settings,
    Document,
    Rule,
    Synonym,
}

fn push_typed_failure(entries: &mut Vec<TranslationReportEntry>, failure: TypedTranslationFailure) {
    let invalid_id_already_explains_failure = entries.iter().any(|entry| {
        entry.code == ReportCode::InvalidObjectId
            && entry.resource == failure.resource
            && entry.page_index == failure.page_index
            && entry.item_index == failure.item_index
    });
    if !invalid_id_already_explains_failure {
        push_unique_entry(entries, typed_failure_entry(failure));
    }
}

fn typed_failure_entry(failure: TypedTranslationFailure) -> TranslationReportEntry {
    hard_entry(
        failure.code,
        failure.resource,
        failure.page_index,
        failure.item_index,
        &failure.json_path,
    )
}

pub(super) fn push_unique_entry(
    entries: &mut Vec<TranslationReportEntry>,
    candidate: TranslationReportEntry,
) {
    if !entries.contains(&candidate) {
        entries.push(candidate);
    }
}

fn validate_settings_payload(settings: &Value, entries: &mut Vec<TranslationReportEntry>) {
    let Some(settings_object) = settings.as_object() else {
        entries.push(hard_entry(
            ReportCode::MalformedSettingsPayload,
            ReportResource::Settings,
            None,
            None,
            "$",
        ));
        return;
    };

    for (key, value) in settings_object {
        let row = resolve_source_field(ResourceKind::Settings, key);
        match row.disposition {
            Disposition::Rejected => entries.push(hard_entry(
                row.rejection_code
                    .unwrap_or(ReportCode::UnsupportedSourceField),
                ReportResource::Settings,
                None,
                None,
                &field_path(key),
            )),
            Disposition::Warned => {
                if let Some(code) = row.warning_code.map(report_code_for_warning) {
                    entries.push(warning_entry(
                        code,
                        ReportResource::Settings,
                        None,
                        None,
                        &field_path(key),
                    ));
                }
            }
            Disposition::Exact | Disposition::Transformed => {}
        }

        if key == "distinct" && parse_distinct_value_strict(Some(value.clone())).is_err() {
            entries.push(hard_entry(
                ReportCode::MalformedSettingsPayload,
                ReportResource::Settings,
                None,
                None,
                "$.distinct",
            ));
        }
    }
}

fn field_path(field: &str) -> String {
    format!("$.{field}")
}

fn report_code_for_warning(code: WarningCode) -> ReportCode {
    match code {
        WarningCode::PersistedNoBehaviorSetting => ReportCode::PersistedNoBehaviorSetting,
        WarningCode::ReadOnlySourceField => ReportCode::ReadOnlySourceField,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RuleSchemaPath {
    Condition,
    Consequence,
    ConsequenceParams,
    Promote,
    Hide,
    TimeRange,
    AutomaticFacetFilter,
    ConsequenceQuery,
    QueryEdit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SourceMatcher {
    Field(&'static str),
    DocumentAttribute,
    RuleSchema(RuleSchemaMatcher),
    SynonymSchema(SynonymSchemaMatcher),
    UnknownClosedSchema,
}

impl SourceMatcher {
    fn matches_field(self, resource: ResourceKind, field: &str) -> bool {
        match self {
            Self::Field(candidate) => candidate == field,
            Self::DocumentAttribute => resource == ResourceKind::Document,
            Self::UnknownClosedSchema => matches!(
                resource,
                ResourceKind::Settings | ResourceKind::Rule | ResourceKind::Synonym
            ),
            Self::RuleSchema(_) | Self::SynonymSchema(_) => false,
        }
    }

    fn matches_schema(self, resource: ResourceKind, source: &serde_json::Value) -> bool {
        match self {
            Self::SynonymSchema(matcher) if resource == ResourceKind::Synonym => {
                matcher.matches(source)
            }
            Self::UnknownClosedSchema => matches!(
                resource,
                ResourceKind::Settings | ResourceKind::Rule | ResourceKind::Synonym
            ),
            Self::Field(_)
            | Self::DocumentAttribute
            | Self::RuleSchema(_)
            | Self::SynonymSchema(_) => false,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn matches_rule_schema(self, path: RuleSchemaPath, source: &serde_json::Value) -> bool {
        match self {
            Self::RuleSchema(matcher) => matcher.matches(path, source),
            Self::UnknownClosedSchema => true,
            Self::Field(_) | Self::DocumentAttribute | Self::SynonymSchema(_) => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RuleSchemaMatcher {
    Condition,
    Consequence,
    ConsequenceParams,
    PromoteSingle,
    PromoteMultiple,
    Hide,
    TimeRange,
    AutomaticFacetFilter,
    ConsequenceQueryLiteral,
    ConsequenceQueryEdits,
    QueryEdit,
}

impl RuleSchemaMatcher {
    #[cfg_attr(not(test), allow(dead_code))]
    fn matches(self, path: RuleSchemaPath, source: &serde_json::Value) -> bool {
        if self == Self::ConsequenceQueryLiteral {
            return path == RuleSchemaPath::ConsequenceQuery && source.is_string();
        }

        if self == Self::AutomaticFacetFilter && source.is_string() {
            return path == RuleSchemaPath::AutomaticFacetFilter;
        }

        let Some((owner_path, fields)) = self.object_field_spec() else {
            return false;
        };
        path == owner_path && fields.matches(source)
    }

    fn object_field_spec(self) -> Option<(RuleSchemaPath, ClosedObjectFields)> {
        Some(match self {
            Self::Condition => (
                RuleSchemaPath::Condition,
                closed_fields(
                    &[],
                    &["pattern", "anchoring", "alternatives", "context", "filters"],
                ),
            ),
            Self::Consequence => (
                RuleSchemaPath::Consequence,
                closed_fields(
                    &[],
                    &["promote", "hide", "filterPromotes", "userData", "params"],
                ),
            ),
            Self::ConsequenceParams => (
                RuleSchemaPath::ConsequenceParams,
                closed_fields(&[], CONSEQUENCE_PARAM_FIELDS),
            ),
            Self::PromoteSingle => (
                RuleSchemaPath::Promote,
                closed_fields(&["objectID", "position"], &["objectID", "position"]),
            ),
            Self::PromoteMultiple => (
                RuleSchemaPath::Promote,
                closed_fields(&["objectIDs", "position"], &["objectIDs", "position"]),
            ),
            Self::Hide => (
                RuleSchemaPath::Hide,
                closed_fields(&["objectID"], &["objectID"]),
            ),
            Self::TimeRange => (
                RuleSchemaPath::TimeRange,
                closed_fields(&["from", "until"], &["from", "until"]),
            ),
            Self::AutomaticFacetFilter => (
                RuleSchemaPath::AutomaticFacetFilter,
                closed_fields(&["facet"], &["facet", "disjunctive", "score", "negative"]),
            ),
            Self::ConsequenceQueryEdits => (
                RuleSchemaPath::ConsequenceQuery,
                closed_fields(&[], &["remove", "edits"]),
            ),
            Self::QueryEdit => (
                RuleSchemaPath::QueryEdit,
                closed_fields(&["type", "delete"], &["type", "delete", "insert"]),
            ),
            Self::ConsequenceQueryLiteral => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SynonymSchemaMatcher {
    Regular,
    OneWay,
    AltCorrection1,
    AltCorrection2,
    Placeholder,
}

impl SynonymSchemaMatcher {
    fn matches(self, source: &serde_json::Value) -> bool {
        let (synonym_type, fields) = self.field_spec();
        object_string(source, "type") == Some(synonym_type) && fields.matches(source)
    }

    fn field_spec(self) -> (&'static str, ClosedObjectFields) {
        match self {
            Self::Regular => (
                "synonym",
                closed_fields(&["objectID", "synonyms"], &["objectID", "type", "synonyms"]),
            ),
            Self::OneWay => (
                "onewaysynonym",
                closed_fields(
                    &["objectID", "input", "synonyms"],
                    &["objectID", "type", "input", "synonyms"],
                ),
            ),
            Self::AltCorrection1 => (
                "altcorrection1",
                closed_fields(
                    &["objectID", "word", "corrections"],
                    &["objectID", "type", "word", "corrections"],
                ),
            ),
            Self::AltCorrection2 => (
                "altcorrection2",
                closed_fields(
                    &["objectID", "word", "corrections"],
                    &["objectID", "type", "word", "corrections"],
                ),
            ),
            Self::Placeholder => (
                "placeholder",
                closed_fields(
                    &["objectID", "placeholder", "replacements"],
                    &["objectID", "type", "placeholder", "replacements"],
                ),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ClosedObjectFields {
    required: &'static [&'static str],
    allowed: &'static [&'static str],
}

impl ClosedObjectFields {
    fn matches(self, source: &serde_json::Value) -> bool {
        let Some(object) = source.as_object() else {
            return false;
        };

        self.required.iter().all(|key| object.contains_key(*key))
            && object
                .keys()
                .all(|key| self.allowed.contains(&key.as_str()))
    }
}

const fn closed_fields(
    required: &'static [&'static str],
    allowed: &'static [&'static str],
) -> ClosedObjectFields {
    ClosedObjectFields { required, allowed }
}

const CONSEQUENCE_PARAM_FIELDS: &[&str] = &[
    "query",
    "automaticFacetFilters",
    "automaticOptionalFacetFilters",
    "renderingContent",
    "filters",
    "facetFilters",
    "numericFilters",
    "optionalFilters",
    "tagFilters",
    "aroundLatLng",
    "aroundRadius",
    "hitsPerPage",
    "restrictSearchableAttributes",
];

fn object_string<'a>(source: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    source.as_object()?.get(key)?.as_str()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidationRule {
    ExistingSettingsMerge,
    ExistingCoreSerde,
    RequiredStringObjectId,
    UserDefinedJson,
    RejectClosedSchemaUnknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerPathPrecondition {
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetOwner {
    SettingsPayloadMerge,
    DocumentJson,
    RuleCoreStore,
    SynonymCoreStore,
    TranslationReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    Exact,
    Transformed,
    Warned,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum WarningCode {
    PersistedNoBehaviorSetting,
    ReadOnlySourceField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoundTripOracle {
    ExistingPersistence,
    JsonIdentity,
    CoreSerde,
    ClassificationOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompatibilityRow {
    resource: ResourceKind,
    matcher: SourceMatcher,
    validation_rule: ValidationRule,
    target_owner: TargetOwner,
    disposition: Disposition,
    warning_code: Option<WarningCode>,
    rejection_code: Option<ReportCode>,
    round_trip: RoundTripOracle,
    owner_path_precondition: OwnerPathPrecondition,
    precedence: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowSemantics {
    validation_rule: ValidationRule,
    target_owner: TargetOwner,
    disposition: Disposition,
    warning_code: Option<WarningCode>,
    rejection_code: Option<ReportCode>,
    round_trip: RoundTripOracle,
    owner_path_precondition: OwnerPathPrecondition,
}

const fn row(
    resource: ResourceKind,
    matcher: SourceMatcher,
    semantics: RowSemantics,
    precedence: u16,
) -> CompatibilityRow {
    CompatibilityRow {
        resource,
        matcher,
        validation_rule: semantics.validation_rule,
        target_owner: semantics.target_owner,
        disposition: semantics.disposition,
        warning_code: semantics.warning_code,
        rejection_code: semantics.rejection_code,
        round_trip: semantics.round_trip,
        owner_path_precondition: semantics.owner_path_precondition,
        precedence,
    }
}

const fn exact_settings(field: &'static str) -> CompatibilityRow {
    row(
        ResourceKind::Settings,
        SourceMatcher::Field(field),
        RowSemantics {
            validation_rule: ValidationRule::ExistingSettingsMerge,
            target_owner: TargetOwner::SettingsPayloadMerge,
            disposition: Disposition::Exact,
            warning_code: None,
            rejection_code: None,
            round_trip: RoundTripOracle::ExistingPersistence,
            owner_path_precondition: OwnerPathPrecondition::None,
        },
        FIELD_PRECEDENCE,
    )
}

const fn transformed_settings(field: &'static str) -> CompatibilityRow {
    row(
        ResourceKind::Settings,
        SourceMatcher::Field(field),
        RowSemantics {
            validation_rule: ValidationRule::ExistingSettingsMerge,
            target_owner: TargetOwner::SettingsPayloadMerge,
            disposition: Disposition::Transformed,
            warning_code: None,
            rejection_code: None,
            round_trip: RoundTripOracle::ExistingPersistence,
            owner_path_precondition: OwnerPathPrecondition::None,
        },
        FIELD_PRECEDENCE,
    )
}

const fn read_only_source_reader_setting(field: &'static str) -> CompatibilityRow {
    row(
        ResourceKind::Settings,
        SourceMatcher::Field(field),
        RowSemantics {
            validation_rule: ValidationRule::ExistingSettingsMerge,
            target_owner: TargetOwner::TranslationReport,
            disposition: Disposition::Warned,
            warning_code: Some(WarningCode::ReadOnlySourceField),
            rejection_code: None,
            round_trip: RoundTripOracle::ClassificationOnly,
            owner_path_precondition: OwnerPathPrecondition::None,
        },
        FIELD_PRECEDENCE,
    )
}

/// Settings that Flapjack persists faithfully but whose value the search runtime
/// does not yet read, so migration must warn that the value has no live behavior
/// rather than claim exact preservation.
const fn persisted_no_behavior_setting(field: &'static str) -> CompatibilityRow {
    row(
        ResourceKind::Settings,
        SourceMatcher::Field(field),
        RowSemantics {
            validation_rule: ValidationRule::ExistingSettingsMerge,
            target_owner: TargetOwner::SettingsPayloadMerge,
            disposition: Disposition::Warned,
            warning_code: Some(WarningCode::PersistedNoBehaviorSetting),
            rejection_code: None,
            round_trip: RoundTripOracle::ExistingPersistence,
            owner_path_precondition: OwnerPathPrecondition::None,
        },
        FIELD_PRECEDENCE,
    )
}

const fn exact_resource_field(resource: ResourceKind, field: &'static str) -> CompatibilityRow {
    row(
        resource,
        SourceMatcher::Field(field),
        RowSemantics {
            validation_rule: ValidationRule::ExistingCoreSerde,
            target_owner: match resource {
                ResourceKind::Rule => TargetOwner::RuleCoreStore,
                ResourceKind::Synonym => TargetOwner::SynonymCoreStore,
                ResourceKind::Settings | ResourceKind::Document => TargetOwner::TranslationReport,
            },
            disposition: Disposition::Exact,
            warning_code: None,
            rejection_code: None,
            round_trip: RoundTripOracle::CoreSerde,
            owner_path_precondition: OwnerPathPrecondition::None,
        },
        FIELD_PRECEDENCE,
    )
}

const fn schema_row(resource: ResourceKind, matcher: SourceMatcher) -> CompatibilityRow {
    row(
        resource,
        matcher,
        RowSemantics {
            validation_rule: ValidationRule::ExistingCoreSerde,
            target_owner: match resource {
                ResourceKind::Rule => TargetOwner::RuleCoreStore,
                ResourceKind::Synonym => TargetOwner::SynonymCoreStore,
                ResourceKind::Settings | ResourceKind::Document => TargetOwner::TranslationReport,
            },
            disposition: Disposition::Exact,
            warning_code: None,
            rejection_code: None,
            round_trip: RoundTripOracle::CoreSerde,
            owner_path_precondition: OwnerPathPrecondition::None,
        },
        SCHEMA_PRECEDENCE,
    )
}

const fn rule_schema(matcher: RuleSchemaMatcher) -> CompatibilityRow {
    schema_row(ResourceKind::Rule, SourceMatcher::RuleSchema(matcher))
}

const fn synonym_schema(matcher: SynonymSchemaMatcher) -> CompatibilityRow {
    schema_row(ResourceKind::Synonym, SourceMatcher::SynonymSchema(matcher))
}

const fn closed_unknown(resource: ResourceKind) -> CompatibilityRow {
    row(
        resource,
        SourceMatcher::UnknownClosedSchema,
        RowSemantics {
            validation_rule: ValidationRule::RejectClosedSchemaUnknown,
            target_owner: TargetOwner::TranslationReport,
            disposition: Disposition::Rejected,
            warning_code: None,
            rejection_code: Some(ReportCode::UnsupportedSourceField),
            round_trip: RoundTripOracle::ClassificationOnly,
            owner_path_precondition: OwnerPathPrecondition::None,
        },
        FALLBACK_PRECEDENCE,
    )
}

#[cfg_attr(not(test), allow(dead_code))]
fn stage1_matrix() -> &'static [CompatibilityRow] {
    STAGE1_MATRIX
}

#[cfg_attr(not(test), allow(dead_code))]
fn resolve_source_field(resource: ResourceKind, field: &str) -> CompatibilityRow {
    resolve_matching_row(resource, |matcher| matcher.matches_field(resource, field))
}

#[cfg_attr(not(test), allow(dead_code))]
fn resolve_source_schema(resource: ResourceKind, source: &serde_json::Value) -> CompatibilityRow {
    resolve_matching_row(resource, |matcher| matcher.matches_schema(resource, source))
}

#[cfg_attr(not(test), allow(dead_code))]
fn resolve_rule_schema(path: RuleSchemaPath, source: &serde_json::Value) -> CompatibilityRow {
    resolve_matching_row(ResourceKind::Rule, |matcher| {
        matcher.matches_rule_schema(path, source)
    })
}

fn resolve_matching_row(
    resource: ResourceKind,
    mut matches: impl FnMut(SourceMatcher) -> bool,
) -> CompatibilityRow {
    let mut best: Option<CompatibilityRow> = None;
    let mut same_precedence_matches = 0;

    for row in STAGE1_MATRIX
        .iter()
        .copied()
        .filter(|row| row.resource == resource && matches(row.matcher))
    {
        match best {
            Some(current) if row.precedence > current.precedence => {}
            Some(current) if row.precedence == current.precedence => same_precedence_matches += 1,
            _ => {
                best = Some(row);
                same_precedence_matches = 1;
            }
        }
    }

    assert_eq!(
        same_precedence_matches, 1,
        "matrix rows for {resource:?} must resolve to exactly one highest-precedence match"
    );
    best.expect("every resolver has an explicit fallback row")
}

// Algolia standard and virtual replicas both migrate as Flapjack virtual replicas.
// Flapjack sorts at query time, so a physical replica would duplicate the corpus without benefit.
static STAGE1_MATRIX: &[CompatibilityRow] = &[
    exact_settings("attributesForFaceting"),
    exact_settings("searchableAttributes"),
    transformed_settings("attributesToIndex"),
    exact_settings("ranking"),
    exact_settings("customRanking"),
    exact_settings("attributesToRetrieve"),
    exact_settings("unretrievableAttributes"),
    persisted_no_behavior_setting("attributesToHighlight"),
    persisted_no_behavior_setting("attributesToSnippet"),
    exact_settings("paginationLimitedTo"),
    exact_settings("attributeForDistinct"),
    transformed_settings("distinct"),
    persisted_no_behavior_setting("highlightPreTag"),
    persisted_no_behavior_setting("highlightPostTag"),
    persisted_no_behavior_setting("hitsPerPage"),
    exact_settings("minWordSizefor1Typo"),
    exact_settings("minWordSizefor2Typos"),
    exact_settings("maxValuesPerFacet"),
    exact_settings("exactOnSingleWordQuery"),
    exact_settings("removeWordsIfNoResults"),
    exact_settings("separatorsToIndex"),
    exact_settings("alternativesAsExact"),
    persisted_no_behavior_setting("optionalWords"),
    read_only_source_reader_setting("synonyms"),
    read_only_source_reader_setting("version"),
    exact_settings("removeStopWords"),
    exact_settings("ignorePlurals"),
    exact_settings("queryLanguages"),
    exact_settings("queryType"),
    exact_settings("embedders"),
    exact_settings("mode"),
    exact_settings("semanticSearch"),
    exact_settings("enablePersonalization"),
    exact_settings("renderingContent"),
    exact_settings("userData"),
    exact_settings("enableRules"),
    exact_settings("advancedSyntaxFeatures"),
    exact_settings("sortFacetValuesBy"),
    exact_settings("snippetEllipsisText"),
    exact_settings("restrictHighlightAndSnippetArrays"),
    exact_settings("minProximity"),
    exact_settings("disableExactOnAttributes"),
    exact_settings("replaceSynonymsInHighlight"),
    exact_settings("attributeCriteriaComputedByMinProximity"),
    exact_settings("enableReRanking"),
    exact_settings("disableTypoToleranceOnWords"),
    exact_settings("disableTypoToleranceOnAttributes"),
    transformed_settings("replicas"),
    exact_settings("numericAttributesForFiltering"),
    transformed_settings("numericAttributesToIndex"),
    persisted_no_behavior_setting("allowCompressionOfIntegerArray"),
    transformed_settings("relevancyStrictness"),
    closed_unknown(ResourceKind::Settings),
    row(
        ResourceKind::Document,
        SourceMatcher::Field("objectID"),
        RowSemantics {
            validation_rule: ValidationRule::RequiredStringObjectId,
            target_owner: TargetOwner::DocumentJson,
            disposition: Disposition::Exact,
            warning_code: None,
            rejection_code: None,
            round_trip: RoundTripOracle::JsonIdentity,
            owner_path_precondition: OwnerPathPrecondition::None,
        },
        FIELD_PRECEDENCE,
    ),
    row(
        ResourceKind::Document,
        SourceMatcher::DocumentAttribute,
        RowSemantics {
            validation_rule: ValidationRule::UserDefinedJson,
            target_owner: TargetOwner::DocumentJson,
            disposition: Disposition::Exact,
            warning_code: None,
            rejection_code: None,
            round_trip: RoundTripOracle::JsonIdentity,
            owner_path_precondition: OwnerPathPrecondition::None,
        },
        FALLBACK_PRECEDENCE,
    ),
    exact_resource_field(ResourceKind::Rule, "objectID"),
    exact_resource_field(ResourceKind::Rule, "conditions"),
    exact_resource_field(ResourceKind::Rule, "consequence"),
    exact_resource_field(ResourceKind::Rule, "description"),
    exact_resource_field(ResourceKind::Rule, "enabled"),
    exact_resource_field(ResourceKind::Rule, "validity"),
    rule_schema(RuleSchemaMatcher::Condition),
    rule_schema(RuleSchemaMatcher::Consequence),
    rule_schema(RuleSchemaMatcher::ConsequenceParams),
    rule_schema(RuleSchemaMatcher::PromoteSingle),
    rule_schema(RuleSchemaMatcher::PromoteMultiple),
    rule_schema(RuleSchemaMatcher::Hide),
    rule_schema(RuleSchemaMatcher::TimeRange),
    rule_schema(RuleSchemaMatcher::AutomaticFacetFilter),
    rule_schema(RuleSchemaMatcher::ConsequenceQueryLiteral),
    rule_schema(RuleSchemaMatcher::ConsequenceQueryEdits),
    rule_schema(RuleSchemaMatcher::QueryEdit),
    closed_unknown(ResourceKind::Rule),
    exact_resource_field(ResourceKind::Synonym, "objectID"),
    exact_resource_field(ResourceKind::Synonym, "type"),
    synonym_schema(SynonymSchemaMatcher::Regular),
    synonym_schema(SynonymSchemaMatcher::OneWay),
    synonym_schema(SynonymSchemaMatcher::AltCorrection1),
    synonym_schema(SynonymSchemaMatcher::AltCorrection2),
    synonym_schema(SynonymSchemaMatcher::Placeholder),
    closed_unknown(ResourceKind::Synonym),
];

#[cfg(test)]
#[path = "translation_tests.rs"]
mod tests;
