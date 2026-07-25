use super::{
    acknowledge_algolia_migration, authenticated_owner_identity,
    import::{
        wait_for_live_import_barrier_with_timeout, ImportTestHooks, LiveImportBarrier,
        PrivacyScrubBoundary, PrivacyScrubTestHooks, LIVE_IMPORT_BARRIER_OBSERVED_FILE,
        LIVE_IMPORT_BARRIER_RELEASE_FILE, LIVE_IMPORT_POST_COMMIT_BARRIER_DIR_ENV,
        LIVE_IMPORT_POST_COMMIT_SOURCE_ENV, LIVE_IMPORT_PRE_ACTIVATION_BARRIER_DIR_ENV,
        LIVE_IMPORT_PRE_ACTIVATION_SOURCE_ENV,
    },
    migrate_from_algolia_with_test_source_factory,
    migrate_from_algolia_with_test_source_factory_and_hooks, AsyncMigrationStatusResponse,
    MigrateFromAlgoliaRequest, MigrateFromAlgoliaResponse, MIGRATION_HA_UNSUPPORTED_CODE,
    MIGRATION_HA_UNSUPPORTED_MESSAGE,
};
use crate::auth::{ApiKey, AuthenticatedAppId, KeyStore, PRIVATE_MIGRATION_ACL};
use crate::handlers::indices::list_indices;
use crate::handlers::migration::algolia_client::{
    AlgoliaClientError, AlgoliaErrorKind, AlgoliaIndexRecord,
};
use crate::handlers::migration::source_reader::{
    MigrationSourceReader, PageConsumer, SourceFuture,
};
use crate::handlers::migration::source_test_support::{
    sorted_exact_hits_by_object_id, ScriptedSourceReader,
};
use crate::handlers::migration::spool::AsyncMigrationPublicationSemantic;
use crate::handlers::migration::spool::{
    MigrationCancelRequest, MigrationDisposition, MigrationExportProgress, MigrationPhase,
    MigrationPhaseRecord, SpoolErrorKind, SpoolLimits, SpoolStore,
};
use crate::test_helpers::{body_json, EnvVarRestoreGuard, TestStateBuilder, ENV_MUTEX};
use axum::{
    body::Body,
    extract::{Extension, Query, State},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode},
    response::IntoResponse,
    Json,
};
use flapjack::analytics::{AnalyticsCollector, AnalyticsConfig};
use flapjack::index::manager::publication::{
    ContentDigest, PublicationEvent, PublicationGenerationEvidence, PublicationJournal,
    PublicationPaths, PublicationTarget, PublicationTransactionId,
};
use flapjack::{
    error::FlapjackError,
    types::{Document, FieldValue},
};
use flapjack_replication::{
    config::{NodeConfig, PeerConfig},
    manager::ReplicationManager,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier, Mutex,
    },
    thread,
    time::Duration,
};
use tempfile::TempDir;
use tokio::sync::Notify;
use tower::ServiceExt;

#[path = "import_contract_recovery_tests.rs"]
mod import_contract_recovery_tests;
#[path = "import_contract_replica_tests.rs"]
mod import_contract_replica_tests;
#[path = "import_contract_test_support.rs"]
mod import_contract_test_support;
use import_contract_test_support::{
    assert_no_retained_accepted_spool_document_artifacts, assert_object_fields,
    assert_preexisting_target_resources, assert_preexisting_target_resources_exactly_absent,
    assert_query_returns_document, assert_spool_lifecycle_with_artifacts,
    assert_target_absent_from_disk_and_list, directory_snapshot, query_hit_count,
    seed_preexisting_target_resources,
};

const SOURCE_APP_ID: &str = "LOCALMIGRATIONTEST";
const SOURCE_API_KEY: &str = "hermetic-source-key-not-used";
const SOURCE_INDEX: &str = "source_products";
const TARGET_INDEX: &str = "migrated_products";
const ASYNC_REPLACE_TARGET: &str = "async_replace_products";
const ASYNC_REPLACE_OWNER_APP_ID: &str = "async-replace-owner";
const ASYNC_REPLACE_OWNER_API_KEY: &str = "async-replace-owner-key";
const EXPECTED_DOCUMENTS: [(&str, &str, &str); 2] = [
    ("doc-1", "Quartz adapter", "hardware"),
    ("doc-2", "Velvet compass", "navigation"),
];
const ASYNC_REPLACE_INITIAL_DOCUMENTS: [(&str, &str, &str, &str); 3] = [
    ("doc-1", "Quartz adapter", "hardware", "initial"),
    ("doc-2", "Velvet compass", "navigation", "initial"),
    ("doc-3", "Cedar caliper", "hardware", "initial"),
];
const ASYNC_REPLACE_FINAL_DOCUMENTS: [(&str, &str, &str, &str); 3] = [
    ("doc-1", "Quartz adapter", "hardware", "replacement"),
    (
        "doc-2",
        "Velvet compass revised",
        "navigation",
        "replacement",
    ),
    ("doc-4", "Ivory beacon", "navigation", "replacement"),
];
const LIVE_IMPORT_BARRIER_SOURCE: &str = "live-import-barrier-source";
const PRIVACY_SCRUB_ROUTE: &str = "/1/migrations/privacy-scrub";
const PRIVACY_SCRUB_ID: &str = "privacy-scrub-stable-contract-id";
const PRIVACY_SCRUB_TARGET: &str = "privacy_scrub_contract_target";
const PRIVACY_SCRUB_CURRENT_GENERATION: &str = "generation-current";
const PRIVACY_SCRUB_BOUNDARY_TIMEOUT: Duration = Duration::from_secs(2);
const PRIVACY_SCRUB_OWNER_APP_ID: &str = "privacy-scrub-contract-app";
const PRIVACY_SCRUB_INTENT_FILE: &str = "privacy_scrub_intent.json";

// Closed crash-safety matrix (5/5 public/test-local failure seams):
// 1. translation hard rejection;
// 2. corrupt accepted spool artifact;
// 3. staging document writer failure;
// 4. source read failure after a committed page;
// 5. prepared staging handle dropped by a pre-activation unwind.

fn privacy_scrub_payload(scrub_id: &str, expected_generation: &str) -> serde_json::Value {
    json!({
        "scrubId": scrub_id,
        "tenant": PRIVACY_SCRUB_TARGET,
        "expectedGeneration": expected_generation,
        "objectIDs": ["kept-1"],
        "synonymIDs": ["kept-synonym"],
        "ruleIDs": ["kept-rule"]
    })
}

fn privacy_scrub_private_key(key_store: &KeyStore) -> String {
    let key = ApiKey {
        hash: String::new(),
        salt: String::new(),
        hmac_key: None,
        created_at: 0,
        acl: vec![PRIVATE_MIGRATION_ACL.to_string()],
        description: "Private privacy scrub test key".to_string(),
        indexes: vec![],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        restrict_sources: None,
        validity: 0,
    };
    key_store.create_key(key).1
}

fn privacy_scrub_owner_identity(api_key: &str) -> String {
    format!(
        "{PRIVACY_SCRUB_OWNER_APP_ID}:{}",
        hex::encode(Sha256::digest(api_key.as_bytes()))
    )
}

fn write_current_generation_evidence(base: &Path, target_index: &str, generation: &str) {
    let target = PublicationTarget::new(target_index).unwrap();
    let transaction = PublicationTransactionId::new(format!("{target_index}_current_gen")).unwrap();
    let paths = PublicationPaths::new(base, &target, &transaction);
    let journal = PublicationJournal::prepare(
        transaction,
        target,
        PublicationGenerationEvidence::new(generation).unwrap(),
        ContentDigest::new(format!("sha256:{}", "0".repeat(64))).unwrap(),
        paths.clone(),
    )
    .apply(PublicationEvent::Commit)
    .unwrap();

    fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    fs::write(paths.journal, journal.to_json_value().to_string()).unwrap();
}

fn seed_privacy_scrub_intent_fixture(
    base: &Path,
    owner_identity: &str,
    scrub_id: &str,
    expected_generation: &str,
    phase: MigrationPhase,
) -> uuid::Uuid {
    let spool = SpoolStore::new(base, SpoolLimits::default()).unwrap();
    let job_uuid = uuid::Uuid::new_v4();
    spool
        .create_async_migration_admission_for_owner(
            job_uuid,
            PRIVACY_SCRUB_TARGET,
            Some(owner_identity),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    for next_phase in [
        MigrationPhase::Exporting,
        MigrationPhase::Preparing,
        MigrationPhase::Staging,
        MigrationPhase::Activating,
    ] {
        if next_phase.order_for_privacy_scrub_test() > phase.order_for_privacy_scrub_test() {
            break;
        }
        spool
            .transition_migration_phase(job_uuid, next_phase)
            .unwrap();
    }
    let intent = json!({
        "kind": "privacy_scrub_intent",
        "scrubId": scrub_id,
        "tenant": PRIVACY_SCRUB_TARGET,
        "expectedGeneration": expected_generation,
        "objectIDs": ["kept-1"],
        "synonymIDs": ["kept-synonym"],
        "ruleIDs": ["kept-rule"],
        "authenticatedAppId": owner_identity,
        "createdAt": "2026-07-24T00:00:00Z"
    });
    fs::write(
        spool.job_dir(job_uuid).join(PRIVACY_SCRUB_INTENT_FILE),
        intent.to_string(),
    )
    .unwrap();
    job_uuid
}

fn read_privacy_scrub_intent(spool: &SpoolStore, job_uuid: uuid::Uuid) -> serde_json::Value {
    let bytes = fs::read(spool.job_dir(job_uuid).join(PRIVACY_SCRUB_INTENT_FILE))
        .expect("privacy scrub intent must be durable in the migration spool");
    serde_json::from_slice(&bytes).expect("privacy scrub intent must be valid JSON")
}

fn assert_privacy_scrub_intent(
    intent: &serde_json::Value,
    owner_identity: &str,
    scrub_id: &str,
    expected_generation: &str,
) {
    assert_eq!(intent["kind"], "privacy_scrub_intent");
    assert_eq!(intent["scrubId"], scrub_id);
    assert_eq!(intent["tenant"], PRIVACY_SCRUB_TARGET);
    assert_eq!(intent["expectedGeneration"], expected_generation);
    assert_eq!(intent["objectIDs"], json!(["kept-1"]));
    assert_eq!(intent["synonymIDs"], json!(["kept-synonym"]));
    assert_eq!(intent["ruleIDs"], json!(["kept-rule"]));
    assert_eq!(
        intent["authenticatedAppId"], owner_identity,
        "durable scrub intent must retain its authenticated owner"
    );
    let created_at = intent["createdAt"]
        .as_str()
        .expect("durable scrub intent must contain a timestamp");
    chrono::DateTime::parse_from_rfc3339(created_at)
        .expect("durable scrub intent timestamp must be RFC 3339");
}

trait PrivacyScrubPhaseOrder {
    fn order_for_privacy_scrub_test(self) -> u8;
}

impl PrivacyScrubPhaseOrder for MigrationPhase {
    fn order_for_privacy_scrub_test(self) -> u8 {
        match self {
            MigrationPhase::Submitted => 0,
            MigrationPhase::Exporting => 1,
            MigrationPhase::Preparing => 2,
            MigrationPhase::Staging => 3,
            MigrationPhase::Activating => 4,
        }
    }
}

fn privacy_scrub_test_router(
    tmp: &TempDir,
    state: Arc<crate::handlers::AppState>,
    key_store: Arc<KeyStore>,
) -> axum::Router {
    let analytics_config = AnalyticsConfig {
        enabled: false,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 60,
        flush_size: 1000,
        retention_days: 30,
    };
    let analytics_collector = AnalyticsCollector::new(analytics_config);
    let trusted_proxy_matcher =
        Arc::new(crate::middleware::TrustedProxyMatcher::from_optional_csv(None).unwrap());

    crate::router::build_router(
        state,
        Some(key_store),
        analytics_collector,
        trusted_proxy_matcher,
        tmp.path(),
        crate::router::RouterConfig {
            cors_mode: crate::startup::CorsMode::LoopbackOnly,
            disable_dashboard: false,
        },
    )
}

fn privacy_scrub_test_router_with_hooks(
    tmp: &TempDir,
    state: Arc<crate::handlers::AppState>,
    key_store: Arc<KeyStore>,
    hooks: Arc<PrivacyScrubTestHooks>,
) -> axum::Router {
    privacy_scrub_test_router(tmp, state, key_store).layer(Extension(hooks))
}

async fn post_privacy_scrub(
    app: &axum::Router,
    api_key: &str,
    payload: serde_json::Value,
) -> axum::response::Response {
    post_privacy_scrub_with_credentials(
        app,
        Some(PRIVACY_SCRUB_OWNER_APP_ID),
        Some(api_key),
        payload,
    )
    .await
}

async fn post_privacy_scrub_with_credentials(
    app: &axum::Router,
    app_id: Option<&str>,
    api_key: Option<&str>,
    payload: serde_json::Value,
) -> axum::response::Response {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(PRIVACY_SCRUB_ROUTE)
        .header("content-type", "application/json");
    if let Some(app_id) = app_id {
        request = request.header("x-algolia-application-id", app_id);
    }
    if let Some(api_key) = api_key {
        request = request.header("x-algolia-api-key", api_key);
    }

    app.clone()
        .oneshot(request.body(Body::from(payload.to_string())).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn privacy_scrub_auth_failures_leave_no_intent_or_target_mutation() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let private_key = privacy_scrub_private_key(&key_store);
    seed_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
    let app = privacy_scrub_test_router(&tmp, Arc::clone(&state), key_store);
    let payload = privacy_scrub_payload(PRIVACY_SCRUB_ID, "generation-auth-refusal");

    let refusal_credentials = [
        ("missing credentials", None, None),
        (
            "wrong credentials",
            Some(PRIVACY_SCRUB_OWNER_APP_ID),
            Some("wrong-key"),
        ),
        ("incomplete app material", None, Some(private_key.as_str())),
        (
            "ordinary admin credentials",
            Some(PRIVACY_SCRUB_OWNER_APP_ID),
            Some("admin-key"),
        ),
    ];

    for (case, app_id, api_key) in refusal_credentials {
        let response =
            post_privacy_scrub_with_credentials(&app, app_id, api_key, payload.clone()).await;

        assert_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
        assert!(
            privacy_scrub_spool_jobs(&state.manager.base_path).is_empty(),
            "{case} must not create a durable scrub intent"
        );
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{case} must fail closed at the real migration router before reaching the scrub handler"
        );
    }
}

#[tokio::test]
async fn privacy_scrub_rejects_same_scrub_id_from_different_private_key() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let first_private_key = privacy_scrub_private_key(&key_store);
    let second_private_key = privacy_scrub_private_key(&key_store);
    seed_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
    write_current_generation_evidence(
        &state.manager.base_path,
        PRIVACY_SCRUB_TARGET,
        "generation-owner-fingerprint",
    );
    let app = privacy_scrub_test_router(&tmp, Arc::clone(&state), key_store);
    let payload = privacy_scrub_payload(PRIVACY_SCRUB_ID, "generation-owner-fingerprint");

    let first = post_privacy_scrub(&app, &first_private_key, payload.clone()).await;
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first_ack = body_json(first).await;
    assert_eq!(first_ack["scrubId"], PRIVACY_SCRUB_ID);
    assert_eq!(first_ack["disposition"], "acknowledged");

    let second = post_privacy_scrub(&app, &second_private_key, payload).await;
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(second).await,
        json!({
            "message": "Privacy scrub intent identity does not match",
            "status": 409,
            "code": "privacy_scrub_mismatched_intent"
        })
    );

    let spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    assert_eq!(
        spool.job_uuids().unwrap().len(),
        1,
        "a second private key must not be able to replay or duplicate another key's scrub ownership"
    );
}

