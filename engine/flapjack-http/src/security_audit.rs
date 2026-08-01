//! Canonical, structurally safe security audit events.

use sha2::{Digest, Sha256};

const AUTH_FAILURE_EVENT: &str = "security_audit_auth_failure";
const AUTH_SUCCESS_EVENT: &str = "security_audit_auth_success";
const ADMIN_ACTION_EVENT: &str = "security_audit_admin_action";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Authenticate,
    CreateKey,
    UpdateKey,
    DeleteKey,
    RestoreKey,
    GenerateSecuredKey,
    DeleteIndex,
    SetSettings,
    ImportSnapshot,
    RestoreSnapshotFromS3,
    RotateAdminKey,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Authenticate => "authenticate",
            Self::CreateKey => "create_key",
            Self::UpdateKey => "update_key",
            Self::DeleteKey => "delete_key",
            Self::RestoreKey => "restore_key",
            Self::GenerateSecuredKey => "generate_secured_key",
            Self::DeleteIndex => "delete_index",
            Self::SetSettings => "set_settings",
            Self::ImportSnapshot => "import_snapshot",
            Self::RestoreSnapshotFromS3 => "restore_snapshot_from_s3",
            Self::RotateAdminKey => "rotate_admin_key",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor(&'static str);

impl Actor {
    pub const fn admin_api_key() -> Self {
        Self("admin_api_key")
    }

    fn as_str(self) -> &'static str {
        self.0
    }
}

/// An index name validated for use in an operator-consumed audit target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditIndexName(String);

impl AuditIndexName {
    pub fn new(name: &str) -> flapjack::Result<Self> {
        flapjack::validate_index_name(name)?;
        Ok(Self::from_validated(name))
    }

