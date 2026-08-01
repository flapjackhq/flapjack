use crate::error::Result;
use s3::bucket::Bucket;
use s3::creds::Credentials;
use s3::Region;

const SSE_ENV: &str = "FLAPJACK_S3_SSE";
const SSE_KMS_KEY_ID_ENV: &str = "FLAPJACK_S3_SSE_KMS_KEY_ID";
const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_KMS_KEY_ID_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";

#[derive(Clone)]
pub struct S3Config {
    pub bucket_name: String,
    pub region: String,
    pub endpoint: Option<String>,
}

impl S3Config {
    pub fn from_env() -> Option<Self> {
        let bucket_name = std::env::var("FLAPJACK_S3_BUCKET").ok()?;
        let region = std::env::var("FLAPJACK_S3_REGION").unwrap_or_else(|_| "us-east-1".into());
        let endpoint = std::env::var("FLAPJACK_S3_ENDPOINT").ok();
        Some(Self {
            bucket_name,
            region,
            endpoint,
        })
    }

    /// Construct an S3 `Bucket` from the configured name, region, and credentials,
    /// applying path-style addressing when a custom endpoint is set.
    pub fn bucket_internal(&self) -> Result<Box<Bucket>> {
        let bucket = Bucket::new(
            &self.bucket_name,
            self.region()?,
            Credentials::default().map_err(|error| s3_error("S3 credentials", error))?,
        )
        .map_err(|error| s3_error("S3 bucket", error))?;
        Ok(if self.endpoint.is_some() {
            // MinIO and most S3-compatible stores require path-style addressing
            // (http://host:port/bucket/key) instead of virtual-hosted-style
            // (http://bucket.host:port/key). Enable path-style when a custom endpoint is set.
            bucket.with_path_style()
        } else {
            bucket
        })
    }

    fn region(&self) -> Result<Region> {
        match &self.endpoint {
            Some(ep) => Ok(Region::Custom {
                region: self.region.clone(),
                endpoint: ep.clone(),
            }),
            None => self
                .region
                .parse()
                .map_err(|error| s3_error("Invalid region", error)),
        }
    }
}

fn s3_error(context: &str, error: impl std::fmt::Display) -> crate::error::FlapjackError {
    crate::error::FlapjackError::S3(format!("{context}: {error}"))
}

enum SnapshotServerSideEncryption {
    Aes256,
    AwsKms { key_id: Option<String> },
}

impl SnapshotServerSideEncryption {
    fn from_env() -> Result<Self> {
        match std::env::var(SSE_ENV) {
            // An explicit AES256 header is the safest compatibility default because it
            // satisfies S3-compatible policies that require SSE without requiring KMS.
            Ok(value) if value == "AES256" => Ok(Self::Aes256),
            Ok(value) if value == "aws:kms" => Ok(Self::AwsKms {
                key_id: optional_kms_key_id_from_env()?,
            }),
            Ok(value) => Err(s3_error(
                "Invalid FLAPJACK_S3_SSE",
                format!("expected AES256 or aws:kms, got {value:?}"),
            )),
            Err(std::env::VarError::NotPresent) => Ok(Self::Aes256),
            Err(error) => Err(s3_error("Invalid FLAPJACK_S3_SSE", error)),
        }
    }

    fn algorithm(&self) -> &'static str {
        match self {
            Self::Aes256 => "AES256",
            Self::AwsKms { .. } => "aws:kms",
        }
    }

    fn kms_key_id(&self) -> Option<&str> {
        match self {
            Self::Aes256 => None,
            Self::AwsKms { key_id } => key_id.as_deref(),
        }
    }
}

fn optional_kms_key_id_from_env() -> Result<Option<String>> {
    match std::env::var(SSE_KMS_KEY_ID_ENV) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(s3_error("Invalid FLAPJACK_S3_SSE_KMS_KEY_ID", error)),
    }
}

