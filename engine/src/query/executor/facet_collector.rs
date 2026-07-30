use once_cell::sync::Lazy;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::mem::size_of;
use std::ops::Bound;
use std::sync::{Arc, Mutex};
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::fastfield::FacetReader;
use tantivy::index::SegmentId;
use tantivy::schema::Facet;
use tantivy::{DocId, Score, SegmentOrdinal, SegmentReader};

const MAPPING_CACHE_MAX_ENTRIES: usize = 128;
const MAPPING_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;
const NO_COLLAPSED_FACET: usize = usize::MAX;

static FACET_MAPPING_CACHE: Lazy<Mutex<FacetMappingCache>> =
    Lazy::new(|| Mutex::new(FacetMappingCache::default()));

#[derive(Clone, Eq, Hash, PartialEq)]
struct FacetMappingKey {
    segment_id: SegmentId,
    field_name: String,
    roots: Vec<Facet>,
}

struct FacetMappingCacheEntry {
    mapping: Arc<PreparedFacetMapping>,
    estimated_bytes: usize,
}

struct FacetMappingCache {
    entries: HashMap<FacetMappingKey, FacetMappingCacheEntry>,
    insertion_order: VecDeque<FacetMappingKey>,
    estimated_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for FacetMappingCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
            estimated_bytes: 0,
            max_entries: MAPPING_CACHE_MAX_ENTRIES,
            max_bytes: MAPPING_CACHE_MAX_BYTES,
        }
    }
}

impl FacetMappingCache {
    fn get(&self, key: &FacetMappingKey) -> Option<Arc<PreparedFacetMapping>> {
        self.entries
            .get(key)
            .map(|entry| Arc::clone(&entry.mapping))
    }

    fn insert(
        &mut self,
        key: FacetMappingKey,
        mapping: Arc<PreparedFacetMapping>,
    ) -> Arc<PreparedFacetMapping> {
        if let Some(existing) = self.get(&key) {
            return existing;
        }

        let estimated_bytes = mapping.estimated_bytes(&key);
        if estimated_bytes > self.max_bytes {
            return mapping;
        }
        self.evict_until_fits(estimated_bytes);
        self.estimated_bytes += estimated_bytes;
        self.insertion_order.push_back(key.clone());
        self.entries.insert(
            key,
            FacetMappingCacheEntry {
                mapping: Arc::clone(&mapping),
                estimated_bytes,
            },
        );
        mapping
    }

    fn evict_until_fits(&mut self, incoming_bytes: usize) {
        while self.entries.len() >= self.max_entries
            || self.estimated_bytes + incoming_bytes > self.max_bytes
        {
            let Some(oldest_key) = self.insertion_order.pop_front() else {
                break;
            };
            if let Some(oldest) = self.entries.remove(&oldest_key) {
                self.estimated_bytes = self.estimated_bytes.saturating_sub(oldest.estimated_bytes);
            }
        }
    }
}

struct PreparedFacetMapping {
    collapsed_id_by_facet_ord: Vec<usize>,
    collapsed_facets: Vec<Facet>,
}

impl PreparedFacetMapping {
    fn build(reader: &FacetReader, roots: &[Facet]) -> tantivy::Result<Self> {
        let mut collapsed_id_by_facet_ord = vec![NO_COLLAPSED_FACET; reader.num_facets()];
        let mut collapsed_facets = Vec::new();
        let mut collapsed_ids = HashMap::new();
        let mut facet = Facet::root();

        for (facet_ord, collapsed_id_slot) in collapsed_id_by_facet_ord.iter_mut().enumerate() {
            reader.facet_from_ord(facet_ord as u64, &mut facet)?;
            let Some(collapsed_facet) = collapse_to_requested_child(&facet, roots) else {
                continue;
            };
            let next_id = collapsed_facets.len();
            let collapsed_id = *collapsed_ids
                .entry(collapsed_facet.clone())
                .or_insert_with(|| {
                    collapsed_facets.push(collapsed_facet);
                    next_id
                });
            *collapsed_id_slot = collapsed_id;
        }

        Ok(Self {
            collapsed_id_by_facet_ord,
            collapsed_facets,
        })
    }

    fn estimated_bytes(&self, key: &FacetMappingKey) -> usize {
        self.collapsed_id_by_facet_ord.len() * size_of::<usize>()
            + self
                .collapsed_facets
                .iter()
                .map(|facet| size_of::<Facet>() + facet.encoded_str().len())
                .sum::<usize>()
            + key.field_name.len()
            + key
                .roots
                .iter()
                .map(|facet| size_of::<Facet>() + facet.encoded_str().len())
                .sum::<usize>()
            + size_of::<FacetMappingKey>()
            + size_of::<FacetMappingCacheEntry>()
    }
}

