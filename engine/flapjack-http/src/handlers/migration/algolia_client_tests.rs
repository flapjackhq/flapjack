//! Stub summary for engine/flapjack-http/src/handlers/migration/algolia_client_tests.rs.
use super::*;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::collections::{HashSet, VecDeque};
use std::convert::Infallible;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

#[derive(Debug)]
struct CountingStaticResolver {
    calls: Mutex<Vec<String>>,
    address: SocketAddr,
}

impl CountingStaticResolver {
    fn new(address: SocketAddr) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            address,
        }
    }

    fn observed_hosts(&self) -> Vec<String> {
        self.calls
            .lock()
            .expect("resolver call list mutex poisoned")
            .clone()
    }
}

impl reqwest::dns::Resolve for CountingStaticResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        self.calls
            .lock()
            .expect("resolver call list mutex poisoned")
            .push(name.as_str().to_string());
        let selected = self.address;
        Box::pin(async move {
            let addrs: reqwest::dns::Addrs = Box::new(std::iter::once(selected));
            Ok(addrs)
        })
    }
}

#[derive(Debug, Clone)]
struct ScriptedTransport {
    responses: VecDeque<Result<RawResponse, AlgoliaClientError>>,
    requests: Vec<PlannedRequest>,
}

impl ScriptedTransport {
    fn new(responses: Vec<Result<RawResponse, AlgoliaClientError>>) -> Self {
        Self {
            responses: responses.into(),
            requests: Vec::new(),
        }
    }
}

impl AlgoliaTransport for ScriptedTransport {
    fn send<'a>(
        &'a mut self,
        request: PlannedRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RawResponse, AlgoliaClientError>> + Send + 'a>> {
        self.requests.push(request);
        let response = self.responses.pop_front().unwrap_or_else(|| {
            Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Transport,
                "scripted response missing",
            ))
        });
        Box::pin(async move { response })
    }
}

fn ok(body: Value) -> Result<RawResponse, AlgoliaClientError> {
    Ok(RawResponse {
        status: 200,
        body: serde_json::to_vec(&body).unwrap(),
    })
}

fn status(status: u16) -> Result<RawResponse, AlgoliaClientError> {
    Ok(RawResponse {
        status,
        body: br#"{"message":"hidden"}"#.to_vec(),
    })
}

fn request_urls(transport: &ScriptedTransport) -> Vec<&str> {
    transport
        .requests
        .iter()
        .map(|request| request.url.as_str())
        .collect()
}

/// Plan a request without touching the base-URL guard. Callers must already
/// hold either a vendor-host or override guard.
fn request_for_test(
    app_id: &str,
    index_name: &str,
    method: AlgoliaMethod,
    suffix: &str,
) -> Result<PlannedRequest, AlgoliaClientError> {
    plan_request(app_id, "key", method, index_path(index_name, suffix), None)
}

fn scripted_json_for_test(
    transport: &mut ScriptedTransport,
    app_id: &str,
    index_name: &str,
    method: AlgoliaMethod,
    suffix: &str,
) -> Result<Value, AlgoliaClientError> {
    let base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    execute_scripted_request_with_guard_for_test(
        &base_url_env,
        transport,
        request_for_test(app_id, index_name, method, suffix),
    )
}

fn execute_scripted_request_with_guard_for_test(
    _base_url_env: &AlgoliaBaseUrlEnvGuard,
    transport: &mut ScriptedTransport,
    request: Result<PlannedRequest, AlgoliaClientError>,
) -> Result<Value, AlgoliaClientError> {
    tokio_test::block_on(execute_json_with_retry(transport, request?))
}

fn list_indexes_for_test(
    transport: &mut ScriptedTransport,
) -> Result<Vec<AlgoliaIndexRecord>, AlgoliaClientError> {
    let base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    list_indexes_with_guard_for_test(&base_url_env, transport)
}

fn list_indexes_with_guard_for_test(
    _base_url_env: &AlgoliaBaseUrlEnvGuard,
    transport: &mut ScriptedTransport,
) -> Result<Vec<AlgoliaIndexRecord>, AlgoliaClientError> {
    tokio_test::block_on(list_indexes_with_transport(transport, "APP123", "key"))
}

fn list_indexes_with_limits_for_test(
    transport: &mut ScriptedTransport,
    limits: TraversalLimits,
) -> Result<Vec<AlgoliaIndexRecord>, AlgoliaClientError> {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    tokio_test::block_on(list_indexes_with_transport_and_limits(
        transport, "APP123", "key", limits,
    ))
}

fn key_allows_unretrievable_with_guard_for_test(
    _base_url_env: &AlgoliaBaseUrlEnvGuard,
    transport: &mut ScriptedTransport,
) -> Result<bool, AlgoliaClientError> {
    tokio_test::block_on(key_allows_unretrievable_with_transport(
        transport, "APP123", "key",
    ))
}

fn expected_algolia_validation_hosts(app_id: &str) -> Vec<(String, Option<u16>)> {
    // `vet_outbound_url_target` passes `Url::host_str()` to the resolver, and
    // URL parsing canonicalizes DNS names to lowercase before that callback.
    let app_id = app_id.to_ascii_lowercase();
    let mut hosts = vec![(format!("{app_id}-dsn.algolia.net"), Some(443))];
    hosts.extend(
        (1..=3).map(|host_index| (format!("{app_id}-{host_index}.algolianet.com"), Some(443))),
    );
    hosts.push((format!("{app_id}.algolia.net"), Some(443)));
    hosts
}

// Recorded (host, port) pairs the validation resolver was asked to resolve.
// Aliased to keep `clippy::type_complexity` quiet under CI's `-D warnings`.
type ValidationResolverCalls = Arc<Mutex<Vec<(String, Option<u16>)>>>;

fn install_recording_validation_resolver(
    calls: ValidationResolverCalls,
    resolved_ip: IpAddr,
) -> flapjack::security::test_helpers::OutboundHostResolverGuard {
    install_scoped_validation_resolver(calls, Some(vec![resolved_ip]))
}

fn install_unresolved_validation_resolver(
    calls: ValidationResolverCalls,
) -> flapjack::security::test_helpers::OutboundHostResolverGuard {
    install_scoped_validation_resolver(calls, None)
}

fn install_scoped_validation_resolver(
    calls: ValidationResolverCalls,
    app123_result: Option<Vec<IpAddr>>,
) -> flapjack::security::test_helpers::OutboundHostResolverGuard {
    install_test_algolia_validation_resolver("APP123", app123_result, move |host, port| {
        calls
            .lock()
            .expect("validation resolver call list mutex poisoned")
            .push((host.to_string(), port));
    })
}

fn validation_calls(calls: &ValidationResolverCalls) -> Vec<(String, Option<u16>)> {
    calls
        .lock()
        .expect("validation resolver call list mutex poisoned")
        .clone()
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn scoped_validation_resolver_preserves_system_dns_for_unrelated_hosts() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let _resolver = install_recording_validation_resolver(calls, REBINDING_VETTED_IP);

    let error = flapjack::security::vet_outbound_url_target("https://localhost./", false)
        .expect_err("the scoped resolver must not replace an unrelated loopback DNS answer");

    assert!(
        error.contains("private or local destination"),
        "system DNS should preserve the loopback policy refusal: {error}"
    );
}

type AlgoliaPinObservations = Arc<Mutex<Vec<(String, Vec<SocketAddr>)>>>;

fn observed_algolia_pins(observations: &AlgoliaPinObservations) -> Vec<(String, Vec<SocketAddr>)> {
    observations
        .lock()
        .expect("Algolia pin observation mutex poisoned")
        .clone()
}

fn redacted_algolia_pins(
    app_id: &str,
    observations: &AlgoliaPinObservations,
) -> Vec<(String, Vec<SocketAddr>)> {
    let app_id_lower = app_id.to_ascii_lowercase();
    observed_algolia_pins(observations)
        .into_iter()
        .map(|(host, addresses)| {
            let host = host
                .replace(app_id, "<app-id>")
                .replace(&app_id_lower, "<app-id>");
            (host, addresses)
        })
        .collect()
}

#[tokio::test]
#[ignore]
#[serial_test::serial(flapjack_outbound_url_policy)]
async fn live_algolia_client_lists_indexes_with_canonical_credentials() {
    let app_id = std::env::var("ALGOLIA_APP_ID")
        .expect("ALGOLIA_APP_ID must be present for the ignored live Algolia probe");
    let api_key = std::env::var("ALGOLIA_ADMIN_KEY")
        .expect("ALGOLIA_ADMIN_KEY must be present for the ignored live Algolia probe");
    eprintln!(
        "live_algolia_client_probe credential_metadata app_id_len={} admin_key_len={} app_id_trim_eq={} admin_key_trim_eq={}",
        app_id.len(),
        api_key.len(),
        app_id == app_id.trim(),
        api_key == api_key.trim()
    );

    let pin_observations = Arc::new(Mutex::new(Vec::new()));
    let _pin_observer = install_test_algolia_pin_observer(Arc::clone(&pin_observations));

    let result = match AlgoliaClient::new(&app_id, &api_key) {
        Ok(client) => client.list_indexes().await,
        Err(error) => Err(error),
    };
    eprintln!(
        "live_algolia_client_probe observed_vetted_pins={:?}",
        redacted_algolia_pins(&app_id, &pin_observations)
    );

    match result {
        Ok(indexes) => {
            eprintln!(
                "live_algolia_client_probe result=ok index_count={}",
                indexes.len()
            );
        }
        Err(error) => {
            eprintln!(
                "live_algolia_client_probe result=error kind={:?} safe_message={}",
                error.kind(),
                error.safe_message()
            );
            panic!(
                "live AlgoliaClient::list_indexes failed: {:?}: {}",
                error.kind(),
                error.safe_message()
            );
        }
    }
}

/// The address the recording validation resolver returns for every Algolia
/// vendor host, i.e. the address a correctly pinned client must dial.
///
/// `192.0.0.8` is the RFC 7600 "dummy address" out of the IETF-protocol-
/// assignments block. Three properties are load-bearing and a later edit must
/// keep all three:
/// 1. `flapjack::security::outbound_ip_block_reason` allows it — it is not
///    loopback, private, link-local, broadcast, or unspecified — so the
///    generic `vet_outbound_url_target` path accepts it exactly like a real
///    vendor address.
/// 2. `flapjack::security::is_public_vendor_ip` also allows it, so the strict
///    `vet_strict_vendor_url_target` path (the shape `typesense_client.rs`
///    uses, and a legitimate choice for the pinning fix) accepts it too. The
///    RFC 5737 TEST-NET literals do NOT satisfy this: `Ipv4Addr::is_documentation()`
///    covers `192.0.2.0/24`, `198.51.100.0/24`, and `203.0.113.0/24`, so a
///    TEST-NET answer would make the strict vet fail DNS validation and force
///    a refusal that this proof reads as an over-broad rejection.
/// 3. It is not a real host and is not globally routable, so a pinned connect
///    from a unit test never reaches a third party.
const REBINDING_VETTED_IP: IpAddr = TEST_VETTED_ALGOLIA_IP;

/// Algolia vendor hosts are always reached over https on the default port, and
/// `VettedOutboundUrlTarget::socket_addrs()` therefore stamps 443 onto every
/// pinned address. `hyper-util`'s `set_port` keeps a resolver-supplied port
/// verbatim when the URL carries no explicit port, so the pinned port is what
/// actually goes on the wire — a pin built from anything but the vetted target
/// would dial a different socket.
const ALGOLIA_VENDOR_PORT: u16 = 443;

/// Single owner for the loopback port the test base-URL override fixture listens
/// on. Both the override URL string and the expected pinned `SocketAddr` derive
/// from this constant so a port change cannot silently desync the two halves of
/// the pin-map contract.
const TEST_FIXTURE_LOOPBACK_PORT: u16 = 18181;

/// Recompute, from the same `flapjack::security` owner the constructor must
/// use, the exact `(host, Vec<SocketAddr>)` map a correctly pinned client has
/// to install. Runs under whatever validation resolver the caller installed, so
/// it is the vetted-address source of truth for this process, not a literal.
///
/// Call this AFTER snapshotting `validation_calls`: it deliberately drives the
/// same recording resolver and would otherwise pollute the observed host list.
fn vetted_algolia_pin_map(app_id: &str) -> Vec<(String, Vec<SocketAddr>)> {
    expected_algolia_validation_hosts(app_id)
        .into_iter()
        .map(|(host, _port)| {
            let target =
                flapjack::security::vet_outbound_url_target(&format!("https://{host}/"), false)
                    .unwrap_or_else(|error| {
                        panic!("vetting `{host}` under the test resolver must succeed: {error}")
                    })
                    .unwrap_or_else(|| panic!("`{host}` must resolve under the test resolver"));
            (host, target.socket_addrs())
        })
        .collect()
}

