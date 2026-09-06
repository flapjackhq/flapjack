//! Stub summary for security.rs.

/// Whether the operator has explicitly opted in to loopback / private
/// outbound destinations.
///
/// Defaults to `false` (fail-closed). The opt-in exists for legitimate
/// local-AI deployments — running Ollama / vLLM / llama.cpp on
/// `http://127.0.0.1:PORT` as either a chat model server or an embedder
/// server. Both seams consume this same flag; setting it once via env opts
/// in both consistently.
///
/// **Link-local / metadata / unspecified destinations are NOT covered by
/// this opt-in.** Those have no legitimate AI-provider use (the EC2/GCP/
/// Azure cloud-metadata endpoint at `169.254.169.254` is a pure SSRF
/// target) and stay blocked unconditionally at the per-seam policy split.
/// Callers consult this flag for the loopback/private class only.
///
/// Accepted truthy values match the chat-side precedent that was in place
/// before this SSOT extraction: `"1"`, `"true"`, `"yes"`, `"on"`
/// (case-insensitive, surrounding whitespace trimmed). Any other value —
/// including empty string, `"0"`, and absence of the variable — is
/// fail-closed.
pub fn allow_local_outbound_urls() -> bool {
    std::env::var("FLAPJACK_AI_ALLOW_LOCAL_URLS")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Reason string when `ip` must be blocked as an outbound destination, or
/// `None` when it is acceptable under the current policy.
pub fn outbound_ip_block_reason(ip: &std::net::IpAddr, allow_local: bool) -> Option<&'static str> {
    if is_always_blocked_ip(ip) {
        return Some("link-local/metadata destination");
    }
    if !allow_local && is_local_network_ip(ip) {
        return Some("private or local destination");
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VettedOutboundUrlTarget {
    pub host: String,
    pub port: Option<u16>,
    pub resolved_ips: Vec<std::net::IpAddr>,
}

/// A crawler URL admitted by the strict public-HTTPS policy.
///
/// The canonical URL retains its hostname for HTTP Host and TLS SNI. The
/// complete DNS answer set is carried separately so the fetch client can pin
/// exactly the addresses that were checked. Callers must admit every fetch
/// afresh; this value is deliberately not a DNS cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VettedCrawlerUrlTarget {
    pub canonical_url: reqwest::Url,
    pub host: String,
    pub port: u16,
    pub resolved_ips: Vec<std::net::IpAddr>,
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) const CRAWLER_FIXTURE_HOST_ENV: &str = "FLAPJACK_TEST_CRAWLER_FIXTURE_HOST";
#[cfg(any(test, feature = "fault-injection"))]
pub(crate) const CRAWLER_FIXTURE_PUBLIC_IP_ENV: &str = "FLAPJACK_TEST_CRAWLER_FIXTURE_PUBLIC_IP";
#[cfg(any(test, feature = "fault-injection"))]
pub(crate) const CRAWLER_FIXTURE_ENDPOINT_ENV: &str = "FLAPJACK_TEST_CRAWLER_FIXTURE_ENDPOINT";
#[cfg(any(test, feature = "fault-injection"))]
pub(crate) const CRAWLER_FIXTURE_CA_PATH_ENV: &str = "FLAPJACK_TEST_CRAWLER_FIXTURE_CA_PATH";

/// Explicit, feature-gated transport coordinates for one hermetic crawler
/// fixture. The public IP is admission evidence only; transport is pinned to
/// the loopback endpoint after the ordinary strict-public check accepts it.
#[cfg(any(test, feature = "fault-injection"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrawlerFixtureTransportConfig {
    pub host: String,
    pub public_ip: std::net::IpAddr,
    pub endpoint: std::net::SocketAddr,
    pub ca_path: std::path::PathBuf,
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) fn crawler_fixture_transport_config() -> Result<Option<CrawlerFixtureTransportConfig>, ()>
{
    let values = [
        std::env::var_os(CRAWLER_FIXTURE_HOST_ENV),
        std::env::var_os(CRAWLER_FIXTURE_PUBLIC_IP_ENV),
        std::env::var_os(CRAWLER_FIXTURE_ENDPOINT_ENV),
        std::env::var_os(CRAWLER_FIXTURE_CA_PATH_ENV),
    ];
    if values.iter().all(Option::is_none) {
        return Ok(None);
    }
    let [Some(host), Some(public_ip), Some(endpoint), Some(ca_path)] = values else {
        return Err(());
    };
    let host = host.into_string().map_err(|_| ())?;
    let public_ip = public_ip.into_string().map_err(|_| ())?;
    let endpoint = endpoint.into_string().map_err(|_| ())?;
    let ca_path = std::path::PathBuf::from(ca_path);
    if host.trim() != host || host != host.to_ascii_lowercase() || !fixture_hostname_is_exact(&host)
    {
        return Err(());
    }
    let public_ip = public_ip.parse::<std::net::IpAddr>().map_err(|_| ())?;
    if !is_strict_public_ip(&public_ip) {
        return Err(());
    }
    let endpoint = endpoint.parse::<std::net::SocketAddr>().map_err(|_| ())?;
    if !endpoint.ip().is_loopback() || endpoint.port() == 0 {
        return Err(());
    }
    if !ca_path.is_absolute() {
        return Err(());
    }
    let ca_metadata = std::fs::symlink_metadata(&ca_path).map_err(|_| ())?;
    if ca_metadata.file_type().is_symlink() || !ca_metadata.is_file() {
        return Err(());
    }
    Ok(Some(CrawlerFixtureTransportConfig {
        host,
        public_ip,
        endpoint,
        ca_path,
    }))
}

