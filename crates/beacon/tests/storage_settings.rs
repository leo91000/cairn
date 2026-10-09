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
            "bucket": "cairn-disks",
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

#[tokio::test]
async fn r2_requires_owner_confirmation_of_provider_privacy_and_retention() {
    let fixture = RelayedInstallation::new(axum::Router::new()).await;
    let response = fixture
        .app
        .client
        .put(format!("{}/settings/storage", fixture.base))
        .header("cookie", &fixture.cookie)
        .header("origin", &fixture.app.url)
        .header("x-csrf-token", fixture.session["csrf"].as_str().unwrap())
        .json(&json!({
            "bucket": "cairn-disks",
            "endpoint": "https://11111111111111111111111111111111.r2.cloudflarestorage.com",
            "region": "auto",
            "accessKeyId": "fixture-only",
            "secretAccessKey": "fixture-secret"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(response.text().await.unwrap().contains("Confirm"));
    fixture.close().await;
}

#[tokio::test]
async fn r2_environment_configuration_accepts_explicit_privacy_confirmation() {
    if let Ok(confirmation) = std::env::var("CAIRN_TEST_R2_CONFIRMATION") {
        let fixture = RelayedInstallation::new(axum::Router::new()).await;
        let response = fixture.get("/settings/storage").send().await.unwrap();
        assert_eq!(response.status(), 200);
        let value: serde_json::Value = response.json().await.unwrap();
        assert_eq!(value["environmentManaged"], true);
        assert_eq!(value["configured"], confirmation == "true");
        fixture.close().await;
        return;
    }

    // Separate processes isolate environment-managed installations from other tests.
    for confirmation in ["false", "true"] {
        let status = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "r2_environment_configuration_accepts_explicit_privacy_confirmation",
                "--nocapture",
            ])
            .env("CAIRN_TEST_R2_CONFIRMATION", confirmation)
            .env("STORAGE_S3_PRIVATE_BUCKET_CONFIRMED", confirmation)
            .env("STORAGE_S3_BUCKET", "cairn-disks")
            .env(
                "STORAGE_S3_ENDPOINT",
                "https://11111111111111111111111111111111.r2.cloudflarestorage.com",
            )
            .env("STORAGE_S3_REGION", "auto")
            .status()
            .await
            .unwrap();
        assert!(
            status.success(),
            "R2 environment confirmation {confirmation}"
        );
    }
}