/// Reasons the owner source does not bind its `resolve_to_addrs` pins to the
/// vetted `socket_addrs()` output. Empty means the binding contract holds.
///
/// This scanner is retained as a known-answer fixture for unsafe source shapes.
/// Production pin equality is also owned by the runtime observer on
/// `pin_resolved_algolia_host`.
///
/// The scanner rejects the substitution shapes a real Stage 2 could plausibly
/// reach: a literal address, a misleadingly named vector, a post-validation
/// re-resolution, an opaque helper around `socket_addrs()`, and mutated vetted
/// vectors. Direct mutation
/// (`let mut pinned = target.socket_addrs(); pinned.push(loopback);`) is caught
/// by the `mut`-binding rule in `pin_binding_failure`; interior mutation
/// is rejected because wrapping or extracting the vetted vector is not a
/// transparent derivation shape.
fn pin_derivation_failures(client_source: &str) -> Vec<String> {
    const PIN_CALL: &str = "resolve_to_addrs(";
    let mut failures = Vec::new();

    if !client_source.contains(PIN_CALL) {
        failures.push(
            "algolia_client.rs never calls `resolve_to_addrs`; the vetted addresses are not \
             pinned, so reqwest resolves every Algolia host again at connect time"
                .to_string(),
        );
    }
    if !client_source.contains("flapjack::security::vet_") {
        failures.push(
            "algolia_client.rs never vets an outbound target through `flapjack::security::vet_*`; \
             pinned addresses must come from a vetted target, not from a private lookup"
                .to_string(),
        );
    }
    if client_source.contains("to_socket_addrs") {
        failures.push(
            "algolia_client.rs performs its own `to_socket_addrs` lookup; a second resolution \
             after validation is the rebinding window the pin exists to close"
                .to_string(),
        );
    }

    let mut rest = client_source;
    while let Some(offset) = rest.find(PIN_CALL) {
        let arguments_start = offset + PIN_CALL.len();
        let arguments = balanced_call_arguments(&rest[arguments_start..]);
        if let Some(message) = pin_binding_failure(client_source, arguments) {
            failures.push(format!(
                "`resolve_to_addrs({arguments})` {message}; pass the same vetted target's `host` \
                 and `socket_addrs()` (directly, or through `let` bindings derived from it) so \
                 every pinned socket address is the one validation already approved"
            ));
        }
        rest = &rest[arguments_start..];
    }

    failures
}

/// Text between a call's opening paren and its matching close paren.
fn balanced_call_arguments(after_open_paren: &str) -> &str {
    let mut depth = 1_usize;
    for (index, character) in after_open_paren.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return &after_open_paren[..index];
                }
            }
            _ => {}
        }
    }
    after_open_paren
}

fn source_identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
}

fn pin_binding_failure(client_source: &str, arguments: &str) -> Option<String> {
    let Some((host_expression, addresses_expression)) = split_top_level_comma(arguments) else {
        return Some("does not pass both a host argument and an address argument".to_string());
    };
    let bindings = let_bindings(client_source);
    let vetted_targets = vetted_target_identifiers(&bindings);
    let host_targets = expression_target_hosts(host_expression, &bindings, 0);
    let address_targets = expression_target_socket_addrs(addresses_expression, &bindings, 0);
    let matching_vetted_targets: Vec<&String> = host_targets
        .iter()
        .filter(|target| address_targets.contains(target) && vetted_targets.contains(target))
        .collect();

    if matching_vetted_targets.is_empty() {
        return Some(format!(
            "does not bind its host argument {host_expression:?} and address argument \
             {addresses_expression:?} to the same vetted target through transparent field \
             access; opaque helpers and interior-mutable extraction are not accepted"
        ));
    }

    // The address argument traces back to `<vetted>.socket_addrs()`, but that is
    // only trustworthy if the vector reqwest actually pins was never mutated
    // after that call. `let mut pinned = target.socket_addrs(); pinned.push(loopback);`
    // still traces to `target`, yet pins a loopback address the vetted set never
    // contained. Any `mut` binding in the address provenance re-opens that hole,
    // so reject it — the vetted `typesense_client.rs` shape never needs one.
    let mutable = mutable_binding_identifiers(client_source);
    let address_references = expression_referenced_identifiers(addresses_expression, &bindings, 0);
    if let Some(mutated) = address_references
        .into_iter()
        .find(|identifier| mutable.contains(identifier))
    {
        return Some(format!(
            "pins address vector `{mutated}`, which is declared `mut`; a mutable pin can \
             diverge from the vetted `socket_addrs()` output before connect, so its addresses \
             are not provably the ones validation approved"
        ));
    }

    None
}

fn split_top_level_comma(arguments: &str) -> Option<(&str, &str)> {
    let mut depth = 0_usize;
    for (index, character) in arguments.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                return Some((arguments[..index].trim(), arguments[index + 1..].trim()));
            }
            _ => {}
        }
    }
    None
}

fn let_bindings(client_source: &str) -> Vec<(String, String)> {
    let mut bindings = Vec::new();
    let mut rest = client_source;
    while let Some(offset) = rest.find("let ") {
        let statement = &rest[offset..];
        let statement = &statement[..statement.find(';').unwrap_or(statement.len())];
        if let Some(equals) = statement.find('=') {
            let (bound, initializer) = statement.split_at(equals);
            if let Some(identifier) = binding_identifier(bound) {
                bindings.push((identifier.to_string(), initializer[1..].trim().to_string()));
            }
        }
        rest = &rest[offset + "let ".len()..];
    }
    bindings
}

fn binding_identifier(bound: &str) -> Option<&str> {
    source_identifiers(bound).find(|token| *token != "let" && *token != "mut")
}

fn vetted_target_identifiers(bindings: &[(String, String)]) -> Vec<String> {
    bindings
        .iter()
        .filter(|(_, initializer)| initializer.contains("flapjack::security::vet_"))
        .map(|(identifier, _)| identifier.clone())
        .collect()
}

/// Identifiers introduced with `let mut`. A pinned address vector that appears
/// in this set (directly, or transitively through its binding chain) cannot be
/// proven equal to the vetted `socket_addrs()` output, because Rust requires
/// `mut` to `push`, reassign, or hand out `&mut` — so the only way to smuggle a
/// loopback address into a vetted vector after the fact is through a `mut`
/// binding. Rejecting every mutable variable in the address chain closes the
/// mutation false-accept without needing to enumerate mutation methods.
///
/// Token-level (not substring) matching, so a variable named `mutex` is not
/// mistaken for a `mut` binding and a `mutation_helper()` initializer on the
/// right-hand side is never inspected (only the `let`-pattern before `=` is).
fn mutable_binding_identifiers(client_source: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut rest = client_source;
    while let Some(offset) = rest.find("let ") {
        let statement = &rest[offset..];
        let statement = &statement[..statement.find(';').unwrap_or(statement.len())];
        if let Some(equals) = statement.find('=') {
            let bound = &statement[..equals];
            if source_identifiers(bound).any(|token| token == "mut") {
                if let Some(identifier) = binding_identifier(bound) {
                    identifiers.push(identifier.to_string());
                }
            }
        }
        rest = &rest[offset + "let ".len()..];
    }
    dedupe_targets(identifiers)
}

/// Every identifier the expression depends on, transitively through `let`
/// bindings. Feeding the address argument through this and intersecting with
/// `mutable_binding_identifiers` catches a mutable pinned vector no matter where
/// in its provenance the `mut` binding sits — a direct `&mut_vec`, a block
/// initializer that mutates a local, or a `mut` intermediate the address is
/// derived from.
fn expression_referenced_identifiers(
    expression: &str,
    bindings: &[(String, String)],
    depth: usize,
) -> Vec<String> {
    if depth > 8 {
        return Vec::new();
    }
    let mut identifiers: Vec<String> = source_identifiers(expression)
        .map(|token| token.to_string())
        .collect();
    for identifier in source_identifiers(expression) {
        if let Some((_, initializer)) = bindings
            .iter()
            .rev()
            .find(|(bound_identifier, _)| bound_identifier == identifier)
        {
            identifiers.extend(expression_referenced_identifiers(
                initializer,
                bindings,
                depth + 1,
            ));
        }
    }
    dedupe_targets(identifiers)
}

fn expression_target_hosts(
    expression: &str,
    bindings: &[(String, String)],
    depth: usize,
) -> Vec<String> {
    expression_target_member(expression, bindings, depth, ".host")
}

fn expression_target_socket_addrs(
    expression: &str,
    bindings: &[(String, String)],
    depth: usize,
) -> Vec<String> {
    expression_target_member(expression, bindings, depth, ".socket_addrs()")
}

/// Trace only transparent member access, optionally through immutable `let`
/// aliases. Any call or field chain around the value is an opaque transform and
/// therefore cannot prove that reqwest receives the value vetting produced.
fn expression_target_member(
    expression: &str,
    bindings: &[(String, String)],
    depth: usize,
    member: &str,
) -> Vec<String> {
    if depth > 8 {
        return Vec::new();
    }
    if let Some(receiver) = exact_member_receiver(expression, member) {
        return vec![receiver.to_string()];
    }
    let Some(identifier) = transparent_identifier(expression) else {
        return Vec::new();
    };
    let Some((_, initializer)) = bindings
        .iter()
        .rev()
        .find(|(bound_identifier, _)| bound_identifier == identifier)
    else {
        return Vec::new();
    };
    expression_target_member(initializer, bindings, depth + 1, member)
}

fn exact_member_receiver<'a>(expression: &'a str, member: &str) -> Option<&'a str> {
    let receiver = transparent_expression(expression).strip_suffix(member)?;
    (!receiver.is_empty()
        && receiver
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_'))
    .then_some(receiver)
}

fn transparent_identifier(expression: &str) -> Option<&str> {
    let expression = transparent_expression(expression);
    (!expression.is_empty()
        && expression
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_'))
    .then_some(expression)
}

fn transparent_expression(mut expression: &str) -> &str {
    expression = expression.trim();
    while let Some(unborrowed) = expression.strip_prefix('&') {
        expression = unborrowed.trim_start();
    }
    expression.trim()
}

fn dedupe_targets(targets: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for target in targets {
        if !deduped.contains(&target) {
            deduped.push(target);
        }
    }
    deduped
}

fn spawn_loopback_sink(max_hits: usize) -> (SocketAddr, Arc<AtomicUsize>, thread::JoinHandle<()>) {
    // A cancelled reqwest connect task can finish after its owning test returns.
    // Never recycle a sink port in this process: otherwise a late connection for
    // the previous specimen can be accepted by the next specimen's listener and
    // falsely reported as a pin bypass without a lookup on its resolver.
    static USED_SINK_PORTS: OnceLock<Mutex<HashSet<u16>>> = OnceLock::new();
    let listener = loop {
        let candidate = TcpListener::bind("127.0.0.1:0").expect("loopback sink should bind");
        let candidate_port = candidate
            .local_addr()
            .expect("loopback sink address")
            .port();
        let mut used_ports = USED_SINK_PORTS
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .expect("loopback sink port registry mutex poisoned");
        if used_ports.insert(candidate_port) {
            break candidate;
        }
    };
    listener
        .set_nonblocking(true)
        .expect("loopback sink should become nonblocking");
    let blocked_address = listener.local_addr().expect("loopback sink address");
    let sink_hits = Arc::new(AtomicUsize::new(0));
    let server_hits = Arc::clone(&sink_hits);
    let server = thread::spawn(move || {
        // Budget ~900ms per specimen so the sink stays alive for the whole loop.
        // A fixed deadline would let later specimens' connect attempts land after
        // the listener already exited, leaving sink_hits at 0 whether the policy
        // blocked the connect or leaked it — a false green under load.
        let deadline =
            std::time::Instant::now() + Duration::from_millis(900) * max_hits.max(1) as u32;
        while std::time::Instant::now() < deadline && server_hits.load(Ordering::SeqCst) < max_hits
        {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
                    let mut buffer = [0_u8; 1024];
                    let read = stream.read(&mut buffer).unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).to_ascii_lowercase();
                    // This suite runs on a shared host where unrelated health
                    // probes can reach ephemeral loopback ports. Count only a
                    // request carrying this specimen's Algolia credential;
                    // anonymous TCP connects are not evidence about this client.
                    if request.contains("x-algolia-api-key: key") {
                        server_hits.fetch_add(1, Ordering::SeqCst);
                    }
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\ncontent-length: 2\r\ncontent-type: application/json\r\n\r\n{{}}"
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("loopback sink accept failed: {error}"),
            }
        }
    });
    (blocked_address, sink_hits, server)
}

struct RebindingPinningEvidence {
    result: Result<(), AlgoliaClientError>,
    sink_hits: usize,
    validation_hosts: Vec<(String, Option<u16>)>,
    expected_validation_hosts: Vec<(String, Option<u16>)>,
    connect_hosts: Vec<String>,
    observed_pins: Vec<(String, Vec<SocketAddr>)>,
    vetted_pins: Vec<(String, Vec<SocketAddr>)>,
    rebind_sink: SocketAddr,
}