    /// Escapes a name whose owner has already applied `flapjack::validate_index_name`.
    ///
    /// Mutation handlers use this after their extractor or manager has established
    /// the validation invariant, so audit target construction cannot introduce a
    /// new error after storage has changed.
    pub fn from_validated(name: &str) -> Self {
        Self(escape_audit_index_name(name))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

fn escape_audit_index_name(name: &str) -> String {
    name.chars().flat_map(char::escape_default).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target(String);

impl Target {
    pub fn api_key_fingerprint(key_value: &str) -> Self {
        let digest = Sha256::digest(key_value.as_bytes());
        Self(format!("api_key:{}", hex::encode(&digest[..8])))
    }

    pub fn admin_key() -> Self {
        Self("admin_key".to_string())
    }

    pub fn route_pattern(path: AuditPath) -> Self {
        Self(path.0.to_string())
    }

    pub fn index(name: &AuditIndexName) -> Self {
        Self(format!("index:{}", name.as_str()))
    }

    pub fn index_settings(name: &AuditIndexName) -> Self {
        Self(format!("index:{}:settings", name.as_str()))
    }

    pub fn index_snapshot(name: &AuditIndexName) -> Self {
        Self(format!("index:{}:snapshot", name.as_str()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsChangedFields(String);

impl SettingsChangedFields {
    pub fn from_static_names(names: Vec<&'static str>) -> Self {
        Self(names.join(","))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// A route label whose private value can only be selected from safe literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditPath(&'static str);

impl AuditPath {
    pub fn for_auth_route(path: &str) -> Self {
        let route = match path {
            "/1/keys" => "route:/1/keys",
            "/1/keys/generateSecuredApiKey" => "route:/1/keys/generateSecuredApiKey",
            "/metrics" => "route:/metrics",
            "/1/migrate-from-algolia" => "route:/1/migrate-from-algolia",
            "/1/algolia-list-indexes" => "route:/1/algolia-list-indexes",
            _ if is_key_restore_path(path) => "route:/1/keys/{key}/restore",
            _ if path.starts_with("/1/keys/") => "route:/1/keys/{key}",
            _ if path.starts_with("/internal/") => "route:/internal/{operation}",
            _ if path.starts_with("/1/migrations/") => "route:/1/migrations/{operation}",
            "/1/security/sources" => "route:/1/security/sources",
            _ if path.starts_with("/1/security/sources") => "route:/1/security/sources/{operation}",
            _ if is_index_base_path(path) => "route:/1/indexes/{index}",
            _ if path.starts_with("/1/indexes/") => "route:/1/indexes/{index}/{operation}",
            _ if path.starts_with("/2/analytics/") => "route:/2/analytics/{operation}",
            _ => "route:/unmapped",
        };
        Self(route)
    }

    fn as_str(self) -> &'static str {
        self.0
    }
}

fn is_key_restore_path(path: &str) -> bool {
    path.strip_prefix("/1/keys/")
        .is_some_and(|suffix| suffix.ends_with("/restore"))
}

fn is_index_base_path(path: &str) -> bool {
    path.strip_prefix("/1/indexes/")
        .is_some_and(|suffix| !suffix.is_empty() && !suffix.contains('/'))
}

pub fn emit_auth_failure(path: AuditPath, auth_attempt_type: &'static str, reason: &'static str) {
    let actor = Actor::admin_api_key().as_str();
    let action = Action::Authenticate.as_str();
    let target = path.as_str();
    let outcome = Outcome::Failure.as_str();
    tracing::warn!(
        event = AUTH_FAILURE_EVENT,
        actor,
        action,
        target,
        outcome,
        auth_attempt_type,
        reason,
        path = target,
        "security event: auth failure"
    );
}

pub fn emit_auth_success(actor: Actor, target: Target) {
    let actor = actor.as_str();
    let action = Action::Authenticate.as_str();
    let target = target.as_str();
    let outcome = Outcome::Success.as_str();
    tracing::info!(
        event = AUTH_SUCCESS_EVENT,
        actor,
        action,
        target,
        outcome,
        "security event: auth success"
    );
}

pub fn emit_admin_action(
    actor: Actor,
    action: Action,
    target: Target,
    outcome: Outcome,
    reason: Option<&'static str>,
) {
    if action == Action::RotateAdminKey {
        emit_rotate_admin_key(actor, target, outcome, reason);
        return;
    }

    emit_canonical_admin_action(admin_action_event(
        actor,
        action,
        &target,
        outcome,
        reason,
        AdminActionMetadata::none(),
    ));
}

pub fn emit_set_settings_action(
    actor: Actor,
    target: Target,
    outcome: Outcome,
    changed_fields: SettingsChangedFields,
    reason: Option<&'static str>,
) {
    emit_canonical_admin_action(admin_action_event(
        actor,
        Action::SetSettings,
        &target,
        outcome,
        reason,
        AdminActionMetadata::settings_changed_fields(&changed_fields),
    ));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdminActionLevel {
    Info,
    Warn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AdminActionMetadata<'a> {
    changed_fields: Option<&'a str>,
}

impl<'a> AdminActionMetadata<'a> {
    fn none() -> Self {
        Self {
            changed_fields: None,
        }
    }

    fn settings_changed_fields(changed_fields: &'a SettingsChangedFields) -> Self {
        Self {
            changed_fields: Some(changed_fields.as_str()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AdminActionEvent<'a> {
    event: &'static str,
    actor: &'static str,
    action: &'static str,
    target: &'a str,
    outcome: &'static str,
    reason: Option<&'static str>,
    changed_fields: Option<&'a str>,
    level: AdminActionLevel,
}

fn admin_action_event<'a>(
    actor: Actor,
    action: Action,
    target: &'a Target,
    outcome: Outcome,
    reason: Option<&'static str>,
    metadata: AdminActionMetadata<'a>,
) -> AdminActionEvent<'a> {
    AdminActionEvent {
        event: ADMIN_ACTION_EVENT,
        actor: actor.as_str(),
        action: action.as_str(),
        target: target.as_str(),
        outcome: outcome.as_str(),
        reason,
        changed_fields: metadata.changed_fields,
        level: admin_action_level(outcome),
    }
}

fn admin_action_level(outcome: Outcome) -> AdminActionLevel {
    match outcome {
        Outcome::Success => AdminActionLevel::Info,
        Outcome::Failure => AdminActionLevel::Warn,
    }
}

#[allow(clippy::cognitive_complexity)] // Static tracing call sites preserve optional-field omission and severity without constructing dynamic event payloads.
fn emit_canonical_admin_action(event: AdminActionEvent<'_>) {
    let AdminActionEvent {
        event: event_name,
        actor,
        action,
        target,
        outcome,
        reason,
        changed_fields,
        level,
    } = event;

    match (level, changed_fields) {
        (AdminActionLevel::Info, Some(changed_fields)) => tracing::info!(
            event = event_name,
            actor,
            action,
            target,
            outcome,
            changed_fields,
            reason,
            "security event: admin action"
        ),
        (AdminActionLevel::Info, None) => tracing::info!(
            event = event_name,
            actor,
            action,
            target,
            outcome,
            reason,
            "security event: admin action"
        ),
        (AdminActionLevel::Warn, Some(changed_fields)) => tracing::warn!(
            event = event_name,
            actor,
            action,
            target,
            outcome,
            changed_fields,
            reason,
            "security event: admin action"
        ),
        (AdminActionLevel::Warn, None) => tracing::warn!(
            event = event_name,
            actor,
            action,
            target,
            outcome,
            reason,
            "security event: admin action"
        ),
    }
}

fn emit_rotate_admin_key(
    actor: Actor,
    target: Target,
    outcome: Outcome,
    reason: Option<&'static str>,
) {
    let actor = actor.as_str();
    let action = Action::RotateAdminKey.as_str();
    let target = target.as_str();
    let outcome_value = outcome.as_str();

    // `admin_action` is a compatibility alias. Removing it is a breaking
    // audit-contract change and requires fjcloud-side coordination.
    match outcome {
        Outcome::Success => tracing::info!(
            event = ADMIN_ACTION_EVENT,
            actor,
            action,
            target,
            outcome = outcome_value,
            admin_action = "rotate_admin_key",
            reason,
            "security event: admin action"
        ),
        Outcome::Failure => tracing::warn!(
            event = ADMIN_ACTION_EVENT,
            actor,
            action,
            target,
            outcome = outcome_value,
            admin_action = "rotate_admin_key",
            reason,
            "security event: admin action"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_fingerprint_matches_known_sha256_prefix() {
        assert_eq!(
            Target::api_key_fingerprint("key-target-known-answer").as_str(),
            "api_key:4e38e2aa63786ebc"
        );
    }

    #[test]
    fn auth_key_path_discards_concrete_key_segment() {
        let path = AuditPath::for_auth_route("/1/keys/do-not-log-this");
        assert_eq!(path.as_str(), "route:/1/keys/{key}");
    }

    #[test]
    fn unknown_auth_path_discards_all_request_text() {
        let path = AuditPath::for_auth_route("/unknown/do-not-log-this");
        assert_eq!(path.as_str(), "route:/unmapped");
    }

    #[test]
    fn admin_auth_base_routes_do_not_invent_operation_segments() {
        assert_eq!(
            AuditPath::for_auth_route("/1/indexes/products").as_str(),
            "route:/1/indexes/{index}"
        );
        assert_eq!(
            AuditPath::for_auth_route("/1/security/sources").as_str(),
            "route:/1/security/sources"
        );
    }

    #[test]
    fn index_targets_require_a_safe_validated_name() {
        assert!(AuditIndexName::new("../request-derived").is_err());
        assert!(AuditIndexName::new("products\0null").is_err());

        let control_name = AuditIndexName::new("products\nforged_field=true").unwrap();
        assert_eq!(
            Target::index(&control_name).as_str(),
            "index:products\\nforged_field=true"
        );

        let name = AuditIndexName::new("products-v2").unwrap();
        assert_eq!(Target::index(&name).as_str(), "index:products-v2");
        assert_eq!(
            Target::index_settings(&name).as_str(),
            "index:products-v2:settings"
        );
        assert_eq!(
            Target::index_snapshot(&name).as_str(),
            "index:products-v2:snapshot"
        );
    }

    #[test]
    fn manager_validated_index_names_have_infallible_escaped_audit_targets() {
        let control_name = AuditIndexName::from_validated("products\nforged_field=true");
        assert_eq!(
            Target::index(&control_name).as_str(),
            "index:products\\nforged_field=true"
        );
    }

    #[test]
    fn settings_changed_fields_joins_only_caller_supplied_static_names() {
        let changed_fields =
            SettingsChangedFields::from_static_names(vec!["attributesForFaceting", "userData"]);
        assert_eq!(changed_fields.as_str(), "attributesForFaceting,userData");
    }

    #[test]
    fn admin_action_event_fields_are_canonical_for_plain_and_settings_events() {
        let actor = Actor::admin_api_key();
        let key_target = Target::api_key_fingerprint("ordinary-admin-event-key");
        let settings_target =
            Target::index_settings(&AuditIndexName::new("settings_event_index").unwrap());
        let changed_fields = SettingsChangedFields::from_static_names(vec!["userData"]);

        let ordinary = admin_action_event(
            actor,
            Action::CreateKey,
            &key_target,
            Outcome::Success,
            None,
            AdminActionMetadata::none(),
        );
        assert_eq!(ordinary.event, ADMIN_ACTION_EVENT);
        assert_eq!(ordinary.actor, "admin_api_key");
        assert_eq!(ordinary.action, "create_key");
        assert_eq!(ordinary.target, key_target.as_str());
        assert_eq!(ordinary.outcome, "success");
        assert_eq!(ordinary.reason, None);
        assert_eq!(ordinary.changed_fields, None);
        assert_eq!(ordinary.level, AdminActionLevel::Info);

        let settings = admin_action_event(
            actor,
            Action::SetSettings,
            &settings_target,
            Outcome::Failure,
            Some("settings_save_failed"),
            AdminActionMetadata::settings_changed_fields(&changed_fields),
        );
        assert_eq!(settings.event, ADMIN_ACTION_EVENT);
        assert_eq!(settings.actor, "admin_api_key");
        assert_eq!(settings.action, "set_settings");
        assert_eq!(settings.target, settings_target.as_str());
        assert_eq!(settings.outcome, "failure");
        assert_eq!(settings.reason, Some("settings_save_failed"));
        assert_eq!(settings.changed_fields, Some("userData"));
        assert_eq!(settings.level, AdminActionLevel::Warn);
    }
}
