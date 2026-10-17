mod common;

use axum::{Json, Router, extract::Request, routing::get};
use cairn_installation::auth::{InstallationIdentity, InstallationRole};
use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn public_oauth_only_relays_callback_parameters_without_account_authority() {
    let instance = uuid::Uuid::new_v4().to_string();
    let target = format!("/api/plugin-oauth/{instance}/callback");
    let extra = Router::new().route(
        &target,
        get(|request: Request| async move {
            let identity = request.extensions().get::<InstallationIdentity>().unwrap();
            Json(json!({
                "scope": identity.public_oauth, "query": request.uri().query(),
                "account": identity.account_id, "owner": identity.role == InstallationRole::Owner,
                "cookie": request.headers().contains_key("cookie"), "mcp": identity.mcp_scopes,
            }))
        }),
    );
    let relay = RelayedInstallation::new(extra).await;
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    let url = format!(
        "{}/api/public/installations/{installation}/plugin-oauth/{instance}/callback?state=fixture&code=a%2Bb",
        relay.app.url
    );
    let response = relay
        .app
        .client
        .get(&url)
        .header("cookie", "session=untrusted")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let value: Value = response.json().await.unwrap();
    assert_eq!(
        value,
        json!({"scope": target, "query": "state=fixture&code=a%2Bb", "account": "", "owner": false, "cookie": false, "mcp": null})
    );
    assert_eq!(
        relay.app.client.post(&url).send().await.unwrap().status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    relay.stop.cancel();
}