#[cfg(any(test, feature = "fault-injection"))]
fn fixture_hostname_is_exact(host: &str) -> bool {
    host.len() <= 253
        && host.contains('.')
        && host.parse::<std::net::IpAddr>().is_err()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

impl VettedCrawlerUrlTarget {
    pub fn socket_addrs(&self) -> Vec<std::net::SocketAddr> {
        self.resolved_ips
            .iter()
            .copied()
            .map(|ip| std::net::SocketAddr::new(ip, self.port))
            .collect()
    }
}

/// Safe crawler admission failures. Deliberately carries no URL, hostname,
/// query, credential, or resolver detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CrawlerTargetAdmissionError {
    #[error("Crawler target is not allowed")]
    TargetRejected,
    #[error("Crawler target DNS validation failed")]
    DnsResolutionFailed,
}

/// Admit one fetch target under the crawler's fail-closed public-HTTPS rule.
///
/// Unlike [`vet_outbound_url_target`], this owner never permits HTTP, local
/// opt-ins, unresolved DNS, empty answers, or mixed public/non-public answers.
/// It rejects URL credentials and removes fragments from fetch identity.
pub fn vet_crawler_url_target(
    raw_url: &str,
) -> Result<VettedCrawlerUrlTarget, CrawlerTargetAdmissionError> {
    use CrawlerTargetAdmissionError::{DnsResolutionFailed, TargetRejected};

    let mut canonical_url = reqwest::Url::parse(raw_url).map_err(|_| TargetRejected)?;
    if canonical_url.scheme() != "https"
        || !canonical_url.username().is_empty()
        || canonical_url.password().is_some()
    {
        return Err(TargetRejected);
    }
    canonical_url.set_fragment(None);
    let host = canonical_url
        .host_str()
        .ok_or(TargetRejected)?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    let port = canonical_url
        .port_or_known_default()
        .ok_or(TargetRejected)?;
    #[cfg(any(test, feature = "fault-injection"))]
    let fixture = crawler_fixture_transport_config().map_err(|_| TargetRejected)?;
    #[cfg(any(test, feature = "fault-injection"))]
    let fixture_public_ip = fixture
        .as_ref()
        .filter(|fixture| fixture.host == host)
        .map(|fixture| fixture.public_ip);
    #[cfg(not(any(test, feature = "fault-injection")))]
    let fixture_public_ip: Option<std::net::IpAddr> = None;
    let mut resolved_ips = match fixture_public_ip {
        Some(public_ip) => vec![public_ip],
        None => resolve_outbound_host_ips(&host, Some(port))
            .filter(|ips| !ips.is_empty())
            .ok_or(DnsResolutionFailed)?,
    };
    if resolved_ips.iter().any(|ip| !is_strict_public_ip(ip)) {
        return Err(DnsResolutionFailed);
    }
    resolved_ips.sort_unstable();
    resolved_ips.dedup();

    Ok(VettedCrawlerUrlTarget {
        canonical_url,
        host,
        port,
        resolved_ips,
    })
}

impl VettedOutboundUrlTarget {
    /// Return the already-vetted socket addresses a client must pin.
    ///
    /// Callers should pass this complete set to their HTTP client's resolver
    /// override instead of performing another DNS lookup after validation.
    pub fn socket_addrs(&self) -> Vec<std::net::SocketAddr> {
        let port = self
            .port
            .expect("vetted outbound URL targets always have a known port");
        self.resolved_ips
            .iter()
            .copied()
            .map(|ip| std::net::SocketAddr::new(ip, port))
            .collect()
    }
}