fn assert_rebinding_pinning_contract(evidence: RebindingPinningEvidence) {
    let RebindingPinningEvidence {
        result,
        sink_hits,
        validation_hosts,
        expected_validation_hosts,
        connect_hosts,
        observed_pins,
        vetted_pins,
        rebind_sink,
    } = evidence;
    let mut failures = Vec::new();

    // Address side of the pin contract. `vetted_pins` is recomputed from
    // `flapjack::security` under the same validation resolver the constructor
    // saw. The production helper observer records the values actually handed to
    // reqwest, so equality here covers arbitrary mutation or laundering shapes
    // without relying on an enumerable source-text scanner.
    if observed_pins != vetted_pins {
        failures.push(format!(
            "reqwest received Algolia pins {observed_pins:?}; expected the exact vetted pin map {vetted_pins:?}"
        ));
    }
    let expected_pin_hosts: Vec<String> = expected_validation_hosts
        .iter()
        .map(|(host, _port)| host.clone())
        .collect();
    let pinned_hosts: Vec<String> = observed_pins.iter().map(|(host, _)| host.clone()).collect();
    let mut sorted_pinned_hosts = pinned_hosts.clone();
    sorted_pinned_hosts.sort();
    let mut sorted_expected_pin_hosts = expected_pin_hosts.clone();
    sorted_expected_pin_hosts.sort();
    if sorted_pinned_hosts != sorted_expected_pin_hosts {
        failures.push(format!(
            "vetted pin map covers {sorted_pinned_hosts:?}; every Algolia host the shared client \
             can put on the wire must carry a pin: {sorted_expected_pin_hosts:?}"
        ));
    }
    for (host, addresses) in &vetted_pins {
        if addresses.is_empty() {
            failures.push(format!(
                "vetted pin map has no address for `{host}`; an empty pin leaves the host \
                 unpinned and reqwest resolves it again at connect time"
            ));
        }
        for address in addresses {
            if address.ip() != REBINDING_VETTED_IP || address.port() != ALGOLIA_VENDOR_PORT {
                failures.push(format!(
                    "`{host}` vets to {address}; expected the validated \
                     {REBINDING_VETTED_IP}:{ALGOLIA_VENDOR_PORT}, so the pin under test is not \
                     the address validation approved"
                ));
            }
            if address.ip().is_loopback() || *address == rebind_sink {
                failures.push(format!(
                    "`{host}` vets to local address {address}; the rebound answer must never \
                     reach the vetted set"
                ));
            }
        }
    }
    let mut validation_hosts = validation_hosts;
    validation_hosts.sort();
    let mut expected_validation_hosts = expected_validation_hosts;
    expected_validation_hosts.sort();

    // The request's ordinary outcome (success / timeout / transport error against the
    // vetted public address) is deliberately outside this contract — a correctly pinned
    // client never observes the rebound loopback answer, so it cannot reclassify it as a
    // policy refusal. But an over-broad Stage 2 constructor that vets every host and then
    // refuses *every* request would otherwise satisfy the three checks below vacuously.
    // Guard that: the vetted-safe destination must NOT come back as a policy refusal.
    if let Err(error) = &result {
        if error.kind() == AlgoliaErrorKind::Validation
            && error.safe_message() == "Algolia outbound destination was refused by policy"
        {
            failures.push(
                "vetted-safe Algolia host was refused by policy; pinning must let the safe \
                 validated address through, not reject every request"
                    .to_string(),
            );
        }
    }
    if validation_hosts != expected_validation_hosts {
        failures.push(format!(
            "validation resolver saw {validation_hosts:?}; expected the constructor to vet the complete Algolia host set {expected_validation_hosts:?} before any request"
        ));
    }
    if sink_hits != 0 {
        failures.push(format!(
            "loopback sink accepted {sink_hits} connection(s); pinned connects must never follow rebound DNS to a local address"
        ));
    }
    if !connect_hosts.is_empty() {
        failures.push(format!(
            "reqwest resolver was consulted at connect time for {connect_hosts:?}; pinned Algolia connects must reuse the vetted addresses"
        ));
    }

    assert!(
        failures.is_empty(),
        "Algolia rebinding proof failed:\n{}",
        failures.join("\n")
    );
}

fn assert_blocked_validation_policy_refusal(
    result: Result<(), AlgoliaClientError>,
    sink_hits: usize,
    validation_hosts: Vec<(String, Option<u16>)>,
    connect_hosts: Vec<String>,
) {
    let mut failures = Vec::new();
    match result {
        Ok(()) => failures.push(
            "blocked validation DNS unexpectedly completed instead of returning a policy error"
                .to_string(),
        ),
        Err(error) => {
            if error.kind() != AlgoliaErrorKind::Validation {
                failures.push(format!(
                    "blocked validation DNS returned {:?}; expected Validation",
                    error.kind()
                ));
            }
            if error.safe_message() != "Algolia outbound destination was refused by policy" {
                failures.push(format!(
                    "blocked validation DNS returned safe message {:?}; expected {:?}",
                    error.safe_message(),
                    "Algolia outbound destination was refused by policy"
                ));
            }
        }
    }
    let expected_validation_hosts = expected_algolia_validation_hosts("APP123");
    if validation_hosts.is_empty()
        || validation_hosts
            .iter()
            .any(|host| !expected_validation_hosts.contains(host))
    {
        failures.push(format!(
            "validation resolver saw {validation_hosts:?}; expected one or more Algolia vendor hosts from {expected_validation_hosts:?} before policy refusal"
        ));
    }
    if sink_hits != 0 {
        failures.push(format!(
            "loopback sink accepted {sink_hits} connection(s); blocked validation DNS must be refused before connect"
        ));
    }
    if !connect_hosts.is_empty() {
        failures.push(format!(
            "reqwest resolver was consulted at connect time for {connect_hosts:?}; blocked validation DNS must be refused first"
        ));
    }

    assert!(
        failures.is_empty(),
        "Algolia blocked-validation proof failed:\n{}",
        failures.join("\n")
    );
}

fn require_unretrievable_access_for_test(
    transport: &mut ScriptedTransport,
    settings: &Value,
) -> Result<(), AlgoliaClientError> {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    tokio_test::block_on(require_unretrievable_access_with_transport(
        transport, "APP123", "key", settings,
    ))
}

fn wait_for_quiescent_source_for_test(
    transport: &mut ScriptedTransport,
) -> Result<AlgoliaIndexRecord, AlgoliaClientError> {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    tokio_test::block_on(wait_for_quiescent_source_with_transport(
        transport,
        "APP123",
        "key",
        "products",
        QuiescencePolicy {
            max_polls: 3,
            poll_interval: Duration::from_millis(1),
        },
        |_| async {},
    ))
}

fn index_page(items: Value, page: usize, nb_pages: usize) -> Value {
    json!({
        "items": items,
        "page": page,
        "nbPages": nb_pages
    })
}

/// TODO: Document paginated_hits_for_test.
fn paginated_hits_for_test<T: DeserializeOwned>(
    transport: &mut ScriptedTransport,
    endpoint: &str,
) -> Result<Vec<T>, AlgoliaClientError> {
    let raw = paginated_raw_hits_for_test(transport, endpoint)?;
    raw.into_iter()
        .map(|hit| {
            serde_json::from_value(hit).map_err(|_| {
                AlgoliaClientError::new(
                    AlgoliaErrorKind::Schema,
                    "Algolia hit did not match the expected schema",
                )
            })
        })
        .collect()
}

/// TODO: Document paginated_raw_hits_for_test.
fn paginated_raw_hits_for_test(
    transport: &mut ScriptedTransport,
    endpoint: &str,
) -> Result<Vec<Value>, AlgoliaClientError> {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let mut delivered = Vec::new();
    let result = tokio_test::block_on(paginated_hits_with_transport(
        transport,
        "APP123",
        "key",
        "products",
        endpoint,
        |page| {
            delivered.extend(page);
            Ok::<_, Infallible>(())
        },
    ));
    match result {
        Ok(()) => Ok(delivered),
        Err(BrowseError::Client(error)) => Err(error),
        Err(BrowseError::Consumer(never)) => match never {},
    }
}

/// TODO: Document paginated_raw_hits_with_limits_for_test.
fn paginated_raw_hits_with_limits_for_test(
    transport: &mut ScriptedTransport,
    endpoint: &str,
    limits: TraversalLimits,
) -> Result<Vec<Value>, AlgoliaClientError> {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let mut delivered = Vec::new();
    let result = tokio_test::block_on(paginated_hits_with_transport_and_limits(
        transport,
        "APP123",
        "key",
        "products",
        endpoint,
        limits,
        |page| {
            delivered.extend(page);
            Ok::<_, Infallible>(())
        },
    ));
    match result {
        Ok(()) => Ok(delivered),
        Err(BrowseError::Client(error)) => Err(error),
        Err(BrowseError::Consumer(never)) => match never {},
    }
}

/// TODO: Document browse_documents_for_test.
fn browse_documents_for_test(
    transport: &mut ScriptedTransport,
) -> Result<Vec<Value>, AlgoliaClientError> {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let mut delivered = Vec::new();
    let result = tokio_test::block_on(browse_documents_with_transport(
        transport,
        "APP123",
        "key",
        "products",
        |documents| {
            delivered.extend(documents);
            Ok::<_, Infallible>(())
        },
    ));
    match result {
        Ok(()) => Ok(delivered),
        Err(BrowseError::Client(error)) => Err(error),
        Err(BrowseError::Consumer(never)) => match never {},
    }
}

/// TODO: Document browse_documents_with_limits_for_test.
fn browse_documents_with_limits_for_test(
    transport: &mut ScriptedTransport,
    limits: TraversalLimits,
) -> Result<Vec<Value>, AlgoliaClientError> {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let mut delivered = Vec::new();
    let result = tokio_test::block_on(browse_documents_with_transport_and_limits(
        transport,
        "APP123",
        "key",
        "products",
        limits,
        |documents| {
            delivered.extend(documents);
            Ok::<_, Infallible>(())
        },
    ));
    match result {
        Ok(()) => Ok(delivered),
        Err(BrowseError::Client(error)) => Err(error),
        Err(BrowseError::Consumer(never)) => match never {},
    }
}

/// TODO: Document client_policy_validates_app_id_before_host_construction.
#[test]
fn client_policy_validates_app_id_before_host_construction() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    for app_id in ["", "bad/id", "bad.example", "bad:443", "bad app"] {
        assert_eq!(
            request_for_test(app_id, "products", AlgoliaMethod::Get, "settings")
                .expect_err("invalid app ID must fail before URL construction")
                .kind(),
            AlgoliaErrorKind::Validation
        );
    }

    let request = request_for_test("APP123", "products", AlgoliaMethod::Get, "settings")
        .expect("valid app ID should produce a request");
    assert_eq!(
        request.url,
        "https://APP123-dsn.algolia.net/1/indexes/products/settings"
    );
}

#[test]
fn client_policy_percent_encodes_index_names() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let request = request_for_test("APP123", "summer/sale 2026", AlgoliaMethod::Post, "browse")
        .expect("valid request should be planned");

    assert_eq!(
        request.url,
        "https://APP123-dsn.algolia.net/1/indexes/summer%2Fsale%202026/browse"
    );
    assert_eq!(request.method, AlgoliaMethod::Post);
}

#[test]
fn client_policy_uses_exact_https_host_and_fixed_timeouts() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let request = request_for_test("APP123", "products", AlgoliaMethod::Get, "settings")
        .expect("valid request should be planned");

    assert!(request.url.starts_with("https://APP123-dsn.algolia.net/"));
    assert!(!request.url.contains("http://"));
    assert_eq!(request.policy.connect_timeout, Duration::from_secs(5));
    assert_eq!(request.policy.request_timeout, Duration::from_secs(30));
    assert!(request.policy.redirects_disabled);
    assert!(request.policy.proxy_disabled);
}

#[test]
fn client_policy_has_no_production_base_url_override() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let request = request_for_test("APP123", "products", AlgoliaMethod::Get, "settings")
        .expect("valid request should be planned");

    assert_eq!(
        request.url,
        "https://APP123-dsn.algolia.net/1/indexes/products/settings"
    );
}

#[test]
fn client_policy_allows_test_base_url_override() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::overridden_to("http://127.0.0.1:18181/");

    let request = request_for_test("APP123", "products", AlgoliaMethod::Get, "settings")
        .expect("test override should still plan a request");

    assert_eq!(
        request.url,
        "http://127.0.0.1:18181/1/indexes/products/settings"
    );
    assert!(
        request.fallback_urls.is_empty(),
        "test override must disable vendor fallback hosts so the fixture stays local"
    );
}

#[test]
fn client_policy_limits_test_base_url_override_to_debug_builds() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::overridden_to("http://127.0.0.1:18181/");

    let request = request_for_test("APP123", "products", AlgoliaMethod::Get, "settings")
        .expect("loopback test override should either apply in debug or be ignored in release");

    if cfg!(any(debug_assertions, test)) {
        assert_eq!(
            request.url,
            "http://127.0.0.1:18181/1/indexes/products/settings"
        );
        assert!(
            request.fallback_urls.is_empty(),
            "debug/test builds must keep fixture traffic local"
        );
    } else {
        assert_eq!(
            request.url,
            "https://APP123-dsn.algolia.net/1/indexes/products/settings"
        );
        assert_eq!(
            request.fallback_urls,
            vec![
                "https://APP123-1.algolianet.com/1/indexes/products/settings",
                "https://APP123-2.algolianet.com/1/indexes/products/settings",
                "https://APP123-3.algolianet.com/1/indexes/products/settings",
            ]
        );
    }
}

#[test]
fn client_policy_rejects_remote_test_base_url_before_planning_credentials() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::overridden_to("http://203.0.113.10:18181/");

    let error = request_for_test("APP123", "products", AlgoliaMethod::Get, "settings").expect_err(
        "remote test override must be rejected before a credentialed request is planned",
    );

    assert_eq!(error.kind(), AlgoliaErrorKind::Validation);
    assert_eq!(
        error.safe_message(),
        "Algolia test base URL must use a literal loopback address"
    );
}

