use axum::http::HeaderValue;
use flapjack_replication::types::RELEASE_TRANSFER_CONTRACT_V1;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

const REQUIRED_RELEASE_IMPORT_HEADERS: [&str; 5] = [
    RELEASE_TRANSFER_CONTRACT_HEADER,
    RELEASE_TRANSFER_TENANT_HEADER,
    RELEASE_TRANSFER_TRANSACTION_HEADER,
    RELEASE_TRANSFER_THROUGH_SEQ_HEADER,
    RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER,
];

#[derive(Clone)]
pub(super) struct ReleaseImportCase {
    pub(super) name: String,
    pub(super) snapshot_bytes: Vec<u8>,
    pub(super) headers: Vec<(&'static str, HeaderValue)>,
}

impl ReleaseImportCase {
    pub(super) fn request(&self) -> Request<Body> {
        let mut request = Request::builder()
            .method("POST")
            .uri("/1/indexes/products/import")
            .body(Body::from(self.snapshot_bytes.clone()))
            .unwrap();
        for (name, value) in &self.headers {
            request.headers_mut().append(*name, value.clone());
        }
        request
    }
}

pub(super) fn release_import_headers(
    snapshot_digest: &str,
    through_sequence: Option<&str>,
) -> Vec<(&'static str, HeaderValue)> {
    REQUIRED_RELEASE_IMPORT_HEADERS
        .into_iter()
        .filter_map(|name| {
            release_header_value(name, snapshot_digest, through_sequence).map(|value| (name, value))
        })
        .collect()
}

fn replace_release_header(
    headers: &mut Vec<(&'static str, HeaderValue)>,
    name: &'static str,
    value: HeaderValue,
) {
    headers.retain(|(candidate, _)| *candidate != name);
    headers.push((name, value));
}

fn release_header_value(
    name: &'static str,
    snapshot_digest: &str,
    through_sequence: Option<&str>,
) -> Option<HeaderValue> {
    match name {
        RELEASE_TRANSFER_CONTRACT_HEADER => {
            Some(HeaderValue::from_static(RELEASE_TRANSFER_CONTRACT_V1))
        }
        RELEASE_TRANSFER_TENANT_HEADER => Some(HeaderValue::from_static("products")),
        RELEASE_TRANSFER_TRANSACTION_HEADER => {
            Some(HeaderValue::from_static("release-import-transaction"))
        }
        RELEASE_TRANSFER_THROUGH_SEQ_HEADER => {
            through_sequence.map(|value| HeaderValue::from_str(value).unwrap())
        }
        RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER => {
            Some(HeaderValue::from_str(snapshot_digest).unwrap())
        }
        _ => panic!("unexpected required release header {name}"),
    }
}

fn has_lifecycle_phase(tenant_id: &str, phase: &str) -> bool {
    flapjack::index::write_queue::writer_lifecycle_test_events(tenant_id)
        .iter()
        .any(|event| event.phase == phase)
}

struct RejectionBaseline {
    tenant_path: PathBuf,
    publication_path: PathBuf,
    tenant_tree: BTreeMap<PathBuf, crate::test_helpers::SnapshotTreeEntry>,
    publication_tree: BTreeMap<PathBuf, crate::test_helpers::SnapshotTreeEntry>,
    writer_closes: usize,
}

async fn rejection_baseline(
    destination: &Arc<AppState>,
    destination_kind: ReleaseImportDestination,
) -> RejectionBaseline {
    let tenant_path = destination.manager.base_path.join("products");
    let publication_path = destination
        .manager
        .base_path
        .join(".publication")
        .join("products");
    let tenant_tree = if matches!(destination_kind, ReleaseImportDestination::Existing) {
        crate::test_helpers::settled_snapshot_tree(&tenant_path).await
    } else {
        crate::test_helpers::snapshot_tree(&tenant_path)
    };
    let publication_tree = crate::test_helpers::snapshot_tree(&publication_path);
    flapjack::index::write_queue::clear_writer_lifecycle_test_events();
    RejectionBaseline {
        tenant_path,
        publication_path,
        tenant_tree,
        publication_tree,
        writer_closes: retained_channel_closed_count("products"),
    }
}

async fn assert_release_import_bad_request(response: Response, case_name: &str) {
    assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{case_name}");
    assert_no_release_import_proof(response.headers());
    let response_body = body_json(response).await;
    assert_eq!(response_body["status"], 400, "{case_name}");
    assert!(
        response_body["message"]
            .as_str()
            .is_some_and(|message| message.contains("release") || message.contains("digest")),
        "{case_name} must return a validation message: {response_body}"
    );
}

fn assert_no_restore_effects(case_name: &str, baseline: &RejectionBaseline) {
    assert_eq!(
        retained_channel_closed_count("products"),
        baseline.writer_closes,
        "{case_name} must not close a destination writer"
    );
    for forbidden_phase in ["snapshot_restore_entry", "snapshot_restore_publication"] {
        assert!(
            !has_lifecycle_phase("products", forbidden_phase),
            "{case_name} must not reach {forbidden_phase}"
        );
    }
    assert_eq!(
        crate::test_helpers::snapshot_tree(&baseline.tenant_path),
        baseline.tenant_tree,
        "{case_name} changed the tenant tree"
    );
    assert_eq!(
        crate::test_helpers::snapshot_tree(&baseline.publication_path),
        baseline.publication_tree,
        "{case_name} changed durable publication state"
    );
}

fn assert_destination_unchanged(
    case_name: &str,
    destination_kind: ReleaseImportDestination,
    destination: &Arc<AppState>,
    baseline: &RejectionBaseline,
) {
    if matches!(destination_kind, ReleaseImportDestination::Existing) {
        assert_document_title(destination, "products", "original", "original destination");
        assert!(destination
            .manager
            .get_document("products", "1")
            .unwrap()
            .is_none());
    } else {
        assert!(
            !baseline.tenant_path.exists(),
            "{case_name} created an absent tenant"
        );
        assert!(
            !baseline.publication_path.exists(),
            "{case_name} created an absent publication namespace"
        );
    }
}

fn assert_original_destination_persisted(case_name: &str, restarted: &Arc<AppState>) {
    assert_original_destination_persisted_for(case_name, restarted, "products");
}

fn assert_original_destination_persisted_for(
    case_name: &str,
    restarted: &Arc<AppState>,
    tenant_id: &str,
) {
    let documents = restarted
        .manager
        .search(tenant_id, "", None, None, 10)
        .unwrap();
    assert_eq!(documents.total, 1, "{case_name} after restart");
    assert_document_title(restarted, tenant_id, "original", "original destination");
    assert!(restarted
        .manager
        .get_document(tenant_id, "1")
        .unwrap()
        .is_none());
}

fn assert_restarted_destination_unchanged(
    case_name: &str,
    destination_kind: ReleaseImportDestination,
    restarted: &Arc<AppState>,
) {
    let tenant_path = restarted.manager.base_path.join("products");
    let publication_path = restarted
        .manager
        .base_path
        .join(".publication")
        .join("products");
    if matches!(destination_kind, ReleaseImportDestination::Existing) {
        assert_original_destination_persisted(case_name, restarted);
    } else {
        assert!(
            !tenant_path.exists(),
            "{case_name} created a tenant after restart"
        );
        assert!(
            !publication_path.exists(),
            "{case_name} retained publication state after restart"
        );
    }
}

async fn assert_rejected_without_restore_effects(
    case: &ReleaseImportCase,
    destination_kind: ReleaseImportDestination,
) {
    let (destination_tmp, destination, app) = release_import_destination(destination_kind).await;
    let baseline = rejection_baseline(&destination, destination_kind).await;

    let response = app.clone().oneshot(case.request()).await.unwrap();
    assert_release_import_bad_request(response, &case.name).await;
    assert_no_restore_effects(&case.name, &baseline);
    assert_destination_unchanged(&case.name, destination_kind, &destination, &baseline);

    destination.manager.graceful_shutdown().await;
    drop(app);
    drop(destination);
    let restarted = TestStateBuilder::new(&destination_tmp).build_shared();
    assert_restarted_destination_unchanged(&case.name, destination_kind, &restarted);
}

async fn run_rejection_cases(cases: &[ReleaseImportCase]) {
    for case in cases {
        for destination_kind in [
            ReleaseImportDestination::Absent,
            ReleaseImportDestination::Existing,
        ] {
            assert_rejected_without_restore_effects(case, destination_kind).await;
        }
    }
}

#[tokio::test]
async fn release_import_rejects_every_partial_or_response_only_header_set_before_restore() {
    let (_source_tmp, snapshot_bytes) = release_import_snapshot_bytes().await;
    let snapshot_digest = format!("{:x}", Sha256::digest(&snapshot_bytes));
    let mut cases = Vec::new();

    for presence_mask in 1_u8..31 {
        let present_names = REQUIRED_RELEASE_IMPORT_HEADERS
            .iter()
            .enumerate()
            .filter(|(index, _)| presence_mask & (1 << index) != 0)
            .map(|(_, name)| *name)
            .collect::<Vec<_>>();
        cases.push(ReleaseImportCase {
            name: format!("required subset: {}", present_names.join(", ")),
            snapshot_bytes: snapshot_bytes.clone(),
            headers: present_names
                .into_iter()
                .filter_map(|name| {
                    release_header_value(name, &snapshot_digest, Some("7"))
                        .map(|value| (name, value))
                })
                .collect(),
        });
    }
    for forbidden_name in [
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        RELEASE_TRANSFER_STATUS_HEADER,
        RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER,
    ] {
        cases.push(ReleaseImportCase {
            name: format!("response-only singleton: {forbidden_name}"),
            snapshot_bytes: snapshot_bytes.clone(),
            headers: vec![(forbidden_name, HeaderValue::from_static("sentinel"))],
        });
    }

    assert_eq!(cases.len(), 33);
    run_rejection_cases(&cases).await;
}

fn replace_value_case(
    name: &str,
    snapshot_bytes: &[u8],
    snapshot_digest: &str,
    header_name: &'static str,
    header_value: HeaderValue,
) -> ReleaseImportCase {
    let mut headers = release_import_headers(snapshot_digest, Some("7"));
    replace_release_header(&mut headers, header_name, header_value);
    ReleaseImportCase {
        name: name.to_string(),
        snapshot_bytes: snapshot_bytes.to_vec(),
        headers,
    }
}

fn add_duplicate_header_cases(
    cases: &mut Vec<ReleaseImportCase>,
    snapshot_bytes: &[u8],
    snapshot_digest: &str,
) {
    for header_name in REQUIRED_RELEASE_IMPORT_HEADERS {
        let value = release_header_value(header_name, snapshot_digest, Some("7")).unwrap();
        let mut duplicate_headers = release_import_headers(snapshot_digest, Some("7"));
        duplicate_headers.push((header_name, value.clone()));
        cases.push(ReleaseImportCase {
            name: format!("duplicate {header_name}"),
            snapshot_bytes: snapshot_bytes.to_vec(),
            headers: duplicate_headers,
        });
        cases.push(replace_value_case(
            &format!("comma-combined {header_name}"),
            snapshot_bytes,
            snapshot_digest,
            header_name,
            HeaderValue::from_str(&format!(
                "{}, {}",
                value.to_str().unwrap(),
                value.to_str().unwrap()
            ))
            .unwrap(),
        ));
    }
}

fn add_contract_and_transaction_cases(
    cases: &mut Vec<ReleaseImportCase>,
    snapshot_bytes: &[u8],
    snapshot_digest: &str,
) {
    for (name, value) in [
        ("unknown contract", HeaderValue::from_static("unknown")),
        ("empty contract", HeaderValue::from_static("")),
        (
            "padded contract",
            HeaderValue::from_static(" one-uid-contiguous-v1 "),
        ),
        (
            "non-visible-ASCII contract",
            HeaderValue::from_bytes(&[0x80]).unwrap(),
        ),
    ] {
        cases.push(replace_value_case(
            name,
            snapshot_bytes,
            snapshot_digest,
            RELEASE_TRANSFER_CONTRACT_HEADER,
            value,
        ));
    }
    for transaction in ["", ".", "..", "a..b", "a/b", "a\\b", "a b", "a:b"] {
        cases.push(replace_value_case(
            &format!("invalid transaction {transaction:?}"),
            snapshot_bytes,
            snapshot_digest,
            RELEASE_TRANSFER_TRANSACTION_HEADER,
            HeaderValue::from_str(transaction).unwrap(),
        ));
    }
}

fn add_sequence_cases(
    cases: &mut Vec<ReleaseImportCase>,
    snapshot_bytes: &[u8],
    snapshot_digest: &str,
) {
    for sequence in [
        "+1",
        "-1",
        "00",
        " 1",
        "1 ",
        "",
        "one",
        "18446744073709551616",
    ] {
        cases.push(replace_value_case(
            &format!("invalid through sequence {sequence:?}"),
            snapshot_bytes,
            snapshot_digest,
            RELEASE_TRANSFER_THROUGH_SEQ_HEADER,
            HeaderValue::from_str(sequence).unwrap(),
        ));
    }
}

fn add_digest_and_coordinate_cases(
    cases: &mut Vec<ReleaseImportCase>,
    snapshot_bytes: &[u8],
    snapshot_digest: &str,
) {
    for (name, digest) in [
        ("uppercase digest", snapshot_digest.to_uppercase()),
        ("63-character digest", "a".repeat(63)),
        ("65-character digest", "a".repeat(65)),
        ("nonhex digest", "g".repeat(64)),
        ("padded digest", format!(" {snapshot_digest}")),
        ("canonical incorrect digest", "0".repeat(64)),
    ] {
        cases.push(replace_value_case(
            name,
            snapshot_bytes,
            snapshot_digest,
            RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER,
            HeaderValue::from_str(&digest).unwrap(),
        ));
    }
    cases.push(replace_value_case(
        "tenant differs from route",
        snapshot_bytes,
        snapshot_digest,
        RELEASE_TRANSFER_TENANT_HEADER,
        HeaderValue::from_static("other-products"),
    ));
}

fn add_forbidden_header_cases(
    cases: &mut Vec<ReleaseImportCase>,
    snapshot_bytes: &[u8],
    snapshot_digest: &str,
) {
    for forbidden_name in [
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        RELEASE_TRANSFER_STATUS_HEADER,
        RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER,
    ] {
        for value in [HeaderValue::from_static("1"), HeaderValue::from_static("")] {
            let mut headers = release_import_headers(snapshot_digest, Some("7"));
            headers.push((forbidden_name, value));
            cases.push(ReleaseImportCase {
                name: format!("forbidden {forbidden_name}"),
                snapshot_bytes: snapshot_bytes.to_vec(),
                headers,
            });
        }
    }
}

fn add_alternate_gzip_digest_case(
    cases: &mut Vec<ReleaseImportCase>,
    snapshot_bytes: &[u8],
    snapshot_digest: &str,
) {
    let mut alternate_gzip = snapshot_bytes.to_vec();
    assert_eq!(&alternate_gzip[..2], &[0x1f, 0x8b]);
    alternate_gzip[4] ^= 1;
    assert_ne!(
        Sha256::digest(snapshot_bytes),
        Sha256::digest(&alternate_gzip),
        "gzip representations must have distinct raw-byte hashes"
    );
    let original_extract = TempDir::new().unwrap();
    let alternate_extract = TempDir::new().unwrap();
    flapjack::index::snapshot::import_from_bytes(snapshot_bytes, original_extract.path()).unwrap();
    flapjack::index::snapshot::import_from_bytes(&alternate_gzip, alternate_extract.path())
        .unwrap();
    assert_eq!(
        crate::test_helpers::snapshot_tree(original_extract.path()),
        crate::test_helpers::snapshot_tree(alternate_extract.path()),
        "gzip metadata mutation must preserve decoded snapshot content"
    );
    cases.push(ReleaseImportCase {
        name: "same decoded snapshot with a different raw-byte digest".to_string(),
        snapshot_bytes: alternate_gzip,
        headers: release_import_headers(snapshot_digest, Some("7")),
    });
}

#[tokio::test]
async fn release_import_rejects_noncanonical_metadata_and_raw_digest_mismatch_before_restore() {
    let (_source_tmp, snapshot_bytes) = release_import_snapshot_bytes().await;
    let snapshot_digest = format!("{:x}", Sha256::digest(&snapshot_bytes));
    let mut cases = Vec::new();

    add_duplicate_header_cases(&mut cases, &snapshot_bytes, &snapshot_digest);
    add_contract_and_transaction_cases(&mut cases, &snapshot_bytes, &snapshot_digest);
    add_sequence_cases(&mut cases, &snapshot_bytes, &snapshot_digest);
    add_digest_and_coordinate_cases(&mut cases, &snapshot_bytes, &snapshot_digest);
    add_forbidden_header_cases(&mut cases, &snapshot_bytes, &snapshot_digest);
    add_alternate_gzip_digest_case(&mut cases, &snapshot_bytes, &snapshot_digest);

    run_rejection_cases(&cases).await;
}

mod admitted_tests {
    use super::*;

    include!("snapshot_release_import_admitted_tests.rs");
}