#[tokio::test]
async fn privacy_scrub_durability_ack_is_idempotent_after_exact_absence() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let private_key = privacy_scrub_private_key(&key_store);
    seed_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
    write_current_generation_evidence(
        &state.manager.base_path,
        PRIVACY_SCRUB_TARGET,
        "generation-exact-absence",
    );
    assert_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
    let app = privacy_scrub_test_router(&tmp, Arc::clone(&state), key_store);
    let payload = privacy_scrub_payload(PRIVACY_SCRUB_ID, "generation-exact-absence");

    let first = post_privacy_scrub(&app, &private_key, payload.clone()).await;
    assert_eq!(
        first.status(),
        StatusCode::ACCEPTED,
        "first scrub delivery must durably admit the private command"
    );
    let first_ack = body_json(first).await;
    assert_eq!(first_ack["scrubId"], PRIVACY_SCRUB_ID);
    assert_eq!(first_ack["disposition"], "acknowledged");
    assert_preexisting_target_resources_exactly_absent(&state, PRIVACY_SCRUB_TARGET).await;

    let duplicate = post_privacy_scrub(&app, &private_key, payload).await;
    assert_eq!(
        duplicate.status(),
        StatusCode::ACCEPTED,
        "duplicate delivery for the same scrub ID must replay the earned ACK"
    );
    assert_eq!(body_json(duplicate).await, first_ack);
}

#[tokio::test]
async fn privacy_scrub_durability_concurrent_duplicate_gets_no_false_ack() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let private_key = privacy_scrub_private_key(&key_store);
    seed_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
    write_current_generation_evidence(
        &state.manager.base_path,
        PRIVACY_SCRUB_TARGET,
        "generation-concurrent-duplicate",
    );
    let hooks = Arc::new(
        PrivacyScrubTestHooks::default().with_boundaries([PrivacyScrubBoundary::PostIntent]),
    );
    let app = privacy_scrub_test_router_with_hooks(
        &tmp,
        Arc::clone(&state),
        key_store,
        Arc::clone(&hooks),
    );
    let payload = privacy_scrub_payload(PRIVACY_SCRUB_ID, "generation-concurrent-duplicate");
    let first_app = app.clone();
    let first_key = private_key.clone();
    let first_payload = payload.clone();
    let mut first =
        tokio::spawn(
            async move { post_privacy_scrub(&first_app, &first_key, first_payload).await },
        );

    observe_privacy_scrub_boundary(&mut first, &hooks, PrivacyScrubBoundary::PostIntent).await;
    let spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    let jobs = spool.job_uuids().unwrap();
    assert_eq!(
        jobs.len(),
        1,
        "first delivery must own the only durable job"
    );
    assert_eq!(
        spool.read_migration_phase(jobs[0]).unwrap().disposition,
        MigrationDisposition::Running
    );
    assert_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;

    let duplicate = post_privacy_scrub(&app, &private_key, payload).await;
    assert_eq!(
        duplicate.status(),
        StatusCode::CONFLICT,
        "duplicate delivery while the first scrub is running must not ACK"
    );
    assert_eq!(
        body_json(duplicate).await["code"],
        "privacy_scrub_interrupted_retryable"
    );
    assert_eq!(
        SpoolStore::new(&state.manager.base_path, SpoolLimits::default())
            .unwrap()
            .job_uuids()
            .unwrap(),
        jobs,
        "running duplicate must not create a second durable job"
    );
    assert_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;

    release_privacy_scrub_boundary(&hooks, PrivacyScrubBoundary::PostIntent);
    let first = tokio::time::timeout(PRIVACY_SCRUB_BOUNDARY_TIMEOUT, first)
        .await
        .expect("first scrub must finish after duplicate refusal")
        .expect("first scrub task must not panic");
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    assert_preexisting_target_resources_exactly_absent(&state, PRIVACY_SCRUB_TARGET).await;
}

#[tokio::test]
async fn privacy_scrub_durability_refuses_stale_unknown_and_mismatched_intents() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let private_key = privacy_scrub_private_key(&key_store);
    let owner_identity = privacy_scrub_owner_identity(&private_key);
    seed_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
    write_current_generation_evidence(
        &state.manager.base_path,
        PRIVACY_SCRUB_TARGET,
        PRIVACY_SCRUB_CURRENT_GENERATION,
    );
    seed_privacy_scrub_intent_fixture(
        &state.manager.base_path,
        &owner_identity,
        PRIVACY_SCRUB_ID,
        PRIVACY_SCRUB_CURRENT_GENERATION,
        MigrationPhase::Submitted,
    );
    let interrupted_job = seed_privacy_scrub_intent_fixture(
        &state.manager.base_path,
        &owner_identity,
        "interrupted-retryable-scrub",
        PRIVACY_SCRUB_CURRENT_GENERATION,
        MigrationPhase::Staging,
    );
    let app = privacy_scrub_test_router(&tmp, Arc::clone(&state), key_store);

    for (payload, expected_code) in [
        (
            privacy_scrub_payload("unknown-target-scrub", "generation-current")
                .as_object()
                .cloned()
                .map(|mut body| {
                    body.insert("tenant".to_string(), json!("missing_privacy_tenant"));
                    serde_json::Value::Object(body)
                })
                .unwrap(),
            "privacy_scrub_unknown_target",
        ),
        (
            privacy_scrub_payload("stale-generation-scrub", "stale-generation"),
            "privacy_scrub_stale_generation",
        ),
        (
            privacy_scrub_payload("different-id-same-intent", "generation-current"),
            "privacy_scrub_mismatched_intent",
        ),
    ] {
        let response = post_privacy_scrub(&app, &private_key, payload).await;
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "refused privacy scrub must remain retryable and never claim success"
        );
        let body = body_json(response).await;
        assert_eq!(body["code"], expected_code);
    }

    let spool_before = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    let phase_before = spool_before.read_migration_phase(interrupted_job).unwrap();
    let intent_before = read_privacy_scrub_intent(&spool_before, interrupted_job);
    assert_eq!(phase_before.phase, MigrationPhase::Staging);
    assert_eq!(phase_before.disposition, MigrationDisposition::Running);
    assert!(
        phase_before.terminal_at.is_none(),
        "interrupted fixture must start nonterminal and retryable"
    );
    assert_privacy_scrub_intent(
        &intent_before,
        &owner_identity,
        "interrupted-retryable-scrub",
        PRIVACY_SCRUB_CURRENT_GENERATION,
    );

    let interrupted_payload = privacy_scrub_payload(
        "interrupted-retryable-scrub",
        PRIVACY_SCRUB_CURRENT_GENERATION,
    );
    for attempt in 1..=2 {
        let response = post_privacy_scrub(&app, &private_key, interrupted_payload.clone()).await;
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "interrupted privacy scrub attempt {attempt} must refuse without terminalizing"
        );
        assert_eq!(
            body_json(response).await["code"],
            "privacy_scrub_interrupted_retryable"
        );

        let reopened = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
        let retry_phase = reopened.read_migration_phase(interrupted_job).unwrap();
        assert_eq!(retry_phase.job_uuid, interrupted_job);
        assert_eq!(
            retry_phase.phase,
            MigrationPhase::Staging,
            "refusal attempt {attempt} must preserve the interrupted retry phase"
        );
        assert_eq!(
            retry_phase.disposition,
            MigrationDisposition::Running,
            "refusal attempt {attempt} must remain nonterminal"
        );
        assert!(
            retry_phase.terminal_at.is_none(),
            "refusal attempt {attempt} must not terminalize the retryable scrub"
        );
        assert!(
            !retry_phase.cancel_requested,
            "refusal attempt {attempt} must not cancel the retryable scrub"
        );
        assert_eq!(
            retry_phase.created_at, phase_before.created_at,
            "refusal attempt {attempt} must preserve the original durable admission"
        );
        assert_eq!(
            read_privacy_scrub_intent(&reopened, interrupted_job),
            intent_before,
            "refusal attempt {attempt} must preserve the durable scrub identity"
        );
    }

    assert_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
}