#[test]
fn client_policy_rejects_loopback_hostname_to_prevent_dns_rebinding() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::overridden_to("http://localhost:18181/");

    let error = request_for_test("APP123", "products", AlgoliaMethod::Get, "settings")
        .expect_err("test override must not re-resolve a hostname after loopback validation");

    assert_eq!(error.kind(), AlgoliaErrorKind::Validation);
    assert_eq!(
        error.safe_message(),
        "Algolia test base URL must use a literal loopback address"
    );
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn client_policy_refuses_rebound_data_host_before_loopback_connect() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let (blocked_address, sink_hits, server) = spawn_loopback_sink(4);
    let validation_resolver_calls = Arc::new(Mutex::new(Vec::new()));
    let _validation_resolver = install_recording_validation_resolver(
        Arc::clone(&validation_resolver_calls),
        // Allowed by both outbound policies and not a real host — see
        // `REBINDING_VETTED_IP` for why all three of those properties matter.
        REBINDING_VETTED_IP,
    );
    let resolver = Arc::new(CountingStaticResolver::new(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        blocked_address.port(),
    )));
    let _connect_resolver = install_test_dns_resolver(resolver.clone());
    let pin_observations = Arc::new(Mutex::new(Vec::new()));
    let _pin_observer = install_test_algolia_pin_observer(Arc::clone(&pin_observations));

    // The post-pin request may time out, return a transport error, or receive an
    // upstream response from the vetted public address. Its ordinary outcome is
    // intentionally outside this security contract: the rebound loopback answer
    // must be bypassed rather than detected and reclassified after the fact. We
    // still hand it to the contract so an over-broad refusal is caught.
    let ordinary_result: Result<(), AlgoliaClientError> = tokio_test::block_on(async {
        let client = AlgoliaClient::for_source("APP123", "key", "products")?;
        client.settings().await.map(|_| ())
    });

    server.join().expect("loopback sink thread should finish");
    // Snapshot the observed hosts BEFORE recomputing the vetted pin map: that
    // recomputation drives the same recording resolver.
    let observed_validation_hosts = validation_calls(&validation_resolver_calls);
    let observed_pins = observed_algolia_pins(&pin_observations);
    assert_rebinding_pinning_contract(RebindingPinningEvidence {
        result: ordinary_result,
        sink_hits: sink_hits.load(Ordering::SeqCst),
        validation_hosts: observed_validation_hosts,
        expected_validation_hosts: expected_algolia_validation_hosts("APP123"),
        connect_hosts: resolver.observed_hosts(),
        observed_pins,
        vetted_pins: vetted_algolia_pin_map("APP123"),
        rebind_sink: blocked_address,
    });
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn client_policy_refuses_rebound_control_host_before_loopback_connect() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let (blocked_address, sink_hits, server) = spawn_loopback_sink(4);
    let validation_resolver_calls = Arc::new(Mutex::new(Vec::new()));
    let _validation_resolver = install_recording_validation_resolver(
        Arc::clone(&validation_resolver_calls),
        // Allowed by policy but not a real host; see the data-host proof above.
        REBINDING_VETTED_IP,
    );
    let resolver = Arc::new(CountingStaticResolver::new(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        blocked_address.port(),
    )));
    let _connect_resolver = install_test_dns_resolver(resolver.clone());
    let pin_observations = Arc::new(Mutex::new(Vec::new()));
    let _pin_observer = install_test_algolia_pin_observer(Arc::clone(&pin_observations));

    // See the data-host proof above: success versus an ordinary public-network
    // failure is irrelevant here as long as reqwest cannot observe the rebound.
    let ordinary_result: Result<(), AlgoliaClientError> = tokio_test::block_on(async {
        let client = AlgoliaClient::new("APP123", "key")?;
        client.list_indexes().await.map(|_| ())
    });

    server.join().expect("loopback sink thread should finish");
    // See the data-host proof: snapshot before recomputing the vetted pin map.
    let observed_validation_hosts = validation_calls(&validation_resolver_calls);
    let observed_pins = observed_algolia_pins(&pin_observations);
    assert_rebinding_pinning_contract(RebindingPinningEvidence {
        result: ordinary_result,
        sink_hits: sink_hits.load(Ordering::SeqCst),
        validation_hosts: observed_validation_hosts,
        expected_validation_hosts: expected_algolia_validation_hosts("APP123"),
        connect_hosts: resolver.observed_hosts(),
        observed_pins,
        vetted_pins: vetted_algolia_pin_map("APP123"),
        rebind_sink: blocked_address,
    });
}

/// Deterministic equality proof for the exact values handed to reqwest.
#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn client_policy_pins_only_addresses_returned_by_outbound_validation() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let validation_resolver_calls = Arc::new(Mutex::new(Vec::new()));
    let _validation_resolver = install_recording_validation_resolver(
        Arc::clone(&validation_resolver_calls),
        REBINDING_VETTED_IP,
    );
    let pin_observations = Arc::new(Mutex::new(Vec::new()));
    let _pin_observer = install_test_algolia_pin_observer(Arc::clone(&pin_observations));

    AlgoliaClient::new("APP123", "key").expect("vetted Algolia hosts should build a client");

    assert_eq!(
        validation_calls(&validation_resolver_calls),
        expected_algolia_validation_hosts("APP123"),
        "the shared client must vet every host it can put on the wire"
    );
    assert_eq!(
        observed_algolia_pins(&pin_observations),
        vetted_algolia_pin_map("APP123"),
        "reqwest must receive the exact host/address values returned by outbound validation"
    );
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn client_policy_pins_the_loopback_fixture_destination_when_the_base_url_override_is_active() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::overridden_to(&format!(
        "http://127.0.0.1:{TEST_FIXTURE_LOOPBACK_PORT}/"
    ));
    let validation_resolver_calls = Arc::new(Mutex::new(Vec::new()));
    let _validation_resolver =
        install_unresolved_validation_resolver(Arc::clone(&validation_resolver_calls));
    let pin_observations = Arc::new(Mutex::new(Vec::new()));
    let _pin_observer = install_test_algolia_pin_observer(Arc::clone(&pin_observations));

    AlgoliaClient::new("APP123", "key")
        .expect("an active loopback fixture override should build an Algolia client");

    // An active loopback override must short-circuit vendor vetting entirely: no
    // `APP123-*` synthetic host may be handed to `vet_outbound_url_target`. The
    // scoped resolver only records the five APP123 vendor hosts, so an empty call
    // list is the achievable-correct value — the override's own `127.0.0.1` vet
    // falls through to system resolution unrecorded. This fails a Stage 2 fix that
    // keeps vetting the unresolvable vendor hosts and merely appends the loopback
    // pin.
    assert_eq!(
        validation_calls(&validation_resolver_calls),
        Vec::<(String, Option<u16>)>::new(),
        "an active loopback override must not vet any synthetic Algolia vendor host"
    );
    // Defense-in-depth pin. `hyper-util`'s `HttpConnector` short-circuits
    // IP-literal hosts through `dns::SocketAddrs::try_parse` before consulting the
    // `resolve_to_addrs` map, and `test_algolia_base_url_override` rejects any
    // non-literal-loopback base URL, so this pin never changes the socket hyper
    // dials for the fixture — it documents the exact destination and guarantees no
    // vendor host is pinned in its place.
    assert_eq!(
        observed_algolia_pins(&pin_observations),
        vec![(
            "127.0.0.1".to_string(),
            vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                TEST_FIXTURE_LOOPBACK_PORT
            )]
        )],
        "reqwest must pin only the active loopback fixture destination"
    );
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn algolia_pin_observer_restores_previous_scope_on_drop() {
    let outer_observations = Arc::new(Mutex::new(Vec::new()));
    let _outer_observer = install_test_algolia_pin_observer(Arc::clone(&outer_observations));
    let inner_observations = Arc::new(Mutex::new(Vec::new()));

    {
        let _inner_observer = install_test_algolia_pin_observer(Arc::clone(&inner_observations));
        let _builder = pin_resolved_algolia_host(
            reqwest::Client::builder(),
            "inner.algolia.test".to_string(),
            vec![SocketAddr::new(REBINDING_VETTED_IP, ALGOLIA_VENDOR_PORT)],
        );
    }
    let _builder = pin_resolved_algolia_host(
        reqwest::Client::builder(),
        "outer.algolia.test".to_string(),
        vec![SocketAddr::new(REBINDING_VETTED_IP, ALGOLIA_VENDOR_PORT)],
    );

    assert_eq!(
        observed_algolia_pins(&inner_observations),
        vec![(
            "inner.algolia.test".to_string(),
            vec![SocketAddr::new(REBINDING_VETTED_IP, ALGOLIA_VENDOR_PORT)]
        )]
    );
    assert_eq!(
        observed_algolia_pins(&outer_observations),
        vec![(
            "outer.algolia.test".to_string(),
            vec![SocketAddr::new(REBINDING_VETTED_IP, ALGOLIA_VENDOR_PORT)]
        )]
    );
}

/// The retained pin-derivation scanner must itself go red for its unsafe fixtures
/// and accept the vetted shape `typesense_client.rs::from_vetted_target` uses.
#[test]
fn pin_derivation_scanner_accepts_vetted_addresses_and_rejects_substitutes() {
    let inline_vetted = r#"
        let target = flapjack::security::vet_outbound_url_target(url, false)?;
        builder = builder.resolve_to_addrs(&target.host, &target.socket_addrs());
    "#;
    assert_eq!(
        pin_derivation_failures(inline_vetted),
        Vec::<String>::new(),
        "an inline `target.socket_addrs()` pin is the vetted shape and must pass"
    );

    let bound_vetted = r#"
        let target = flapjack::security::vet_strict_vendor_url_target(url, HOSTS)?;
        let pinned_addresses = target.socket_addrs();
        builder = builder.resolve_to_addrs(&target.host, &pinned_addresses);
    "#;
    assert_eq!(
        pin_derivation_failures(bound_vetted),
        Vec::<String>::new(),
        "a `let`-bound `socket_addrs()` pin is the same vetted shape and must pass"
    );

    let mutated_vetted = r#"
        let target = flapjack::security::vet_outbound_url_target(url, false)?;
        let mut pinned_addresses = target.socket_addrs();
        pinned_addresses.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443));
        builder = builder.resolve_to_addrs(&target.host, &pinned_addresses);
    "#;
    assert_eq!(
        pin_derivation_failures(mutated_vetted).len(),
        1,
        "mutating vetted addresses with a literal loopback pin must be reported"
    );

    // The mutation guard keys on the `mut` binding, not on `.push` specifically,
    // so a wholesale reassignment of the vetted vector is caught the same way.
    // Keep this so a later "only detect push" simplification cannot re-open the
    // hole through a different mutation.
    let reassigned_vetted = r#"
        let target = flapjack::security::vet_outbound_url_target(url, false)?;
        let mut pinned_addresses = target.socket_addrs();
        pinned_addresses = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443)];
        builder = builder.resolve_to_addrs(&target.host, &pinned_addresses);
    "#;
    assert_eq!(
        pin_derivation_failures(reassigned_vetted).len(),
        1,
        "reassigning the vetted address vector through a `mut` binding must be reported"
    );

    let interior_mutated_vetted = r#"
        let target = flapjack::security::vet_outbound_url_target(url, false)?;
        let pinned_addresses = std::sync::Mutex::new(target.socket_addrs());
        pinned_addresses
            .lock()
            .unwrap()
            .push(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443));
        let locked_addresses = pinned_addresses.lock().unwrap();
        builder = builder.resolve_to_addrs(&target.host, &locked_addresses);
    "#;
    assert_eq!(
        pin_derivation_failures(interior_mutated_vetted).len(),
        1,
        "mutating vetted addresses through an immutable Mutex binding must be reported"
    );
    assert!(
        pin_derivation_failures(interior_mutated_vetted)[0].contains("interior-mutable"),
        "{:?}",
        pin_derivation_failures(interior_mutated_vetted)
    );

    let rwlock_mutated_vetted = r#"
        let target = flapjack::security::vet_outbound_url_target(url, false)?;
        let pinned_addresses = std::sync::RwLock::new(target.socket_addrs());
        pinned_addresses
            .write()
            .unwrap()
            .push(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443));
        let locked_addresses = pinned_addresses.read().unwrap();
        builder = builder.resolve_to_addrs(&target.host, &locked_addresses);
    "#;
    assert_eq!(
        pin_derivation_failures(rwlock_mutated_vetted).len(),
        1,
        "mutating vetted addresses through an immutable RwLock binding must be reported"
    );
    assert!(
        pin_derivation_failures(rwlock_mutated_vetted)[0].contains("interior-mutable"),
        "{:?}",
        pin_derivation_failures(rwlock_mutated_vetted)
    );

    let refcell_mutated_vetted = r#"
        let target = flapjack::security::vet_outbound_url_target(url, false)?;
        let pinned_addresses = std::cell::RefCell::new(target.socket_addrs());
        pinned_addresses
            .borrow_mut()
            .push(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443));
        let borrowed_addresses = pinned_addresses.borrow();
        builder = builder.resolve_to_addrs(&target.host, &borrowed_addresses);
    "#;
    assert_eq!(
        pin_derivation_failures(refcell_mutated_vetted).len(),
        1,
        "mutating vetted addresses through an immutable RefCell binding must be reported"
    );
    assert!(
        pin_derivation_failures(refcell_mutated_vetted)[0].contains("interior-mutable"),
        "{:?}",
        pin_derivation_failures(refcell_mutated_vetted)
    );

    let literal_address = r#"
        let target = flapjack::security::vet_outbound_url_target(url, false)?;
        let pinned_addresses = vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443)];
        builder = builder.resolve_to_addrs(&target.host, &pinned_addresses);
    "#;
    assert_eq!(
        pin_derivation_failures(literal_address).len(),
        1,
        "pinning a literal address instead of the vetted one must be reported"
    );

    let misleading_name = r#"
        let target = flapjack::security::vet_outbound_url_target(url, false)?;
        let unvetted_socket_addrs =
            vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443)];
        builder = builder.resolve_to_addrs(&target.host, &unvetted_socket_addrs);
    "#;
    assert_eq!(
        pin_derivation_failures(misleading_name).len(),
        1,
        "a misleading variable name containing `socket_addrs` must not certify a literal pin"
    );

    let second_lookup = r#"
        let target = flapjack::security::vet_outbound_url_target(url, false)?;
        let pinned_addresses: Vec<SocketAddr> = (host, 443).to_socket_addrs()?.collect();
        builder = builder.resolve_to_addrs(&target.host, &pinned_addresses);
    "#;
    assert_eq!(
        pin_derivation_failures(second_lookup).len(),
        2,
        "a post-validation re-resolution must be reported as both a second lookup and an \
         unvetted pin"
    );

    assert_eq!(
        pin_derivation_failures("let client = reqwest::Client::builder().build();").len(),
        2,
        "a client with no pin and no vet must be reported for both"
    );

    let laundered_vetted = r#"
        fn launder(mut addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
            addresses.clear();
            addresses.push(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443));
            addresses
        }
        let target = flapjack::security::vet_outbound_url_target(url, false)?;
        builder = builder.resolve_to_addrs(&target.host, &launder(target.socket_addrs()));
    "#;
    assert_eq!(
        pin_derivation_failures(laundered_vetted).len(),
        1,
        "a helper can mutate or substitute the vector between vetting and pinning"
    );
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn client_policy_rejects_blocked_vendor_resolution_before_connect() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let (blocked_address, sink_hits, server) = spawn_loopback_sink(1);
    let validation_resolver_calls = Arc::new(Mutex::new(Vec::new()));
    let _validation_resolver = install_recording_validation_resolver(
        Arc::clone(&validation_resolver_calls),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    );
    let resolver = Arc::new(CountingStaticResolver::new(blocked_address));
    let _connect_resolver = install_test_dns_resolver(resolver.clone());

    let result: Result<(), AlgoliaClientError> = tokio_test::block_on(async {
        let client = AlgoliaClient::for_source("APP123", "key", "products")?;
        client.settings().await.map(|_| ())
    });

    server.join().expect("loopback sink thread should finish");
    assert_blocked_validation_policy_refusal(
        result,
        sink_hits.load(Ordering::SeqCst),
        validation_calls(&validation_resolver_calls),
        resolver.observed_hosts(),
    );
}

