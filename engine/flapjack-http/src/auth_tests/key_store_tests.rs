use super::*;
use std::sync::{Arc, Barrier};

// ── hash_key / verify_key ──

#[test]
fn hash_and_verify_roundtrip() {
    let salt = "test_salt_123";
    let key = "my_secret_key";
    let hash = hash_key(key, salt);
    assert!(verify_key(key, &hash, salt));
}

#[test]
fn verify_wrong_key_fails() {
    let salt = "salt";
    let hash = hash_key("correct_key", salt);
    assert!(!verify_key("wrong_key", &hash, salt));
}

#[test]
fn verify_wrong_salt_fails() {
    let hash = hash_key("key", "salt1");
    assert!(!verify_key("key", &hash, "salt2"));
}

#[test]
fn hash_is_hex_64_chars() {
    let hash = hash_key("key", "salt");
    assert_eq!(hash.len(), 64); // SHA-256 = 32 bytes = 64 hex chars
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn hash_deterministic() {
    let h1 = hash_key("key", "salt");
    let h2 = hash_key("key", "salt");
    assert_eq!(h1, h2);
}

// ── generate_hex_key ──

#[test]
fn generate_hex_key_format() {
    let key = generate_hex_key();
    assert_eq!(key.len(), 32); // 16 bytes = 32 hex chars
    assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn generate_hex_key_is_16_bytes_rendered_as_hex() {
    let key = generate_hex_key();
    let decoded = hex::decode(&key).expect("generate_hex_key() must return valid hex");
    assert_eq!(
        decoded.len(),
        16,
        "generate_hex_key() must generate exactly 16 random bytes"
    );
}

#[test]
fn generate_hex_key_unique() {
    let k1 = generate_hex_key();
    let k2 = generate_hex_key();
    assert_ne!(k1, k2);
}

// ── generate_admin_key ──

#[test]
fn generate_admin_key_prefix() {
    let key = generate_admin_key();
    assert!(key.starts_with("fj_admin_"));
    assert_eq!(key.len(), 9 + 32); // prefix + 32 hex chars
}

#[cfg(unix)]
#[test]
fn keys_json_has_restricted_permissions() {
    let temp_dir = TempDir::new().unwrap();
    let _store = KeyStore::load_or_create(temp_dir.path(), "test-admin-key");
    let keys_path = temp_dir.path().join("keys.json");
    assert!(keys_path.exists(), "keys.json must be created");
    let metadata = std::fs::metadata(&keys_path).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "keys.json must have 0600 permissions (got {:o})",
        mode
    );
}

/// Verify that the default admin key's ACL set is identical to `VALID_ACLS`, catching any drift between the two sources of truth.
#[test]
fn default_admin_key_acls_match_canonical_valid_acls() {
    let temp_dir = TempDir::new().unwrap();
    let store = KeyStore::load_or_create(temp_dir.path(), "test-admin-key");
    let admin = store
        .list_all()
        .into_iter()
        .find(|k| k.description == "Admin API Key")
        .expect("admin key must exist");

    let expected: BTreeSet<&str> = VALID_ACLS.iter().copied().collect();
    let actual: BTreeSet<&str> = admin.acl.iter().map(|s| s.as_str()).collect();
    assert_eq!(
        actual, expected,
        "default admin ACL set drifted from canonical VALID_ACLS"
    );
}

#[test]
fn default_search_key_has_bounded_per_ip_rate_limit() {
    let temp_dir = TempDir::new().unwrap();
    let store = KeyStore::load_or_create(temp_dir.path(), "test-admin-key");
    let search_key = store
        .list_all()
        .into_iter()
        .find(|k| k.description == "Default Search API Key")
        .expect("default search key must exist");

    assert!(
        search_key.max_queries_per_ip_per_hour > 0,
        "default search key must ship with a positive maxQueriesPerIPPerHour limit"
    );
}
#[test]
fn load_or_create_recreates_default_keys_when_keys_json_is_corrupt() {
    let temp_dir = TempDir::new().unwrap();
    let keys_path = temp_dir.path().join("keys.json");
    std::fs::write(&keys_path, "{ definitely-not-valid-json").unwrap();

    let store = KeyStore::load_or_create(temp_dir.path(), "test-admin-key");
    let stored_data: KeyStoreData =
        serde_json::from_str(&std::fs::read_to_string(&keys_path).unwrap())
            .expect("load_or_create should rewrite corrupt keys.json with valid key data");

    assert_eq!(
        stored_data.keys.len(),
        2,
        "default key store should contain admin and search keys"
    );
    assert!(
        store.lookup("test-admin-key").is_some(),
        "recreated key store should contain a usable admin key"
    );
}
#[test]
fn load_or_create_rotates_admin_hash_when_admin_key_changes() {
    let temp_dir = TempDir::new().unwrap();
    let original_admin_key = "original-admin-key";
    let rotated_admin_key = "rotated-admin-key";

    let original_store = KeyStore::load_or_create(temp_dir.path(), original_admin_key);
    let original_admin_entry = original_store
        .list_all()
        .into_iter()
        .find(|key| key.description == "Admin API Key")
        .expect("admin key must exist");

    let rotated_store = KeyStore::load_or_create(temp_dir.path(), rotated_admin_key);
    let rotated_admin_entry = rotated_store
        .list_all()
        .into_iter()
        .find(|key| key.description == "Admin API Key")
        .expect("admin key must still exist after rotation");

    assert!(
        rotated_store.lookup(rotated_admin_key).is_some(),
        "rotated admin key should authenticate after load_or_create"
    );
    assert!(
        rotated_store.lookup(original_admin_key).is_none(),
        "old admin key should no longer authenticate after rotation"
    );
    assert_ne!(
        (original_admin_entry.hash, original_admin_entry.salt),
        (rotated_admin_entry.hash, rotated_admin_entry.salt),
        "admin key rotation should replace both hash and salt"
    );
}

#[test]
fn concurrent_runtime_admin_key_rotations_leave_persisted_key_authorized() {
    let temp_dir = TempDir::new().unwrap();
    let store = Arc::new(KeyStore::load_or_create(
        temp_dir.path(),
        "original-admin-key-for-concurrent-rotation",
    ));
    let key_path = temp_dir.path().join(".admin_key");
    let worker_count = 8;
    let start = Arc::new(Barrier::new(worker_count));

    let handles: Vec<_> = (0..worker_count)
        .map(|_| {
            let store = Arc::clone(&store);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                store
                    .rotate_admin_key()
                    .expect("runtime admin key rotation should succeed")
            })
        })
        .collect();

    let rotated_keys: Vec<String> = handles
        .into_iter()
        .map(|handle| handle.join().expect("rotation worker should not panic"))
        .collect();

    let persisted_key = std::fs::read_to_string(&key_path)
        .expect(".admin_key must exist after runtime rotation")
        .trim()
        .to_string();
    assert!(
        rotated_keys.contains(&persisted_key),
        "persisted key must be one of the successful rotation results"
    );
    assert_eq!(
        store.admin_key_value(),
        persisted_key,
        "persisted .admin_key must match the in-memory admin key after concurrent rotations"
    );
    assert!(
        store.lookup(&persisted_key).is_some(),
        "persisted .admin_key must authorize via the stored admin hash"
    );
}
#[test]
fn create_key_persists_restrict_sources_across_reload() {
    let temp_dir = TempDir::new().unwrap();
    let admin_key = "test-admin-key";
    let store = KeyStore::load_or_create(temp_dir.path(), admin_key);
    let expected_restrict_sources = vec!["192.168.1.0/24".to_string(), "10.0.0.1".to_string()];

    let mut persisted_key = test_search_api_key("persisted key");
    persisted_key.restrict_sources = Some(expected_restrict_sources.clone());

    let (_, plaintext_key) = store.create_key(persisted_key);
    drop(store);

    let reloaded_store = KeyStore::load_or_create(temp_dir.path(), admin_key);
    let persisted_key = reloaded_store
        .lookup(&plaintext_key)
        .expect("created key must survive a reload");

    assert_eq!(
        persisted_key.restrict_sources,
        Some(expected_restrict_sources),
        "restrictSources must round-trip through keys.json persistence"
    );
}
#[test]
fn load_or_create_defaults_missing_restrict_sources_to_none() {
    let temp_dir = TempDir::new().unwrap();
    let admin_key = "test-admin-key";
    let store = KeyStore::load_or_create(temp_dir.path(), admin_key);

    let legacy_key = test_search_api_key("legacy key");
    let (_, plaintext_key) = store.create_key(legacy_key);
    drop(store);

    let keys_json = std::fs::read_to_string(temp_dir.path().join("keys.json")).unwrap();
    assert!(
        !keys_json.contains("\"restrictSources\""),
        "legacy keys.json fixture must omit restrictSources entirely"
    );

    let reloaded_store = KeyStore::load_or_create(temp_dir.path(), admin_key);
    let legacy_key = reloaded_store
        .lookup(&plaintext_key)
        .expect("legacy key must still load without restrictSources in keys.json");

    assert_eq!(
        legacy_key.restrict_sources, None,
        "missing restrictSources must deserialize to None"
    );
}