#[tokio::test]
async fn privacy_scrub_boundary_persists_intent_before_mutation_and_outcome_before_ack() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let private_key = privacy_scrub_private_key(&key_store);
    let owner_identity = privacy_scrub_owner_identity(&private_key);
    seed_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
    write_current_generation_evidence(
        &state.manager.base_path,
        PRIVACY_SCRUB_TARGET,
        "generation-boundary",
    );
    let hooks = Arc::new(PrivacyScrubTestHooks::default().with_boundaries([
        PrivacyScrubBoundary::PreIntent,
        PrivacyScrubBoundary::PostIntent,
        PrivacyScrubBoundary::EngineCommit,
        PrivacyScrubBoundary::PreAck,
        PrivacyScrubBoundary::ResponseLoss,
        PrivacyScrubBoundary::Restart,
        PrivacyScrubBoundary::AckReplay,
    ]));
    let app = privacy_scrub_test_router_with_hooks(
        &tmp,
        Arc::clone(&state),
        key_store,
        Arc::clone(&hooks),
    );
    let payload = privacy_scrub_payload(PRIVACY_SCRUB_ID, "generation-boundary");

    let request_app = app.clone();
    let replay_private_key = private_key.clone();
    let mut request =
        tokio::spawn(async move { post_privacy_scrub(&request_app, &private_key, payload).await });

    observe_privacy_scrub_boundary(&mut request, &hooks, PrivacyScrubBoundary::PreIntent).await;
    assert_eq!(
        privacy_scrub_spool_jobs(&state.manager.base_path).len(),
        0,
        "pre-intent boundary must occur before any durable scrub intent is admitted"
    );
    release_privacy_scrub_boundary(&hooks, PrivacyScrubBoundary::PreIntent);

    observe_privacy_scrub_boundary(&mut request, &hooks, PrivacyScrubBoundary::PostIntent).await;
    let spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    let jobs = spool.job_uuids().unwrap();
    assert_eq!(jobs.len(), 1);
    let intent_phase = spool.read_migration_phase(jobs[0]).unwrap();
    assert_eq!(intent_phase.phase, MigrationPhase::Submitted);
    assert_eq!(intent_phase.disposition, MigrationDisposition::Running);
    assert!(
        intent_phase.terminal_at.is_none(),
        "intent must be durable in the migration spool before any scrub mutation"
    );
    assert_privacy_scrub_intent(
        &read_privacy_scrub_intent(&spool, jobs[0]),
        &owner_identity,
        PRIVACY_SCRUB_ID,
        "generation-boundary",
    );
    assert_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
    release_privacy_scrub_boundary(&hooks, PrivacyScrubBoundary::PostIntent);

    observe_privacy_scrub_boundary(&mut request, &hooks, PrivacyScrubBoundary::EngineCommit).await;
    assert_preexisting_target_resources_exactly_absent(&state, PRIVACY_SCRUB_TARGET).await;
    release_privacy_scrub_boundary(&hooks, PrivacyScrubBoundary::EngineCommit);

    observe_privacy_scrub_boundary(&mut request, &hooks, PrivacyScrubBoundary::PreAck).await;
    let terminal_phase = SpoolStore::new(&state.manager.base_path, SpoolLimits::default())
        .unwrap()
        .read_migration_phase(jobs[0])
        .unwrap();
    assert_eq!(terminal_phase.disposition, MigrationDisposition::Succeeded);
    assert!(
        terminal_phase.terminal_at.is_some(),
        "durable engine outcome must be terminal before the ACK is written"
    );
    release_privacy_scrub_boundary(&hooks, PrivacyScrubBoundary::PreAck);

    observe_privacy_scrub_boundary(&mut request, &hooks, PrivacyScrubBoundary::ResponseLoss).await;
    request.abort();
    release_privacy_scrub_boundary(&hooks, PrivacyScrubBoundary::ResponseLoss);
    let cancelled = tokio::time::timeout(PRIVACY_SCRUB_BOUNDARY_TIMEOUT, request)
        .await
        .expect("aborted response-loss request must settle")
        .expect_err("response-loss request must be cancelled before its ACK is consumed");
    assert!(cancelled.is_cancelled());
    drop(app);
    drop(state);

    let rebuilt_state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let rebuilt_key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let restart_state = Arc::clone(&rebuilt_state);
    let restart_hooks = Arc::clone(&hooks);
    let mut restart = tokio::spawn(async move {
        restart_hooks.wait_at(PrivacyScrubBoundary::Restart).await;
        let reports = restart_state
            .manager
            .repair_publications_before_serve()
            .unwrap();
        restart_state
            .migration_runner
            .recover_async_jobs_before_serve(&reports)
            .await
    });
    observe_privacy_scrub_boundary(&mut restart, &hooks, PrivacyScrubBoundary::Restart).await;
    let restart_spool =
        SpoolStore::new(&rebuilt_state.manager.base_path, SpoolLimits::default()).unwrap();
    assert_eq!(
        restart_spool.read_migration_phase(jobs[0]).unwrap(),
        terminal_phase,
        "restart boundary must expose the same durable terminal outcome before recovery"
    );
    assert_privacy_scrub_intent(
        &read_privacy_scrub_intent(&restart_spool, jobs[0]),
        &owner_identity,
        PRIVACY_SCRUB_ID,
        "generation-boundary",
    );
    assert_preexisting_target_resources_exactly_absent(&rebuilt_state, PRIVACY_SCRUB_TARGET).await;
    release_privacy_scrub_boundary(&hooks, PrivacyScrubBoundary::Restart);
    tokio::time::timeout(PRIVACY_SCRUB_BOUNDARY_TIMEOUT, restart)
        .await
        .expect("restart recovery must finish after release")
        .expect("restart recovery task must not panic")
        .expect("restart recovery must reconcile the durable scrub job");

    let replay_app = privacy_scrub_test_router_with_hooks(
        &tmp,
        Arc::clone(&rebuilt_state),
        rebuilt_key_store,
        Arc::clone(&hooks),
    );
    let replay_request_app = replay_app.clone();
    let mut replay = tokio::spawn(async move {
        post_privacy_scrub(
            &replay_request_app,
            &replay_private_key,
            privacy_scrub_payload(PRIVACY_SCRUB_ID, "generation-boundary"),
        )
        .await
    });
    observe_privacy_scrub_boundary(&mut replay, &hooks, PrivacyScrubBoundary::AckReplay).await;
    let replay_spool =
        SpoolStore::new(&rebuilt_state.manager.base_path, SpoolLimits::default()).unwrap();
    assert_eq!(
        replay_spool.read_migration_phase(jobs[0]).unwrap(),
        terminal_phase,
        "ACK replay must not rewrite the terminal scrub outcome"
    );
    assert_privacy_scrub_intent(
        &read_privacy_scrub_intent(&replay_spool, jobs[0]),
        &owner_identity,
        PRIVACY_SCRUB_ID,
        "generation-boundary",
    );
    assert_preexisting_target_resources_exactly_absent(&rebuilt_state, PRIVACY_SCRUB_TARGET).await;
    release_privacy_scrub_boundary(&hooks, PrivacyScrubBoundary::AckReplay);
    let replay = tokio::time::timeout(PRIVACY_SCRUB_BOUNDARY_TIMEOUT, replay)
        .await
        .expect("ACK replay must finish after release")
        .expect("ACK replay request task must not panic");
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    let ack = body_json(replay).await;
    assert_eq!(ack["scrubId"], PRIVACY_SCRUB_ID);
    assert_eq!(ack["disposition"], "acknowledged");
}

fn privacy_scrub_spool_jobs(base_path: &Path) -> Vec<uuid::Uuid> {
    SpoolStore::new(base_path, SpoolLimits::default())
        .unwrap()
        .job_uuids()
        .unwrap()
}

async fn observe_privacy_scrub_boundary<T>(
    task: &mut tokio::task::JoinHandle<T>,
    hooks: &PrivacyScrubTestHooks,
    boundary: PrivacyScrubBoundary,
) {
    let observed = hooks.control(boundary).observed.notified();
    tokio::select! {
        result = tokio::time::timeout(PRIVACY_SCRUB_BOUNDARY_TIMEOUT, observed) => {
            result.unwrap_or_else(|_| {
                panic!(
                    "privacy scrub must observe boundary {} before timeout",
                    boundary.name()
                )
            });
        }
        result = task => {
            result.expect("privacy scrub boundary task must not panic");
            panic!(
                "privacy scrub task completed before observing boundary {}",
                boundary.name()
            );
        }
    }
}

fn release_privacy_scrub_boundary(hooks: &PrivacyScrubTestHooks, boundary: PrivacyScrubBoundary) {
    hooks.control(boundary).release.notify_one();
}

#[test]
fn live_import_barriers_are_inert_without_environment() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex poisoned");
    let _pre_source = EnvVarRestoreGuard::remove(LIVE_IMPORT_PRE_ACTIVATION_SOURCE_ENV);
    let _pre_dir = EnvVarRestoreGuard::remove(LIVE_IMPORT_PRE_ACTIVATION_BARRIER_DIR_ENV);
    let _post_source = EnvVarRestoreGuard::remove(LIVE_IMPORT_POST_COMMIT_SOURCE_ENV);
    let _post_dir = EnvVarRestoreGuard::remove(LIVE_IMPORT_POST_COMMIT_BARRIER_DIR_ENV);

    let job_uuid = uuid::Uuid::new_v4();
    wait_for_live_import_barrier_with_timeout(
        LIVE_IMPORT_BARRIER_SOURCE,
        job_uuid,
        LiveImportBarrier::PreActivation,
        Duration::from_millis(1),
    )
    .expect("pre-activation barrier must be inert by default");
    wait_for_live_import_barrier_with_timeout(
        LIVE_IMPORT_BARRIER_SOURCE,
        job_uuid,
        LiveImportBarrier::PostCommit,
        Duration::from_millis(1),
    )
    .expect("post-commit barrier must be inert by default");
}

#[test]
fn live_import_barrier_ignores_non_matching_source() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex poisoned");
    let tmp = TempDir::new().unwrap();
    let _source = EnvVarRestoreGuard::set(LIVE_IMPORT_PRE_ACTIVATION_SOURCE_ENV, "another-source");
    let _dir = EnvVarRestoreGuard::set(
        LIVE_IMPORT_PRE_ACTIVATION_BARRIER_DIR_ENV,
        tmp.path().to_str().expect("temp path should be UTF-8"),
    );

    wait_for_live_import_barrier_with_timeout(
        LIVE_IMPORT_BARRIER_SOURCE,
        uuid::Uuid::new_v4(),
        LiveImportBarrier::PreActivation,
        Duration::from_millis(1),
    )
    .expect("barrier must ignore sources it does not own");
    assert!(
        !tmp.path().join(LIVE_IMPORT_BARRIER_OBSERVED_FILE).exists(),
        "non-matching source must not create an observed file"
    );
}

#[test]
fn live_import_barrier_records_job_and_waits_for_release() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex poisoned");
    let tmp = TempDir::new().unwrap();
    let _source = EnvVarRestoreGuard::set(
        LIVE_IMPORT_PRE_ACTIVATION_SOURCE_ENV,
        LIVE_IMPORT_BARRIER_SOURCE,
    );
    let _dir = EnvVarRestoreGuard::set(
        LIVE_IMPORT_PRE_ACTIVATION_BARRIER_DIR_ENV,
        tmp.path().to_str().expect("temp path should be UTF-8"),
    );

    let job_uuid = uuid::Uuid::new_v4();
    let observed = tmp.path().join(LIVE_IMPORT_BARRIER_OBSERVED_FILE);
    let observed_for_thread = observed.clone();
    let release = tmp.path().join(LIVE_IMPORT_BARRIER_RELEASE_FILE);
    let release_thread = thread::spawn(move || {
        for _ in 0..100 {
            if observed_for_thread.exists() {
                fs::write(release, b"").unwrap();
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("live import barrier observation file was not created");
    });

    wait_for_live_import_barrier_with_timeout(
        LIVE_IMPORT_BARRIER_SOURCE,
        job_uuid,
        LiveImportBarrier::PreActivation,
        Duration::from_secs(5),
    )
    .expect("release file should unblock the import barrier");
    release_thread.join().unwrap();
    assert_eq!(fs::read_to_string(observed).unwrap(), job_uuid.to_string());
}

#[test]
fn live_import_post_commit_barrier_times_out_bounded() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex poisoned");
    let tmp = TempDir::new().unwrap();
    let _source = EnvVarRestoreGuard::set(
        LIVE_IMPORT_POST_COMMIT_SOURCE_ENV,
        LIVE_IMPORT_BARRIER_SOURCE,
    );
    let _dir = EnvVarRestoreGuard::set(
        LIVE_IMPORT_POST_COMMIT_BARRIER_DIR_ENV,
        tmp.path().to_str().expect("temp path should be UTF-8"),
    );

    let error = wait_for_live_import_barrier_with_timeout(
        LIVE_IMPORT_BARRIER_SOURCE,
        uuid::Uuid::new_v4(),
        LiveImportBarrier::PostCommit,
        Duration::from_millis(1),
    )
    .expect_err("unreleased post-commit barrier must return a bounded failure");
    assert_eq!(error.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        tmp.path().join(LIVE_IMPORT_BARRIER_OBSERVED_FILE).exists(),
        "timeout branch must still record observed job evidence"
    );
}

fn valid_request() -> MigrateFromAlgoliaRequest {
    MigrateFromAlgoliaRequest {
        app_id: SOURCE_APP_ID.to_string(),
        api_key: SOURCE_API_KEY.to_string(),
        source_index: SOURCE_INDEX.to_string(),
        target_index: Some(TARGET_INDEX.to_string()),
        overwrite: false,
    }
}

fn hermetic_source_reader() -> ScriptedSourceReader {
    hermetic_source_reader_with_documents(EXPECTED_DOCUMENTS)
}

fn hermetic_source_reader_with_documents(
    documents: [(&'static str, &'static str, &'static str); 2],
) -> ScriptedSourceReader {
    hermetic_source_reader_with_settings_and_pages(
        json!({
            "searchableAttributes": ["title"],
            "attributesForFaceting": ["category"],
        }),
        vec![scripted_documents(documents)],
    )
}

fn hermetic_source_reader_with_settings_and_pages(
    settings: serde_json::Value,
    document_pages: Vec<Vec<serde_json::Value>>,
) -> ScriptedSourceReader {
    let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
    let source_record = AlgoliaIndexRecord {
        name: SOURCE_INDEX.to_string(),
        entries: document_pages.iter().map(Vec::len).sum::<usize>() as u64,
        updated_at: "2026-07-16T00:00:00Z".to_string(),
        pending_task: false,
    };
    reader.push_quiescent(source_record.clone());
    reader.push_pass(settings.clone(), document_pages.clone(), vec![], vec![]);
    reader.push_pass(settings.clone(), document_pages, vec![], vec![]);
    reader.push_quiescent(source_record);
    // Export fetches each replica's settings before acceptance, so queue one read
    // in primary-list order for every replica carried into translation.
    if let Some(replicas) = settings.get("replicas").and_then(|value| value.as_array()) {
        let mut queued_names = HashSet::new();
        for entry in replicas {
            if let Some(raw) = entry.as_str() {
                let Ok(parsed) = flapjack::index::replica::parse_replica_entry(raw) else {
                    continue;
                };
                let name = parsed.name();
                if queued_names.insert(name.to_string()) {
                    reader.push_index_settings(name, Ok(json!({"primary": SOURCE_INDEX})));
                }
            }
        }
    }
    reader
}

fn async_replace_source_reader(
    documents: [(&'static str, &'static str, &'static str, &'static str); 3],
) -> ScriptedSourceReader {
    hermetic_source_reader_with_settings_and_pages(
        json!({
            "searchableAttributes": ["title"],
            "attributesForFaceting": ["category"],
        }),
        vec![documents
            .into_iter()
            .map(|(object_id, title, category, revision)| {
                json!({
                    "objectID": object_id,
                    "title": title,
                    "category": category,
                    "revision": revision,
                })
            })
            .collect()],
    )
}

async fn sorted_async_replace_hits(
    state: &Arc<crate::handlers::AppState>,
) -> Vec<(String, String, String, String)> {
    sorted_exact_hits_by_object_id(
        state,
        ASYNC_REPLACE_TARGET,
        10,
        "async replacement target should be queryable",
        |hit| {
            (
                hit["objectID"]
                    .as_str()
                    .expect("hit should contain objectID")
                    .to_string(),
                hit["title"]
                    .as_str()
                    .expect("hit should contain title")
                    .to_string(),
                hit["category"]
                    .as_str()
                    .expect("hit should contain category")
                    .to_string(),
                hit["revision"]
                    .as_str()
                    .expect("hit should contain revision")
                    .to_string(),
            )
        },
    )
    .await
}

fn async_replace_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-algolia-api-key",
        HeaderValue::from_static(ASYNC_REPLACE_OWNER_API_KEY),
    );
    headers
}

