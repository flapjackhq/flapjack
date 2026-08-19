use crate::error::{FlapjackError, Result};
use dashmap::DashMap;
use instant_acme::{
    Account, AccountBuilder, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier,
    NewAccount, NewOrder, Order, RetryPolicy,
};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::Once;

#[cfg(test)]
static OBSERVED_ACCOUNT_DIRECTORIES: std::sync::Mutex<Vec<String>> =
    std::sync::Mutex::new(Vec::new());

/// Install the process-wide rustls crypto provider the ACME transport needs.
///
/// `instant-acme` builds a rustls client config, and rustls only auto-selects a
/// provider when exactly one provider feature is enabled in the final binary.
/// The server binary links both `ring` (this crate, via instant-acme) and
/// `aws-lc-rs` (the AWS SDKs), so auto-selection is ambiguous and rustls
/// *panics* instead of returning an error — taking the whole process down at
/// startup, past every `Result` seam in the SSL initialisation path.
///
/// Installing explicitly makes the choice unambiguous. `install_default`
/// returns `Err` when a provider is already installed (for example by
/// `flapjack-http`'s static-TLS serve path), which is a benign race we ignore:
/// both call sites install the same ring provider.
pub fn install_default_crypto_provider() {
    static INSTALL_PROVIDER: Once = Once::new();
    INSTALL_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// ACME client for handling Let's Encrypt certificate operations
pub struct AcmeClient {
    /// ACME account (persisted)
    account: Arc<Account>,
    /// Challenge responses (token -> key_authorization)
    /// Stored in-memory during http-01 validation
    challenges: Arc<DashMap<String, String>>,
}

struct ChallengeTokenGuard<'a> {
    challenges: &'a DashMap<String, String>,
    tokens: Vec<String>,
}

impl<'a> ChallengeTokenGuard<'a> {
    fn new(challenges: &'a DashMap<String, String>) -> Self {
        Self {
            challenges,
            tokens: Vec::new(),
        }
    }

    fn insert(&mut self, token: String, key_authorization: String) {
        self.challenges.insert(token.clone(), key_authorization);
        self.tokens.push(token);
    }
}

impl Drop for ChallengeTokenGuard<'_> {
    fn drop(&mut self) {
        for token in &self.tokens {
            self.challenges.remove(token);
        }
    }
}

impl AcmeClient {
    /// Create a new ACME client or load existing account
    pub async fn new(
        email: &str,
        acme_directory: &str,
        root_ca_pem: Option<&Path>,
    ) -> Result<Self> {
        tracing::info!("[SSL] Initializing ACME client for {}", email);

        let directory_url = acme_directory.to_string();
        tracing::info!("[SSL] Using ACME directory {}", directory_url);

        // Create a new account
        let new_account = NewAccount {
            contact: &[&format!("mailto:{}", email)],
            terms_of_service_agreed: true,
            only_return_existing: false,
        };
        let (account, _credentials) = Self::create_account(
            Self::account_builder(root_ca_pem)?,
            &new_account,
            directory_url,
        )
        .await
        .map_err(|e| FlapjackError::Acme(format!("Failed to create ACME account: {e}")))?;

        tracing::info!("[SSL] ACME account created successfully");

        Ok(Self {
            account: Arc::new(account),
            challenges: Arc::new(DashMap::new()),
        })
    }

    /// Request a new certificate for the configured ACME identifier.
    /// Returns (certificate_pem, private_key_pem)
    pub async fn request_certificate(&self, requested_value: &str) -> Result<(String, String)> {
        tracing::info!("[SSL] Requesting certificate for {}", requested_value);

        let identifier = Self::identifier_for_requested_value(requested_value)?;
        let mut order = self.create_order(identifier).await?;
        let mut challenge_tokens = ChallengeTokenGuard::new(&self.challenges);
        self.prepare_http01_challenges(&mut order, &mut challenge_tokens)
            .await?;
        self.wait_for_order_ready(&mut order).await?;
        self.finalize_order(&mut order).await
    }

    /// Get the challenge response for a given token (used by HTTP handler)
    pub fn get_challenge_response(&self, token: &str) -> Option<String> {
        self.challenges.get(token).map(|v| v.clone())
    }

