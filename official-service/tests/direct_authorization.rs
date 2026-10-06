mod common;

use common::RelayedInstallation;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

fn fingerprint() -> String {
    format!("sha-256 {}", vec!["AB"; 32].join(":"))
}

#[tokio::test]
async fn owner_receives_a_session_bound_grant_verified_by_the_real_installation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let response = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            Method::POST,
            &format!("/api/installations/{id}/direct/authorize"),
        )
        .json(&json!({ "fingerprint": fingerprint(), "versions": [3] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let grant: Value = response.json().await.unwrap();
    assert_eq!(grant["available"], true);
    assert!(grant["grant"]["signature"].is_string());
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}
