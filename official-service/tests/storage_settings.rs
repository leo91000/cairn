mod common;

use common::RelayedInstallation;
use serde_json::json;

#[tokio::test]
async fn owner_reads_storage_settings_through_the_official_api_without_credentials() {
    let fixture = RelayedInstallation::new(axum::Router::new()).await;
    let response = fixture.get("/settings/storage").send().await.unwrap();
    assert_eq!(response.status(), 200);
    let value: serde_json::Value = response.json().await.unwrap();
    assert_eq!(value["configured"], false);
    assert!(value.get("secretAccessKey").is_none());
    assert!(value.get("accessKeyId").is_none());
    fixture.close().await;
}

#[tokio::test]
async fn owner_cannot_replace_storage_with_an_insecure_external_endpoint() {
    let fixture = RelayedInstallation::new(axum::Router::new()).await;
    let response = fixture
        .app
        .client
        .put(format!("{}/settings/storage", fixture.base))
        .header("cookie", &fixture.cookie)
        .header("origin", &fixture.app.url)
        .header("x-csrf-token", fixture.session["csrf"].as_str().unwrap())
        .json(&json!({
            "bucket": "leo-disks",
            "endpoint": "http://external.example.test",
            "region": "test",
            "accessKeyId": "fixture-only",
            "secretAccessKey": "fixture-secret"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body = response.text().await.unwrap();
    assert!(body.contains("HTTPS"));
    assert!(!body.contains("fixture-secret"));
    fixture.close().await;
}

#[tokio::test]
async fn storage_check_explains_missing_configuration_through_the_relay() {
    let fixture = RelayedInstallation::new(axum::Router::new()).await;
    let response = fixture
        .app
        .client
        .post(format!("{}/settings/storage/check", fixture.base))
        .header("cookie", &fixture.cookie)
        .header("origin", &fixture.app.url)
        .header("x-csrf-token", fixture.session["csrf"].as_str().unwrap())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    fixture.close().await;
}