#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn client_policy_rejects_unresolved_vendor_host_before_connect() {
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let validation_resolver_calls = Arc::new(Mutex::new(Vec::new()));
    let _validation_resolver =
        install_unresolved_validation_resolver(Arc::clone(&validation_resolver_calls));
    let resolver = Arc::new(CountingStaticResolver::new(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        ALGOLIA_VENDOR_PORT,
    )));
    let _connect_resolver = install_test_dns_resolver(resolver.clone());
    let pin_observations = Arc::new(Mutex::new(Vec::new()));
    let _pin_observer = install_test_algolia_pin_observer(Arc::clone(&pin_observations));

    let error = match AlgoliaClient::new("APP123", "key") {
        Ok(_) => panic!("unresolved Algolia vendor host must fail closed"),
        Err(error) => error,
    };

    assert_eq!(error.kind(), AlgoliaErrorKind::Validation);
    assert_eq!(
        error.safe_message(),
        "Algolia outbound destination was refused by policy"
    );
    assert_eq!(
        validation_calls(&validation_resolver_calls),
        vec![expected_algolia_validation_hosts("APP123")[0].clone()]
    );
    assert!(resolver.observed_hosts().is_empty());
    assert!(observed_algolia_pins(&pin_observations).is_empty());
}

// The replica-settings method reuses the exact index_path / plan_request /
// execute_json_with_retry seam. This known-answer test proves the requested path
// is percent-encoded for an arbitrary index name, the full settings JSON is
// returned verbatim, and any non-2xx stays in the typed, scrubbed error owner.
/// TODO: Document index_settings_encodes_arbitrary_name_returns_full_json_and_scrubs_errors.
#[test]
fn index_settings_encodes_arbitrary_name_returns_full_json_and_scrubs_errors() {
    let base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let replica_name = "réplica price/asc 2026";

    let request = request_for_test("APP123", replica_name, AlgoliaMethod::Get, "settings")
        .expect("valid replica index name should plan a request");
    assert_eq!(
        request.url,
        "https://APP123-dsn.algolia.net/1/indexes/r%C3%A9plica%20price%2Fasc%202026/settings"
    );

    let full_settings = json!({
        "ranking": ["desc(price)"],
        "customRanking": ["asc(name)"],
        "relevancyStrictness": 80,
        "searchableAttributes": ["title", "brand"],
        "primary": "products"
    });
    let mut ok_transport = ScriptedTransport::new(vec![ok(full_settings.clone())]);
    let returned = execute_scripted_request_with_guard_for_test(
        &base_url_env,
        &mut ok_transport,
        request_for_test("APP123", replica_name, AlgoliaMethod::Get, "settings"),
    )
    .expect("2xx settings response should decode to the full JSON");
    assert_eq!(returned, full_settings);

    let mut missing_transport = ScriptedTransport::new(vec![status(404)]);
    let error = execute_scripted_request_with_guard_for_test(
        &base_url_env,
        &mut missing_transport,
        request_for_test("APP123", replica_name, AlgoliaMethod::Get, "settings"),
    )
    .expect_err("a 404 missing replica must be a typed error");
    assert_eq!(error.kind(), AlgoliaErrorKind::Upstream);
    assert_eq!(
        error.safe_message(),
        "Algolia upstream rejected the request"
    );
    assert!(
        !format!("{error:?}").contains("réplica"),
        "typed errors must not echo the requested index name"
    );
}

/// TODO: Document retry_policy_retries_transient_failures_and_stops_on_success.
#[test]
fn retry_policy_retries_transient_failures_and_stops_on_success() {
    let mut transport = ScriptedTransport::new(vec![
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Timeout,
            "Algolia request timed out",
        )),
        status(429),
        ok(json!({"done": true})),
    ]);

    let response = scripted_json_for_test(
        &mut transport,
        "APP123",
        "products",
        AlgoliaMethod::Get,
        "settings",
    )
    .expect("third attempt should succeed");

    assert_eq!(response, json!({"done": true}));
    assert_eq!(transport.requests.len(), 3);
}

/// TODO: Document retry_policy_uses_algolia_fallback_hosts_after_transient_data_failure.
#[test]
fn retry_policy_uses_algolia_fallback_hosts_after_transient_data_failure() {
    let base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let mut transport = ScriptedTransport::new(vec![
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Timeout,
            "Algolia request timed out",
        )),
        ok(json!({"done": true})),
    ]);

    let response = execute_scripted_request_with_guard_for_test(
        &base_url_env,
        &mut transport,
        request_for_test("APP123", "products", AlgoliaMethod::Get, "settings"),
    )
    .expect("first fallback host should succeed");

    assert_eq!(response, json!({"done": true}));
    assert_eq!(
        request_urls(&transport),
        vec![
            "https://APP123-dsn.algolia.net/1/indexes/products/settings",
            "https://APP123-1.algolianet.com/1/indexes/products/settings",
        ]
    );
}

/// TODO: Document retry_policy_uses_algolia_fallback_hosts_after_transient_control_failure.
#[test]
fn retry_policy_uses_algolia_fallback_hosts_after_transient_control_failure() {
    let base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let mut transport = ScriptedTransport::new(vec![
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Transport,
            "Algolia request failed",
        )),
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 2,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": false
            }]),
            0,
            1,
        )),
    ]);

    let indexes = list_indexes_with_guard_for_test(&base_url_env, &mut transport)
        .expect("control fallback should succeed");

    assert_eq!(indexes.len(), 1);
    assert_eq!(
        request_urls(&transport),
        vec![
            "https://APP123.algolia.net/1/indexes?page=0&hitsPerPage=100",
            "https://APP123-1.algolianet.com/1/indexes?page=0&hitsPerPage=100",
        ]
    );
}

/// TODO: Document retry_policy_stops_immediately_for_non_retryable_failures.
#[test]
fn retry_policy_stops_immediately_for_non_retryable_failures() {
    for kind in [
        AlgoliaErrorKind::Validation,
        AlgoliaErrorKind::Schema,
        AlgoliaErrorKind::Decode,
        AlgoliaErrorKind::Redirect,
        AlgoliaErrorKind::Progress,
        AlgoliaErrorKind::Limit,
    ] {
        let mut transport =
            ScriptedTransport::new(vec![Err(AlgoliaClientError::new(kind, "non retryable"))]);
        let result = scripted_json_for_test(
            &mut transport,
            "APP123",
            "products",
            AlgoliaMethod::Get,
            "settings",
        );
        assert_eq!(
            result.expect_err("non-retryable error should fail").kind(),
            kind
        );
        assert_eq!(transport.requests.len(), 1);
    }

    let mut transport = ScriptedTransport::new(vec![status(400)]);
    assert_eq!(
        scripted_json_for_test(
            &mut transport,
            "APP123",
            "products",
            AlgoliaMethod::Get,
            "settings",
        )
        .expect_err("4xx should fail")
        .kind(),
        AlgoliaErrorKind::Upstream
    );
    assert_eq!(transport.requests.len(), 1);
}

/// TODO: Document retry_policy_returns_stable_variant_after_retry_budget.
#[test]
fn retry_policy_returns_stable_variant_after_retry_budget() {
    let base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    for (responses, expected_kind) in [
        (
            vec![
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Timeout,
                    "Algolia request timed out",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Timeout,
                    "Algolia request timed out",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Timeout,
                    "Algolia request timed out",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Timeout,
                    "Algolia request timed out",
                )),
            ],
            AlgoliaErrorKind::Timeout,
        ),
        (
            vec![
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Algolia request failed",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Algolia request failed",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Algolia request failed",
                )),
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Transport,
                    "Algolia request failed",
                )),
            ],
            AlgoliaErrorKind::Transport,
        ),
        (
            vec![status(429), status(429), status(429), status(429)],
            AlgoliaErrorKind::RateLimit,
        ),
        (
            vec![status(503), status(500), status(502), status(504)],
            AlgoliaErrorKind::Server,
        ),
    ] {
        let mut transport = ScriptedTransport::new(responses);

        let result = execute_scripted_request_with_guard_for_test(
            &base_url_env,
            &mut transport,
            request_for_test("APP123", "products", AlgoliaMethod::Get, "settings"),
        );

        assert_eq!(
            result.expect_err("retry budget should fail").kind(),
            expected_kind
        );
        assert_eq!(
            request_urls(&transport),
            vec![
                "https://APP123-dsn.algolia.net/1/indexes/products/settings",
                "https://APP123-1.algolianet.com/1/indexes/products/settings",
                "https://APP123-2.algolianet.com/1/indexes/products/settings",
                "https://APP123-3.algolianet.com/1/indexes/products/settings",
            ]
        );
    }
}

/// TODO: Document list_indexes_pagination_starts_at_page_zero_and_follows_nb_pages_changes.
#[test]
fn list_indexes_pagination_starts_at_page_zero_and_follows_nb_pages_changes() {
    let base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "items": [{"name": "products", "entries": 2, "updatedAt": "2026-01-01T00:00:00Z", "pendingTask": false}],
            "nbPages": 2
        })),
        ok(json!({
            "items": [{"name": "articles", "entries": 1, "updatedAt": "2026-01-02T00:00:00Z", "pendingTask": false}],
            "nbPages": 3
        })),
        ok(json!({
            "items": [{"name": "archive", "entries": 0, "updatedAt": "2026-01-03T00:00:00Z", "pendingTask": false}],
            "nbPages": 3
        })),
    ]);

    let indexes = list_indexes_with_guard_for_test(&base_url_env, &mut transport)
        .expect("pagination should complete");

    assert_eq!(indexes.len(), 3);
    assert_eq!(indexes[0].name, "products");
    assert_eq!(
        transport.requests[0].url,
        "https://APP123.algolia.net/1/indexes?page=0&hitsPerPage=100"
    );
    assert_eq!(
        transport.requests[1].url,
        "https://APP123.algolia.net/1/indexes?page=1&hitsPerPage=100"
    );
    assert_eq!(
        transport.requests[2].url,
        "https://APP123.algolia.net/1/indexes?page=2&hitsPerPage=100"
    );
}

