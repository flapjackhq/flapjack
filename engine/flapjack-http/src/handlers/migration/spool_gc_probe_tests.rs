use super::*;
use chrono::{Duration, TimeZone, Utc};
use serde::Serialize;
use std::env;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const PROBE_DATA_DIR_ENV: &str = "FJ_MIGRATION_SPOOL_GC_PROBE_DATA_DIR";
const PROBE_EXPECTED_PATH_ENV: &str = "FJ_MIGRATION_SPOOL_GC_PROBE_EXPECTED_PATH";
const PROBE_APP_ID_ENV: &str = "FJ_MIGRATION_SPOOL_GC_PROBE_APP_ID";
const PROBE_CONTROL_SNAPSHOT_DIR_ENV: &str = "FJ_MIGRATION_SPOOL_GC_PROBE_CONTROL_SNAPSHOT_DIR";

#[derive(Serialize)]
struct ProbeExpected {
    authenticated_app_id: String,
    expired: ProbeJobExpected,
    controls: Vec<ProbeControlExpected>,
}

#[derive(Serialize)]
struct ProbeJobExpected {
    job_id: Uuid,
    job_dir: PathBuf,
    manifest_path: PathBuf,
    payload_paths: Vec<PathBuf>,
    completed_sidecar_paths: Vec<PathBuf>,
    terminal_at: String,
    payload_artifact_count: usize,
    reclaimable_bytes: u64,
}

#[derive(Serialize)]
struct ProbeControlExpected {
    label: &'static str,
    job_id: Uuid,
    job_dir: PathBuf,
    snapshot_root: PathBuf,
}

#[test]
#[ignore = "manual real-server probe fixture seeder"]
fn seed_migration_spool_gc_probe_fixture() {
    let data_dir = required_path_env(PROBE_DATA_DIR_ENV);
    let expected_path = required_path_env(PROBE_EXPECTED_PATH_ENV);
    let app_id = required_env(PROBE_APP_ID_ENV);
    let snapshot_dir = required_path_env(PROBE_CONTROL_SNAPSHOT_DIR_ENV);
    std::fs::create_dir_all(&snapshot_dir).expect("control snapshot dir should be created");

    let now = Utc.timestamp_opt(Utc::now().timestamp(), 0).unwrap();
    let limits = SpoolLimits::default();
    let store = SpoolStore::new(&data_dir, limits).expect("probe spool store should open");

    let expired_terminal_at = now - Duration::seconds(limits.retention_seconds + 3600);
    let retained_terminal_at = now - Duration::seconds(60);
    let expired = seed_probe_job(
        &store,
        Uuid::from_u128(0x40000000000000000000000000000001),
        &app_id,
        AdmissionKind::Async,
        ProbePhase {
            phase: MigrationPhase::Activating,
            disposition: MigrationDisposition::Succeeded,
            terminal_at: Some(expired_terminal_at),
        },
    );
    let retained = seed_probe_job(
        &store,
        Uuid::from_u128(0x40000000000000000000000000000002),
        &app_id,
        AdmissionKind::Async,
        ProbePhase {
            phase: MigrationPhase::Activating,
            disposition: MigrationDisposition::Succeeded,
            terminal_at: Some(retained_terminal_at),
        },
    );
    let running = seed_probe_job(
        &store,
        Uuid::from_u128(0x40000000000000000000000000000003),
        &app_id,
        AdmissionKind::PhaseOnly,
        ProbePhase {
            phase: MigrationPhase::Activating,
            disposition: MigrationDisposition::Running,
            terminal_at: None,
        },
    );

    let expected = ProbeExpected {
        authenticated_app_id: app_id,
        expired,
        controls: vec![
            ProbeControlExpected {
                label: "within_retention_terminal",
                job_id: retained.job_id,
                job_dir: retained.job_dir,
                snapshot_root: snapshot_dir.join("within_retention_terminal"),
            },
            ProbeControlExpected {
                label: "running_no_terminal",
                job_id: running.job_id,
                job_dir: running.job_dir,
                snapshot_root: snapshot_dir.join("running_no_terminal"),
            },
        ],
    };
    let bytes = serde_json::to_vec_pretty(&expected).expect("probe expected JSON should encode");
    std::fs::write(&expected_path, bytes).expect("probe expected JSON should be written");
}

