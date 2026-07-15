use super::allow_local_outbound_urls;
use super::first_blocked_outbound_host_ip;
use super::outbound_ip_block_reason;
use super::test_helpers::install_test_outbound_host_resolver;
use super::test_helpers::AllowLocalUrlsGuard;
use serial_test::serial;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

#[test]
#[serial(flapjack_outbound_url_policy)]
fn unset_defaults_to_false() {
    let _g = AllowLocalUrlsGuard::clear();
    assert!(
        !allow_local_outbound_urls(),
        "fail-closed default must hold when env var is absent"
    );
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn empty_string_defaults_to_false() {
    let _g = AllowLocalUrlsGuard::set("");
    assert!(
        !allow_local_outbound_urls(),
        "empty string must not be treated as opt-in"
    );
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn zero_is_false() {
    let _g = AllowLocalUrlsGuard::set("0");
    assert!(!allow_local_outbound_urls(), "\"0\" must be fail-closed");
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn truthy_values_all_opt_in() {
    // Each of these is accepted as opt-in by the chat-side precedent; this
    // SSOT extraction must preserve the same set so the vector seam honors
    // identical opt-in tokens.
    for token in [
        "1", "true", "TRUE", "True", "yes", "YES", "on", "ON", "  true  ",
    ] {
        let _g = AllowLocalUrlsGuard::set(token);
        assert!(
            allow_local_outbound_urls(),
            "token {token:?} must be treated as opt-in"
        );
    }
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn arbitrary_string_is_false() {
    let _g = AllowLocalUrlsGuard::set("maybe");
    assert!(
        !allow_local_outbound_urls(),
        "non-truthy strings must be fail-closed"
    );
}

#[test]
fn outbound_ip_policy_classification_matrix() {
    let public_v4: IpAddr = "8.8.8.8".parse().unwrap();
    let loopback_v4: IpAddr = "127.0.0.1".parse().unwrap();
    let private_v4: IpAddr = "10.0.0.7".parse().unwrap();
    let metadata_v4: IpAddr = "169.254.169.254".parse().unwrap();
    let broadcast_v4: IpAddr = "255.255.255.255".parse().unwrap();
    let unspecified_v4: IpAddr = "0.0.0.0".parse().unwrap();
    let loopback_v6: IpAddr = "::1".parse().unwrap();
    let ula_v6: IpAddr = "fd00::42".parse().unwrap();
    let linklocal_v6: IpAddr = "fe80::1".parse().unwrap();
    let unspecified_v6: IpAddr = "::".parse().unwrap();
    let mapped_metadata_v6: IpAddr = "::ffff:169.254.169.254".parse().unwrap();
    let mapped_loopback_v6: IpAddr = "::ffff:127.0.0.1".parse().unwrap();

    assert_eq!(outbound_ip_block_reason(&public_v4, false), None);
    assert_eq!(outbound_ip_block_reason(&public_v4, true), None);

    assert_eq!(
        outbound_ip_block_reason(&loopback_v4, false),
        Some("private or local destination")
    );
    assert_eq!(outbound_ip_block_reason(&loopback_v4, true), None);

    assert_eq!(
        outbound_ip_block_reason(&private_v4, false),
        Some("private or local destination")
    );
    assert_eq!(outbound_ip_block_reason(&private_v4, true), None);

    assert_eq!(
        outbound_ip_block_reason(&loopback_v6, false),
        Some("private or local destination")
    );
    assert_eq!(outbound_ip_block_reason(&loopback_v6, true), None);

    assert_eq!(
        outbound_ip_block_reason(&ula_v6, false),
        Some("private or local destination")
    );
    assert_eq!(outbound_ip_block_reason(&ula_v6, true), None);

    for blocked in [
        metadata_v4,
        broadcast_v4,
        unspecified_v4,
        linklocal_v6,
        unspecified_v6,
        mapped_metadata_v6,
    ] {
        assert_eq!(
            outbound_ip_block_reason(&blocked, false),
            Some("link-local/metadata destination"),
            "always-blocked class must reject under strict policy for {blocked}"
        );
        assert_eq!(
            outbound_ip_block_reason(&blocked, true),
            Some("link-local/metadata destination"),
            "always-blocked class must reject even under allow_local for {blocked}"
        );
    }

    assert_eq!(
        outbound_ip_block_reason(&mapped_loopback_v6, false),
        Some("private or local destination")
    );
    assert_eq!(
        outbound_ip_block_reason(&mapped_loopback_v6, true),
        None,
        "allow_local must treat IPv4-mapped loopback as local opt-in eligible"
    );
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn outbound_host_resolution_policy_edge_cases() {
    let loopback = first_blocked_outbound_host_ip("localhost", None, false);
    assert_eq!(
        loopback.map(|(_, reason)| reason),
        Some("private or local destination"),
        "localhost should fail closed without allow_local"
    );

    assert_eq!(
        first_blocked_outbound_host_ip("localhost", None, true),
        None,
        "allow_local must permit localhost"
    );

    assert_eq!(
        first_blocked_outbound_host_ip("localhost.", None, false).map(|(_, reason)| reason),
        Some("private or local destination"),
        "localhost trailing dot should be rejected after resolution"
    );

    assert_eq!(
        first_blocked_outbound_host_ip("2130706433", None, false).map(|(_, reason)| reason),
        Some("private or local destination"),
        "non-canonical numeric host must not bypass local-address block"
    );

    assert_eq!(
        first_blocked_outbound_host_ip("flapjack.invalid", Some(443), false),
        None,
        "resolution failure must stay non-blocking for offline/live-DNS-independent validation"
    );
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn vet_outbound_url_target_accepts_public_http_https_urls() {
    let _resolver = install_test_outbound_host_resolver(Arc::new(|_host, _port| {
        Some(vec!["93.184.216.34".parse::<IpAddr>().unwrap()])
    }));
    let target = super::vet_outbound_url_target("https://api.example.com/v1", false)
        .expect("public https URL should be valid")
        .expect("resolvable hostname should return vetted target");
    assert_eq!(target.host, "api.example.com");
    assert_eq!(target.port, Some(443));
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn vet_outbound_url_target_rejects_unsupported_scheme() {
    let error =
        super::vet_outbound_url_target("file:///etc/passwd", false).expect_err("file:// must fail");
    assert!(
        error.contains("unsupported scheme"),
        "error must explain scheme rejection: {error}"
    );
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn vet_outbound_url_target_rejects_missing_host() {
    let error =
        super::vet_outbound_url_target("https://", false).expect_err("missing host must fail");
    assert!(
        error.to_ascii_lowercase().contains("host"),
        "error must mention missing/invalid host: {error}"
    );
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn vet_outbound_url_target_rejects_localhost_when_allow_local_false() {
    let error = super::vet_outbound_url_target("http://localhost:11434/v1", false)
        .expect_err("localhost must fail under strict mode");
    assert!(
        error.contains("localhost"),
        "error must explain localhost rejection: {error}"
    );
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn vet_outbound_url_target_returns_none_for_unresolved_host() {
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, _port| {
        if host == "unresolved.flapjack.test" {
            return None;
        }
        Some(vec!["93.184.216.34".parse().unwrap()])
    }));

    let vetted = super::vet_outbound_url_target("https://unresolved.flapjack.test/v1", false)
        .expect("unresolved host should be fail-open");
    assert!(
        vetted.is_none(),
        "unresolved host should return Ok(None) for fail-open semantics"
    );
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn vet_outbound_url_target_returns_vetted_target_on_safe_resolution() {
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, port| {
        let mut answers = HashMap::new();
        answers.insert(
            ("safe.example.com".to_string(), Some(8443)),
            vec![
                "203.0.113.10".parse().unwrap(),
                "203.0.113.11".parse().unwrap(),
            ],
        );
        answers.get(&(host.to_string(), port)).cloned()
    }));

    let target = super::vet_outbound_url_target("https://safe.example.com:8443/v1", false)
        .expect("safe host should pass")
        .expect("resolved safe host should return vetted target");

    assert_eq!(target.host, "safe.example.com");
    assert_eq!(target.port, Some(8443));
    assert_eq!(
        target.resolved_ips,
        vec![
            "203.0.113.10".parse::<IpAddr>().unwrap(),
            "203.0.113.11".parse::<IpAddr>().unwrap()
        ]
    );
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn vet_outbound_url_target_returns_blocked_ip_context() {
    let _resolver = install_test_outbound_host_resolver(Arc::new(|host, _port| {
        if host == "blocked.example.com" {
            return Some(vec!["169.254.169.254".parse().unwrap()]);
        }
        Some(vec!["93.184.216.34".parse().unwrap()])
    }));

    let error = super::vet_outbound_url_target("https://blocked.example.com/v1", false)
        .expect_err("metadata IP should be rejected");
    assert!(
        error.contains("169.254.169.254") && error.contains("link-local/metadata destination"),
        "error should include blocked IP and classification reason: {error}"
    );
}

#[test]
#[serial(flapjack_outbound_url_policy)]
fn vet_outbound_url_target_prefers_literal_ip_classification_over_test_resolver() {
    let _resolver = install_test_outbound_host_resolver(Arc::new(|_host, _port| {
        Some(vec!["93.184.216.34".parse().unwrap()])
    }));

    let error = super::vet_outbound_url_target("http://169.254.169.254/latest/meta-data/", true)
        .expect_err("literal metadata IP must stay blocked even if a test resolver is installed");
    assert!(
        error.contains("169.254.169.254") && error.contains("link-local/metadata destination"),
        "literal IP classification must win over test resolver override: {error}"
    );
}