fn collapse_to_requested_child(facet: &Facet, roots: &[Facet]) -> Option<Facet> {
    roots.iter().find_map(|root| {
        if !root.is_prefix_of(facet) {
            return None;
        }
        let encoded = facet.encoded_str().as_bytes();
        let child_start = if root.is_root() {
            0
        } else {
            root.encoded_str().len() + 1
        };
        let child_end = encoded[child_start..]
            .iter()
            .position(|byte| *byte == 0)
            .map_or(encoded.len(), |offset| child_start + offset);
        Facet::from_encoded(encoded[..child_end].to_vec()).ok()
    })
}

pub(crate) struct PreparedFacetCollector {
    field_name: String,
    roots: BTreeSet<Facet>,
}

impl PreparedFacetCollector {
    pub(crate) fn for_field(field_name: impl ToString) -> Self {
        Self {
            field_name: field_name.to_string(),
            roots: BTreeSet::new(),
        }
    }

    pub(crate) fn add_facet<T>(&mut self, facet_from: T)
    where
        Facet: From<T>,
    {
        let facet = Facet::from(facet_from);
        for existing in &self.roots {
            assert!(
                !existing.is_prefix_of(&facet) && !facet.is_prefix_of(existing),
                "facet roots may not contain one another"
            );
        }
        self.roots.insert(facet);
    }

    fn mapping_for_segment(
        &self,
        reader: &SegmentReader,
        facet_reader: &FacetReader,
    ) -> tantivy::Result<Arc<PreparedFacetMapping>> {
        let key = FacetMappingKey {
            segment_id: reader.segment_id(),
            field_name: self.field_name.clone(),
            roots: self.roots.iter().cloned().collect(),
        };
        if let Some(mapping) = mapping_cache().get(&key) {
            return Ok(mapping);
        }

        let mapping = Arc::new(PreparedFacetMapping::build(facet_reader, &key.roots)?);
        Ok(mapping_cache().insert(key, mapping))
    }
}

fn mapping_cache() -> std::sync::MutexGuard<'static, FacetMappingCache> {
    FACET_MAPPING_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) struct PreparedFacetSegmentCollector {
    reader: FacetReader,
    mapping: Arc<PreparedFacetMapping>,
    counts: Vec<u64>,
}

#[cfg(test)]
impl PreparedFacetSegmentCollector {
    fn shares_mapping_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.mapping, &other.mapping)
    }
}

impl SegmentCollector for PreparedFacetSegmentCollector {
    type Fruit = PreparedFacetCounts;

    fn collect(&mut self, doc: DocId, _: Score) {
        let mut previous_collapsed_id = NO_COLLAPSED_FACET;
        // Cold facet evidence put more than 99% in collection. Reusing only the
        // immutable segment plan preserves per-document counts; the prepared-mapping
        // unit test and executor facet parity fail if collapse or deduplication changes.
        for facet_ord in self.reader.facet_ords(doc) {
            let collapsed_id = self.mapping.collapsed_id_by_facet_ord[facet_ord as usize];
            if collapsed_id != NO_COLLAPSED_FACET && collapsed_id != previous_collapsed_id {
                self.counts[collapsed_id] += 1;
            }
            previous_collapsed_id = collapsed_id;
        }
    }

    fn harvest(self) -> Self::Fruit {
        let facet_counts = self
            .mapping
            .collapsed_facets
            .iter()
            .cloned()
            .zip(self.counts)
            .filter(|(_, count)| *count > 0)
            .collect();
        PreparedFacetCounts { facet_counts }
    }
}

impl Collector for PreparedFacetCollector {
    type Fruit = PreparedFacetCounts;
    type Child = PreparedFacetSegmentCollector;