/// Verify that `KeyApiResponse::from_api_key` maps all fields correctly and preserves `createdAt` as epoch milliseconds in both the struct and JSON serialization.
#[test]
fn key_api_response_maps_fields_and_serializes_created_at_as_epoch_millis() {
    let internal = ApiKey {
        hash: "hash".to_string(),
        salt: "salt".to_string(),
        hmac_key: Some("hmac".to_string()),
        created_at: 1_739_000_001_234,
        acl: vec!["search".to_string()],
        description: "desc".to_string(),
        indexes: vec!["products".to_string()],
        max_hits_per_query: 7,
        max_queries_per_ip_per_hour: 42,
        query_parameters: "hitsPerPage=3".to_string(),
        referers: vec!["*.example.com".to_string()],
        validity: 3600,
        restrict_sources: None,
    };

    let dto = KeyApiResponse::from_api_key(&internal, "plain_key_value".to_string());
    assert_eq!(dto.value, "plain_key_value");
    assert_eq!(
        dto.created_at, internal.created_at,
        "createdAt DTO field must remain epoch millis"
    );
    assert_eq!(dto.acl, internal.acl);
    assert_eq!(dto.indexes, internal.indexes);

    let json = serde_json::to_value(&dto).unwrap();
    assert_eq!(json["value"], "plain_key_value");
    assert!(
        json["createdAt"].is_i64() || json["createdAt"].is_u64(),
        "createdAt must serialize as integer epoch millis, got: {}",
        json["createdAt"]
    );
}

