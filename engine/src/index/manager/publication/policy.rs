/// NODE-LOCAL artifact disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactDisposition {
    Preserve,
    Journal,
    PostcommitRebuild,
    Reject,
}

/// NODE-LOCAL owner documented artifact policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPolicy {
    pub key: &'static str,
    pub owner: &'static str,
    pub disposition: ArtifactDisposition,
    pub root_resolver: &'static str,
    pub rationale: &'static str,
    pub source: &'static str,
    pub prior_destination: &'static str,
    pub promoted_destination: &'static str,
    pub rollback: &'static str,
    pub repair: &'static str,
}

/// NODE-LOCAL artifact policy table.
pub fn artifact_policy_table() -> &'static [ArtifactPolicy] {
    &ARTIFACT_POLICIES
}

macro_rules! artifact_policy {
    ($key:literal, $owner:literal, $disposition:expr, $root:literal, $why:literal, $source:literal, $prior:literal, $promoted:literal, $rollback:literal, $repair:literal) => {
        ArtifactPolicy {
            key: $key,
            owner: $owner,
            disposition: $disposition,
            root_resolver: $root,
            rationale: $why,
            source: $source,
            prior_destination: $prior,
            promoted_destination: $promoted,
            rollback: $rollback,
            repair: $repair,
        }
    };
}

static ARTIFACT_POLICIES: [ArtifactPolicy; 13] = [
    artifact_policy!(
        "tenant_tantivy",
        "engine/src/index/mod.rs",
        ArtifactDisposition::Preserve,
        "Tantivy managed-directory metadata",
        "Dynamic segment files move only when owner evidence lists them.",
        "tenant directory",
        "backup tenant directory",
        "target tenant directory",
        "restore tenant directory backup",
        "re-open Tantivy from preserved managed files"
    ),
    artifact_policy!(
        "index_metadata",
        "engine/src/index/index_metadata.rs",
        ArtifactDisposition::Preserve,
        "METADATA_FILE",
        "Metadata is a tenant child and moves with the directory.",
        "tenant/index_meta.json",
        "backup tenant directory",
        "target tenant directory",
        "restore tenant directory backup",
        "load metadata from promoted tenant"
    ),
    artifact_policy!(
        "settings_rules_synonyms",
        "engine/src/index/manager/config.rs",
        ArtifactDisposition::Preserve,
        "SETTINGS_FILE/RULES_FILE/SYNONYMS_FILE",
        "Config files are durable tenant children; caches rebuild after commit.",
        "tenant config files",
        "backup tenant directory",
        "target tenant directory",
        "restore tenant directory backup",
        "invalidate and reload config caches"
    ),
    artifact_policy!(
        "oplog",
        "engine/src/index/oplog.rs",
        ArtifactDisposition::Preserve,
        "OPLOG_DIR and COMMITTED_SEQ_FILE",
        "Local write recovery evidence moves with the node-local tenant directory.",
        "tenant/oplog and tenant/committed_seq",
        "backup tenant directory",
        "target tenant directory",
        "restore tenant directory backup",
        "replay from committed_seq"
    ),
    artifact_policy!(
        "vectors",
        "engine/src/index/write_queue/finalization.rs",
        ArtifactDisposition::Preserve,
        "PERSISTED_VECTORS_DIR",
        "Persisted vector files are tenant children; in-memory vector maps rebuild.",
        "tenant/vectors",
        "backup tenant directory",
        "target tenant directory",
        "restore tenant directory backup",
        "reload or rebuild vector state"
    ),
    artifact_policy!(
        "dictionaries",
        "engine/src/dictionaries/persistence.rs",
        ArtifactDisposition::Preserve,
        "DICTIONARIES_DIR",
        "Dictionary data is per tenant and moves with the tenant directory.",
        "tenant/.dictionaries",
        "backup tenant directory",
        "target tenant directory",
        "restore tenant directory backup",
        "reload dictionary store"
    ),
    artifact_policy!(
        "recommend_rules",
        "engine/src/recommend/rules.rs",
        ArtifactDisposition::Preserve,
        "RECOMMEND_RULES_DIR",
        "Recommend rules are tenant children and move with the tenant directory.",
        "tenant/recommend_rules",
        "backup tenant directory",
        "target tenant directory",
        "restore tenant directory backup",
        "reload recommend rules"
    ),
    artifact_policy!(
        "query_suggestions",
        "engine/src/query_suggestions/config.rs",
        ArtifactDisposition::Journal,
        "QsConfigStore::target_artifact_paths",
        "Target-keyed sibling files outside the tenant dir require journaled mutation.",
        ".query_suggestions target files",
        "journaled prior target-keyed siblings",
        "journaled promoted target-keyed siblings",
        "restore journaled query suggestion siblings",
        "complete or undo journaled sibling mutation"
    ),
    artifact_policy!(
        "analytics",
        "engine/src/analytics/config.rs",
        ArtifactDisposition::Journal,
        "AnalyticsConfig::target_artifact_paths",
        "Independently configured analytics roots must not stay under the wrong target key.",
        "analytics target-keyed root",
        "journaled prior analytics root",
        "journaled promoted analytics root",
        "restore journaled analytics root",
        "complete or undo journaled analytics mutation"
    ),
    artifact_policy!(
        "runtime_index_manager_maps",
        "engine/src/index/manager/mod.rs",
        ArtifactDisposition::PostcommitRebuild,
        "IndexManager runtime maps",
        "loaded, writer, queue, oplog, cache, LWW, and vector maps are in-memory state.",
        "process memory",
        "not durable",
        "rebuilt after commit",
        "drop invalidated runtime entries",
        "reload from durable owners"
    ),
    artifact_policy!(
        "facet_cache",
        "engine/src/index/manager/config.rs",
        ArtifactDisposition::PostcommitRebuild,
        "IndexManager::invalidate_facet_cache",
        "Facet entries are cache state keyed by tenant.",
        "process memory",
        "not durable",
        "rebuilt on query",
        "drop invalidated cache entries",
        "recompute on demand"
    ),
    artifact_policy!(
        "experiments",
        "engine/src/experiments/store.rs",
        ArtifactDisposition::Reject,
        "ExperimentStore global records",
        "Experiment records are global lifecycle records, not publication-owned target sidecars.",
        ".experiments",
        "not mutated by publication",
        "not mutated by publication",
        "no publication mutation",
        "experiment owner reconciles records"
    ),
    artifact_policy!(
        "unknown_artifacts",
        "engine/src/index/manager/publication.rs",
        ArtifactDisposition::Reject,
        "closed inventory",
        "Fail closed instead of guessing from names, extensions, or directory shape.",
        "unclassified path",
        "rejected",
        "rejected",
        "rejected before mutation",
        "add an owner descriptor and test"
    ),
];