fn replication_manager_with_peers(peers: Vec<PeerConfig>) -> (TempDir, Arc<ReplicationManager>) {
    let data_dir = TempDir::new().unwrap();
    let manager = ReplicationManager::new(
        NodeConfig {
            node_id: "local-test-node".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers,
        },
        None,
        data_dir.path().to_path_buf(),
    );
    (data_dir, manager)
}

fn peer_configured_replication_manager() -> (TempDir, Arc<ReplicationManager>) {
    replication_manager_with_peers(vec![PeerConfig {
        node_id: "remote-test-node".to_string(),
        addr: "http://127.0.0.1:7701".to_string(),
    }])
}

fn read_migration_phase_at(base_path: &Path, job_uuid: uuid::Uuid) -> MigrationPhaseRecord {
    SpoolStore::new(base_path, SpoolLimits::default())
        .expect("spool should reopen")
        .read_migration_phase(job_uuid)
        .expect("migration phase should be directly readable")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_import_submission_returns_uuid_while_source_is_blocked_then_succeeds() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let reached_source = Arc::new(Notify::new());
    let release_source = Arc::new(Notify::new());

    let job_uuid = state
        .migration_runner
        .submit_algolia_import(valid_request(), {
            let reached_source = Arc::clone(&reached_source);
            let release_source = Arc::clone(&release_source);
            move |_| {
                Ok(BlockingSourceReader::new(
                    hermetic_source_reader(),
                    reached_source,
                    release_source,
                ))
            }
        })
        .await
        .expect("async submission should return an admitted job uuid")
        .0;

    tokio::time::timeout(std::time::Duration::from_secs(5), reached_source.notified())
        .await
        .expect("spawned import should reach the source before submission waits for completion");
    let phase = read_migration_phase_at(&state.manager.base_path, job_uuid);
    assert_eq!(phase.job_uuid, job_uuid);
    assert_eq!(phase.disposition, MigrationDisposition::Running);
    assert_eq!(state.migration_runner.active_count_for_test(), 1);

    release_source.notify_waiters();
    wait_for_terminal_phase(&state, job_uuid, MigrationDisposition::Succeeded).await;
    wait_for_active_count(&state, 0).await;
    let metadata = SpoolStore::new(&state.manager.base_path, SpoolLimits::default())
        .unwrap()
        .read_async_migration_metadata(job_uuid)
        .expect("successful async import should retain execution metadata");
    assert_eq!(metadata.job_uuid, job_uuid);
    assert_eq!(metadata.target_index, TARGET_INDEX);
    assert!(
        metadata.publication_transaction_id.is_some(),
        "publication transaction identity must be persisted before activation"
    );
    for (object_id, title, category) in EXPECTED_DOCUMENTS {
        assert_query_returns_document(&state, TARGET_INDEX, title, object_id, title, category)
            .await;
    }
    assert_eq!(
        state.migration_runner.active_count_for_test(),
        0,
        "successful async jobs must release permit and handle accounting"
    );

    let next_job_uuid = state
        .migration_runner
        .submit_algolia_import(
            MigrateFromAlgoliaRequest {
                target_index: Some("migrated_products_again".to_string()),
                ..valid_request()
            },
            |_| Ok(hermetic_source_reader()),
        )
        .await
        .expect("completed async import should return capacity for a later submission")
        .0;
    wait_for_terminal_phase(&state, next_job_uuid, MigrationDisposition::Succeeded).await;
    wait_for_active_count(&state, 0).await;
    assert_eq!(
        query_hit_count(&state, "migrated_products_again", "Quartz adapter").await,
        1
    );
}