/// Parse and vet an outbound URL target under the shared policy.
///
/// Returns `Ok(None)` when hostname resolution is unavailable so callers keep
/// fail-open behavior for unresolved hosts at config-validation time.
pub fn vet_outbound_url_target(
    raw_url: &str,
    allow_local: bool,
) -> Result<Option<VettedOutboundUrlTarget>, String> {
    let parsed = reqwest::Url::parse(raw_url).map_err(|error| error.to_string())?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!("unsupported scheme `{scheme}`"));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| "URL must include a host".to_string())?;
    if host.eq_ignore_ascii_case("localhost") && !allow_local {
        return Err("localhost is not allowed".to_string());
    }

    let port = parsed.port_or_known_default();
    let Some(resolved_ips) = resolve_outbound_host_ips(host, port) else {
        return Ok(None);
    };

    if let Some((ip, reason)) = first_blocked_outbound_ip(&resolved_ips, allow_local) {
        return Err(format!("{reason} `{ip}` is not allowed"));
    }

    Ok(Some(VettedOutboundUrlTarget {
        host: host.to_string(),
        port,
        resolved_ips,
    }))
}

/// Vet a strict vendor endpoint and return the addresses to pin in the client.
///
/// The accepted host list is deliberately supplied by the vendor-fact owner.
/// An empty list therefore provides a conservative fail-closed constructor
/// while a hostname contract is still unknown.
pub fn vet_strict_vendor_url_target(
    raw_url: &str,
    accepted_hosts: &[&str],
) -> Result<VettedOutboundUrlTarget, &'static str> {
    const ENDPOINT_ERROR: &str = "Vendor endpoint is not allowed";
    const DNS_ERROR: &str = "Vendor endpoint DNS validation failed";

    let parsed = reqwest::Url::parse(raw_url).map_err(|_| ENDPOINT_ERROR)?;
    if parsed.scheme() != "https"
        || parsed.port_or_known_default() != Some(443)
        || parsed.port().is_some_and(|port| port != 443)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(ENDPOINT_ERROR);
    }

    let host = parsed.host_str().ok_or(ENDPOINT_ERROR)?;
    if host.parse::<std::net::IpAddr>().is_ok()
        || !accepted_hosts
            .iter()
            .any(|accepted| strict_vendor_host_matches(host, accepted))
    {
        return Err(ENDPOINT_ERROR);
    }

    let resolved_ips = resolve_outbound_host_ips(host, Some(443))
        .filter(|ips| !ips.is_empty())
        .ok_or(DNS_ERROR)?;
    if resolved_ips.iter().any(|ip| !is_public_vendor_ip(ip)) {
        return Err(DNS_ERROR);
    }

    Ok(VettedOutboundUrlTarget {
        host: host.to_ascii_lowercase(),
        port: Some(443),
        resolved_ips,
    })
}

/// Vet a Typesense Cloud endpoint and return the pinned addresses clients must use.
pub fn vet_typesense_cloud_url_target(
    raw_url: &str,
) -> Result<VettedOutboundUrlTarget, &'static str> {
    const TYPESENSE_CLOUD_HOST_SUFFIX: &str = ".typesense.net";
    vet_strict_vendor_url_target(raw_url, &[TYPESENSE_CLOUD_HOST_SUFFIX])
}

fn strict_vendor_host_matches(host: &str, accepted: &str) -> bool {
    if let Some(suffix) = accepted.strip_prefix('.') {
        return host.eq_ignore_ascii_case(suffix)
            || host
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", suffix.to_ascii_lowercase()));
    }
    host.eq_ignore_ascii_case(accepted)
}

/// Returns the first blocked destination IP for `host`, checking both literal
/// IP hosts and resolver results for non-literal hosts.
///
/// Resolution failure returns `None` so config validation does not require live
/// DNS and remains fail-open for currently-unresolvable hostnames.
pub fn first_blocked_outbound_host_ip(
    host: &str,
    port: Option<u16>,
    allow_local: bool,
) -> Option<(std::net::IpAddr, &'static str)> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return outbound_ip_block_reason(&ip, allow_local).map(|reason| (ip, reason));
    }

    first_blocked_outbound_ip(&resolve_outbound_host_ips(host, port)?, allow_local)
}

