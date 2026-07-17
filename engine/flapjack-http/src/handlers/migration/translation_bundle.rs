use crate::handlers::settings::{
    payload_merge::merge_non_topology_settings_payload, SetSettingsRequest,
};
use flapjack::index::rules::Rule;
use flapjack::index::settings::IndexSettings;
use flapjack::index::synonyms::Synonym;
use flapjack::types::Document;
use serde::de::{DeserializeOwned, IntoDeserializer};
use serde_json::Value;

use super::translation_report::{ReportCode, ReportResource};
use super::SpoolTranslationInput;

#[derive(Debug, Clone)]
pub(in crate::handlers::migration) struct TranslationBundle {
    pub(in crate::handlers::migration) settings: IndexSettings,
    pub(in crate::handlers::migration) documents: Vec<Document>,
    pub(in crate::handlers::migration) rules: Vec<Rule>,
    pub(in crate::handlers::migration) synonyms: Vec<Synonym>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TypedTranslationFailure {
    pub(super) code: ReportCode,
    pub(super) resource: ReportResource,
    pub(super) page_index: Option<usize>,
    pub(super) item_index: Option<usize>,
    pub(super) json_path: String,
}

pub(super) fn translate_typed_bundle(
    input: &SpoolTranslationInput,
) -> Result<TranslationBundle, Vec<TypedTranslationFailure>> {
    let mut failures = Vec::new();
    let settings = translate_settings(&input.settings, &mut failures);
    let documents = translate_documents(&input.document_pages, &mut failures);
    let rules = translate_serde_pages(
        &input.rule_pages,
        ReportCode::MalformedRulePayload,
        ReportResource::Rule,
        &mut failures,
    );
    let synonyms = translate_serde_pages(
        &input.synonym_pages,
        ReportCode::MalformedSynonymPayload,
        ReportResource::Synonym,
        &mut failures,
    );

    if !failures.is_empty() {
        return Err(failures);
    }

    Ok(TranslationBundle {
        settings: settings.expect("settings exist when translation has no failures"),
        documents,
        rules,
        synonyms,
    })
}

fn translate_settings(
    settings_value: &Value,
    failures: &mut Vec<TypedTranslationFailure>,
) -> Option<IndexSettings> {
    let mut settings = IndexSettings::default();
    let mut payload = match deserialize_with_path::<SetSettingsRequest>(settings_value.clone()) {
        Ok(payload) => payload,
        Err(json_path) => {
            failures.push(failure(
                ReportCode::MalformedSettingsPayload,
                ReportResource::Settings,
                None,
                None,
                json_path,
            ));
            return None;
        }
    };

    if merge_non_topology_settings_payload(&mut settings, &mut payload).is_err() {
        failures.push(failure(
            ReportCode::MalformedSettingsPayload,
            ReportResource::Settings,
            None,
            None,
            "$.distinct".to_string(),
        ));
        return None;
    }
    Some(settings)
}

fn translate_documents(
    document_pages: &[Vec<Value>],
    failures: &mut Vec<TypedTranslationFailure>,
) -> Vec<Document> {
    let mut documents = Vec::new();
    for (page_index, page) in document_pages.iter().enumerate() {
        for (item_index, document) in page.iter().enumerate() {
            match Document::from_json(document) {
                Ok(document) => documents.push(document),
                Err(_) => failures.push(failure(
                    ReportCode::MalformedDocumentPayload,
                    ReportResource::Document,
                    Some(page_index),
                    Some(item_index),
                    "$".to_string(),
                )),
            }
        }
    }
    documents
}

fn translate_serde_pages<T: DeserializeOwned>(
    pages: &[Vec<Value>],
    code: ReportCode,
    resource: ReportResource,
    failures: &mut Vec<TypedTranslationFailure>,
) -> Vec<T> {
    let mut translated = Vec::new();
    for (page_index, page) in pages.iter().enumerate() {
        for (item_index, value) in page.iter().enumerate() {
            match deserialize_with_path(value.clone()) {
                Ok(value) => translated.push(value),
                Err(json_path) => failures.push(failure(
                    code,
                    resource,
                    Some(page_index),
                    Some(item_index),
                    json_path,
                )),
            }
        }
    }
    translated
}

fn deserialize_with_path<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_path_to_error::deserialize(value.into_deserializer())
        .map_err(|error| canonical_json_path(&error.path().to_string()))
}

fn canonical_json_path(path: &str) -> String {
    if path.is_empty() || path == "." {
        "$".to_string()
    } else if path.starts_with('[') {
        format!("${path}")
    } else {
        format!("$.{path}")
    }
}

fn failure(
    code: ReportCode,
    resource: ReportResource,
    page_index: Option<usize>,
    item_index: Option<usize>,
    json_path: String,
) -> TypedTranslationFailure {
    TypedTranslationFailure {
        code,
        resource,
        page_index,
        item_index,
        json_path,
    }
}