#[tokio::test]
async fn async_import_create_then_overwrite_replaces_exact_target() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let owner_headers = async_replace_headers();
    let owner_identity =
        authenticated_owner_identity(ASYNC_REPLACE_OWNER_APP_ID.to_string(), &owner_headers);
    let initial_request = MigrateFromAlgoliaRequest {
        target_index: Some(ASYNC_REPLACE_TARGET.to_string()),
        ..valid_request()
    };

    let (initial_job_uuid, _) = state
        .migration_runner
        .submit_algolia_import_for_owner(initial_request, Some(owner_identity.clone()), |_| {
            Ok(async_replace_source_reader(ASYNC_REPLACE_INITIAL_DOCUMENTS))
        })
        .await
        .expect("initial async create should be admitted");
    wait_for_terminal_phase(&state, initial_job_uuid, MigrationDisposition::Succeeded).await;
    let initial_terminal = read_migration_phase_at(&state.manager.base_path, initial_job_uuid);
    assert_eq!(initial_terminal.phase, MigrationPhase::Activating);
    assert_eq!(
        initial_terminal.disposition,
        MigrationDisposition::Succeeded
    );
    assert!(initial_terminal.terminal_at.is_some());
    assert_eq!(
        sorted_async_replace_hits(&state).await,
        ASYNC_REPLACE_INITIAL_DOCUMENTS
            .into_iter()
            .map(|(object_id, title, category, revision)| (
                object_id.to_string(),
                title.to_string(),
                category.to_string(),
                revision.to_string(),
            ))
            .collect::<Vec<_>>()
    );

    for attempt in 1..=2 {
        let status = acknowledge_algolia_migration(
            State(Arc::clone(&state)),
            Extension(AuthenticatedAppId(ASYNC_REPLACE_OWNER_APP_ID.to_string())),
            owner_headers.clone(),
            axum::extract::Path(initial_job_uuid.to_string()),
        )
        .await
        .unwrap_or_else(|error| panic!("initial authenticated ACK {attempt} failed: {error:?}"));
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            read_migration_phase_at(&state.manager.base_path, initial_job_uuid),
            initial_terminal,
            "authenticated ACK {attempt} must not mutate the terminal phase"
        );
    }

    let replacement_request = MigrateFromAlgoliaRequest {
        target_index: Some(ASYNC_REPLACE_TARGET.to_string()),
        overwrite: true,
        ..valid_request()
    };
    let (replacement_job_uuid, _) = state
        .migration_runner
        .submit_algolia_import_for_owner(replacement_request, Some(owner_identity), |_| {
            Ok(async_replace_source_reader(ASYNC_REPLACE_FINAL_DOCUMENTS))
        })
        .await
        .expect("overwrite=true async replacement should be admitted");
    assert_ne!(
        replacement_job_uuid, initial_job_uuid,
        "replacement must receive a distinct durable job UUID"
    );

    wait_for_terminal_phase(
        &state,
        replacement_job_uuid,
        MigrationDisposition::Succeeded,
    )
    .await;
    let replacement_terminal =
        read_migration_phase_at(&state.manager.base_path, replacement_job_uuid);
    assert_eq!(replacement_terminal.phase, MigrationPhase::Activating);
    assert_eq!(
        replacement_terminal.disposition,
        MigrationDisposition::Succeeded
    );
    assert!(replacement_terminal.terminal_at.is_some());

    let wire_status = serde_json::to_value(AsyncMigrationStatusResponse::from(
        replacement_terminal.clone(),
    ))
    .expect("terminal async status should serialize");
    for forbidden_key in [
        "resumeHandle",
        "checkpointHandle",
        "resume",
        "resumable",
        "operation",
    ] {
        assert!(
            wire_status.get(forbidden_key).is_none(),
            "non-resumable async status must not advertise {forbidden_key}"
        );
    }

    for attempt in 1..=2 {
        let status = acknowledge_algolia_migration(
            State(Arc::clone(&state)),
            Extension(AuthenticatedAppId(ASYNC_REPLACE_OWNER_APP_ID.to_string())),
            owner_headers.clone(),
            axum::extract::Path(replacement_job_uuid.to_string()),
        )
        .await
        .unwrap_or_else(|error| {
            panic!("replacement authenticated ACK {attempt} failed: {error:?}")
        });
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            read_migration_phase_at(&state.manager.base_path, replacement_job_uuid),
            replacement_terminal,
            "authenticated ACK {attempt} must not mutate the replacement terminal phase"
        );
    }

    let final_hits = sorted_async_replace_hits(&state).await;
    assert_eq!(final_hits.len(), 3);
    assert_eq!(
        final_hits,
        ASYNC_REPLACE_FINAL_DOCUMENTS
            .into_iter()
            .map(|(object_id, title, category, revision)| (
                object_id.to_string(),
                title.to_string(),
                category.to_string(),
                revision.to_string(),
            ))
            .collect::<Vec<_>>()
    );
    assert!(final_hits.iter().all(|hit| hit.0 != "doc-3"));
    assert!(final_hits.iter().any(|hit| hit.0 == "doc-4"));
    assert_eq!(
        final_hits
            .iter()
            .find(|hit| hit.0 == "doc-2")
            .expect("replacement must retain doc-2")
            .3,
        "replacement"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn async_import_runner_allows_two_targets_to_reach_source_barrier_together() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let first_reached = Arc::new(Notify::new());
    let second_reached = Arc::new(Notify::new());
    let release_sources = Arc::new(Notify::new());

    let first = submit_blocked_async_import(
        &state,
        "async_target_a",
        Arc::clone(&first_reached),
        Arc::clone(&release_sources),
    )
    .await;
    let second = submit_blocked_async_import(
        &state,
        "async_target_b",
        Arc::clone(&second_reached),
        Arc::clone(&release_sources),
    )
    .await;

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        first_reached.notified().await;
        second_reached.notified().await;
    })
    .await
    .expect("both async imports should run concurrently up to the source barrier");
    assert_eq!(state.migration_runner.active_count_for_test(), 2);

    release_sources.notify_waiters();
    wait_for_terminal_phase(&state, first, MigrationDisposition::Succeeded).await;
    wait_for_terminal_phase(&state, second, MigrationDisposition::Succeeded).await;
    wait_for_active_count(&state, 0).await;
    assert_eq!(
        query_hit_count(&state, "async_target_a", "Quartz adapter").await,
        1
    );
    assert_eq!(
        query_hit_count(&state, "async_target_b", "Velvet compass").await,
        1
    );
    assert_eq!(state.migration_runner.active_count_for_test(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_import_cancel_after_export_acceptance_settles_cancelled_and_publishes_no_target() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let captured_job_uuid = Arc::new(Mutex::new(None));
    let hook_job_uuid = Arc::clone(&captured_job_uuid);
    let hooks = ImportTestHooks::default().with_after_accepted_export(move |spool, job_uuid| {
        *hook_job_uuid.lock().unwrap() = Some(job_uuid);
        spool
            .request_migration_cancel(job_uuid)
            .expect("cancel request should persist after export acceptance");
    });

    let job_uuid = state
        .migration_runner
        .submit_algolia_import_with_test_hooks(
            valid_request(),
            |_| Ok(hermetic_source_reader()),
            hooks,
        )
        .await
        .expect("async import should be admitted")
        .0;

    wait_for_terminal_phase(&state, job_uuid, MigrationDisposition::Cancelled).await;
    wait_for_active_count(&state, 0).await;
    assert_eq!(*captured_job_uuid.lock().unwrap(), Some(job_uuid));
    let phase = read_migration_phase_at(&state.manager.base_path, job_uuid);
    assert!(phase.cancel_requested);
    assert_no_retained_accepted_spool_document_artifacts(&state);
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_import_cancel_during_document_staging_drains_writer_and_releases_accounting() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let base_path = state.manager.base_path.clone();
    let captured_job_uuid = Arc::new(Mutex::new(None));
    let hook_job_uuid = Arc::clone(&captured_job_uuid);
    let staging_job_uuid = Arc::clone(&captured_job_uuid);
    let first_batch_seen = Arc::new(AtomicBool::new(false));
    let first_batch_seen_by_hook = Arc::clone(&first_batch_seen);
    let hooks = ImportTestHooks::default()
        .with_after_accepted_export(move |_spool, job_uuid| {
            *hook_job_uuid.lock().unwrap() = Some(job_uuid);
        })
        .with_before_document_batch_write(move |_| {
            if !first_batch_seen_by_hook.swap(true, Ordering::SeqCst) {
                let job_uuid = staging_job_uuid
                    .lock()
                    .unwrap()
                    .expect("accepted export hook should capture job uuid");
                SpoolStore::new(&base_path, SpoolLimits::default())
                    .unwrap()
                    .request_migration_cancel(job_uuid)
                    .expect("cancel request should persist during staging");
            }
            Ok(())
        });
    let request = MigrateFromAlgoliaRequest {
        target_index: Some("cancel_during_staging_target".to_string()),
        ..valid_request()
    };

    let job_uuid = state
        .migration_runner
        .submit_algolia_import_with_test_hooks(
            request,
            |_| {
                Ok(hermetic_source_reader_with_settings_and_pages(
                    json!({
                        "searchableAttributes": ["title"],
                        "attributesForFaceting": ["category"],
                    }),
                    vec![document_page(0, 1001)],
                ))
            },
            hooks,
        )
        .await
        .expect("async import should be admitted")
        .0;

    wait_for_terminal_phase(&state, job_uuid, MigrationDisposition::Cancelled).await;
    wait_for_active_count(&state, 0).await;
    assert!(first_batch_seen.load(Ordering::SeqCst));
    let phase = read_migration_phase_at(&state.manager.base_path, job_uuid);
    assert!(phase.cancel_requested);
    assert_no_retained_accepted_spool_document_artifacts(&state);
    assert_target_absent_from_disk_and_list(&state, "cancel_during_staging_target").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_import_cancel_before_activation_aborts_publication_transaction() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let base_path = state.manager.base_path.clone();
    let captured_job_uuid = Arc::new(Mutex::new(None));
    let hook_job_uuid = Arc::clone(&captured_job_uuid);
    let activation_job_uuid = Arc::clone(&captured_job_uuid);
    let activation_base_path = base_path.clone();
    let transaction_namespace = Arc::new(Mutex::new(None));
    let activation_namespace = Arc::clone(&transaction_namespace);
    let hooks = ImportTestHooks::default()
        .with_after_accepted_export(move |_spool, job_uuid| {
            *hook_job_uuid.lock().unwrap() = Some(job_uuid);
        })
        .with_before_activation(move || {
            let job_uuid = activation_job_uuid
                .lock()
                .unwrap()
                .expect("accepted export hook should capture job uuid");
            let spool = SpoolStore::new(&activation_base_path, SpoolLimits::default()).unwrap();
            let metadata = spool
                .read_async_migration_metadata(job_uuid)
                .expect("async metadata should persist publication transaction");
            let target = PublicationTarget::new(TARGET_INDEX.to_string()).unwrap();
            let paths = PublicationPaths::new(
                &activation_base_path,
                &target,
                metadata
                    .publication_transaction_id
                    .as_ref()
                    .expect("publication transaction should be recorded before activation"),
            );
            *activation_namespace.lock().unwrap() = paths.staging.parent().map(Path::to_path_buf);
            spool
                .request_migration_cancel(job_uuid)
                .expect("cancel request should persist before activation");
        });

    let job_uuid = state
        .migration_runner
        .submit_algolia_import_with_test_hooks(
            valid_request(),
            |_| Ok(hermetic_source_reader()),
            hooks,
        )
        .await
        .expect("async import should be admitted")
        .0;

    wait_for_terminal_phase(&state, job_uuid, MigrationDisposition::Cancelled).await;
    wait_for_active_count(&state, 0).await;
    assert_eq!(*captured_job_uuid.lock().unwrap(), Some(job_uuid));
    assert!(
        !transaction_namespace
            .lock()
            .unwrap()
            .as_ref()
            .expect("activation hook should capture transaction namespace")
            .exists(),
        "pre-activation cancellation must abort the unjournaled publication transaction"
    );
    assert_no_retained_accepted_spool_document_artifacts(&state);
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_import_precommit_cancel_preserves_preexisting_destination() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    seed_preexisting_target_resources(&state, TARGET_INDEX).await;
    state.manager.unload(&TARGET_INDEX.to_string()).unwrap();
    let before_snapshot = directory_snapshot(&state.manager.base_path.join(TARGET_INDEX));
    let base_path = state.manager.base_path.clone();
    let captured_job_uuid = Arc::new(Mutex::new(None));
    let hook_job_uuid = Arc::clone(&captured_job_uuid);
    let activation_job_uuid = Arc::clone(&captured_job_uuid);
    let activation_base_path = base_path.clone();
    let transaction_namespace = Arc::new(Mutex::new(None));
    let activation_namespace = Arc::clone(&transaction_namespace);
    let hooks = ImportTestHooks::default()
        .with_after_accepted_export(move |_spool, job_uuid| {
            *hook_job_uuid.lock().unwrap() = Some(job_uuid);
        })
        .with_before_activation(move || {
            let job_uuid = activation_job_uuid
                .lock()
                .unwrap()
                .expect("accepted export hook should capture job uuid");
            let spool = SpoolStore::new(&activation_base_path, SpoolLimits::default()).unwrap();
            let metadata = spool
                .read_async_migration_metadata(job_uuid)
                .expect("async metadata should persist publication transaction");
            let target = PublicationTarget::new(TARGET_INDEX.to_string()).unwrap();
            let paths = PublicationPaths::new(
                &activation_base_path,
                &target,
                metadata
                    .publication_transaction_id
                    .as_ref()
                    .expect("publication transaction should be recorded before activation"),
            );
            *activation_namespace.lock().unwrap() = paths.staging.parent().map(Path::to_path_buf);
            spool
                .request_migration_cancel(job_uuid)
                .expect("cancel request should persist before activation");
        });

    let job_uuid = state
        .migration_runner
        .submit_algolia_import_with_test_hooks(
            valid_request(),
            |_| Ok(hermetic_source_reader()),
            hooks,
        )
        .await
        .expect("async import should be admitted")
        .0;

    wait_for_terminal_phase(&state, job_uuid, MigrationDisposition::Cancelled).await;
    wait_for_active_count(&state, 0).await;
    assert_eq!(*captured_job_uuid.lock().unwrap(), Some(job_uuid));
    assert!(
        !transaction_namespace
            .lock()
            .unwrap()
            .as_ref()
            .expect("activation hook should capture transaction namespace")
            .exists(),
        "pre-commit cancellation must abort the unjournaled publication transaction"
    );
    assert_eq!(
        directory_snapshot(&state.manager.base_path.join(TARGET_INDEX)),
        before_snapshot,
        "pre-commit cancellation must preserve pre-existing target bytes"
    );
    assert_preexisting_target_resources(&state, TARGET_INDEX).await;
    assert_no_retained_accepted_spool_document_artifacts(&state);
    let Json(indices) = list_indices(State(Arc::clone(&state)), Query(HashMap::new()))
        .await
        .expect("index list should remain readable");
    assert_eq!(
        indices
            .items
            .iter()
            .filter(|item| item.name == TARGET_INDEX)
            .count(),
        1,
        "the target must remain listable exactly once as the pre-existing index"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_import_cancel_after_activation_is_too_late_and_keeps_committed_target() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let base_path = state.manager.base_path.clone();
    let captured_job_uuid = Arc::new(Mutex::new(None));
    let hook_job_uuid = Arc::clone(&captured_job_uuid);
    let sidecar_job_uuid = Arc::clone(&captured_job_uuid);
    let late_cancel_decision = Arc::new(Mutex::new(None));
    let sidecar_decision = Arc::clone(&late_cancel_decision);
    let hooks = ImportTestHooks::default()
        .with_after_accepted_export(move |_spool, job_uuid| {
            *hook_job_uuid.lock().unwrap() = Some(job_uuid);
        })
        .with_before_replica_materialization(move |_| {
            let job_uuid = sidecar_job_uuid
                .lock()
                .unwrap()
                .expect("accepted export hook should capture job uuid");
            let decision = SpoolStore::new(&base_path, SpoolLimits::default())
                .unwrap()
                .request_async_migration_cancel(job_uuid)
                .expect("late cancel decision should be typed");
            *sidecar_decision.lock().unwrap() = Some(decision);
            Ok(())
        });
    let request = MigrateFromAlgoliaRequest {
        target_index: Some("late_cancel_target".to_string()),
        ..valid_request()
    };

    let job_uuid = state
        .migration_runner
        .submit_algolia_import_with_test_hooks(
            request,
            |_| {
                Ok(hermetic_source_reader_with_settings_and_pages(
                    json!({
                        "searchableAttributes": ["title"],
                        "attributesForFaceting": ["category"],
                        "replicas": ["late_cancel_replica"],
                    }),
                    vec![scripted_documents(EXPECTED_DOCUMENTS)],
                ))
            },
            hooks,
        )
        .await
        .expect("async import should be admitted")
        .0;

    wait_for_terminal_phase(&state, job_uuid, MigrationDisposition::Succeeded).await;
    wait_for_active_count(&state, 0).await;
    let late_cancel_record = match late_cancel_decision
        .lock()
        .unwrap()
        .clone()
        .expect("late cancel hook should capture a typed decision")
    {
        MigrationCancelRequest::TooLate(record) => record,
        MigrationCancelRequest::Requested(record) => {
            panic!("post-activation cancellation should be too late, got {record:?}")
        }
    };
    assert_eq!(late_cancel_record.phase, MigrationPhase::Activating);
    assert!(!late_cancel_record.cancel_requested);
    let phase = read_migration_phase_at(&state.manager.base_path, job_uuid);
    assert!(!phase.cancel_requested);
    assert_query_returns_document(
        &state,
        "late_cancel_target",
        "Quartz adapter",
        "doc-1",
        "Quartz adapter",
        "hardware",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_import_capacity_refusal_is_retryable_and_writes_no_new_job_artifacts() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp)
        .with_migration_capacity(1)
        .build_shared();
    let reached_source = Arc::new(Notify::new());
    let release_source = Arc::new(Notify::new());
    let first_job = submit_blocked_async_import(
        &state,
        TARGET_INDEX,
        Arc::clone(&reached_source),
        Arc::clone(&release_source),
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), reached_source.notified())
        .await
        .expect("first job should occupy the only permit");
    let jobs_before_refusal = spool_job_count(&state);
    let source_factory_invoked = Arc::new(AtomicBool::new(false));

    let refused = state
        .migration_runner
        .submit_algolia_import(
            MigrateFromAlgoliaRequest {
                target_index: Some("capacity_refused_target".to_string()),
                ..valid_request()
            },
            {
                let source_factory_invoked = Arc::clone(&source_factory_invoked);
                move |_| {
                    source_factory_invoked.store(true, Ordering::SeqCst);
                    Ok(hermetic_source_reader())
                }
            },
        )
        .await
        .expect_err("runner at capacity should refuse without queueing");

    assert_eq!(refused.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(refused.1.into_response()).await,
        json!({
            "message": "Algolia migration import capacity is exhausted; retry later.",
            "status": 503,
            "code": "migration_capacity_exhausted"
        })
    );
    assert!(!source_factory_invoked.load(Ordering::SeqCst));
    assert_eq!(spool_job_count(&state), jobs_before_refusal);

    release_source.notify_waiters();
    wait_for_terminal_phase(&state, first_job, MigrationDisposition::Succeeded).await;
    wait_for_active_count(&state, 0).await;
}

#[tokio::test]
async fn async_import_overwrite_true_records_replacement_semantic() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    seed_preexisting_target_resources(&state, TARGET_INDEX).await;
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let (job_uuid, _) = state
        .migration_runner
        .submit_algolia_import(
            MigrateFromAlgoliaRequest {
                overwrite: true,
                ..valid_request()
            },
            {
                let source_factory_invoked = Arc::clone(&source_factory_invoked);
                move |_| {
                    source_factory_invoked.store(true, Ordering::SeqCst);
                    Ok(hermetic_source_reader())
                }
            },
        )
        .await
        .expect("node-local overwrite=true async migration should be admitted");

    wait_for_terminal_phase(&state, job_uuid, MigrationDisposition::Succeeded).await;
    wait_for_active_count(&state, 0).await;
    assert!(source_factory_invoked.load(Ordering::SeqCst));
    let metadata = SpoolStore::new(&state.manager.base_path, SpoolLimits::default())
        .unwrap()
        .read_async_migration_metadata(job_uuid)
        .unwrap();
    assert_eq!(
        metadata.publication_semantic,
        AsyncMigrationPublicationSemantic::ReplaceExisting
    );
}

#[tokio::test]
async fn async_import_ha_state_is_refused_by_shared_admission_owner() {
    let tmp = TempDir::new().unwrap();
    let (_repl_data_dir, repl_mgr) = peer_configured_replication_manager();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr)
        .build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let mut request = valid_request();
    request.overwrite = false;
    let refused = state
        .migration_runner
        .submit_algolia_import(request, {
            let source_factory_invoked = Arc::clone(&source_factory_invoked);
            move |_| {
                source_factory_invoked.store(true, Ordering::SeqCst);
                Ok(hermetic_source_reader())
            }
        })
        .await
        .expect_err("HA async migration should reuse shared admission refusal");

    assert_eq!(refused.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(refused.1.into_response()).await,
        json!({"message": MIGRATION_HA_UNSUPPORTED_MESSAGE, "status": 503, "code": MIGRATION_HA_UNSUPPORTED_CODE})
    );
    assert!(!source_factory_invoked.load(Ordering::SeqCst));
    assert_no_migration_artifacts(&state);
}