/// TODO: Document resolve_outbound_host_ips.
fn resolve_outbound_host_ips(host: &str, port: Option<u16>) -> Option<Vec<std::net::IpAddr>> {
    use std::net::ToSocketAddrs;

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Some(vec![ip]);
    }

    if let Some(test_resolver) = test_helpers::take_test_outbound_host_resolver() {
        return test_resolver(host, port);
    }

    Some(
        (host, port.unwrap_or(0))
            .to_socket_addrs()
            .ok()?
            .map(|sa| sa.ip())
            .collect(),
    )
}

fn first_blocked_outbound_ip(
    ips: &[std::net::IpAddr],
    allow_local: bool,
) -> Option<(std::net::IpAddr, &'static str)> {
    ips.iter()
        .find_map(|ip| outbound_ip_block_reason(ip, allow_local).map(|reason| (*ip, reason)))
}

fn is_always_blocked_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_link_local() || v4.is_broadcast() || v4.is_unspecified(),
        std::net::IpAddr::V6(v6) => {
            v6.is_unspecified()
                || v6.is_unicast_link_local()
                || v6.to_ipv4_mapped().is_some_and(|v4| {
                    v4.is_link_local() || v4.is_broadcast() || v4.is_unspecified()
                })
        }
    }
}

fn is_local_network_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unique_local()
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_loopback() || v4.is_private())
        }
    }
}

fn is_public_vendor_ip(ip: &std::net::IpAddr) -> bool {
    if outbound_ip_block_reason(ip, false).is_some() {
        return false;
    }

    match ip {
        std::net::IpAddr::V4(ip) => {
            let octets = ip.octets();
            !(ip.is_documentation()
                || ip.is_multicast()
                || octets[0] == 100 && (64..=127).contains(&octets[1])
                || octets[0] == 198 && (18..=19).contains(&octets[1])
                || octets[0] >= 224)
        }
        std::net::IpAddr::V6(ip) => {
            !(ip.is_multicast() || ip.segments()[0] == 0x2001 && ip.segments()[1] == 0x0db8)
                && ip
                    .to_ipv4_mapped()
                    .is_none_or(|mapped| is_public_vendor_ip(&std::net::IpAddr::V4(mapped)))
        }
    }
}

/// Conservative globally-routable classification for crawler destinations.
///
/// This intentionally rejects whole special-purpose allocations instead of
/// maintaining exception lists inside them. A false negative is safe; a false
/// positive can become an SSRF primitive.
fn is_strict_public_ip(ip: &std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(matches!(a, 0 | 10 | 127)
                || a == 100 && (64..=127).contains(&b)
                || a == 169 && b == 254
                || a == 172 && (16..=31).contains(&b)
                || a == 192 && b == 0 && c == 0
                || a == 192 && b == 0 && c == 2
                || a == 192 && b == 88 && c == 99
                || a == 192 && b == 168
                || a == 198 && (18..=19).contains(&b)
                || a == 198 && b == 51 && c == 100
                || a == 203 && b == 0 && c == 113
                || a >= 224)
        }
        std::net::IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_strict_public_ip(&std::net::IpAddr::V4(mapped));
            }
            let segments = ip.segments();
            // Admit only global-unicast 2000::/3, then subtract the
            // special-purpose ranges inside that allocation.
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && segments[1] <= 0x01ff)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] <= 0x0fff)
        }
    }
}

/// Test helpers for opting in / out of the local-URL policy in unit tests.
///
/// Scope of intended use: tests that exercise the full production hydration
/// path (settings.json → `IndexSettings::load` → embedder construction →
/// wiremock loopback). Those tests legitimately need to simulate the
/// operator opt-in because they reproduce the operator's runtime
/// configuration, not because of test-only ceremony.
///
/// Tests that construct `EmbedderConfig` literals and call embedder
/// constructors directly do NOT need this helper — those code paths
/// intentionally skip URL safety (it lives at the trust boundary, not at
/// construction time), so wiremock loopback URLs flow through unhindered.
///
/// Every test that uses this guard MUST also be annotated with
/// `#[serial_test::serial(flapjack_outbound_url_policy)]` so concurrent
/// tests on the process-global env var do not race.
///
/// **Always compiled (not behind `#[cfg(test)]`)** so downstream crates'
/// test binaries — notably `flapjack-http` integration tests that hydrate
/// settings through `IndexSettings::load` — can reach the helper. The
/// type is zero-cost when not constructed; the carry-over of a few RAII
/// types into the release binary is acceptable in exchange for SSOT on
/// the opt-in semantics across both test populations.
pub mod test_helpers {
    type OutboundHostResolver =
        dyn Fn(&str, Option<u16>) -> Option<Vec<std::net::IpAddr>> + Send + Sync;