#[derive(Clone, Copy)]
struct ProbePhase {
    phase: MigrationPhase,
    disposition: MigrationDisposition,
    terminal_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Clone, Copy)]
enum AdmissionKind {
    Async,
    PhaseOnly,
}

fn seed_probe_job(
    store: &SpoolStore,
    job_id: Uuid,
    authenticated_app_id: &str,
    admission: AdmissionKind,
    phase: ProbePhase,
) -> ProbeJobExpected {
    match admission {
        AdmissionKind::Async => {
            store
                .create_async_migration_admission_for_owner(
                    job_id,
                    &format!("migration_probe_target_{job_id}"),
                    Some(authenticated_app_id),
                )
                .expect("async migration admission should be created");
        }
        AdmissionKind::PhaseOnly => {
            store
                .create_migration_phase(job_id)
                .expect("migration phase should be created");
        }
    }
    store
        .create_export(
            job_id,
            &hex_digest(format!("source-{job_id}").as_bytes()),
            denominators(),
        )
        .expect("export should be created");
    store
        .commit_settings(job_id, br#"{"searchableAttributes":["name"]}"#, 1)
        .expect("settings artifact should be committed");
    store
        .commit_document_page_with_ids(job_id, br#"[{"objectID":"doc-1","name":"A"}]"#, &["doc-1"])
        .expect("document payload and sidecar should be committed");
    store
        .commit_rule_page_with_ids(job_id, br#"[{"objectID":"rule-1"}]"#, &["rule-1"])
        .expect("rule payload and sidecar should be committed");
    store
        .commit_synonym_page_with_ids(job_id, br#"[{"objectID":"syn-1"}]"#, &["syn-1"])
        .expect("synonym payload and sidecar should be committed");
    commit_probe_phase(store, job_id, phase);

    let payload_paths = payload_paths(store, job_id);
    let completed_sidecar_paths = completed_sidecar_paths(store, job_id);
    let reclaimable_bytes = payload_paths
        .iter()
        .chain(completed_sidecar_paths.iter())
        .map(|path| {
            std::fs::metadata(path)
                .expect("reclaimable path should exist")
                .len()
        })
        .sum();
    ProbeJobExpected {
        job_id,
        job_dir: store.job_dir(job_id),
        manifest_path: store.manifest_path(job_id),
        payload_artifact_count: payload_paths.len(),
        payload_paths,
        completed_sidecar_paths,
        terminal_at: phase
            .terminal_at
            .map(|timestamp| timestamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default(),
        reclaimable_bytes,
    }
}

fn denominators() -> ResourceDenominators {
    ResourceDenominators {
        settings: 1,
        documents: 1,
        rules: 1,
        synonyms: 1,
        config: 0,
    }
}

fn commit_probe_phase(store: &SpoolStore, job_id: Uuid, phase: ProbePhase) {
    let mut record = store
        .read_migration_phase(job_id)
        .expect("migration phase should be readable");
    record.phase = phase.phase;
    record.disposition = phase.disposition;
    record.terminal_at = phase.terminal_at;
    record.updated_at = phase.terminal_at.unwrap_or_else(|| {
        Utc.timestamp_opt(Utc::now().timestamp(), 0).unwrap()
            - Duration::seconds(SpoolLimits::default().retention_seconds + 3600)
    });
    store
        .commit_migration_phase(&record)
        .expect("migration phase should be committed");
}

fn payload_paths(store: &SpoolStore, job_id: Uuid) -> Vec<PathBuf> {
    let manifest = store.read_manifest(job_id).expect("manifest should read");
    visible_artifacts(&manifest)
        .map(|artifact| store.job_dir(job_id).join(&artifact.final_path))
        .collect()
}

fn completed_sidecar_paths(store: &SpoolStore, job_id: Uuid) -> Vec<PathBuf> {
    [
        ObjectResource::Documents,
        ObjectResource::Rules,
        ObjectResource::Synonyms,
    ]
    .map(|resource| store.resource_sidecar_path(job_id, resource))
    .into()
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is required"))
}

fn required_path_env(name: &str) -> PathBuf {
    Path::new(&required_env(name)).to_path_buf()
}