#[test]
fn async_admission_recovery_removes_metadata_only_partial_and_preserves_sync_phase() {
    let tmp = TempDir::new().unwrap();
    let store = SpoolStore::new(tmp.path(), SpoolLimits::default()).unwrap();
    let partial_async_job = uuid::Uuid::new_v4();
    let synchronous_job = uuid::Uuid::new_v4();

    store
        .create_async_metadata_only_admission_for_test(
            partial_async_job,
            TARGET_INDEX,
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    store.create_migration_phase(synchronous_job).unwrap();

    let cleaned = store.recover_async_admissions().unwrap();

    assert_eq!(cleaned, vec![partial_async_job]);
    assert!(
        !store.job_dir(partial_async_job).exists(),
        "metadata-only async admission residue must be removed before restart recovery"
    );
    let synchronous_phase = store
        .read_migration_phase(synchronous_job)
        .expect("synchronous phase-only migration record must not be treated as async");
    assert_eq!(synchronous_phase.job_uuid, synchronous_job);
    assert_eq!(synchronous_phase.disposition, MigrationDisposition::Running);
    assert_eq!(
        store
            .read_async_migration_metadata_if_exists(synchronous_job)
            .unwrap(),
        None
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_import_source_error_settles_failed_and_releases_accounting() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    let job_uuid = state
        .migration_runner
        .submit_algolia_import(valid_request(), |_| {
            Ok(SourceErrorReader::new(AlgoliaClientError::new(
                AlgoliaErrorKind::Transport,
                "deterministic async source failure",
            )))
        })
        .await
        .expect("source construction succeeds so the failure happens inside the spawned import")
        .0;

    wait_for_terminal_phase(&state, job_uuid, MigrationDisposition::Failed).await;
    wait_for_active_count(&state, 0).await;
    let phase = read_migration_phase_at(&state.manager.base_path, job_uuid);
    assert_eq!(phase.phase, MigrationPhase::Exporting);
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_import_panic_settles_failed_and_releases_accounting() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    let job_uuid = state
        .migration_runner
        .submit_algolia_import(valid_request(), |_| Ok(PanickingSourceReader::new()))
        .await
        .expect("source construction succeeds so panic supervision belongs to the runner")
        .0;

    wait_for_terminal_phase(&state, job_uuid, MigrationDisposition::Failed).await;
    wait_for_active_count(&state, 0).await;
    let phase = read_migration_phase_at(&state.manager.base_path, job_uuid);
    assert_eq!(phase.phase, MigrationPhase::Exporting);
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
}

#[tokio::test]
async fn migrate_phase_observed_during_staging_keeps_export_progress_nonterminal() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let base_path = state.manager.base_path.clone();
    let job_uuid = Arc::new(Mutex::new(None));
    let captured_job_uuid = Arc::clone(&job_uuid);
    let observations = Arc::new(Mutex::new(Vec::new()));
    let staging_observations = Arc::clone(&observations);
    let hooks = ImportTestHooks::default()
        .with_after_accepted_export(move |_spool, job| {
            *captured_job_uuid.lock().unwrap() = Some(job);
        })
        .with_before_document_batch_write(move |_| {
            let job = job_uuid
                .lock()
                .unwrap()
                .expect("export hook should capture job uuid before staging");
            staging_observations
                .lock()
                .unwrap()
                .push(read_migration_phase_at(&base_path, job));
            Ok(())
        });

    assert_import_reported_equals_target_contents(
        &state,
        migrate_from_algolia_with_test_source_factory_and_hooks(
            State(Arc::clone(&state)),
            Json(valid_request()),
            |_| Ok(hermetic_source_reader()),
            hooks,
        )
        .await,
    )
    .await;

    let observations = observations.lock().unwrap();
    let observed = observations
        .first()
        .expect("document staging hook should observe a phase record");
    assert_eq!(observed.phase, MigrationPhase::Staging);
    assert_eq!(observed.disposition, MigrationDisposition::Running);
    assert_eq!(observed.terminal_at, None);
    assert_eq!(
        observed.export_progress,
        Some(MigrationExportProgress {
            completed: 3,
            total: 3,
        })
    );
}

#[tokio::test]
async fn migrate_staging_failure_records_terminal_failure() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let base_path = state.manager.base_path.clone();
    let job_uuid = Arc::new(Mutex::new(None));
    let captured_job_uuid = Arc::clone(&job_uuid);
    let hooks = ImportTestHooks::default()
        .with_after_accepted_export(move |_spool, job| {
            *captured_job_uuid.lock().unwrap() = Some(job);
        })
        .with_before_document_batch_write(|_| {
            Err(FlapjackError::Io(
                "deterministic staging write failure".to_string(),
            ))
        });

    let response = migrate_from_algolia_with_test_source_factory_and_hooks(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| Ok(hermetic_source_reader()),
        hooks,
    )
    .await;

    assert_eq!(
        response.expect_err("staging failure should fail").0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
    let job = job_uuid
        .lock()
        .unwrap()
        .expect("accepted export hook should capture job uuid");
    let phase = read_migration_phase_at(&base_path, job);
    assert_eq!(phase.phase, MigrationPhase::Staging);
    assert_eq!(phase.disposition, MigrationDisposition::Failed);
    assert!(phase.terminal_at.is_some());
}

#[tokio::test]
async fn migrate_failure_settlement_phase_write_error_surfaces_storage_failure() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let base_path = state.manager.base_path.clone();
    let marker_base_path = base_path.clone();
    let job_uuid = Arc::new(Mutex::new(None));
    let captured_job_uuid = Arc::clone(&job_uuid);
    let marker_job_uuid = Arc::clone(&job_uuid);
    let hooks = ImportTestHooks::default()
        .with_after_accepted_export(move |_spool, job| {
            *captured_job_uuid.lock().unwrap() = Some(job);
        })
        .with_before_document_batch_write(move |_| {
            let job = marker_job_uuid
                .lock()
                .unwrap()
                .expect("accepted export hook should capture job uuid");
            SpoolStore::new(&marker_base_path, SpoolLimits::default())
                .unwrap()
                .fail_next_migration_phase_commit_for_test(job)
                .unwrap();
            Err(FlapjackError::Io(
                "deterministic staging write failure".to_string(),
            ))
        });

    let response = migrate_from_algolia_with_test_source_factory_and_hooks(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| Ok(hermetic_source_reader()),
        hooks,
    )
    .await;

    let error = response.expect_err("phase persistence failure should fail closed");
    assert_eq!(error.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({"message": "Internal server error", "status": 500})
    );
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
    let job = job_uuid
        .lock()
        .unwrap()
        .expect("accepted export hook should capture job uuid");
    let phase = read_migration_phase_at(&base_path, job);
    assert_eq!(phase.phase, MigrationPhase::Staging);
    assert_eq!(phase.disposition, MigrationDisposition::Running);
    assert_eq!(phase.terminal_at, None);
}

#[tokio::test]
async fn migrate_pre_activation_phase_is_nonterminal_activating() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let base_path = state.manager.base_path.clone();
    let job_uuid = Arc::new(Mutex::new(None));
    let captured_job_uuid = Arc::clone(&job_uuid);
    let observations = Arc::new(Mutex::new(Vec::new()));
    let activation_observations = Arc::clone(&observations);
    let hooks = ImportTestHooks::default()
        .with_after_accepted_export(move |_spool, job| {
            *captured_job_uuid.lock().unwrap() = Some(job);
        })
        .with_before_activation(move || {
            let job = job_uuid
                .lock()
                .unwrap()
                .expect("export hook should capture job uuid before activation");
            activation_observations
                .lock()
                .unwrap()
                .push(read_migration_phase_at(&base_path, job));
        });

    assert_import_reported_equals_target_contents(
        &state,
        migrate_from_algolia_with_test_source_factory_and_hooks(
            State(Arc::clone(&state)),
            Json(valid_request()),
            |_| Ok(hermetic_source_reader()),
            hooks,
        )
        .await,
    )
    .await;

    let observations = observations.lock().unwrap();
    let observed = observations
        .first()
        .expect("activation hook should observe a phase record");
    assert_eq!(observed.phase, MigrationPhase::Activating);
    assert_eq!(observed.disposition, MigrationDisposition::Running);
    assert_eq!(observed.terminal_at, None);
}

#[tokio::test]
async fn migrate_success_records_terminal_success_after_artifact_deletion() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let base_path = state.manager.base_path.clone();
    let job_uuid = Arc::new(Mutex::new(None));
    let captured_job_uuid = Arc::clone(&job_uuid);
    let hooks = ImportTestHooks::default().with_after_accepted_export(move |_spool, job| {
        *captured_job_uuid.lock().unwrap() = Some(job);
    });

    assert_import_reported_equals_target_contents(
        &state,
        migrate_from_algolia_with_test_source_factory_and_hooks(
            State(Arc::clone(&state)),
            Json(valid_request()),
            |_| Ok(hermetic_source_reader()),
            hooks,
        )
        .await,
    )
    .await;

    let job = job_uuid
        .lock()
        .unwrap()
        .expect("accepted export hook should capture job uuid");
    let phase = read_migration_phase_at(&base_path, job);
    assert_eq!(phase.phase, MigrationPhase::Activating);
    assert_eq!(phase.disposition, MigrationDisposition::Succeeded);
    assert!(phase.terminal_at.is_some());
    assert_eq!(
        phase.export_progress,
        Some(MigrationExportProgress {
            completed: 3,
            total: 3
        })
    );
}

#[tokio::test]
async fn migrate_reported_counts_imply_target_contains_documents() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    assert!(
        source_factory_invoked.load(Ordering::SeqCst),
        "implemented migration must use the hermetic source fixture"
    );
    assert_import_reported_equals_target_contents(&state, response).await;
}

#[tokio::test]
async fn migrate_published_target_serves_facets_from_source_settings() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| {
            Ok(hermetic_source_reader_with_settings_and_pages(
                json!({
                    "searchableAttributes": ["title"],
                    "attributesForFaceting": ["category"],
                }),
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
            ))
        },
    )
    .await;

    assert_import_reported_equals_target_contents(&state, response).await;
    assert_target_facets(
        &state,
        TARGET_INDEX,
        json!({
            "hardware": 1,
            "navigation": 1,
        }),
    )
    .await;
}

#[tokio::test]
async fn migrate_refuses_ha_cluster_before_import_admission() {
    let tmp = TempDir::new().unwrap();
    let (_repl_data_dir, repl_mgr) = peer_configured_replication_manager();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr)
        .build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);
    let mut request = valid_request();
    request.overwrite = false;

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(request),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    let error = response.expect_err("HA migration should be refused before import admission");

    assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": MIGRATION_HA_UNSUPPORTED_MESSAGE,
            "status": 503,
            "code": MIGRATION_HA_UNSUPPORTED_CODE
        })
    );
    assert!(
        !source_factory_invoked.load(Ordering::SeqCst),
        "HA refusal must happen before any source reader is constructed"
    );
    assert_no_migration_artifacts(&state);
}

#[tokio::test]
async fn migrate_overwrite_true_ha_is_refused_before_admission() {
    let tmp = TempDir::new().unwrap();
    let (_repl_data_dir, repl_mgr) = peer_configured_replication_manager();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr)
        .build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);
    let mut request = valid_request();
    request.overwrite = true;

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(request),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    let error = response.expect_err("overwrite=true should be refused by HA admission guard");

    assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({"message": MIGRATION_HA_UNSUPPORTED_MESSAGE, "status": 503, "code": MIGRATION_HA_UNSUPPORTED_CODE})
    );
    assert!(
        !source_factory_invoked.load(Ordering::SeqCst),
        "overwrite refusal must happen before any source reader is constructed"
    );
    assert_no_migration_artifacts(&state);
}

