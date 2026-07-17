#![allow(dead_code)]

use super::algolia_client::{AlgoliaClientError, AlgoliaErrorKind};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceSnapshot {
    pub(super) settings: SourceResourceSnapshot,
    pub(super) documents: SourceResourceSnapshot,
    pub(super) rules: SourceResourceSnapshot,
    pub(super) synonyms: SourceResourceSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceResourceSnapshot {
    pub(super) count: usize,
    pub(super) hash: String,
    pub(super) ids: BTreeSet<String>,
}

impl SourceSnapshot {
    pub(super) fn from_raw(
        settings: Value,
        documents: Vec<Value>,
        rules: Vec<Value>,
        synonyms: Vec<Value>,
    ) -> Result<Self, AlgoliaClientError> {
        Ok(Self {
            settings: settings_resource_snapshot(&settings),
            documents: object_resource_snapshot(SourceSnapshotResource::Document, &documents)?,
            rules: object_resource_snapshot(SourceSnapshotResource::Rule, &rules)?,
            synonyms: object_resource_snapshot(SourceSnapshotResource::Synonym, &synonyms)?,
        })
    }
}

#[derive(Debug, Default)]
pub(super) struct SourceSnapshotBuilder {
    settings: Option<SourceResourceSnapshot>,
    documents: SourceResourceAccumulator,
    rules: SourceResourceAccumulator,
    synonyms: SourceResourceAccumulator,
}

impl SourceSnapshotBuilder {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn record_settings(&mut self, settings: &Value) {
        self.settings = Some(settings_resource_snapshot(settings));
    }

    pub(super) fn record_documents(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.record_documents_page(0, page)
            .map_err(AlgoliaClientError::from)
    }

    pub(super) fn record_rules(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.record_rules_page(0, page)
            .map_err(AlgoliaClientError::from)
    }

    pub(super) fn record_synonyms(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.record_synonyms_page(0, page)
            .map_err(AlgoliaClientError::from)
    }

    pub(super) fn record_documents_page(
        &mut self,
        page_index: usize,
        page: &[Value],
    ) -> Result<(), SourceSnapshotSchemaViolation> {
        self.documents
            .record_items(SourceSnapshotResource::Document, page_index, page)
    }

    pub(super) fn record_rules_page(
        &mut self,
        page_index: usize,
        page: &[Value],
    ) -> Result<(), SourceSnapshotSchemaViolation> {
        self.rules
            .record_items(SourceSnapshotResource::Rule, page_index, page)
    }

    pub(super) fn record_synonyms_page(
        &mut self,
        page_index: usize,
        page: &[Value],
    ) -> Result<(), SourceSnapshotSchemaViolation> {
        self.synonyms
            .record_items(SourceSnapshotResource::Synonym, page_index, page)
    }

    pub(super) fn finish(self) -> Result<SourceSnapshot, AlgoliaClientError> {
        let settings = self.settings.ok_or_else(|| {
            source_snapshot_schema_error("Algolia source settings were not captured")
        })?;
        Ok(SourceSnapshot {
            settings,
            documents: self.documents.finish(),
            rules: self.rules.finish(),
            synonyms: self.synonyms.finish(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceSnapshotResource {
    Document,
    Rule,
    Synonym,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceSnapshotSchemaViolationKind {
    InvalidObjectId,
    DuplicateObjectId,
    MalformedPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SourceSnapshotSchemaViolation {
    pub(super) resource: SourceSnapshotResource,
    pub(super) kind: SourceSnapshotSchemaViolationKind,
    pub(super) page_index: usize,
    pub(super) item_index: usize,
}

impl From<SourceSnapshotSchemaViolation> for AlgoliaClientError {
    fn from(violation: SourceSnapshotSchemaViolation) -> Self {
        match violation.kind {
            SourceSnapshotSchemaViolationKind::InvalidObjectId => {
                source_snapshot_schema_error("Algolia source item was missing a string objectID")
            }
            SourceSnapshotSchemaViolationKind::DuplicateObjectId => {
                source_snapshot_schema_error("Algolia source item contained a duplicate objectID")
            }
            SourceSnapshotSchemaViolationKind::MalformedPayload => {
                source_snapshot_schema_error("Algolia source item was not a JSON object")
            }
        }
    }
}

#[derive(Debug, Default)]
struct SourceResourceAccumulator {
    ids: BTreeMap<String, (usize, usize)>,
    item_hashes: Vec<(String, String)>,
}

impl SourceResourceAccumulator {
    fn record_items(
        &mut self,
        resource: SourceSnapshotResource,
        page_index: usize,
        items: &[Value],
    ) -> Result<(), SourceSnapshotSchemaViolation> {
        for (item_index, item) in items.iter().enumerate() {
            let id = object_stable_id(item).map_err(|kind| SourceSnapshotSchemaViolation {
                resource,
                kind,
                page_index,
                item_index,
            })?;
            if self
                .ids
                .insert(id.clone(), (page_index, item_index))
                .is_some()
            {
                return Err(SourceSnapshotSchemaViolation {
                    resource,
                    kind: SourceSnapshotSchemaViolationKind::DuplicateObjectId,
                    page_index,
                    item_index,
                });
            }
            self.item_hashes.push((id, source_item_hash(item)));
        }
        Ok(())
    }

    fn finish(self) -> SourceResourceSnapshot {
        SourceResourceSnapshot {
            count: self.ids.len(),
            hash: aggregate_source_item_hashes(self.item_hashes),
            ids: self.ids.into_keys().collect(),
        }
    }
}

fn settings_resource_snapshot(settings: &Value) -> SourceResourceSnapshot {
    let id = "settings".to_string();
    let item_hash = source_item_hash(settings);
    SourceResourceSnapshot {
        count: 1,
        hash: aggregate_source_item_hashes(vec![(id.clone(), item_hash)]),
        ids: BTreeSet::from([id]),
    }
}

fn object_resource_snapshot(
    resource: SourceSnapshotResource,
    items: &[Value],
) -> Result<SourceResourceSnapshot, AlgoliaClientError> {
    let mut accumulator = SourceResourceAccumulator::default();
    accumulator
        .record_items(resource, 0, items)
        .map_err(AlgoliaClientError::from)?;
    Ok(accumulator.finish())
}

fn object_stable_id(item: &Value) -> Result<String, SourceSnapshotSchemaViolationKind> {
    let object = item
        .as_object()
        .ok_or(SourceSnapshotSchemaViolationKind::MalformedPayload)?;
    object
        .get("objectID")
        .and_then(Value::as_str)
        .filter(|object_id| !object_id.is_empty())
        .map(str::to_string)
        .ok_or(SourceSnapshotSchemaViolationKind::InvalidObjectId)
}

fn source_snapshot_schema_error(message: &'static str) -> AlgoliaClientError {
    AlgoliaClientError::new(AlgoliaErrorKind::Schema, message)
}

pub(super) fn source_item_hash(value: &Value) -> String {
    sha256_hex(&canonical_json_bytes(value))
}

fn aggregate_source_item_hashes(mut item_hashes: Vec<(String, String)>) -> String {
    item_hashes.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (stable_id, item_hash) in item_hashes {
        hasher.update(stable_id.as_bytes());
        hasher.update([0]);
        hasher.update(item_hash.as_bytes());
        hasher.update([b'\n']);
    }
    hex::encode(hasher.finalize())
}

pub(super) fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_canonical_json(value, &mut bytes);
    bytes
}

fn write_canonical_json(value: &Value, bytes: &mut Vec<u8>) {
    match value {
        Value::Null => bytes.extend_from_slice(b"null"),
        Value::Bool(true) => bytes.extend_from_slice(b"true"),
        Value::Bool(false) => bytes.extend_from_slice(b"false"),
        Value::Number(number) => bytes.extend_from_slice(number.to_string().as_bytes()),
        Value::String(string) => write_json_string(string, bytes),
        Value::Array(values) => write_canonical_array(values, bytes),
        Value::Object(object) => write_canonical_object(object, bytes),
    }
}

fn write_canonical_array(values: &[Value], bytes: &mut Vec<u8>) {
    bytes.push(b'[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            bytes.push(b',');
        }
        write_canonical_json(value, bytes);
    }
    bytes.push(b']');
}

fn write_canonical_object(object: &serde_json::Map<String, Value>, bytes: &mut Vec<u8>) {
    bytes.push(b'{');
    let mut keys = object.keys().collect::<Vec<_>>();
    keys.sort();
    for (index, key) in keys.into_iter().enumerate() {
        if index > 0 {
            bytes.push(b',');
        }
        write_json_string(key, bytes);
        bytes.push(b':');
        write_canonical_json(&object[key], bytes);
    }
    bytes.push(b'}');
}

fn write_json_string(value: &str, bytes: &mut Vec<u8>) {
    serde_json::to_writer(bytes, value).expect("serializing a JSON string into memory cannot fail");
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