/// TODO: Document list_indexes_pagination_rejects_missing_metadata_and_bad_items.
#[test]
fn list_indexes_pagination_rejects_missing_metadata_and_bad_items() {
    for (body, expected_kind) in [
        (json!({"items": []}), AlgoliaErrorKind::Schema),
        (
            json!({"items": [], "page": 1, "nbPages": 1}),
            AlgoliaErrorKind::Progress,
        ),
        (
            json!({"items": [{"name": 7}], "page": 0, "nbPages": 1}),
            AlgoliaErrorKind::Schema,
        ),
        (
            json!({"items": [{"name": "x", "entries": "many", "updatedAt": "", "pendingTask": false}], "page": 0, "nbPages": 1}),
            AlgoliaErrorKind::Schema,
        ),
    ] {
        let mut transport = ScriptedTransport::new(vec![ok(body)]);
        assert_eq!(
            list_indexes_for_test(&mut transport)
                .expect_err("invalid listing should fail")
                .kind(),
            expected_kind
        );
    }
}

/// TODO: Document list_indexes_pagination_rejects_repeated_content.
#[test]
fn list_indexes_pagination_rejects_repeated_content() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "items": [{"name": "products", "entries": 2, "updatedAt": "2026-01-01T00:00:00Z", "pendingTask": false}],
            "page": 0,
            "nbPages": 2
        })),
        ok(json!({
            "items": [{"name": "products", "entries": 2, "updatedAt": "2026-01-01T00:00:00Z", "pendingTask": false}],
            "page": 1,
            "nbPages": 2
        })),
    ]);

    assert_eq!(
        list_indexes_for_test(&mut transport)
            .expect_err("repeated content should fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
}

/// TODO: Document list_indexes_pagination_rejects_page_equal_to_shrunk_nb_pages.
#[test]
fn list_indexes_pagination_rejects_page_equal_to_shrunk_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "items": [{"name": "products", "entries": 2, "updatedAt": "2026-01-01T00:00:00Z", "pendingTask": false}],
            "page": 0,
            "nbPages": 2
        })),
        ok(json!({
            "items": [{"name": "articles", "entries": 1, "updatedAt": "2026-01-02T00:00:00Z", "pendingTask": false}],
            "page": 1,
            "nbPages": 1
        })),
    ]);

    assert_eq!(
        list_indexes_for_test(&mut transport)
            .expect_err("page equal to the shrunk page count must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
    assert_eq!(transport.requests.len(), 2);
}

#[test]
fn list_indexes_pagination_rejects_nonempty_zero_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "items": [{"name": "products", "entries": 2, "updatedAt": "2026-01-01T00:00:00Z", "pendingTask": false}],
        "page": 0,
        "nbPages": 0
    }))]);

    assert_eq!(
        list_indexes_for_test(&mut transport)
            .expect_err("non-empty zero-page listing must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
}

#[test]
fn list_indexes_pagination_accepts_empty_zero_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "items": [],
        "page": 0,
        "nbPages": 0
    }))]);

    let indexes = list_indexes_for_test(&mut transport).expect("empty zero-page listing is valid");

    assert!(indexes.is_empty());
    assert_eq!(transport.requests.len(), 1);
}

/// TODO: Document list_indexes_pagination_accepts_public_rows_without_pending_task.
#[test]
fn list_indexes_pagination_accepts_public_rows_without_pending_task() {
    let mut transport = ScriptedTransport::new(vec![ok(index_page(
        json!([{
            "name": "products",
            "entries": 2,
            "updatedAt": "2026-01-01T00:00:00Z"
        }]),
        0,
        1,
    ))]);

    let indexes =
        list_indexes_for_test(&mut transport).expect("public listing does not need pendingTask");

    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].name, "products");
    assert_eq!(indexes[0].entries, 2);
    assert_eq!(indexes[0].updated_at, "2026-01-01T00:00:00Z");
}

/// TODO: Document source_export_acl_and_quiescence_reads_key_acl_through_strict_planner.
#[test]
fn source_export_acl_and_quiescence_reads_key_acl_through_strict_planner() {
    let base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
    for (acl, expected) in [
        (json!(["search", "seeUnretrievableAttributes"]), true),
        (json!(["admin"]), true),
        (json!(["search", "browse"]), false),
    ] {
        let mut transport = ScriptedTransport::new(vec![ok(json!({ "acl": acl }))]);

        assert_eq!(
            key_allows_unretrievable_with_guard_for_test(&base_url_env, &mut transport)
                .expect("ACL lookup should parse"),
            expected
        );
        assert_eq!(transport.requests.len(), 1);
        assert_eq!(transport.requests[0].method, AlgoliaMethod::Get);
        assert_eq!(
            transport.requests[0].url,
            "https://APP123.algolia.net/1/keys/key"
        );
        assert_eq!(transport.requests[0].body, None);
    }
}

/// TODO: Document source_export_acl_and_quiescence_rejects_unretrievable_without_capability.
#[test]
fn source_export_acl_and_quiescence_rejects_unretrievable_without_capability() {
    let settings = json!({ "unretrievableAttributes": ["secret"] });
    let mut denied = ScriptedTransport::new(vec![ok(json!({ "acl": ["search"] }))]);

    let error = require_unretrievable_access_for_test(&mut denied, &settings)
        .expect_err("settings with hidden fields need capability proof");

    assert_eq!(error.kind(), AlgoliaErrorKind::Validation);
    assert!(!error.safe_message().contains("source-secret"));

    let mut secret_key_transport = ScriptedTransport::new(vec![ok(json!({ "acl": ["search"] }))]);
    let secret_key_error = {
        let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();
        tokio_test::block_on(require_unretrievable_access_with_transport(
            &mut secret_key_transport,
            "APP123",
            "source-secret",
            &settings,
        ))
        .expect_err("scrubbed errors must not echo the API key")
    };
    assert!(!secret_key_error.safe_message().contains("source-secret"));

    let mut allowed = ScriptedTransport::new(vec![ok(json!({
        "acl": ["seeUnretrievableAttributes"]
    }))]);
    require_unretrievable_access_for_test(&mut allowed, &settings)
        .expect("seeUnretrievableAttributes should allow export");

    let mut no_hidden_fields = ScriptedTransport::new(Vec::new());
    require_unretrievable_access_for_test(&mut no_hidden_fields, &json!({}))
        .expect("ACL lookup is unnecessary without unretrievableAttributes");
    assert!(no_hidden_fields.requests.is_empty());
}

/// TODO: Document source_export_acl_and_quiescence_polls_until_selected_index_is_not_pending.
#[test]
fn source_export_acl_and_quiescence_polls_until_selected_index_is_not_pending() {
    let mut transport = ScriptedTransport::new(vec![
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 7,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": true
            }]),
            0,
            1,
        )),
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 7,
                "updatedAt": "2026-01-01T00:00:01Z",
                "pendingTask": false
            }]),
            0,
            1,
        )),
    ]);

    let record = wait_for_quiescent_source_for_test(&mut transport)
        .expect("pending selected index should eventually settle");

    assert_eq!(record.name, "products");
    assert_eq!(record.entries, 7);
    assert_eq!(record.updated_at, "2026-01-01T00:00:01Z");
    assert!(!record.pending_task);
    assert_eq!(transport.requests.len(), 2);
}

/// TODO: Document source_export_acl_and_quiescence_rejects_ambiguous_selected_index_metadata.
#[test]
fn source_export_acl_and_quiescence_rejects_ambiguous_selected_index_metadata() {
    for body in [
        index_page(
            json!([{
                "name": "other",
                "entries": 1,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": false
            }]),
            0,
            1,
        ),
        index_page(
            json!([
                {
                    "name": "products",
                    "entries": 1,
                    "updatedAt": "2026-01-01T00:00:00Z",
                    "pendingTask": false
                },
                {
                    "name": "products",
                    "entries": 2,
                    "updatedAt": "2026-01-01T00:00:00Z",
                    "pendingTask": false
                }
            ]),
            0,
            1,
        ),
    ] {
        let mut transport = ScriptedTransport::new(vec![ok(body)]);

        assert_eq!(
            wait_for_quiescent_source_for_test(&mut transport)
                .expect_err("missing or duplicate selected index should fail")
                .kind(),
            AlgoliaErrorKind::Progress
        );
    }
}

/// TODO: Document source_export_acl_and_quiescence_requires_selected_pending_task_metadata.
#[test]
fn source_export_acl_and_quiescence_requires_selected_pending_task_metadata() {
    for item in [
        json!({
            "name": "products",
            "entries": 1,
            "updatedAt": "2026-01-01T00:00:00Z"
        }),
        json!({
            "name": "products",
            "entries": 1,
            "updatedAt": "2026-01-01T00:00:00Z",
            "pendingTask": "false"
        }),
    ] {
        let mut transport = ScriptedTransport::new(vec![ok(index_page(json!([item]), 0, 1))]);

        let error = wait_for_quiescent_source_for_test(&mut transport)
            .expect_err("selected source quiescence requires pendingTask");

        assert_eq!(error.kind(), AlgoliaErrorKind::Schema);
    }
}

/// TODO: Document source_export_acl_and_quiescence_deadline_expiry_is_scrubbed.
#[test]
fn source_export_acl_and_quiescence_deadline_expiry_is_scrubbed() {
    let mut transport = ScriptedTransport::new(vec![
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 1,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": true
            }]),
            0,
            1,
        )),
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 1,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": true
            }]),
            0,
            1,
        )),
        ok(index_page(
            json!([{
                "name": "products",
                "entries": 1,
                "updatedAt": "secret-body-value",
                "pendingTask": true
            }]),
            0,
            1,
        )),
    ]);

    let error = wait_for_quiescent_source_for_test(&mut transport)
        .expect_err("poll budget should bound pending tasks");

    assert_eq!(error.kind(), AlgoliaErrorKind::Progress);
    assert!(!error.safe_message().contains("source-secret"));
    assert!(!error.safe_message().contains("secret-body-value"));
    assert_eq!(transport.requests.len(), 3);
}

#[test]
fn strict_source_progress_rejects_malformed_hits() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [{"objectID": "ok"}, "bad"],
        "page": 0,
        "nbPages": 1
    }))]);

    let result: Result<Vec<Value>, AlgoliaClientError> =
        paginated_hits_for_test(&mut transport, "rules/search");

    assert_eq!(
        result.expect_err("malformed hit should fail").kind(),
        AlgoliaErrorKind::Schema
    );
}

/// TODO: Document strict_source_progress_uses_explicit_nb_pages_not_short_page_heuristic.
#[test]
fn strict_source_progress_uses_explicit_nb_pages_not_short_page_heuristic() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "hits": [{"objectID": "one"}],
            "page": 0,
            "nbPages": 2
        })),
        ok(json!({
            "hits": [{"objectID": "two"}],
            "page": 1,
            "nbPages": 2
        })),
    ]);

    let result: Vec<Value> =
        paginated_hits_for_test(&mut transport, "rules/search").expect("two pages should load");
    assert_eq!(result.len(), 2);
    assert_eq!(transport.requests.len(), 2);
}

/// TODO: Document strict_source_progress_rejects_page_equal_to_shrunk_nb_pages.
#[test]
fn strict_source_progress_rejects_page_equal_to_shrunk_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "hits": [{"objectID": "one"}],
            "page": 0,
            "nbPages": 2
        })),
        ok(json!({
            "hits": [{"objectID": "two"}],
            "page": 1,
            "nbPages": 1
        })),
    ]);

    let result: Result<Vec<Value>, AlgoliaClientError> =
        paginated_hits_for_test(&mut transport, "rules/search");

    assert_eq!(
        result
            .expect_err("page equal to the shrunk page count must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
    assert_eq!(transport.requests.len(), 2);
}

/// TODO: Document strict_source_progress_rejects_nonempty_zero_nb_pages.
#[test]
fn strict_source_progress_rejects_nonempty_zero_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [{"objectID": "one"}],
        "page": 0,
        "nbPages": 0
    }))]);

    let result: Result<Vec<Value>, AlgoliaClientError> =
        paginated_hits_for_test(&mut transport, "rules/search");

    assert_eq!(
        result
            .expect_err("non-empty zero-page search result must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
}

#[test]
fn strict_source_progress_accepts_empty_zero_nb_pages() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [],
        "page": 0,
        "nbPages": 0
    }))]);

    let result: Vec<Value> = paginated_hits_for_test(&mut transport, "rules/search")
        .expect("empty zero-page result is valid");

    assert!(result.is_empty());
    assert_eq!(transport.requests.len(), 1);
}

/// TODO: Document strict_source_progress_rejects_empty_intermediate_search_page.
#[test]
fn strict_source_progress_rejects_empty_intermediate_search_page() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "hits": [{"objectID": "one"}],
            "page": 0,
            "nbPages": 3
        })),
        ok(json!({
            "hits": [],
            "page": 1,
            "nbPages": 3
        })),
    ]);

    let result: Result<Vec<Value>, AlgoliaClientError> =
        paginated_hits_for_test(&mut transport, "rules/search");

    assert_eq!(
        result
            .expect_err("empty intermediate search page must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
    assert_eq!(transport.requests.len(), 2);
}

#[test]
fn strict_source_progress_rejects_repeated_browse_cursor() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({"hits": [{"objectID": "one"}], "cursor": "same"})),
        ok(json!({"hits": [{"objectID": "two"}], "cursor": "same"})),
    ]);

    assert_eq!(
        browse_documents_for_test(&mut transport)
            .expect_err("repeated cursor must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
}

#[test]
fn strict_source_progress_rejects_malformed_browse_hits() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [{"objectID": "one"}, "bad"]
    }))]);

    assert_eq!(
        browse_documents_for_test(&mut transport)
            .expect_err("malformed browse hit should fail")
            .kind(),
        AlgoliaErrorKind::Schema
    );
}