#[tokio::test]
async fn migrate_overwrite_true_node_local_sync_is_admitted_after_fence_contract() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    seed_preexisting_target_resources(&state, TARGET_INDEX).await;
    assert_preexisting_target_resources(&state, TARGET_INDEX).await;
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(MigrateFromAlgoliaRequest {
            overwrite: true,
            ..valid_request()
        }),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    assert_import_reported_equals_target_contents(&state, response).await;
    assert!(
        source_factory_invoked.load(Ordering::SeqCst),
        "node-local overwrite admission must construct the hermetic source reader"
    );
    assert_eq!(
        query_hit_count(&state, TARGET_INDEX, "Cedar Caliper").await,
        0,
        "overwrite=true must replace, not merge with, preexisting target contents"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migrate_overwrite_with_acknowledged_pre_activation_write_completes() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    seed_preexisting_target_resources(&state, TARGET_INDEX).await;

    let activation_reached = Arc::new(Barrier::new(2));
    let activation_release = Arc::new(Barrier::new(2));
    let hook_reached = Arc::clone(&activation_reached);
    let hook_release = Arc::clone(&activation_release);
    let hooks = ImportTestHooks::default().with_before_activation(move || {
        hook_reached.wait();
        hook_release.wait();
    });
    let migration_state = Arc::clone(&state);
    let migration = tokio::spawn(async move {
        migrate_from_algolia_with_test_source_factory_and_hooks(
            State(migration_state),
            Json(MigrateFromAlgoliaRequest {
                overwrite: true,
                ..valid_request()
            }),
            |_| Ok(hermetic_source_reader()),
            hooks,
        )
        .await
    });

    tokio::task::spawn_blocking(move || activation_reached.wait())
        .await
        .expect("migration must reach the pre-activation barrier");
    state
        .manager
        .add_documents_durable(
            TARGET_INDEX,
            vec![Document {
                id: "overlap-1".to_string(),
                fields: HashMap::from([
                    (
                        "title".to_string(),
                        FieldValue::Text("Acknowledged Overlap".to_string()),
                    ),
                    (
                        "category".to_string(),
                        FieldValue::Text("concurrent".to_string()),
                    ),
                ]),
            }],
        )
        .await
        .expect("overlapping write must publish before activation");
    tokio::task::spawn_blocking(move || activation_release.wait())
        .await
        .expect("migration activation barrier must release");

    let response = tokio::time::timeout(Duration::from_secs(30), migration)
        .await
        .expect("migration must complete without hanging")
        .expect("migration task must not panic")
        .expect("acknowledged overlap must not turn successful activation into HTTP 500")
        .0;
    assert_eq!(response.status, "complete");
    assert_eq!(response.objects.imported, EXPECTED_DOCUMENTS.len());
    assert_eq!(response.rules.imported, 0);
    assert_eq!(response.synonyms.imported, 0);

    let Json(indices) = list_indices(State(Arc::clone(&state)), Query(HashMap::new()))
        .await
        .expect("replacement target should remain listable");
    let target = indices
        .items
        .iter()
        .find(|item| item.name == TARGET_INDEX)
        .expect("replacement target should exist");
    assert_eq!(
        target.entries,
        (EXPECTED_DOCUMENTS.len() + 1) as u64,
        "target must contain exactly the replacement source plus the acknowledged overlap"
    );
    for (object_id, title, category) in EXPECTED_DOCUMENTS {
        assert_query_returns_document(&state, TARGET_INDEX, title, object_id, title, category)
            .await;
    }
    assert_query_returns_document(
        &state,
        TARGET_INDEX,
        "Acknowledged Overlap",
        "overlap-1",
        "Acknowledged Overlap",
        "concurrent",
    )
    .await;
    assert_eq!(
        query_hit_count(&state, TARGET_INDEX, "Cedar Caliper").await,
        0,
        "seeded stale document must not survive replacement"
    );
}

#[tokio::test]
async fn migrate_preexisting_searchable_target_survives_canonical_conflict() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    seed_preexisting_target_resources(&state, TARGET_INDEX).await;
    assert_preexisting_target_resources(&state, TARGET_INDEX).await;
    state.manager.unload(&TARGET_INDEX.to_string()).unwrap();
    let before_snapshot = directory_snapshot(&state.manager.base_path.join(TARGET_INDEX));

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| Ok(hermetic_source_reader()),
    )
    .await;

    let error = response.expect_err("existing target should fail at create-only activation");
    assert_eq!(error.0, StatusCode::CONFLICT);
    assert_eq!(
        body_json(error.1.into_response()).await,
        body_json(FlapjackError::IndexAlreadyExists(TARGET_INDEX.to_string()).into_response())
            .await
    );
    assert_eq!(
        directory_snapshot(&state.manager.base_path.join(TARGET_INDEX)),
        before_snapshot,
        "failed create-only import must not mutate the preexisting target bytes"
    );
    assert_preexisting_target_resources(&state, TARGET_INDEX).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migrate_two_concurrent_imports_admit_exactly_one_create_only_winner() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let activation_barrier = Arc::new(Barrier::new(3));
    let first_hooks =
        ImportTestHooks::default().with_before_activation_barrier(Arc::clone(&activation_barrier));
    let second_hooks =
        ImportTestHooks::default().with_before_activation_barrier(Arc::clone(&activation_barrier));
    let first_state = Arc::clone(&state);
    let second_state = Arc::clone(&state);

    let first = tokio::spawn(async move {
        migrate_from_algolia_with_test_source_factory_and_hooks(
            State(first_state),
            Json(valid_request()),
            |_| Ok(hermetic_source_reader_with_documents(FIRST_RACE_DOCUMENTS)),
            first_hooks,
        )
        .await
    });
    let second = tokio::spawn(async move {
        migrate_from_algolia_with_test_source_factory_and_hooks(
            State(second_state),
            Json(valid_request()),
            |_| Ok(hermetic_source_reader_with_documents(SECOND_RACE_DOCUMENTS)),
            second_hooks,
        )
        .await
    });
    tokio::task::spawn_blocking(move || activation_barrier.wait())
        .await
        .unwrap();

    let results = vec![first.await.unwrap(), second.await.unwrap()];
    let successes = results.iter().filter(|result| result.is_ok()).count();
    let conflicts = results
        .iter()
        .filter(|result| {
            result
                .as_ref()
                .is_err_and(|error| error.0 == StatusCode::CONFLICT)
        })
        .count();
    assert_eq!(
        successes, 1,
        "exactly one import may win create-only activation"
    );
    assert_eq!(
        conflicts, 1,
        "the losing import must receive a canonical conflict"
    );

    let response = results
        .into_iter()
        .find_map(Result::ok)
        .expect("one import should succeed")
        .0;
    assert_eq!(response.status, "complete");
    assert_eq!(response.objects.imported, 2);
    assert_eq!(response.rules.imported, 0);
    assert_eq!(response.synonyms.imported, 0);

    let first_hits = query_hit_count(&state, TARGET_INDEX, "Linen Shuttle").await
        + query_hit_count(&state, TARGET_INDEX, "Brass Relay").await;
    let second_hits = query_hit_count(&state, TARGET_INDEX, "Ivory Beacon").await
        + query_hit_count(&state, TARGET_INDEX, "Copper Sextant").await;
    assert!(
        (first_hits == 2 && second_hits == 0) || (first_hits == 0 && second_hits == 2),
        "target must contain exactly one source dataset, got first={first_hits} second={second_hits}"
    );
}

#[tokio::test]
async fn migrate_translation_hard_rejection_aborts_publication_and_keeps_spool_evidence() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| {
            Ok(hermetic_source_reader_with_settings_and_pages(
                json!({"searchableAttributes": ["title"], "replicas": ["replica_idx"]}),
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
            ))
        },
    )
    .await;

    let response = response.expect("replica topology should migrate with warnings");
    assert_eq!(response.status, "complete");
    assert_eq!(response.objects.imported, EXPECTED_DOCUMENTS.len());
    let warning_codes = response
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect::<Vec<_>>();
    assert!(warning_codes.contains(&"ReplicaExhaustiveSortApproximated"));
    assert!(warning_codes.contains(&"ReplicaRelevancyStrictnessSemanticMismatch"));
    assert!(warning_codes.contains(&"ReplicaMatchingCriticalFieldDiverges"));
    let exhaustive_warning = response
        .warnings
        .iter()
        .find(|warning| warning.code == "ReplicaExhaustiveSortApproximated")
        .expect("response should expose exhaustive-sort approximation warning");
    assert_eq!(
        exhaustive_warning.message,
        "Algolia standard replica exhaustive sorting is approximated as a Flapjack virtual replica."
    );
    assert_eq!(exhaustive_warning.json_path, "$.replicas[0]");
    let settings = state.manager.get_settings(TARGET_INDEX).unwrap();
    assert_eq!(
        settings.replicas,
        Some(vec!["virtual(replica_idx)".to_string()])
    );
    assert!(settings.relevancy_strictness.is_none());
    assert_no_retained_accepted_spool_document_artifacts(&state);
}

#[tokio::test]
async fn migrate_corrupt_accepted_artifact_aborts_publication_and_keeps_spool_evidence() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let hooks = ImportTestHooks::default().with_after_accepted_export(|spool, job_uuid| {
        let artifact = spool.visible_artifacts(job_uuid).unwrap().remove(0);
        let path = spool.job_dir(job_uuid).join(artifact);
        let mut bytes = fs::read(&path).unwrap();
        bytes[0] = if bytes[0] == b'[' { b'{' } else { b'[' };
        fs::write(path, bytes).unwrap();
    });

    let response = migrate_from_algolia_with_test_source_factory_and_hooks(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| Ok(hermetic_source_reader()),
        hooks,
    )
    .await;

    let error = response.expect_err("manifest digest mismatch should abort before activation");
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({"message": format!("migration spool error: {:?}", SpoolErrorKind::ResourceVerificationFailed), "status": 400})
    );
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
    assert_spool_lifecycle_with_artifacts(&state, "Accepted");
}

#[tokio::test]
async fn migrate_staging_document_write_failure_aborts_publication_and_keeps_spool_evidence() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let hooks = ImportTestHooks::default().with_before_document_batch_write(|_| {
        Err(FlapjackError::Io(
            "deterministic staging write failure".to_string(),
        ))
    });

    let response = migrate_from_algolia_with_test_source_factory_and_hooks(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| Ok(hermetic_source_reader()),
        hooks,
    )
    .await;

    let error = response.expect_err("staging document writer failure should abort publication");
    assert_eq!(error.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({"message": "Internal server error", "status": 500})
    );
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
    assert_spool_lifecycle_with_artifacts(&state, "Accepted");
}

#[tokio::test]
async fn migrate_mid_document_export_failure_fails_job_without_activating_target() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| {
            let mut reader = ScriptedSourceReader::new(SOURCE_APP_ID, SOURCE_INDEX);
            let source_record = AlgoliaIndexRecord {
                name: SOURCE_INDEX.to_string(),
                entries: 2,
                updated_at: "2026-07-16T00:00:00Z".to_string(),
                pending_task: false,
            };
            let settings =
                json!({"searchableAttributes": ["title"], "attributesForFaceting": ["category"]});
            reader.push_quiescent(source_record);
            reader.push_pass(
                settings.clone(),
                vec![scripted_documents(EXPECTED_DOCUMENTS)],
                vec![],
                vec![],
            );
            reader.push_document_pass_failing_after_page(
                settings,
                vec![
                    vec![json!({"objectID": "partial-1", "title": "Partial One", "category": "test"})],
                    vec![json!({"objectID": "partial-2", "title": "Partial Two", "category": "test"})],
                ],
                1,
                AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Algolia document export failed after a committed page",
                ),
            );
            Ok(reader)
        },
    )
    .await;

    let error = response.expect_err("mid-document export failure should fail the import");
    assert_eq!(error.0, StatusCode::BAD_GATEWAY);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({"message": "Algolia document export failed after a committed page", "status": 502})
    );
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
    assert_spool_lifecycle_with_artifacts(&state, "Failed");
}

#[tokio::test]
async fn migrate_pre_activation_unwind_drops_staging_without_activating_target() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let task_state = Arc::clone(&state);
    let hooks = ImportTestHooks::default().with_before_activation(|| {
        panic!("deterministic pre-activation unwind");
    });

    let task = tokio::spawn(async move {
        migrate_from_algolia_with_test_source_factory_and_hooks(
            State(task_state),
            Json(valid_request()),
            |_| Ok(hermetic_source_reader()),
            hooks,
        )
        .await
    });
    let join_error = task
        .await
        .expect_err("pre-activation hook should unwind the import task");

    assert!(join_error.is_panic());
    assert_target_absent_from_disk_and_list(&state, TARGET_INDEX).await;
    assert_spool_lifecycle_with_artifacts(&state, "Accepted");
}

#[tokio::test]
async fn migrate_ha_retry_after_success_is_refused_before_reader_or_spool_write() {
    let tmp = TempDir::new().unwrap();
    let standalone = TestStateBuilder::new(&tmp).build_shared();
    assert_import_reported_equals_target_contents(
        &standalone,
        migrate_from_algolia_with_test_source_factory(
            State(Arc::clone(&standalone)),
            Json(valid_request()),
            |_| Ok(hermetic_source_reader()),
        )
        .await,
    )
    .await;
    standalone
        .manager
        .unload(&TARGET_INDEX.to_string())
        .unwrap();
    let spool_before = directory_snapshot(&standalone.manager.base_path.join("migration_exports"));
    drop(standalone);

    let (_repl_data_dir, repl_mgr) = peer_configured_replication_manager();
    let ha_state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr)
        .build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let invoked = Arc::clone(&source_factory_invoked);
    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&ha_state)),
        Json(valid_request()),
        move |_| {
            invoked.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    let error = response.expect_err("HA retry should be refused before import admission");
    assert_eq!(error.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({"message": MIGRATION_HA_UNSUPPORTED_MESSAGE, "status": 503, "code": MIGRATION_HA_UNSUPPORTED_CODE})
    );
    assert!(!source_factory_invoked.load(Ordering::SeqCst));
    assert_eq!(
        directory_snapshot(&ha_state.manager.base_path.join("migration_exports")),
        spool_before
    );
    for (object_id, title, category) in EXPECTED_DOCUMENTS {
        assert_query_returns_document(&ha_state, TARGET_INDEX, title, object_id, title, category)
            .await;
    }
}