pub async fn upload_snapshot(config: &S3Config, index_name: &str, data: &[u8]) -> Result<String> {
    let sse = SnapshotServerSideEncryption::from_env()?;
    let bucket = config.bucket_internal()?;
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let key = format!("snapshots/{}/{}.tar.gz", index_name, timestamp);

    let mut builder = bucket
        .put_object_builder(&key, data)
        .with_server_side_encryption(sse.algorithm())
        .map_err(|error| s3_error("S3 upload", error))?;
    if let Some(kms_key_id) = sse.kms_key_id() {
        builder = builder
            .with_header(SSE_KMS_KEY_ID_HEADER, kms_key_id)
            .map_err(|error| s3_error("S3 upload", error))?;
    }

    let response = builder
        .execute()
        .await
        .map_err(|error| s3_error("S3 upload", error))?;
    let status = response.status_code();
    if !(200..300).contains(&status) {
        // engine/Cargo.toml disables rust-s3 default features, so fail-on-err is absent
        // and rejected PUT responses must be checked here.
        return Err(s3_error("S3 upload", format!("HTTP {status}")));
    }

    let response_headers = response.headers();
    let sse_echo = response_headers.get(SSE_HEADER);
    if sse_echo.is_none() {
        tracing::warn!(
            "S3 upload response missing {} header for s3://{}/{}",
            SSE_HEADER,
            config.bucket_name,
            key
        );
    }

    tracing::info!(
        sse = sse_echo.map(String::as_str).unwrap_or("missing"),
        "Uploaded snapshot s3://{}/{}",
        config.bucket_name,
        key
    );
    Ok(key)
}

pub async fn download_snapshot(config: &S3Config, key: &str) -> Result<Vec<u8>> {
    let bucket = config.bucket_internal()?;
    let response = bucket
        .get_object(key)
        .await
        .map_err(|e| crate::error::FlapjackError::S3(format!("S3 download: {}", e)))?;
    if response.status_code() != 200 {
        return Err(crate::error::FlapjackError::S3(format!(
            "S3 download failed: HTTP {}",
            response.status_code()
        )));
    }
    Ok(response.to_vec())
}

pub async fn download_latest_snapshot(
    config: &S3Config,
    index_name: &str,
) -> Result<(String, Vec<u8>)> {
    let keys = list_snapshots(config, index_name).await?;
    let latest = keys.last().ok_or_else(|| {
        crate::error::FlapjackError::S3(format!("No snapshots found for {}", index_name))
    })?;
    let data = download_snapshot(config, latest).await?;
    Ok((latest.clone(), data))
}

pub async fn list_snapshots(config: &S3Config, index_name: &str) -> Result<Vec<String>> {
    let bucket = config.bucket_internal()?;
    let prefix = format!("snapshots/{}/", index_name);
    let results = bucket
        .list(prefix, None)
        .await
        .map_err(|e| crate::error::FlapjackError::S3(format!("S3 list: {}", e)))?;
    let mut keys: Vec<String> = results
        .into_iter()
        .flat_map(|r| r.contents)
        .map(|obj| obj.key)
        .collect();
    keys.sort();
    Ok(keys)
}

pub async fn delete_snapshot(config: &S3Config, key: &str) -> Result<()> {
    let bucket = config.bucket_internal()?;
    bucket
        .delete_object(key)
        .await
        .map_err(|e| crate::error::FlapjackError::S3(format!("S3 delete: {}", e)))?;
    Ok(())
}