    struct OutboundHostResolverEntry {
        guard_identity: std::sync::Arc<()>,
        resolver: std::sync::Arc<OutboundHostResolver>,
    }

    #[derive(Default)]
    struct OutboundHostResolverState {
        live_resolvers: Vec<OutboundHostResolverEntry>,
    }

    fn outbound_host_resolver_slot() -> &'static std::sync::Mutex<OutboundHostResolverState> {
        static SLOT: std::sync::OnceLock<std::sync::Mutex<OutboundHostResolverState>> =
            std::sync::OnceLock::new();
        SLOT.get_or_init(|| std::sync::Mutex::new(OutboundHostResolverState::default()))
    }

    pub(crate) fn take_test_outbound_host_resolver() -> Option<std::sync::Arc<OutboundHostResolver>>
    {
        outbound_host_resolver_slot()
            .lock()
            .expect("test outbound host resolver slot mutex poisoned")
            .live_resolvers
            .last()
            .map(|entry| std::sync::Arc::clone(&entry.resolver))
    }

    /// RAII guard for a test-only outbound hostname resolver override.
    ///
    /// The override is consumed by `first_blocked_outbound_host_ip` before it
    /// falls back to OS DNS resolution. Returning `None` from the resolver
    /// preserves fail-open-on-unresolved-host semantics for that lookup.
    pub struct OutboundHostResolverGuard {
        identity: std::sync::Arc<()>,
    }

    impl Drop for OutboundHostResolverGuard {
        fn drop(&mut self) {
            let mut state = outbound_host_resolver_slot()
                .lock()
                .expect("test outbound host resolver slot mutex poisoned");
            let entry_index = state
                .live_resolvers
                .iter()
                .position(|entry| std::sync::Arc::ptr_eq(&entry.guard_identity, &self.identity))
                .expect("test outbound host resolver guard must remain live until drop");
            state.live_resolvers.remove(entry_index);
        }
    }

    /// Install a test-only outbound hostname resolver override and return an
    /// RAII guard that restores the nearest still-live predecessor on drop.
    pub fn install_test_outbound_host_resolver(
        resolver: std::sync::Arc<OutboundHostResolver>,
    ) -> OutboundHostResolverGuard {
        let mut state = outbound_host_resolver_slot()
            .lock()
            .expect("test outbound host resolver slot mutex poisoned");
        let identity = std::sync::Arc::new(());
        state.live_resolvers.push(OutboundHostResolverEntry {
            guard_identity: std::sync::Arc::clone(&identity),
            resolver,
        });
        OutboundHostResolverGuard { identity }
    }

    /// RAII guard: override `FLAPJACK_AI_ALLOW_LOCAL_URLS` for the guard's
    /// lifetime, then restore the prior value (or absence) on drop. The
    /// guard restores correctly even if the test panics, so the env state
    /// stays clean for subsequent tests.
    pub struct AllowLocalUrlsGuard {
        prior: Option<String>,
    }

    impl AllowLocalUrlsGuard {
        /// Opt in: set the env var to `"1"` for the guard lifetime.
        pub fn enable() -> Self {
            let prior = std::env::var("FLAPJACK_AI_ALLOW_LOCAL_URLS").ok();
            std::env::set_var("FLAPJACK_AI_ALLOW_LOCAL_URLS", "1");
            Self { prior }
        }

        /// Set to an arbitrary string value (used by the security-helper's
        /// own truthy-value parsing tests).
        pub fn set(value: &str) -> Self {
            let prior = std::env::var("FLAPJACK_AI_ALLOW_LOCAL_URLS").ok();
            std::env::set_var("FLAPJACK_AI_ALLOW_LOCAL_URLS", value);
            Self { prior }
        }

        /// Force fail-closed posture for the guard's lifetime.
        pub fn clear() -> Self {
            let prior = std::env::var("FLAPJACK_AI_ALLOW_LOCAL_URLS").ok();
            std::env::remove_var("FLAPJACK_AI_ALLOW_LOCAL_URLS");
            Self { prior }
        }
    }

    impl Drop for AllowLocalUrlsGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var("FLAPJACK_AI_ALLOW_LOCAL_URLS", v),
                None => std::env::remove_var("FLAPJACK_AI_ALLOW_LOCAL_URLS"),
            }
        }
    }
}

#[cfg(test)]
#[path = "security_tests.rs"]
mod tests;