#[tokio::test]
async fn migrate_three_page_import_streams_bounded_batches_and_activates_all_documents() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let written_batches = Arc::new(Mutex::new(Vec::new()));
    let observed_batches = Arc::clone(&written_batches);
    let hooks = ImportTestHooks::default().with_before_document_batch_write(move |batch| {
        observed_batches.lock().unwrap().push(batch.len());
        Ok(())
    });

    let response = migrate_from_algolia_with_test_source_factory_and_hooks(
        State(Arc::clone(&state)),
        Json(valid_request()),
        |_| {
            Ok(hermetic_source_reader_with_settings_and_pages(
                json!({"searchableAttributes": ["title"], "attributesForFaceting": ["page_marker"]}),
                vec![document_page(0, 700), document_page(700, 301), document_page(1001, 204)],
            ))
        },
        hooks,
    )
    .await
    .expect("three-page import should succeed")
    .0;

    assert_eq!(written_batches.lock().unwrap().as_slice(), [1_000, 205]);
    assert_eq!(response.objects.imported, 1_205);
    assert_object_fields(&state, TARGET_INDEX, "doc-0000", "Document 0", 0, 0).await;
    assert_object_fields(&state, TARGET_INDEX, "doc-1000", "Document 1000", 700, 1000).await;
    assert_object_fields(
        &state,
        TARGET_INDEX,
        "doc-1204",
        "Document 1204",
        1001,
        1204,
    )
    .await;
    assert_no_retained_accepted_spool_document_artifacts(&state);
}

#[tokio::test]
async fn migrate_validates_request_before_ha_admission_guard() {
    let tmp = TempDir::new().unwrap();
    let (_repl_data_dir, repl_mgr) = peer_configured_replication_manager();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr)
        .build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(MigrateFromAlgoliaRequest {
            app_id: String::new(),
            api_key: SOURCE_API_KEY.to_string(),
            source_index: SOURCE_INDEX.to_string(),
            target_index: Some(TARGET_INDEX.to_string()),
            overwrite: false,
        }),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    let error = response.expect_err("invalid request should fail before HA admission");

    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": "appId, apiKey, and sourceIndex are required",
            "status": 400
        })
    );
    assert!(
        !source_factory_invoked.load(Ordering::SeqCst),
        "validation refusal must happen before any source reader is constructed"
    );
    assert_no_migration_artifacts(&state);
}

#[tokio::test]
async fn migrate_validates_target_index_before_ha_admission_guard() {
    let tmp = TempDir::new().unwrap();
    let (_repl_data_dir, repl_mgr) = peer_configured_replication_manager();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr)
        .build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let source_factory_invoked_by_handler = Arc::clone(&source_factory_invoked);

    let response = migrate_from_algolia_with_test_source_factory(
        State(Arc::clone(&state)),
        Json(MigrateFromAlgoliaRequest {
            app_id: SOURCE_APP_ID.to_string(),
            api_key: SOURCE_API_KEY.to_string(),
            source_index: SOURCE_INDEX.to_string(),
            target_index: Some("../escape".to_string()),
            overwrite: false,
        }),
        move |_| {
            source_factory_invoked_by_handler.store(true, Ordering::SeqCst);
            Ok(hermetic_source_reader())
        },
    )
    .await;

    let error = response.expect_err("invalid targetIndex should fail before HA admission");

    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": "Invalid query: Index name contains invalid characters (path traversal not allowed)",
            "status": 400
        })
    );
    assert!(
        !source_factory_invoked.load(Ordering::SeqCst),
        "targetIndex validation must happen before any source reader is constructed"
    );
    assert_no_migration_artifacts(&state);
}

fn assert_no_migration_artifacts(state: &Arc<crate::handlers::AppState>) {
    assert!(
        !state.manager.base_path.join(TARGET_INDEX).exists(),
        "refused migration must not create the target index"
    );
    assert!(
        !state
            .manager
            .base_path
            .join("migration_exports")
            .join("jobs")
            .exists(),
        "refused migration must not create spool jobs"
    );
}

fn spool_job_count(state: &Arc<crate::handlers::AppState>) -> usize {
    SpoolStore::new(&state.manager.base_path, SpoolLimits::default())
        .unwrap()
        .job_uuids()
        .unwrap()
        .len()
}

async fn assert_import_reported_equals_target_contents(
    state: &Arc<crate::handlers::AppState>,
    response: Result<Json<MigrateFromAlgoliaResponse>, super::MigrateError>,
) {
    let Json(response) = response.expect("implemented migration should succeed");
    assert_eq!(response.status, "complete");
    assert!(response.settings);
    assert_eq!(response.objects.imported, EXPECTED_DOCUMENTS.len());
    assert_eq!(response.synonyms.imported, 0);
    assert_eq!(response.rules.imported, 0);
    assert_eq!(response.task_id, 0);

    let Json(indices) = list_indices(State(Arc::clone(state)), Query(HashMap::new()))
        .await
        .expect("target index should be listable after import");
    let target = indices
        .items
        .iter()
        .find(|item| item.name == TARGET_INDEX)
        .expect("successful import should create the target index");

    assert_eq!(
        response.objects.imported as u64, target.entries,
        "reported imported object count must match target index entries"
    );
    assert_eq!(target.entries, EXPECTED_DOCUMENTS.len() as u64);

    for (object_id, title, category) in EXPECTED_DOCUMENTS {
        assert_query_returns_document(state, TARGET_INDEX, title, object_id, title, category).await;
    }
}

async fn assert_target_facets(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
    expected_category_facets: serde_json::Value,
) {
    let Json(search_response) = crate::handlers::search::search_single(
        State(Arc::clone(state)),
        target_index.to_string(),
        crate::dto::SearchRequest {
            query: String::new(),
            facets: Some(vec!["category".to_string()]),
            hits_per_page: Some(10),
            ..Default::default()
        },
    )
    .await
    .expect("published target should be searchable with facets");

    assert_eq!(search_response["nbHits"], EXPECTED_DOCUMENTS.len());
    assert_eq!(
        search_response["facets"]["category"],
        expected_category_facets
    );
}

const FIRST_RACE_DOCUMENTS: [(&str, &str, &str); 2] = [
    ("race-a-1", "Linen Shuttle", "textiles"),
    ("race-a-2", "Brass Relay", "electronics"),
];
const SECOND_RACE_DOCUMENTS: [(&str, &str, &str); 2] = [
    ("race-b-1", "Ivory Beacon", "navigation"),
    ("race-b-2", "Copper Sextant", "navigation"),
];

fn scripted_documents(documents: [(&str, &str, &str); 2]) -> Vec<serde_json::Value> {
    documents
        .iter()
        .map(|(object_id, title, category)| {
            json!({
                "objectID": object_id,
                "title": title,
                "category": category,
            })
        })
        .collect()
}

fn document_page(start: usize, count: usize) -> Vec<serde_json::Value> {
    (start..start + count)
        .map(|index| {
            json!({
                "objectID": format!("doc-{index:04}"),
                "title": format!("Document {index}"),
                "page_marker": start,
                "score": index as i64
            })
        })
        .collect()
}

async fn submit_blocked_async_import(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
    reached_source: Arc<Notify>,
    release_source: Arc<Notify>,
) -> uuid::Uuid {
    let request = MigrateFromAlgoliaRequest {
        target_index: Some(target_index.to_string()),
        ..valid_request()
    };
    state
        .migration_runner
        .submit_algolia_import(request, move |_| {
            Ok(BlockingSourceReader::new(
                hermetic_source_reader(),
                reached_source,
                release_source,
            ))
        })
        .await
        .expect("async import should be admitted")
        .0
}

async fn wait_for_terminal_phase(
    state: &Arc<crate::handlers::AppState>,
    job_uuid: uuid::Uuid,
    expected: MigrationDisposition,
) {
    let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let phase = read_migration_phase_at(&state.manager.base_path, job_uuid);
            if phase.disposition == expected {
                assert!(phase.terminal_at.is_some());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    if result.is_err() {
        let phase = read_migration_phase_at(&state.manager.base_path, job_uuid);
        panic!(
            "async migration {job_uuid} should reach {expected:?}; observed phase {:?}, disposition {:?}, terminal_at {:?}",
            phase.phase, phase.disposition, phase.terminal_at
        );
    }
}

async fn wait_for_active_count(state: &Arc<crate::handlers::AppState>, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if state.migration_runner.active_count_for_test() == expected {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("migration runner active count should settle");
}

struct BlockingSourceReader {
    inner: ScriptedSourceReader,
    reached_source: Arc<Notify>,
    release_source: Arc<Notify>,
    blocked_once: bool,
}

impl BlockingSourceReader {
    fn new(
        inner: ScriptedSourceReader,
        reached_source: Arc<Notify>,
        release_source: Arc<Notify>,
    ) -> Self {
        Self {
            inner,
            reached_source,
            release_source,
            blocked_once: false,
        }
    }
}

impl MigrationSourceReader for BlockingSourceReader {
    fn app_id(&self) -> &str {
        self.inner.app_id()
    }

    fn source_name(&self) -> &str {
        self.inner.source_name()
    }

    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord> {
        Box::pin(async move {
            if !self.blocked_once {
                self.blocked_once = true;
                let released = self.release_source.notified();
                tokio::pin!(released);
                released.as_mut().enable();
                self.reached_source.notify_one();
                released.await;
            }
            self.inner.wait_for_quiescent_source().await
        })
    }

    fn read_settings(&mut self) -> SourceFuture<'_, serde_json::Value> {
        self.inner.read_settings()
    }

    fn read_index_settings<'a>(
        &'a mut self,
        index_name: &'a str,
    ) -> SourceFuture<'a, serde_json::Value> {
        self.inner.read_index_settings(index_name)
    }

    fn require_unretrievable_access<'a>(
        &'a mut self,
        settings: &'a serde_json::Value,
    ) -> SourceFuture<'a, ()> {
        self.inner.require_unretrievable_access(settings)
    }

    fn read_documents<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        self.inner.read_documents(consume_page)
    }

    fn read_rules<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        self.inner.read_rules(consume_page)
    }

    fn read_synonyms<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        self.inner.read_synonyms(consume_page)
    }
}

struct SourceErrorReader {
    error: Option<AlgoliaClientError>,
}

impl SourceErrorReader {
    fn new(error: AlgoliaClientError) -> Self {
        Self { error: Some(error) }
    }
}

impl MigrationSourceReader for SourceErrorReader {
    fn app_id(&self) -> &str {
        SOURCE_APP_ID
    }

    fn source_name(&self) -> &str {
        SOURCE_INDEX
    }

    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord> {
        let error = self.error.take().unwrap_or_else(|| {
            AlgoliaClientError::new(AlgoliaErrorKind::Progress, "unexpected second source wait")
        });
        Box::pin(async move { Err(error) })
    }

    fn read_settings(&mut self) -> SourceFuture<'_, serde_json::Value> {
        unreachable_source_step()
    }

    fn read_index_settings<'a>(
        &'a mut self,
        _index_name: &'a str,
    ) -> SourceFuture<'a, serde_json::Value> {
        unreachable_source_step()
    }

    fn require_unretrievable_access<'a>(
        &'a mut self,
        _settings: &'a serde_json::Value,
    ) -> SourceFuture<'a, ()> {
        unreachable_source_step()
    }

    fn read_documents<'a>(
        &'a mut self,
        _consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        unreachable_source_step()
    }

    fn read_rules<'a>(
        &'a mut self,
        _consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        unreachable_source_step()
    }

    fn read_synonyms<'a>(
        &'a mut self,
        _consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        unreachable_source_step()
    }
}

struct PanickingSourceReader;

impl PanickingSourceReader {
    fn new() -> Self {
        Self
    }
}

impl MigrationSourceReader for PanickingSourceReader {
    fn app_id(&self) -> &str {
        SOURCE_APP_ID
    }

    fn source_name(&self) -> &str {
        SOURCE_INDEX
    }

    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord> {
        Box::pin(async { panic!("deterministic async source panic") })
    }

    fn read_settings(&mut self) -> SourceFuture<'_, serde_json::Value> {
        unreachable_source_step()
    }

    fn read_index_settings<'a>(
        &'a mut self,
        _index_name: &'a str,
    ) -> SourceFuture<'a, serde_json::Value> {
        unreachable_source_step()
    }

    fn require_unretrievable_access<'a>(
        &'a mut self,
        _settings: &'a serde_json::Value,
    ) -> SourceFuture<'a, ()> {
        unreachable_source_step()
    }

    fn read_documents<'a>(
        &'a mut self,
        _consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        unreachable_source_step()
    }

    fn read_rules<'a>(
        &'a mut self,
        _consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        unreachable_source_step()
    }

    fn read_synonyms<'a>(
        &'a mut self,
        _consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        unreachable_source_step()
    }
}

fn unreachable_source_step<T>() -> SourceFuture<'static, T>
where
    T: Send + 'static,
{
    Box::pin(async {
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Progress,
            "unreachable source reader step",
        ))
    })
}