#[test]
fn strict_source_progress_rejects_non_string_browse_cursor() {
    let mut transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [{"objectID": "one"}],
        "cursor": 123
    }))]);

    assert_eq!(
        browse_documents_for_test(&mut transport)
            .expect_err("malformed browse cursor must fail")
            .kind(),
        AlgoliaErrorKind::Progress
    );
}

/// TODO: Document strict_source_progress_streams_browse_page_before_following_request_failure.
#[test]
fn strict_source_progress_streams_browse_page_before_following_request_failure() {
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({"hits": [{"objectID": "one"}], "cursor": "next"})),
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Transport,
            "Algolia request failed",
        )),
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Transport,
            "Algolia request failed",
        )),
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Transport,
            "Algolia request failed",
        )),
    ]);
    let mut delivered_page_sizes = Vec::new();
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();

    let result = tokio_test::block_on(browse_documents_with_transport(
        &mut transport,
        "APP123",
        "key",
        "products",
        |documents| {
            delivered_page_sizes.push(documents.len());
            Ok::<_, Infallible>(())
        },
    ));

    assert_eq!(
        result
            .expect_err("the second request must surface its transport failure")
            .client_error()
            .expect("the traversal should report a client error")
            .kind(),
        AlgoliaErrorKind::Transport
    );
    assert_eq!(delivered_page_sizes, vec![1]);
}

/// TODO: Document response_byte_limit_is_enforced_before_json_decoding.
#[test]
fn response_byte_limit_is_enforced_before_json_decoding() {
    let mut transport = ScriptedTransport::new(vec![Ok(RawResponse {
        status: 200,
        body: vec![b' '; MAX_RESPONSE_BYTES + 1],
    })]);

    assert_eq!(
        scripted_json_for_test(
            &mut transport,
            "APP123",
            "products",
            AlgoliaMethod::Get,
            "settings",
        )
        .expect_err("oversized response should fail before JSON decode")
        .kind(),
        AlgoliaErrorKind::Limit
    );
}

/// TODO: Document response_byte_limit_rejects_production_content_length_before_buffering.
#[test]
fn response_byte_limit_rejects_production_content_length_before_buffering() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let address = listener.local_addr().expect("test server address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("test request should arrive");
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\n\r\n",
            MAX_RESPONSE_BYTES + 1
        )
        .expect("headers should write");
    });

    let client = reqwest::Client::builder()
        .connect_timeout(ALGOLIA_CONNECT_TIMEOUT)
        .timeout(ALGOLIA_REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .expect("test client should build");
    let mut transport = ReqwestTransport { client: &client };
    let request = PlannedRequest {
        method: AlgoliaMethod::Get,
        url: format!("http://{address}/oversized"),
        fallback_urls: Vec::new(),
        headers: Vec::new(),
        body: None,
        policy: RequestPolicy {
            connect_timeout: ALGOLIA_CONNECT_TIMEOUT,
            request_timeout: ALGOLIA_REQUEST_TIMEOUT,
            redirects_disabled: true,
            proxy_disabled: true,
        },
        max_response_bytes: MAX_RESPONSE_BYTES,
    };

    let result = tokio_test::block_on(transport.send(request));

    server.join().expect("test server should finish");
    assert_eq!(
        result
            .expect_err("oversized content-length should fail before body buffering")
            .kind(),
        AlgoliaErrorKind::Limit
    );
}

/// TODO: Document source_export_raw_traversal_preserves_json_and_uses_strict_browse_bodies.
#[test]
fn source_export_raw_traversal_preserves_json_and_uses_strict_browse_bodies() {
    let raw = json!({
        "objectID": "doc-1",
        "enabled": true,
        "deletedAt": null,
        "nested": {"z": 1, "items": [false, null, {"x": "y"}]},
        "_highlightResult": {"must": "remain"}
    });
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({"hits": [raw.clone()], "cursor": "next"})),
        ok(json!({"hits": []})),
    ]);
    let mut delivered = Vec::new();
    let _base_url_env = AlgoliaBaseUrlEnvGuard::vendor_hosts();

    tokio_test::block_on(browse_documents_with_transport(
        &mut transport,
        "APP123",
        "key",
        "summer/sale",
        |page| {
            delivered.extend(page);
            Ok::<_, Infallible>(())
        },
    ))
    .expect("raw traversal should succeed");

    assert_eq!(delivered, vec![raw]);
    assert!(transport
        .requests
        .iter()
        .all(|request| request.url.contains("/1/indexes/summer%2Fsale/browse")));
    assert_eq!(
        transport.requests[0].body,
        Some(json!({"hitsPerPage": 1000, "attributesToRetrieve": ["*"]}))
    );
    assert_eq!(transport.requests[1].body, Some(json!({"cursor": "next"})));
}

#[test]
fn source_export_raw_traversal_requires_unique_string_object_ids() {
    for hits in [
        json!([{"value": 1}]),
        json!([{"objectID": 7}]),
        json!([{"objectID": "same"}, {"objectID": "same"}]),
    ] {
        let mut transport = ScriptedTransport::new(vec![ok(json!({"hits": hits}))]);
        let result = browse_documents_for_test(&mut transport);
        assert_eq!(
            result.expect_err("invalid objectID must fail").kind(),
            AlgoliaErrorKind::Schema
        );
    }
}

/// Search-only response decorations are not part of saved rule or synonym definitions.
#[test]
fn source_export_raw_traversal_strips_search_decorations_without_normalizing_definitions() {
    let expected_rule = json!({
        "objectID": "rule-1",
        "enabled": true,
        "condition": {"pattern": "sale"},
        "consequence": {"params": {"filters": ["brand:acme", null]}},
        "customDefinitionField": {"nested": {"keep": true}}
    });
    let expected_synonym = json!({
        "objectID": "syn-1",
        "type": "synonym",
        "synonyms": ["tv", "television"],
        "metadata": {"nested": {"keep": false}}
    });
    let mut decorated_rule = expected_rule.clone();
    decorated_rule["_highlightResult"] = json!({"condition": {"pattern": {"value": "sale"}}});
    decorated_rule["_metadata"] = json!({"lastUpdate": 123});
    let mut decorated_synonym = expected_synonym.clone();
    decorated_synonym["_highlightResult"] = json!({"synonyms": [{"value": "tv"}]});
    let mut rules_transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [decorated_rule],
        "page": 0,
        "nbPages": 1
    }))]);
    let mut synonyms_transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [decorated_synonym],
        "page": 0,
        "nbPages": 1
    }))]);

    let rules = paginated_raw_hits_for_test(&mut rules_transport, "rules/search")
        .expect("rules should stream as raw JSON");
    let synonyms = paginated_raw_hits_for_test(&mut synonyms_transport, "synonyms/search")
        .expect("synonyms should stream as raw JSON");

    assert_eq!(rules, vec![expected_rule]);
    assert_eq!(synonyms, vec![expected_synonym]);
    assert_eq!(
        rules_transport.requests[0].body,
        Some(json!({"query": "", "hitsPerPage": 1000, "page": 0}))
    );
    assert!(rules_transport.requests[0]
        .url
        .ends_with("/1/indexes/products/rules/search"));
    assert!(synonyms_transport.requests[0]
        .url
        .ends_with("/1/indexes/products/synonyms/search"));
}

/// TODO: Document source_export_synonyms_search_accepts_algolia_nbhits_only_pagination.
#[test]
fn source_export_synonyms_search_accepts_algolia_nbhits_only_pagination() {
    let first_page_hits: Vec<Value> = (0..1000)
        .map(|index| {
            json!({
                "objectID": format!("syn-{index:04}"),
                "type": "synonym",
                "synonyms": [format!("term-{index:04}"), format!("alias-{index:04}")]
            })
        })
        .collect();
    let last_hit = json!({
        "objectID": "syn-1000",
        "type": "synonym",
        "synonyms": ["term-1000", "alias-1000"]
    });
    let mut transport = ScriptedTransport::new(vec![
        ok(json!({
            "hits": first_page_hits,
            "nbHits": 1001
        })),
        ok(json!({
            "hits": [last_hit.clone()],
            "nbHits": 1001
        })),
    ]);

    let synonyms = paginated_raw_hits_for_test(&mut transport, "synonyms/search")
        .expect("nbHits-only synonym pagination should stream all pages");

    assert_eq!(synonyms.len(), 1001);
    assert_eq!(synonyms[0]["objectID"], "syn-0000");
    assert_eq!(synonyms[999]["objectID"], "syn-0999");
    assert_eq!(synonyms[1000], last_hit);
    assert_eq!(transport.requests.len(), 2);
    assert_eq!(
        transport.requests[0].body,
        Some(json!({"query": "", "hitsPerPage": 1000, "page": 0}))
    );
    assert_eq!(
        transport.requests[1].body,
        Some(json!({"query": "", "hitsPerPage": 1000, "page": 1}))
    );
}

/// TODO: Document source_export_raw_traversal_requires_unique_string_object_ids_for_rules_and_synonyms.
#[test]
fn source_export_raw_traversal_requires_unique_string_object_ids_for_rules_and_synonyms() {
    for endpoint in ["rules/search", "synonyms/search"] {
        for hits in [
            json!([{"condition": {"pattern": "sale"}}]),
            json!([{"objectID": null}]),
            json!([{"objectID": "dup"}, {"objectID": "dup"}]),
        ] {
            let mut transport = ScriptedTransport::new(vec![ok(json!({
                "hits": hits,
                "page": 0,
                "nbPages": 1
            }))]);
            let result = paginated_raw_hits_for_test(&mut transport, endpoint);
            assert_eq!(
                result.expect_err("invalid objectID must fail").kind(),
                AlgoliaErrorKind::Schema
            );
        }
    }
}

/// TODO: Document source_export_raw_traversal_enforces_index_list_item_limits.
#[test]
fn source_export_raw_traversal_enforces_index_list_item_limits() {
    let limits = TraversalLimits {
        max_pages: 1,
        max_items: 2,
        max_response_bytes: 512,
    };
    let exact_cap_page = index_page(
        json!([
            {
                "name": "products",
                "entries": 2,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": false
            },
            {
                "name": "archive",
                "entries": 0,
                "updatedAt": "2026-01-01T00:00:01Z",
                "pendingTask": false
            }
        ]),
        0,
        1,
    );
    let over_cap_page = index_page(
        json!([
            {
                "name": "products",
                "entries": 2,
                "updatedAt": "2026-01-01T00:00:00Z",
                "pendingTask": false
            },
            {
                "name": "archive",
                "entries": 0,
                "updatedAt": "2026-01-01T00:00:01Z",
                "pendingTask": false
            },
            {
                "name": "logs",
                "entries": 1,
                "updatedAt": "2026-01-01T00:00:02Z",
                "pendingTask": false
            }
        ]),
        0,
        1,
    );

    let mut exact_transport = ScriptedTransport::new(vec![ok(exact_cap_page)]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut exact_transport, limits)
            .expect("exact index item cap should pass")
            .len(),
        2
    );

    let mut over_transport = ScriptedTransport::new(vec![ok(over_cap_page)]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut over_transport, limits)
            .expect_err("index item cap+1 should fail")
            .kind(),
        AlgoliaErrorKind::Limit
    );
}

/// TODO: Document source_export_raw_traversal_enforces_index_list_page_limits.
#[test]
fn source_export_raw_traversal_enforces_index_list_page_limits() {
    let limits = TraversalLimits {
        max_pages: 2,
        max_items: 10,
        max_response_bytes: 512,
    };
    let first_page = index_page(
        json!([{
            "name": "products",
            "entries": 2,
            "updatedAt": "2026-01-01T00:00:00Z",
            "pendingTask": false
        }]),
        0,
        2,
    );
    let second_page = index_page(
        json!([{
            "name": "archive",
            "entries": 0,
            "updatedAt": "2026-01-01T00:00:01Z",
            "pendingTask": false
        }]),
        1,
        2,
    );
    let page_cap_plus_one = index_page(
        json!([{
            "name": "logs",
            "entries": 1,
            "updatedAt": "2026-01-01T00:00:02Z",
            "pendingTask": false
        }]),
        1,
        3,
    );

    let mut exact_transport = ScriptedTransport::new(vec![ok(first_page.clone()), ok(second_page)]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut exact_transport, limits)
            .expect("exact index page cap should pass")
            .len(),
        2
    );

    let mut over_transport = ScriptedTransport::new(vec![ok(first_page), ok(page_cap_plus_one)]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut over_transport, limits)
            .expect_err("index page cap+1 should fail before requesting page 2")
            .kind(),
        AlgoliaErrorKind::Limit
    );
    assert_eq!(over_transport.requests.len(), 2);
}

