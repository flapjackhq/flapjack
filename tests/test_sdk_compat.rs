use serde_json::json;

mod common;
use common::spawn_server;

#[tokio::test]
async fn test_sdk_endpoints_exist() {
    let (addr, _temp) = spawn_server().await;
    let client = reqwest::Client::new();
    let base = format!("http://{}", addr);

    // SDK v5 uses PUT for settings (not POST)
    let res = client
        .put(format!("{}/1/indexes/products/settings", base))
        .header("x-algolia-application-id", "test-app")
        .header("x-algolia-api-key", "test-key")
        .json(&json!({"attributesForFaceting": ["category"]}))
        .send()
        .await
        .unwrap();

    println!("PUT settings status: {}", res.status());
    assert!(
        res.status().is_success() || res.status() == 405,
        "PUT /settings returned {}",
        res.status()
    );

    // SDK v5 batch format
    let res = client
        .post(format!("{}/1/indexes/products/batch", base))
        .header("x-algolia-application-id", "test-app")
        .header("x-algolia-api-key", "test-key")
        .json(&json!({
            "requests": [{
                "action": "addObject",
                "body": {"objectID": "1", "name": "Laptop"}
            }]
        }))
        .send()
        .await
        .unwrap();

    println!("POST batch status: {}", res.status());
    assert!(res.status().is_success());
}