    fn parse_requested_ip(ip: &str) -> Result<IpAddr> {
        ip.parse()
            .map_err(|e| FlapjackError::Acme(format!("Invalid IP address: {}", e)))
    }

    fn identifier_for_requested_value(requested_value: &str) -> Result<Identifier> {
        if let Ok(ip) = Self::parse_requested_ip(requested_value) {
            return Ok(Identifier::Ip(ip));
        }

        let dns_name = requested_value.trim();
        if dns_name.is_empty() {
            return Err(FlapjackError::Acme(
                "ACME identifier must not be empty".to_string(),
            ));
        }
        Ok(Identifier::Dns(dns_name.to_string()))
    }

    async fn create_order(&self, identifier: Identifier) -> Result<Order> {
        let order = self
            .account
            .new_order(&NewOrder::new(&[identifier]))
            .await
            .map_err(|e| FlapjackError::Acme(format!("Failed to create ACME order: {}", e)))?;

        tracing::info!("[SSL] ACME order created");
        Ok(order)
    }

    /// TODO: Document AcmeClient.prepare_http01_challenges.
    async fn prepare_http01_challenges(
        &self,
        order: &mut Order,
        challenge_tokens: &mut ChallengeTokenGuard<'_>,
    ) -> Result<()> {
        let mut authorizations = order.authorizations();

        while let Some(authz_result) = authorizations.next().await {
            let mut authz = authz_result
                .map_err(|e| FlapjackError::Acme(format!("Failed to get authorization: {}", e)))?;

            if matches!(authz.status, AuthorizationStatus::Valid) {
                continue;
            }

            let mut challenge = authz
                .challenge(ChallengeType::Http01)
                .ok_or_else(|| FlapjackError::Acme("No http-01 challenge found".to_string()))?;

            let token = challenge.token.clone();
            let key_authorization = challenge.key_authorization().as_str().to_string();
            challenge_tokens.insert(token.clone(), key_authorization);

            tracing::info!("[SSL] Stored http-01 challenge token: {}", token);

            challenge.set_ready().await.map_err(|e| {
                FlapjackError::Acme(format!("Failed to set challenge ready: {}", e))
            })?;

            tracing::info!("[SSL] Challenge marked as ready, waiting for validation...");
        }

        Ok(())
    }

    async fn wait_for_order_ready(&self, order: &mut Order) -> Result<()> {
        tracing::info!("[SSL] Polling for order ready status...");
        order
            .poll_ready(&RetryPolicy::default())
            .await
            .map(|_| ())
            .map_err(|e| FlapjackError::Acme(format!("Failed to poll order ready: {}", e)))
    }

    /// TODO: Document AcmeClient.finalize_order.
    async fn finalize_order(&self, order: &mut Order) -> Result<(String, String)> {
        tracing::info!("[SSL] Finalizing order...");
        let private_key_pem = order
            .finalize()
            .await
            .map_err(|e| FlapjackError::Acme(format!("Failed to finalize order: {}", e)))?;

        tracing::info!("[SSL] Polling for certificate...");
        let cert_chain_pem = order
            .poll_certificate(&RetryPolicy::default())
            .await
            .map_err(|e| FlapjackError::Acme(format!("Failed to poll certificate: {}", e)))?;

        tracing::info!("[SSL] Certificate issued successfully");
        Ok((cert_chain_pem, private_key_pem))
    }

    fn account_builder(root_ca_pem: Option<&Path>) -> Result<AccountBuilder> {
        // Must precede any rustls client construction inside instant-acme.
        install_default_crypto_provider();
        let builder = match root_ca_pem {
            Some(path) => Account::builder_with_root(path),
            None => Account::builder(),
        };
        builder.map_err(|e| {
            FlapjackError::Acme(format!("Failed to initialize ACME account builder: {}", e))
        })
    }