    fn for_segment(
        &self,
        _: SegmentOrdinal,
        reader: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let facet_reader = reader.facet_reader(&self.field_name)?;
        let mapping = self.mapping_for_segment(reader, &facet_reader)?;
        let counts = vec![0; mapping.collapsed_facets.len()];
        Ok(PreparedFacetSegmentCollector {
            reader: facet_reader,
            mapping,
            counts,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, segment_counts: Vec<Self::Fruit>) -> tantivy::Result<Self::Fruit> {
        let mut facet_counts = BTreeMap::new();
        for segment in segment_counts {
            for (facet, count) in segment.facet_counts {
                *facet_counts.entry(facet).or_insert(0) += count;
            }
        }
        Ok(PreparedFacetCounts { facet_counts })
    }
}

#[derive(Clone, Default)]
pub(crate) struct PreparedFacetCounts {
    facet_counts: BTreeMap<Facet, u64>,
}

impl PreparedFacetCounts {
    pub(crate) fn get<T>(&self, facet_from: T) -> impl Iterator<Item = (&Facet, u64)>
    where
        Facet: From<T>,
    {
        let facet = Facet::from(facet_from);
        let lower_bound = Bound::Excluded(facet.clone());
        let upper_bound = if facet.is_root() {
            Bound::Unbounded
        } else {
            let mut encoded_after = facet.encoded_str().to_owned();
            encoded_after.push('\u{1}');
            let facet_after = Facet::from_encoded(encoded_after.into_bytes())
                .expect("facet bounds are valid UTF-8");
            Bound::Excluded(facet_after)
        };
        self.facet_counts
            .range((lower_bound, upper_bound))
            .map(|(facet, count)| (facet, *count))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FacetMappingCache, FacetMappingKey, PreparedFacetCollector, PreparedFacetMapping,
        NO_COLLAPSED_FACET,
    };
    use std::sync::Arc;
    use tantivy::collector::Collector;
    use tantivy::query::AllQuery;
    use tantivy::schema::{Facet, FacetOptions, Schema};
    use tantivy::{doc, Index};

    #[test]
    fn prepared_mapping_is_reused_without_changing_hierarchical_counts() {
        let mut schema_builder = Schema::builder();
        let facet_field = schema_builder.add_facet_field("_facets", FacetOptions::default());
        let index = Index::create_in_ram(schema_builder.build());
        let mut writer = index.writer(15_000_000).unwrap();
        writer
            .add_document(doc!(
                facet_field => Facet::from("/category/electronics/phones"),
                facet_field => Facet::from("/category/electronics/laptops"),
                facet_field => Facet::from("/brand/Acme"),
            ))
            .unwrap();
        writer
            .add_document(doc!(
                facet_field => Facet::from("/category/electronics/phones"),
                facet_field => Facet::from("/brand/Other"),
            ))
            .unwrap();
        writer
            .add_document(doc!(
                facet_field => Facet::from("/category/books/fiction"),
                facet_field => Facet::from("/brand/Acme"),
            ))
            .unwrap();
        writer.commit().unwrap();

        let searcher = index.reader().unwrap().searcher();
        let mut collector = PreparedFacetCollector::for_field("_facets");
        collector.add_facet("/category");
        collector.add_facet("/brand");

        let first = collector
            .for_segment(0, searcher.segment_reader(0))
            .unwrap();
        let second = collector
            .for_segment(0, searcher.segment_reader(0))
            .unwrap();
        assert!(first.shares_mapping_with(&second));

        let counts = searcher.search(&AllQuery, &collector).unwrap();
        let categories: Vec<(String, u64)> = counts
            .get("/category")
            .map(|(facet, count)| (facet.to_path_string(), count))
            .collect();
        let brands: Vec<(String, u64)> = counts
            .get("/brand")
            .map(|(facet, count)| (facet.to_path_string(), count))
            .collect();
        assert_eq!(
            categories,
            vec![
                ("/category/books".to_string(), 1),
                ("/category/electronics".to_string(), 2),
            ]
        );
        assert_eq!(
            brands,
            vec![
                ("/brand/Acme".to_string(), 2),
                ("/brand/Other".to_string(), 1),
            ]
        );
    }

    #[test]
    fn prepared_mapping_cache_enforces_entry_and_byte_limits() {
        let mut cache = FacetMappingCache {
            max_entries: 2,
            max_bytes: 1_000,
            ..FacetMappingCache::default()
        };
        for _ in 0..3 {
            let key = FacetMappingKey {
                segment_id: tantivy::index::SegmentId::generate_random(),
                field_name: "_facets".to_string(),
                roots: vec![Facet::from("/category")],
            };
            let mapping = Arc::new(PreparedFacetMapping {
                collapsed_id_by_facet_ord: vec![NO_COLLAPSED_FACET],
                collapsed_facets: Vec::new(),
            });
            cache.insert(key, mapping);
        }
        assert_eq!(cache.entries.len(), 2);
        assert!(cache.estimated_bytes <= cache.max_bytes);

        let entries_before_oversized_insert = cache.entries.len();
        let oversized_key = FacetMappingKey {
            segment_id: tantivy::index::SegmentId::generate_random(),
            field_name: "_facets".to_string(),
            roots: vec![Facet::from("/category")],
        };
        let oversized_mapping = Arc::new(PreparedFacetMapping {
            collapsed_id_by_facet_ord: vec![NO_COLLAPSED_FACET; 200],
            collapsed_facets: Vec::new(),
        });
        cache.insert(oversized_key, oversized_mapping);
        assert_eq!(cache.entries.len(), entries_before_oversized_insert);
    }
}
