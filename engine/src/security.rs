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