    async fn create_account(
        builder: AccountBuilder,
        new_account: &NewAccount<'_>,
        directory_url: String,
    ) -> std::result::Result<(Account, AccountCredentials), instant_acme::Error> {
        #[cfg(test)]
        {
            OBSERVED_ACCOUNT_DIRECTORIES
                .lock()
                .unwrap()
                .push(directory_url.clone());
        }
        builder.create(new_account, directory_url, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_requested_ip_accepts_ip_addresses() {
        let ip = AcmeClient::parse_requested_ip("127.0.0.1").unwrap();
        assert_eq!(ip, IpAddr::from([127, 0, 0, 1]));
    }

    #[test]
    fn parse_requested_ip_rejects_invalid_addresses() {
        let error = AcmeClient::parse_requested_ip("not-an-ip").unwrap_err();
        assert!(matches!(error, FlapjackError::Acme(_)));
    }

    #[test]
    fn requested_ip_value_constructs_ip_identifier() {
        assert_eq!(
            AcmeClient::identifier_for_requested_value("127.0.0.1").unwrap(),
            Identifier::Ip(IpAddr::from([127, 0, 0, 1]))
        );
    }

    #[test]
    fn requested_dns_value_constructs_dns_identifier() {
        let identifier = AcmeClient::identifier_for_requested_value("search.example.com");
        assert_eq!(
            identifier.ok(),
            Some(Identifier::Dns("search.example.com".to_string())),
            "DNS certificate requests must construct an ACME DNS identifier"
        );
    }

    #[tokio::test]
    async fn configured_acme_directory_reaches_account_builder_unchanged() {
        let directory = "https://127.0.0.1:1/arbitrary-acme-directory";
        OBSERVED_ACCOUNT_DIRECTORIES.lock().unwrap().clear();
        let error = match AcmeClient::new("test@example.com", directory, None).await {
            Ok(_) => panic!("the closed local endpoint must reject account creation"),
            Err(error) => error,
        };
        assert!(matches!(error, FlapjackError::Acme(_)));
        assert!(
            OBSERVED_ACCOUNT_DIRECTORIES
                .lock()
                .unwrap()
                .iter()
                .any(|observed| observed == directory),
            "the real account-builder boundary must receive the configured directory unchanged"
        );
    }

    #[test]
    fn account_builder_installs_a_process_crypto_provider() {
        // The ACME transport must not depend on rustls guessing a provider from
        // crate features: in the server binary two providers are linked and the
        // guess panics. Reaching the builder at all has to leave a process
        // default installed.
        let _ = AcmeClient::account_builder(None);
        assert!(
            rustls::crypto::CryptoProvider::get_default().is_some(),
            "the ACME account builder must install a process-wide rustls crypto provider"
        );
    }

    #[test]
    fn root_ca_override_fails_closed_before_account_creation() {
        let unique_suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the system clock must be after the unix epoch")
            .as_nanos();
        let missing_root = std::env::temp_dir().join(format!(
            "flapjack-missing-root-ca-{}-{unique_suffix}.pem",
            std::process::id()
        ));
        assert!(
            !missing_root.exists(),
            "the missing-root fixture must be absent"
        );
        let error = match AcmeClient::account_builder(Some(missing_root.as_path())) {
            Ok(_) => panic!("bad root CA override must fail before account creation"),
            Err(error) => error,
        };
        assert!(
            matches!(error, FlapjackError::Acme(_)),
            "bad root CA override must fail locally before account creation; got {error:?}"
        );
    }

    #[test]
    fn challenge_token_guard_removes_only_order_tokens_on_error_exit() {
        fn register_then_fail(challenges: &DashMap<String, String>) -> Result<()> {
            let mut guard = ChallengeTokenGuard::new(challenges);
            guard.insert("drop-1".to_string(), "value-2".to_string());
            guard.insert("drop-2".to_string(), "value-3".to_string());
            Err(FlapjackError::Acme("challenge setup failed".to_string()))
        }

        let challenges = DashMap::new();
        challenges.insert("keep".to_string(), "value-1".to_string());
        register_then_fail(&challenges).expect_err("the simulated setup must return its error");

        assert!(challenges.contains_key("keep"));
        assert!(!challenges.contains_key("drop-1"));
        assert!(!challenges.contains_key("drop-2"));
    }
}
