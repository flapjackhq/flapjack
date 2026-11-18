use super::*;
use flapjack::{Document, IndexManager};
use tempfile::TempDir;

#[tokio::test]
async fn meilisearch_automatic_searchable_attributes_preserve_migrated_search() {
    for (source_attributes, all_fields) in [(json!(["title"]), false), (json!(["*"]), true)] {
        let mut failures = Vec::new();
        let mut warnings = Vec::new();
        let settings = translate_settings_for_provider(
            &json!({"searchableAttributes": source_attributes}),
            SettingsSourceProvider::Meilisearch,
            &mut failures,
            &mut warnings,
        )
        .expect("source searchable attributes must translate");
        assert!(failures.is_empty());
        let temp = TempDir::new().unwrap();
        let manager = IndexManager::new(temp.path());
        let tenant = "migrated_meilisearch";
        manager.create_tenant(tenant).unwrap();
        settings
            .save(temp.path().join(tenant).join("settings.json"))
            .unwrap();
        manager.invalidate_settings_cache(tenant);
        manager.add_documents_sync(tenant, vec![
            Document::from_json(&json!({"objectID":"MEILI-001","sku":"MEILI-001","title":"Espresso Tamper"})).unwrap(),
            Document::from_json(&json!({"objectID":"MEILI-002","sku":"MEILI-002","title":"Pour Over Kettle"})).unwrap(),
        ]).await.unwrap();

        let result = manager
            .search(tenant, "Espresso Tamper", None, None, 10)
            .unwrap();
        assert_eq!(
            result.total, 1,
            "migrated source {source_attributes} must search its title"
        );
        assert_eq!(result.documents[0].document.id, "MEILI-001");
        assert_eq!(
            result.documents[0].document.to_json()["title"],
            "Espresso Tamper"
        );
        let sku = manager.search(tenant, "MEILI-002", None, None, 10).unwrap();
        assert_eq!(
            sku.total,
            usize::from(all_fields),
            "explicit title-only settings must still exclude sku"
        );
        if all_fields {
            assert_eq!(sku.documents[0].document.id, "MEILI-002");
        }
        assert_eq!(
            manager
                .search(tenant, "nonexistentmarker", None, None, 10)
                .unwrap()
                .total,
            0
        );
        assert_eq!(manager.search(tenant, "", None, None, 10).unwrap().total, 2);
    }
}