pub async fn enforce_retention(config: &S3Config, index_name: &str, keep: usize) -> Result<usize> {
    let keys = list_snapshots(config, index_name).await?;
    if keys.len() <= keep {
        return Ok(0);
    }
    let to_delete = &keys[..keys.len() - keep];
    for key in to_delete {
        delete_snapshot(config, key).await?;
        tracing::info!("Deleted old snapshot: {}", key);
    }
    Ok(to_delete.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    const SSE_HEADER: &str = "x-amz-server-side-encryption";
    const SSE_KMS_KEY_ID_HEADER: &str = "x-amz-server-side-encryption-aws-kms-key-id";
    const SSE_ENV: &str = "FLAPJACK_S3_SSE";
    const SSE_KMS_KEY_ID_ENV: &str = "FLAPJACK_S3_SSE_KMS_KEY_ID";

    struct EnvVarRestoreGuard {
        name: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarRestoreGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }

        fn set_os(name: &'static str, value: OsString) -> Self {
            let previous = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, previous }
        }

        fn remove(name: &'static str) -> Self {
            let previous = std::env::var_os(name);
            std::env::remove_var(name);
            Self { name, previous }
        }
    }

    impl Drop for EnvVarRestoreGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    fn set_dummy_aws_creds() {
        std::env::set_var("AWS_ACCESS_KEY_ID", "test");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
    }

    fn test_config(endpoint: Option<&str>) -> S3Config {
        S3Config {
            bucket_name: "test-bucket".into(),
            region: "us-east-1".into(),
            endpoint: endpoint.map(str::to_owned),
        }
    }

    fn error_identifies_http_status(error: &str, expected_status: u16) -> bool {
        let expected_status = expected_status.to_string();
        error
            .split(|character: char| !character.is_ascii_digit())
            .any(|token| token == expected_status)
    }

    async fn capture_upload_request(
        server: &MockServer,
        expected_result: std::result::Result<(), u16>,
    ) -> Vec<wiremock::Request> {
        let result =
            upload_snapshot(&test_config(Some(&server.uri())), "products", b"snapshot").await;
        match expected_result {
            Ok(()) => {
                result.expect("upload_snapshot should succeed");
            }
            Err(expected_status) => {
                let error = result.expect_err("upload_snapshot should fail").to_string();
                assert!(
                    error_identifies_http_status(&error, expected_status),
                    "expected error to identify HTTP status {expected_status}, got {error:?}"
                );
            }
        }

        let requests = server
            .received_requests()
            .await
            .expect("recorded requests should be available");
        let put_requests = requests
            .into_iter()
            .filter(|request| request.method.as_str() == "PUT")
            .collect::<Vec<_>>();
        put_requests
    }

    #[test]
    fn error_status_matcher_accepts_equivalent_403_text() {
        for error in [
            "upload failed with HTTP 403",
            "403 Forbidden",
            "upload failed (status=403)",
        ] {
            assert!(
                error_identifies_http_status(error, 403),
                "expected equivalent 403 status text to match: {error:?}"
            );
        }
        for error in ["request id 1403", "upload failed with HTTP 404"] {
            assert!(
                !error_identifies_http_status(error, 403),
                "expected non-403 text not to match: {error:?}"
            );
        }
    }

    async fn assert_upload_sse_headers(
        sse_env: Option<&str>,
        expected_sse: &str,
        configured_kms_key_id: Option<&str>,
        expected_kms_key_id_header: Option<&str>,
    ) {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let _access_key = EnvVarRestoreGuard::set("AWS_ACCESS_KEY_ID", "test");
        let _secret_key = EnvVarRestoreGuard::set("AWS_SECRET_ACCESS_KEY", "test");
        let _sse = match sse_env {
            Some(value) => EnvVarRestoreGuard::set(SSE_ENV, value),
            None => EnvVarRestoreGuard::remove(SSE_ENV),
        };
        let _kms_key_id = match configured_kms_key_id {
            Some(value) => EnvVarRestoreGuard::set(SSE_KMS_KEY_ID_ENV, value),
            None => EnvVarRestoreGuard::remove(SSE_KMS_KEY_ID_ENV),
        };

        let put_requests = capture_upload_request(&server, Ok(())).await;
        assert_eq!(
            put_requests.len(),
            1,
            "upload_snapshot should make exactly one PUT request"
        );
        let request = &put_requests[0];
        assert_eq!(
            request
                .headers
                .get(SSE_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(expected_sse),
            "upload_snapshot should send the chosen SSE algorithm header"
        );
        assert_eq!(
            request
                .headers
                .get(SSE_KMS_KEY_ID_HEADER)
                .and_then(|value| value.to_str().ok()),
            expected_kms_key_id_header,
            "upload_snapshot should send the KMS key id only for aws:kms"
        );
    }

    #[test]
    #[serial_test::serial]
    fn bucket_internal_uses_path_style_when_endpoint_set() {
        set_dummy_aws_creds();

        let config = test_config(Some("http://localhost:9000"));
        let bucket = config.bucket_internal().expect("bucket_internal failed");
        assert!(
            bucket.is_path_style(),
            "bucket should use path-style when endpoint is set"
        );
    }

    #[test]
    #[serial_test::serial]
    fn bucket_internal_uses_virtual_hosted_style_when_no_endpoint() {
        set_dummy_aws_creds();

        let config = test_config(None);
        let bucket = config.bucket_internal().expect("bucket_internal failed");
        assert!(
            !bucket.is_path_style(),
            "bucket should use virtual-hosted-style when no endpoint"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn upload_snapshot_sends_sse_header() {
        assert_upload_sse_headers(None, "AES256", None, None).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn upload_snapshot_accepts_explicit_aes256_sse() {
        assert_upload_sse_headers(Some("AES256"), "AES256", None, None).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn upload_snapshot_omits_kms_key_id_for_aes256_sse() {
        assert_upload_sse_headers(
            Some("AES256"),
            "AES256",
            Some("arn:aws:kms:us-east-1:123456789012:key/ignored-for-aes256"),
            None,
        )
        .await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn upload_snapshot_accepts_kms_sse_with_key_id() {
        assert_upload_sse_headers(
            Some("aws:kms"),
            "aws:kms",
            Some("arn:aws:kms:us-east-1:123456789012:key/test-key"),
            Some("arn:aws:kms:us-east-1:123456789012:key/test-key"),
        )
        .await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn upload_snapshot_accepts_kms_sse_without_key_id() {
        assert_upload_sse_headers(Some("aws:kms"), "aws:kms", None, None).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn upload_snapshot_rejects_unrecognized_sse_before_put() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let _access_key = EnvVarRestoreGuard::set("AWS_ACCESS_KEY_ID", "test");
        let _secret_key = EnvVarRestoreGuard::set("AWS_SECRET_ACCESS_KEY", "test");
        let _sse = EnvVarRestoreGuard::set(SSE_ENV, "aes256");
        let _kms_key_id = EnvVarRestoreGuard::remove(SSE_KMS_KEY_ID_ENV);

        let result = upload_snapshot(&test_config(Some(&server.uri())), "products", b"snapshot")
            .await
            .expect_err("unrecognized SSE algorithm should fail before upload");
        assert!(
            result.to_string().contains(SSE_ENV),
            "error should name the invalid SSE environment variable: {result}"
        );
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded requests should be available")
                .is_empty(),
            "invalid SSE configuration must fail before any PUT is accepted"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial]
    async fn upload_snapshot_rejects_non_unicode_kms_key_id_before_put() {
        use std::os::unix::ffi::OsStringExt;

        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let _access_key = EnvVarRestoreGuard::set("AWS_ACCESS_KEY_ID", "test");
        let _secret_key = EnvVarRestoreGuard::set("AWS_SECRET_ACCESS_KEY", "test");
        let _sse = EnvVarRestoreGuard::set(SSE_ENV, "aws:kms");
        let _kms_key_id = EnvVarRestoreGuard::set_os(
            SSE_KMS_KEY_ID_ENV,
            OsString::from_vec(b"kms-\xFF".to_vec()),
        );

        let result = upload_snapshot(&test_config(Some(&server.uri())), "products", b"snapshot")
            .await
            .expect_err("non-Unicode KMS key id should fail before upload");
        assert!(
            result.to_string().contains(SSE_KMS_KEY_ID_ENV),
            "error should name the invalid KMS key id environment variable: {result}"
        );
        assert!(
            server
                .received_requests()
                .await
                .expect("recorded requests should be available")
                .is_empty(),
            "invalid KMS key id configuration must fail before any PUT is accepted"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn upload_snapshot_fails_loudly_when_bucket_rejects_the_put() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(403).set_body_string(
                "<Error><Code>AccessDenied</Code><Message>Access denied</Message></Error>",
            ))
            .mount(&server)
            .await;

        let _access_key = EnvVarRestoreGuard::set("AWS_ACCESS_KEY_ID", "test");
        let _secret_key = EnvVarRestoreGuard::set("AWS_SECRET_ACCESS_KEY", "test");
        let _sse = EnvVarRestoreGuard::remove(SSE_ENV);
        let _kms_key_id = EnvVarRestoreGuard::remove(SSE_KMS_KEY_ID_ENV);

        let put_requests = capture_upload_request(&server, Err(403)).await;
        assert_eq!(
            put_requests.len(),
            1,
            "rejected upload should still issue exactly one PUT"
        );
    }
}
