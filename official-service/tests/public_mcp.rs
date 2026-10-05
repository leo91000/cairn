mod common;

use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn personal_mcp_token_is_scoped_to_one_installation_and_revocable() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let tokens = format!("{}/api/installations/{id}/tokens", app.url);
    let response = app.client.post(&tokens)
        .header("cookie", &relay.cookie)
        .header("origin", &app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&json!({ "label": "Read client", "scopes": ["read"] }))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created: Value = response.json().await.unwrap();
    let token = created["token"].as_str().unwrap();
    let call = |name: &str, args: Value| app.client.post(format!("{}/mcp", app.url))
        .bearer_auth(token)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } }));
    let response = call("list_agents", json!({})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: Value = response.json().await.unwrap();
    assert!(result["result"]["structuredContent"]["result"].is_array(), "{result}");
    let denied: Value = call("save_project", json!({ "name": "Denied", "path": "/tmp" }))
        .send().await.unwrap().json().await.unwrap();
    assert_eq!(denied["result"]["isError"], true);
    let response = app.client.delete(format!("{tokens}/{}", created["id"].as_str().unwrap()))
        .header("cookie", &relay.cookie)
        .header("origin", &app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(call("list_agents", json!({})).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
    relay.close().await;
}
