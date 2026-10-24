mod common;

use axum::{Json, Router, body::to_bytes, extract::Request, routing::post};
use cairn_installation::auth::{InstallationIdentity, InstallationRole};
use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn public_webhook_relays_original_bytes_and_signatures_without_account_credentials() {
    let instance = uuid::Uuid::new_v4().to_string();
    let target = format!("/api/plugin-webhooks/{instance}/events");
    let extra = Router::new().route(
        &target,
        post(|request: Request| async move {
            let identity = request
                .extensions()
                .get::<InstallationIdentity>()
                .unwrap()
                .clone();
            let headers = request.headers().clone();
            let body = to_bytes(request.into_body(), 1024).await.unwrap();
            Json(json!({
                "body": std::str::from_utf8(&body).unwrap(),
                "signature": headers["x-example-signature"].to_str().unwrap(),
                "timestamp": headers["x-example-timestamp"].to_str().unwrap(),
                "cookie": headers.contains_key("cookie"),
                "forwarded": headers.contains_key("x-forwarded-host"),
                "account": identity.account_id,
                "owner": identity.role == InstallationRole::Owner,
                "mcp": identity.mcp_scopes,
                "scope": identity.public_webhook,
            }))
        }),
    );
    let relay = RelayedInstallation::new(extra).await;
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    let url = format!(
        "{}/api/public/installations/{installation}/plugins/{instance}/events",
        relay.app.url
    );
    let original = " { \"unchanged\" : true } ";
    let response = relay
        .app
        .client
        .post(&url)
        .header("content-type", "application/json")
        .header("x-example-signature", "v0=signature")
        .header("x-example-timestamp", "1234567890")
        .header("cookie", "session=untrusted")
        .header("x-forwarded-host", "untrusted.example")
        .body(original)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let value: Value = response.json().await.unwrap();
    assert_eq!(
        value,
        json!({
            "body": original, "signature": "v0=signature", "timestamp": "1234567890",
            "cookie": false, "forwarded": false, "account": "", "owner": false, "mcp": null, "scope": target,
        })
    );
    relay.stop.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            if relay.app.client.post(&url).send().await.unwrap().status()
                == StatusCode::SERVICE_UNAVAILABLE
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    relay.close().await;
}