/// restrictSources round-trips through JSON serialization using the Algolia field name.
#[test]
fn api_key_restrict_sources_serializes_with_algolia_field_name() {
    let json = serde_json::json!({
        "hash": "abc",
        "salt": "def",
        "createdAt": 0i64,
        "acl": [],
        "restrictSources": ["192.168.1.0/24", "10.0.0.1"]
    });
    let key: ApiKey = serde_json::from_value(json).unwrap();
    assert_eq!(
        key.restrict_sources,
        Some(vec!["192.168.1.0/24".to_string(), "10.0.0.1".to_string()])
    );

    let re_serialized = serde_json::to_value(&key).unwrap();
    assert_eq!(
        re_serialized["restrictSources"],
        serde_json::json!(["192.168.1.0/24", "10.0.0.1"])
    );
}

/// restrictSources absent from JSON deserializes to None (not an error).
#[test]
fn api_key_restrict_sources_defaults_to_none_when_absent() {
    let json = serde_json::json!({
        "hash": "abc",
        "salt": "def",
        "createdAt": 0i64,
        "acl": []
    });
    let key: ApiKey = serde_json::from_value(json).unwrap();
    assert_eq!(key.restrict_sources, None);
}

/// KeyApiResponse includes restrictSources when set on the source ApiKey.
#[test]
fn key_api_response_includes_restrict_sources_when_set() {
    let key = ApiKey {
        hash: "h".to_string(),
        salt: "s".to_string(),
        hmac_key: None,
        created_at: 0,
        acl: vec![],
        description: String::new(),
        indexes: vec![],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        validity: 0,
        restrict_sources: Some(vec!["192.168.1.0/24".to_string()]),
    };
    let response = KeyApiResponse::from_api_key(&key, "myvalue".to_string());
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(
        json["restrictSources"],
        serde_json::json!(["192.168.1.0/24"])
    );
}

/// Verify that `list_all_as_dto` populates the `value` field with the admin plaintext for the admin key and the `hmac_key` plaintext for non-admin keys.
#[test]
fn list_all_as_dto_populates_value_for_admin_and_non_admin_keys() {
    let temp_dir = TempDir::new().unwrap();
    let admin_key = "admin-secret-value";
    let store = KeyStore::load_or_create(temp_dir.path(), admin_key);

    let internal_keys = store.list_all();
    let internal_search = internal_keys
        .iter()
        .find(|k| k.description == "Default Search API Key")
        .expect("default search key must exist");
    let expected_search_value = internal_search
        .hmac_key
        .clone()
        .expect("default search key must retain hmac_key plaintext");

    let dto_keys = store.list_all_as_dto();
    let admin_dto = dto_keys
        .iter()
        .find(|k| k.description == "Admin API Key")
        .expect("admin dto must exist");
    assert_eq!(admin_dto.value, admin_key);

    let search_dto = dto_keys
        .iter()
        .find(|k| k.description == "Default Search API Key")
        .expect("search dto must exist");
    assert_eq!(search_dto.value, expected_search_value);
}
