use super::super::source_snapshot::source_item_hash;
use super::super::source_snapshot::{
    SourceSnapshotResource, SourceSnapshotSchemaViolation, SourceSnapshotSchemaViolationKind,
};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(in crate::handlers::migration) struct TranslationReport {
    pub(in crate::handlers::migration) entries: Vec<TranslationReportEntry>,
    pub(in crate::handlers::migration) summary: TranslationReportSummary,
    pub(in crate::handlers::migration) report_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(in crate::handlers::migration) struct TranslationReportSummary {
    pub(in crate::handlers::migration) total_entries: usize,
    pub(in crate::handlers::migration) hard_rejections: usize,
    pub(in crate::handlers::migration) warnings: usize,
    pub(in crate::handlers::migration) scope_gaps: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(in crate::handlers::migration) struct TranslationReportEntry {
    pub(in crate::handlers::migration) severity: ReportSeverity,
    pub(in crate::handlers::migration) code: ReportCode,
    pub(in crate::handlers::migration) resource: ReportResource,
    pub(in crate::handlers::migration) page_index: Option<usize>,
    pub(in crate::handlers::migration) item_index: Option<usize>,
    pub(in crate::handlers::migration) json_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(in crate::handlers::migration) enum ReportSeverity {
    ScopeGap,
    Warning,
    HardRejection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(in crate::handlers::migration) enum ReportResource {
    Analytics,
    ApiKeys,
    Document,
    Events,
    Experiments,
    Recommend,
    Rule,
    Settings,
    Synonym,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub(in crate::handlers::migration) enum ReportCode {
    ProductNotMigrated,
    PersistedNoBehaviorSetting,
    ReadOnlySourceField,
    ReplicaTopologyNotMigrated,
    UnsupportedSourceField,
    UnsupportedRuleSchema,
    UnsupportedSynonymSchema,
    InvalidObjectId,
    DuplicateObjectId,
    MalformedSettingsPayload,
    MalformedDocumentPayload,
    MalformedRulePayload,
    MalformedSynonymPayload,
    ReplicaUnknownRankingToken,
    ReplicaExhaustiveSortApproximated,
    ReplicaPrimaryRelevancyStrictnessDropped,
    ReplicaRelevancyStrictnessSemanticMismatch,
    ReplicaMatchingCriticalFieldDiverges,
}

pub(super) fn non_portable_product_entries() -> Vec<TranslationReportEntry> {
    [
        ReportResource::Analytics,
        ReportResource::ApiKeys,
        ReportResource::Events,
        ReportResource::Experiments,
        ReportResource::Recommend,
    ]
    .into_iter()
    .map(|resource| TranslationReportEntry {
        severity: ReportSeverity::ScopeGap,
        code: ReportCode::ProductNotMigrated,
        resource,
        page_index: None,
        item_index: None,
        json_path: "$".to_string(),
    })
    .collect()
}

pub(super) fn warning_entry(
    code: ReportCode,
    resource: ReportResource,
    page_index: Option<usize>,
    item_index: Option<usize>,
    json_path: &str,
) -> TranslationReportEntry {
    TranslationReportEntry {
        severity: ReportSeverity::Warning,
        code,
        resource,
        page_index,
        item_index,
        json_path: json_path.to_string(),
    }
}

pub(super) fn hard_entry(
    code: ReportCode,
    resource: ReportResource,
    page_index: Option<usize>,
    item_index: Option<usize>,
    json_path: &str,
) -> TranslationReportEntry {
    TranslationReportEntry {
        severity: ReportSeverity::HardRejection,
        code,
        resource,
        page_index,
        item_index,
        json_path: json_path.to_string(),
    }
}

pub(super) fn source_snapshot_violation_entry(
    violation: SourceSnapshotSchemaViolation,
) -> TranslationReportEntry {
    let code = match violation.kind {
        SourceSnapshotSchemaViolationKind::InvalidObjectId => ReportCode::InvalidObjectId,
        SourceSnapshotSchemaViolationKind::DuplicateObjectId => ReportCode::DuplicateObjectId,
        SourceSnapshotSchemaViolationKind::MalformedPayload => {
            malformed_payload_code_for_snapshot_resource(violation.resource)
        }
    };
    hard_entry(
        code,
        report_resource_for_snapshot_resource(violation.resource),
        Some(violation.page_index),
        Some(violation.item_index),
        source_snapshot_violation_json_path(violation.kind),
    )
}

fn malformed_payload_code_for_snapshot_resource(resource: SourceSnapshotResource) -> ReportCode {
    match resource {
        SourceSnapshotResource::Document => ReportCode::MalformedDocumentPayload,
        SourceSnapshotResource::Rule => ReportCode::MalformedRulePayload,
        SourceSnapshotResource::Synonym => ReportCode::MalformedSynonymPayload,
    }
}

fn source_snapshot_violation_json_path(kind: SourceSnapshotSchemaViolationKind) -> &'static str {
    match kind {
        SourceSnapshotSchemaViolationKind::InvalidObjectId
        | SourceSnapshotSchemaViolationKind::DuplicateObjectId => "$.objectID",
        SourceSnapshotSchemaViolationKind::MalformedPayload => "$",
    }
}

fn report_resource_for_snapshot_resource(resource: SourceSnapshotResource) -> ReportResource {
    match resource {
        SourceSnapshotResource::Document => ReportResource::Document,
        SourceSnapshotResource::Rule => ReportResource::Rule,
        SourceSnapshotResource::Synonym => ReportResource::Synonym,
    }
}

pub(super) fn contains_hard_rejection(entries: &[TranslationReportEntry]) -> bool {
    entries
        .iter()
        .any(|entry| entry.severity == ReportSeverity::HardRejection)
}

pub(in crate::handlers::migration) fn warning_message(code: ReportCode) -> Option<&'static str> {
    Some(match code {
        ReportCode::PersistedNoBehaviorSetting => {
            "Source setting is preserved for compatibility but has no Flapjack behavior."
        }
        ReportCode::ReadOnlySourceField => {
            "Source field is read-only in Flapjack and is not applied during migration."
        }
        ReportCode::ReplicaTopologyNotMigrated => {
            "Replica topology contains an entry that cannot be translated, such as a malformed, self-referential, or colliding replica."
        }
        ReportCode::ReplicaUnknownRankingToken => {
            "Replica ranking token is not recognized by Flapjack and was ignored."
        }
        ReportCode::ReplicaExhaustiveSortApproximated => {
            "Algolia standard replica exhaustive sorting is approximated as a Flapjack virtual replica."
        }
        ReportCode::ReplicaPrimaryRelevancyStrictnessDropped => {
            "Primary relevancyStrictness is not applied to translated replica settings."
        }
        ReportCode::ReplicaRelevancyStrictnessSemanticMismatch => {
            "Algolia relevancyStrictness semantics differ from Flapjack deterministic-query ranking and may not produce identical ordering."
        }
        ReportCode::ReplicaMatchingCriticalFieldDiverges => {
            "Replica setting changes matching-critical behavior that virtual replicas cannot independently reproduce."
        }
        ReportCode::ProductNotMigrated
        | ReportCode::UnsupportedSourceField
        | ReportCode::UnsupportedRuleSchema
        | ReportCode::UnsupportedSynonymSchema
        | ReportCode::InvalidObjectId
        | ReportCode::DuplicateObjectId
        | ReportCode::MalformedSettingsPayload
        | ReportCode::MalformedDocumentPayload
        | ReportCode::MalformedRulePayload
        | ReportCode::MalformedSynonymPayload => return None,
    })
}

pub(super) fn finalize_report(mut entries: Vec<TranslationReportEntry>) -> TranslationReport {
    entries.sort_by(|left, right| report_entry_sort_key(left).cmp(&report_entry_sort_key(right)));
    let summary = TranslationReportSummary {
        total_entries: entries.len(),
        hard_rejections: entries
            .iter()
            .filter(|entry| entry.severity == ReportSeverity::HardRejection)
            .count(),
        warnings: entries
            .iter()
            .filter(|entry| entry.severity == ReportSeverity::Warning)
            .count(),
        scope_gaps: entries
            .iter()
            .filter(|entry| entry.severity == ReportSeverity::ScopeGap)
            .count(),
    };
    let report_digest = Some(source_item_hash(&serde_json::json!({
        "entries": entries,
        "summary": summary,
    })));

    TranslationReport {
        entries,
        summary,
        report_digest,
    }
}

/// Report entries are canonicalized by severity, resource, page, item, path, then code.
fn report_entry_sort_key(
    entry: &TranslationReportEntry,
) -> (u8, u8, Option<usize>, Option<usize>, &str, u8) {
    (
        severity_rank(entry.severity),
        resource_rank(entry.resource),
        entry.page_index,
        entry.item_index,
        entry.json_path.as_str(),
        report_code_rank(entry.code),
    )
}

fn severity_rank(severity: ReportSeverity) -> u8 {
    match severity {
        ReportSeverity::ScopeGap => 0,
        ReportSeverity::Warning => 1,
        ReportSeverity::HardRejection => 2,
    }
}

fn resource_rank(resource: ReportResource) -> u8 {
    match resource {
        ReportResource::Analytics => 0,
        ReportResource::ApiKeys => 1,
        ReportResource::Events => 2,
        ReportResource::Experiments => 3,
        ReportResource::Recommend => 4,
        ReportResource::Settings => 5,
        ReportResource::Document => 6,
        ReportResource::Rule => 7,
        ReportResource::Synonym => 8,
    }
}

fn report_code_rank(code: ReportCode) -> u8 {
    match code {
        ReportCode::ProductNotMigrated => 0,
        ReportCode::PersistedNoBehaviorSetting => 1,
        ReportCode::ReadOnlySourceField => 2,
        ReportCode::ReplicaTopologyNotMigrated => 3,
        ReportCode::UnsupportedSourceField => 4,
        ReportCode::UnsupportedRuleSchema => 5,
        ReportCode::UnsupportedSynonymSchema => 6,
        ReportCode::InvalidObjectId => 7,
        ReportCode::DuplicateObjectId => 8,
        ReportCode::MalformedSettingsPayload => 9,
        ReportCode::MalformedDocumentPayload => 10,
        ReportCode::MalformedRulePayload => 11,
        ReportCode::MalformedSynonymPayload => 12,
        ReportCode::ReplicaUnknownRankingToken => 13,
        ReportCode::ReplicaExhaustiveSortApproximated => 14,
        ReportCode::ReplicaPrimaryRelevancyStrictnessDropped => 15,
        ReportCode::ReplicaRelevancyStrictnessSemanticMismatch => 16,
        ReportCode::ReplicaMatchingCriticalFieldDiverges => 17,
    }
}
