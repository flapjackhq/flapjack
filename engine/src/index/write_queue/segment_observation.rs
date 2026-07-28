//! Read-only segment and index-file observations for write-path policy and metrics.
//!
//! Searchable segment metadata is the source of live segment IDs and live-document counts
//! (`max_doc - num_deleted_docs`). Tantivy's managed-file listing and the index directory are
//! the sources for byte/file counts and stale segment file sets. Keeping those public
//! observations here gives merge policy, backpressure, metrics, and tests one shared view.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SegmentObservation {
    pub live_segment_ids: BTreeSet<String>,
    pub live_segment_count: usize,
    pub live_docs: u64,
    pub per_segment_doc_counts: BTreeMap<String, u64>,
    pub managed_index_file_count: u64,
    pub index_bytes: u64,
    pub orphan_file_set_ids: BTreeSet<String>,
}

pub(crate) fn observe_segments(
    index: &crate::index::Index,
) -> crate::error::Result<SegmentObservation> {
    let metas = index.inner().searchable_segment_metas()?;
    let mut live_segment_ids = BTreeSet::new();
    let mut live_docs = 0u64;
    let mut per_segment_doc_counts = BTreeMap::new();
    let mut live_segment_files = BTreeSet::new();
    let managed_index_file_count = index.inner().directory().list_managed_files().len() as u64;
    let index_bytes = crate::index::storage_size::dir_size_bytes(index.path())?;

    for meta in &metas {
        let segment_id = meta.id().uuid_string();
        let segment_live_docs = u64::from(meta.max_doc() - meta.num_deleted_docs());
        live_docs += segment_live_docs;
        live_segment_ids.insert(segment_id.clone());
        per_segment_doc_counts.insert(segment_id, segment_live_docs);
        live_segment_files.extend(meta.list_files());
    }

    let orphan_file_set_ids = orphan_file_set_ids(index, &live_segment_files)?;

    Ok(SegmentObservation {
        live_segment_count: live_segment_ids.len(),
        live_segment_ids,
        live_docs,
        per_segment_doc_counts,
        managed_index_file_count,
        index_bytes,
        orphan_file_set_ids,
    })
}

fn orphan_file_set_ids(
    index: &crate::index::Index,
    live_segment_files: &BTreeSet<PathBuf>,
) -> crate::error::Result<BTreeSet<String>> {
    let live_file_names = live_segment_files
        .iter()
        .filter_map(|path| path.file_name().map(PathBuf::from))
        .collect::<BTreeSet<_>>();
    let managed_orphan_ids = index
        .inner()
        .directory()
        .list_managed_files()
        .into_iter()
        .filter(|path| !live_segment_files.contains(path))
        .filter_map(|path| segment_file_set_id(&path))
        .collect::<BTreeSet<_>>();
    let disk_orphan_ids = disk_orphan_file_set_ids(index.path(), &live_file_names)?;
    Ok(managed_orphan_ids
        .union(&disk_orphan_ids)
        .cloned()
        .collect())
}

fn disk_orphan_file_set_ids(
    index_path: &Path,
    live_file_names: &BTreeSet<PathBuf>,
) -> crate::error::Result<BTreeSet<String>> {
    let mut orphan_ids = BTreeSet::new();
    if !index_path.is_dir() {
        return Ok(orphan_ids);
    }
    for entry in std::fs::read_dir(index_path)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let file_name = PathBuf::from(entry.file_name());
        if live_file_names.contains(&file_name) {
            continue;
        }
        if let Some(segment_id) = segment_file_set_id(&file_name) {
            orphan_ids.insert(segment_id);
        }
    }
    Ok(orphan_ids)
}

fn segment_file_set_id(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    let segment_id = file_name.split('.').next()?;
    if segment_id.len() == 32 && segment_id.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(segment_id.to_string())
    } else {
        None
    }
}
