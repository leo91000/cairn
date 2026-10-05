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
    let response = app
        .client
        .post(&tokens)
        .header("cookie", &relay.cookie)
        .header("origin", &app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&json!({ "label": "Read client", "scopes": ["read"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created: Value = response.json().await.unwrap();
    let token = created["token"].as_str().unwrap();
    let call = |name: &str, args: Value| {
        app.client.post(format!("{}/mcp", app.url))
        .bearer_auth(token)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } }))
    };
    let response = call("list_agents", json!({})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: Value = response.json().await.unwrap();
    assert!(
        result["result"]["structuredContent"]["result"].is_array(),
        "{result}"
    );
    let denied: Value = call("save_project", json!({ "name": "Denied", "path": "/tmp" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(denied["result"]["isError"], true);
    let response = app
        .client
        .delete(format!("{tokens}/{}", created["id"].as_str().unwrap()))
        .header("cookie", &relay.cookie)
        .header("origin", &app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        call("list_agents", json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    relay.close().await;
}

fn owner_post(relay: &RelayedInstallation, path: &str, value: Value) -> reqwest::RequestBuilder {
    relay
        .app
        .client
        .post(format!("{}{path}", relay.app.url))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&value)
}

#[tokio::test]
async fn oauth_pkce_registration_rotation_and_reuse_are_bound_to_the_selected_installation() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let metadata: Value = app
        .client
        .get(format!(
            "{}/.well-known/oauth-protected-resource/mcp",
            app.url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metadata["resource"], format!("{}/mcp", app.url));
    let response = app.client.post(format!("{}/oauth/register", app.url))
        .json(&json!({ "client_name": "OAuth client", "redirect_uris": ["http://localhost:9999/callback"], "token_endpoint_auth_method": "none" }))
        .send().await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let client: Value = response.json().await.unwrap();
    let verifier = "a".repeat(43);
    let parameters = json!({
        "client_id": client["client_id"],
        "redirect_uri": "http://localhost:9999/callback",
        "response_type": "code",
        "code_challenge_method": "S256",
        "code_challenge": URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        "resource": format!("{}/mcp", app.url),
        "scope": "read run",
        "state": "client-state",
    });
    let preview: Value = owner_post(&relay, "/api/mcp/oauth/preview", parameters.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        preview["installations"][0]["id"],
        relay.session["installations"][0]["id"]
    );
    let consent: Value = owner_post(
        &relay,
        "/api/mcp/oauth/consent",
        json!({
            "parameters": parameters,
            "installationId": relay.session["installations"][0]["id"],
            "approved": true,
        }),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let redirect = url::Url::parse(consent["redirect"].as_str().unwrap()).unwrap();
    let pairs: std::collections::HashMap<_, _> = redirect.query_pairs().collect();
    assert_eq!(pairs["state"], "client-state");
    let exchange = json!({
        "grant_type": "authorization_code",
        "client_id": client["client_id"],
        "redirect_uri": "http://localhost:9999/callback",
        "code": pairs["code"],
        "code_verifier": verifier,
        "resource": format!("{}/mcp", app.url),
    });
    let exchange_request = |params: &Value| {
        app.client
            .post(format!("{}/oauth/token", app.url))
            .form(params)
    };
    let mut wrong = exchange.clone();
    wrong["code_verifier"] = "b".repeat(43).into();
    assert_eq!(
        exchange_request(&wrong).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let response = exchange_request(&exchange).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let tokens: Value = response.json().await.unwrap();
    assert_eq!(tokens["scope"], "read run");
    assert_eq!(
        exchange_request(&exchange).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let refresh = json!({ "grant_type": "refresh_token", "client_id": client["client_id"], "refresh_token": tokens["refresh_token"], "scope": "read" });
    let mut escalation = refresh.clone();
    escalation["scope"] = "read manage".into();
    assert_eq!(
        exchange_request(&escalation).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    let rotated: Value = exchange_request(&refresh)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rotated["scope"], "read");
    let call = |token: &Value| {
        app.client
            .post(format!("{}/mcp", app.url))
            .bearer_auth(token.as_str().unwrap())
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
    };
    assert_eq!(
        call(&rotated["access_token"])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        exchange_request(&refresh).send().await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    for token in [&rotated["access_token"], &tokens["access_token"]] {
        assert_eq!(
            call(token).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    relay.close().await;
}
