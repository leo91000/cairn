mod common;

use common::RelayedInstallation;
use serde_json::json;

#[tokio::test]
async fn removed_s3_settings_are_unavailable_through_the_beacon_relay() {
    let fixture = RelayedInstallation::new(axum::Router::new()).await;
    for (method, path) in [
        (reqwest::Method::GET, "/settings/storage"),
        (reqwest::Method::PUT, "/settings/storage"),
        (reqwest::Method::POST, "/settings/storage/check"),
    ] {
        let response = fixture
            .app
            .client
            .request(method, format!("{}{path}", fixture.base))
            .header("cookie", &fixture.cookie)
            .header("origin", &fixture.app.url)
            .header("x-csrf-token", fixture.session["csrf"].as_str().unwrap())
            .json(&json!({
                "bucket": "cairn-disks",
                "endpoint": "https://s3.example.test",
                "region": "eu-west-3",
                "accessKeyId": "fixture-key",
                "secretAccessKey": "fixture-secret",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND, "{path}");
        assert!(!response.text().await.unwrap().contains("fixture-secret"));
    }
    fixture.close().await;
}
