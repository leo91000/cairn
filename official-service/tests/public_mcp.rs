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

#[tokio::test]
async fn an_external_mcp_connection_returns_through_the_official_installation_and_account() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let provider_script = "import { mcpProvider } from './tests/mcp-provider.ts'; const p = await mcpProvider(); console.log(p.origin); await new Promise(() => {});";
    let mut provider = tokio::process::Command::new("node")
        .args([
            "--import",
            "tsx",
            "--input-type=module",
            "-e",
            provider_script,
        ])
        .current_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap(),
        )
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let origin = BufReader::new(provider.stdout.take().unwrap())
        .lines()
        .next_line()
        .await
        .unwrap()
        .unwrap();
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let mutate = |path: &str, body: Value| {
        relay
            .app
            .client
            .post(format!("{}{path}", relay.base))
            .header("cookie", &relay.cookie)
            .header("origin", &relay.app.url)
            .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
            .json(&body)
    };
    let saved: Value = mutate("/mcps", json!({ "name": "External", "url": format!("{origin}/mcp"), "auth": "oauth", "allowPrivateNetwork": true }))
        .send().await.unwrap().json().await.unwrap();
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    assert_eq!(
        saved["callbackUrl"],
        format!(
            "{}/installations/{installation}/mcps/callback",
            relay.app.url
        )
    );
    let id = saved["id"].as_str().unwrap();
    let consent: Value = mutate(&format!("/mcps/{id}/connect"), json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = http
        .get(consent["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    let callback = url::Url::parse(response.headers()["location"].to_str().unwrap()).unwrap();
    let parameters: std::collections::HashMap<String, String> = callback
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let completed = mutate("/mcps/oauth/callback", json!(parameters))
        .send()
        .await
        .unwrap();
    assert_eq!(completed.status(), StatusCode::OK);
    assert_eq!(
        completed.json::<Value>().await.unwrap()["result"],
        "connected"
    );
    let replay = mutate("/mcps/oauth/callback", json!(parameters))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::NOT_FOUND);
    let tested = mutate(&format!("/mcps/{id}/test"), json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(tested.status(), StatusCode::OK);
    assert_eq!(tested.json::<Value>().await.unwrap()["state"], "connected");
    provider.kill().await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn detaching_and_reclaiming_an_installation_permanently_revokes_its_mcp_grants() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let minted: Value = owner_post(
        &relay,
        &format!("/api/installations/{id}/tokens"),
        json!({ "label": "Before detachment", "scopes": ["read"] }),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let identity: Value = serde_json::from_slice(
        &std::fs::read(
            relay
                .installation
                .config
                .data_dir
                .join("installation-relay/identity.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let detached = owner_post(
        &relay,
        &format!("/api/installations/{id}/detach"),
        json!({}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(detached.status(), StatusCode::NO_CONTENT);

    let start = relay
        .app
        .post(
            "/api/relay/device-claim/start",
            json!({
                "name": "Reclaimed machine",
                "protocol": 1,
                "identity": { "installationId": id, "token": identity["token"] },
            }),
        )
        .await;
    assert_eq!(start.status(), StatusCode::CREATED);
    let device: Value = start.json().await.unwrap();
    let preview: Value = owner_post(
        &relay,
        "/api/installations/device-claim/preview",
        json!({ "code": device["userCode"] }),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let approved = owner_post(
        &relay,
        "/api/installations/device-claim",
        json!({
            "code": device["userCode"], "confirmation": preview["confirmation"],
        }),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(approved.status(), StatusCode::OK);
    let reclaimed = relay
        .app
        .post(
            "/api/relay/device-claim/poll",
            json!({ "deviceCode": device["deviceCode"] }),
        )
        .await;
    assert_eq!(reclaimed.status(), StatusCode::OK);

    let response = relay
        .app
        .client
        .post(format!("{}/mcp", relay.app.url))
        .bearer_auth(minted["token"].as_str().unwrap())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let grants: Value = relay
        .app
        .client
        .get(format!("{}/api/installations/{id}/tokens", relay.app.url))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(grants, json!([]));
    relay.close().await;
}

#[tokio::test]
async fn dynamic_registration_has_no_permanent_global_client_ceiling() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    for index in 0..101u8 {
        // Distinct callers stay within the per-IP limiter while exercising the
        // central registration contract beyond the former installation cap.
        let client = reqwest::Client::builder()
            .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                127,
                0,
                0,
                index / 10 + 1,
            )))
            .build()
            .unwrap();
        let response = client.post(format!("{}/oauth/register", relay.app.url))
            .json(&json!({ "client_name": "Public client", "redirect_uris": ["http://localhost:9999/callback"] }))
            .send().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "registration {index}"
        );
    }
    relay.close().await;
}