/// TODO: Document source_export_raw_traversal_enforces_index_list_response_byte_limits.
#[test]
fn source_export_raw_traversal_enforces_index_list_response_byte_limits() {
    let page = index_page(
        json!([{
            "name": "products",
            "entries": 2,
            "updatedAt": "2026-01-01T00:00:00Z",
            "pendingTask": false
        }]),
        0,
        1,
    );
    let body = serde_json::to_vec(&page).expect("test fixture should serialize");
    let exact_limits = TraversalLimits {
        max_pages: 1,
        max_items: 1,
        max_response_bytes: body.len(),
    };
    let over_limits = TraversalLimits {
        max_pages: 1,
        max_items: 1,
        max_response_bytes: body.len() - 1,
    };

    let mut exact_transport = ScriptedTransport::new(vec![Ok(RawResponse {
        status: 200,
        body: body.clone(),
    })]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut exact_transport, exact_limits)
            .expect("exact index response byte cap should pass")
            .len(),
        1
    );

    let mut over_transport = ScriptedTransport::new(vec![Ok(RawResponse { status: 200, body })]);
    assert_eq!(
        list_indexes_with_limits_for_test(&mut over_transport, over_limits)
            .expect_err("index response byte cap+1 should fail")
            .kind(),
        AlgoliaErrorKind::Limit
    );
}

/// TODO: Document source_export_raw_traversal_uses_independent_resource_limits.
#[test]
fn source_export_raw_traversal_uses_independent_resource_limits() {
    let exact_two_items = vec![ok(json!({
        "hits": [{"objectID": "one"}, {"objectID": "two"}],
        "page": 0,
        "nbPages": 1
    }))];
    let cap_plus_one = vec![ok(json!({
        "hits": [{"objectID": "one"}, {"objectID": "two"}, {"objectID": "three"}],
        "page": 0,
        "nbPages": 1
    }))];
    let limits = TraversalLimits {
        max_pages: 1,
        max_items: 2,
        max_response_bytes: 512,
    };

    let mut rules_transport = ScriptedTransport::new(exact_two_items.clone());
    let mut synonyms_transport = ScriptedTransport::new(exact_two_items);
    assert_eq!(
        paginated_raw_hits_with_limits_for_test(&mut rules_transport, "rules/search", limits)
            .expect("exact rules item cap should pass")
            .len(),
        2
    );
    assert_eq!(
        paginated_raw_hits_with_limits_for_test(&mut synonyms_transport, "synonyms/search", limits)
            .expect("exact synonyms item cap should pass")
            .len(),
        2
    );

    let mut rules_over_cap = ScriptedTransport::new(cap_plus_one.clone());
    let mut synonyms_over_cap = ScriptedTransport::new(cap_plus_one);
    assert_eq!(
        paginated_raw_hits_with_limits_for_test(&mut rules_over_cap, "rules/search", limits)
            .expect_err("rules cap+1 should fail")
            .kind(),
        AlgoliaErrorKind::Limit
    );
    assert_eq!(
        paginated_raw_hits_with_limits_for_test(&mut synonyms_over_cap, "synonyms/search", limits)
            .expect_err("synonyms cap+1 should fail")
            .kind(),
        AlgoliaErrorKind::Limit
    );

    let mut documents_transport = ScriptedTransport::new(vec![ok(json!({
        "hits": [{"objectID": "one"}, {"objectID": "two"}]
    }))]);
    assert_eq!(
        browse_documents_with_limits_for_test(&mut documents_transport, limits)
            .expect("exact document item cap should pass")
            .len(),
        2
    );
}

#[test]
fn algolia_base_url_environment_has_one_synchronized_test_owner() {
    let client_source = include_str!("algolia_client.rs");

    assert!(
        client_source.contains("mod test_algolia_base_url_env"),
        "the production observer and test mutator must share one synchronization owner"
    );
    assert!(
        client_source.contains("test_algolia_base_url_env::read_override()"),
        "the test-build environment observer must enter the shared synchronization owner"
    );
    let migration_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/handlers/migration");
    for path in rust_sources_recursively(&migration_dir) {
        if path.ends_with("algolia_client.rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(
            !source.contains(TEST_ALGOLIA_BASE_URL_ENV),
            "migration tests must use the shared RAII owner, found direct access in {}",
            path.display()
        );
    }
}

/// Fail-capable SSOT guard for the process-global env-mutation race: every
/// migration source that mutates `environ` must enter the crate's single
/// canonical `test_helpers::ENV_MUTEX`, and no migration source may define its
/// own competing `static ENV_MUTEX`. A second, uncoordinated mutex (or a bare
/// `set_var`/`remove_var` outside the owner) lets two mutators reallocate
/// `environ` while a parallel `getenv` walks it — the exact undefined behavior
/// behind the multithreaded `migration` flake. This guard goes red the moment
/// any migration mutator stops routing through the canonical owner.
#[test]
fn all_migration_env_mutation_shares_one_canonical_synchronization_owner() {
    const CANONICAL_ENV_OWNER: &str = "test_helpers::ENV_MUTEX";
    let migration_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/handlers/migration");

    for path in rust_sources_recursively(&migration_dir) {
        // This guard file necessarily contains the sentinel strings it scans for
        // (as string literals and assertion text), so the policy-defining file
        // exempts itself to avoid matching its own source.
        if path.ends_with("algolia_client_tests.rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();

        assert!(
            !source.contains("static ENV_MUTEX:"),
            "migration source {} defines a competing `static ENV_MUTEX`; the canonical \
             owner is `test_helpers::ENV_MUTEX` and must be the only one",
            path.display()
        );

        // The re-entrancy flag belongs to the mutex, not to any one guard type.
        // A migration source that keeps its own copy only maintains it on the
        // paths it controls, so every OTHER acquisition of `ENV_MUTEX` leaves
        // the flag false — and the reader that trusts it then re-locks a
        // non-reentrant mutex on a thread that already holds it and deadlocks
        // the whole test binary at 0% CPU. That was `TEST-HANG-1` (2026-08-03).
        // Restoring a local `CURRENT_THREAD_HOLDS_ENV_MUTEX` turns this red.
        assert!(
            !source.contains("CURRENT_THREAD_HOLDS_ENV_MUTEX"),
            "migration source {} keeps a private env-lock holder flag; the canonical \
             owner is `test_helpers::current_thread_holds_env_lock`, which every \
             `ENV_MUTEX` acquisition maintains. A partially-maintained flag deadlocks.",
            path.display()
        );

        let mutates_process_env =
            source.contains("std::env::set_var(") || source.contains("std::env::remove_var(");
        if mutates_process_env {
            assert!(
                source.contains(CANONICAL_ENV_OWNER),
                "migration source {} mutates process-global env without entering the \
                 canonical `{CANONICAL_ENV_OWNER}` synchronization owner",
                path.display()
            );
        }
    }
}

/// Census the resolver/pin hook symbols permitted in each source owner, then
/// prove the process-global outbound resolver has one lifetime owner. The
/// symbol-per-owner contract keeps a newly added reader, mutator, re-export, or
/// wrapper visible without coupling this audit to invocation counts. The
/// nested-drop assertion is the fail-capable contract for the audited gap.
#[test]
#[serial_test::serial(flapjack_outbound_url_policy)]
fn resolver_hook_family_has_one_lifetime_isolation_owner() {
    let engine_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("flapjack-http must live below the engine workspace");
    assert_eq!(
        resolver_hook_symbols_by_owner(engine_root),
        expected_resolver_hook_symbols_by_owner(),
        "every hook-family reader, mutator, and wrapper must be permitted for its source owner"
    );
    assert_outbound_resolver_lifetime_isolation();
}

fn resolver_hook_names() -> [String; 7] {
    [
        ["install", "test", "outbound", "host", "resolver"].join("_"),
        ["take", "test", "outbound", "host", "resolver"].join("_"),
        ["install", "test", "dns", "resolver"].join("_"),
        ["take", "test", "dns", "resolver"].join("_"),
        ["install", "test", "algolia", "pin", "observer"].join("_"),
        ["install", "test", "algolia", "validation", "resolver"].join("_"),
        ["with", "test", "algolia", "base", "url", "override"].join("_"),
    ]
}

fn resolver_hook_symbols_by_owner(
    engine_root: &std::path::Path,
) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let hook_names = resolver_hook_names();
    let mut observed = std::collections::BTreeMap::new();
    for path in rust_sources_recursively(engine_root) {
        let source = std::fs::read_to_string(&path).unwrap();
        let symbols = resolver_hook_symbols_outside_line_comments(&source, &hook_names);
        if symbols.is_empty() {
            continue;
        }
        let relative = path
            .strip_prefix(engine_root)
            .expect("workspace Rust source must be below engine root")
            .to_string_lossy()
            .replace('\\', "/");
        observed.insert(relative, symbols);
    }
    observed
}

fn resolver_hook_symbols_outside_line_comments(
    source: &str,
    hook_names: &[String],
) -> std::collections::BTreeSet<String> {
    source
        .lines()
        .filter_map(|line| line.split("//").next())
        .flat_map(|code| {
            code.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        })
        .filter(|identifier| hook_names.iter().any(|hook| hook == identifier))
        .map(str::to_string)
        .collect()
}

fn expected_resolver_hook_symbols_by_owner(
) -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let hook_names = resolver_hook_names();
    let permitted: &[(&str, &[usize])] = &[
        ("flapjack-http/src/ai_provider.rs", &[2, 3]),
        ("flapjack-http/src/handlers/chat_tests.rs", &[0, 2]),
        (
            "flapjack-http/src/handlers/migration/algolia_client.rs",
            &[0, 2, 3, 4, 5, 6],
        ),
        (
            "flapjack-http/src/handlers/migration/algolia_client_tests.rs",
            &[0, 2, 4, 5, 6],
        ),
        (
            "flapjack-http/src/handlers/migration/meilisearch_client_tests.rs",
            &[0],
        ),
        ("flapjack-http/src/handlers/migration/mod.rs", &[6]),
        (
            "flapjack-http/src/handlers/migration/preview_tests/meilisearch.rs",
            &[0],
        ),
        (
            "flapjack-http/src/handlers/migration/preview_tests/typesense.rs",
            &[0],
        ),
        (
            "flapjack-http/src/handlers/migration/source_reader_tests.rs",
            &[5],
        ),
        (
            "flapjack-http/src/handlers/migration/typesense_client_tests.rs",
            &[0],
        ),
        ("flapjack-http/src/router_tests.rs", &[6]),
        ("src/crawler/tests.rs", &[0]),
        ("src/security.rs", &[0, 1]),
        ("src/security_tests.rs", &[0]),
        ("src/vector/embedder.rs", &[2, 3]),
        ("src/vector/embedder_tests.rs", &[0, 2]),
    ];

    permitted
        .iter()
        .map(|(owner, permitted_symbol_indexes)| {
            (
                (*owner).to_string(),
                permitted_symbol_indexes
                    .iter()
                    .map(|index| hook_names[*index].clone())
                    .collect(),
            )
        })
        .collect()
}

fn assert_outbound_resolver_lifetime_isolation() {
    use flapjack::security::test_helpers::install_test_outbound_host_resolver;

    let root_guard = install_test_outbound_host_resolver(Arc::new(|_, _| {
        Some(vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))])
    }));
    let older_guard = install_test_outbound_host_resolver(Arc::new(|_, _| {
        Some(vec![IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))])
    }));
    let newer_guard = install_test_outbound_host_resolver(Arc::new(|_, _| {
        Some(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
    }));

    drop(older_guard);
    let observed_after_older_drop = flapjack::security::first_blocked_outbound_host_ip(
        "resolver-isolation.test",
        Some(443),
        false,
    );

    // Restore the process-global slot before the intentional red assertion so
    // this contract cannot leak its fixture into another test during unwind.
    drop(newer_guard);
    drop(root_guard);

    assert_eq!(
        observed_after_older_drop,
        Some((
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            "private or local destination"
        )),
        "dropping an older guard must not replace the newer resolver; the shared hook family needs one lifetime isolation owner"
    );
}

fn rust_sources_recursively(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut sources = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| {
                matches!(
                    name.to_str(),
                    Some("target" | "node_modules" | ".git" | ".fastembed_cache")
                )
            }) {
                continue;
            }
            sources.extend(rust_sources_recursively(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
    sources
}

#[test]
fn algolia_base_url_environment_guard_restores_the_exact_prior_value() {
    let prior_value = test_algolia_base_url_env::current_value_for_test();
    {
        let _base_url_env =
            AlgoliaBaseUrlEnvGuard::overridden_to("http://127.0.0.1:18181/prior-value-test");
        assert_eq!(
            test_algolia_base_url_override().expect("loopback override should be valid"),
            Some("http://127.0.0.1:18181/prior-value-test".to_string())
        );
    }
    assert_eq!(
        test_algolia_base_url_env::current_value_for_test(),
        prior_value,
        "dropping the synchronized override must restore presence and bytes exactly"
    );
}

#[tokio::test]
async fn algolia_base_url_route_override_is_task_scoped() {
    let expected_url = "http://127.0.0.1:18181/task-scoped-test";
    let observed = with_test_algolia_base_url_override(None, Some(expected_url), async {
        test_algolia_base_url_override().expect("task-scoped loopback override should be valid")
    })
    .await;

    assert_eq!(observed, Some(expected_url.to_string()));
    assert_ne!(
        test_algolia_base_url_override().expect("unscoped override read should remain valid"),
        Some(expected_url.to_string()),
        "the route override must not leak beyond its scoped request future"
    );
}
