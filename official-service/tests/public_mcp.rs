mod common;

use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn personal_mcp_token_enforces_scopes_and_revocation() {
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
        .json(&json!({
            "label": "Read client",
            "scopes": ["read"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let created: Value = response.json().await.unwrap();
    let token = created["token"].as_str().unwrap();
    let call = |name: &str, args: Value| {
        app.client
            .post(format!("{}/mcp", app.url))
            .bearer_auth(token)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": name,
                    "arguments": args,
                },
            }))
    };

    let response = call("list_agents", json!({})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let result: Value = response.json().await.unwrap();
    assert!(
        result["result"]["structuredContent"]["result"].is_array(),
        "{result}"
    );

    let denied: Value = call(
        "save_project",
        json!({
            "name": "Denied",
            "path": "/tmp",
        }),
    )
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

    let response = app
        .client
        .post(format!("{}/oauth/register", app.url))
        .json(&json!({
            "client_name": "OAuth client",
            "redirect_uris": ["http://localhost:9999/callback"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .unwrap();
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
        preview["client"]["redirect_uri"],
        "http://localhost:9999/callback"
    );
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

    // An old registration with a pending code must survive maintenance.
    sqlx_core::query::query("UPDATE mcp_clients SET created_at = now() - interval '31 days'")
        .execute(&app.pool)
        .await
        .unwrap();
    leo_official_service::cleanup_expired(&app.pool)
        .await
        .unwrap();

    let response = exchange_request(&exchange).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let tokens: Value = response.json().await.unwrap();
    assert_eq!(tokens["scope"], "read run");

    let mut wrong_client = exchange.clone();
    wrong_client["client_id"] = "another-client".into();
    assert_eq!(
        exchange_request(&wrong_client)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    let refresh = json!({
        "grant_type": "refresh_token",
        "client_id": client["client_id"],
        "refresh_token": tokens["refresh_token"],
        "scope": "read",
    });
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
    // Preserve active clients and used refresh proofs during rotation.
    leo_official_service::cleanup_expired(&app.pool)
        .await
        .unwrap();

    let call = |token: &Value| {
        app.client
            .post(format!("{}/mcp", app.url))
            .bearer_auth(token.as_str().unwrap())
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
            }))
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
    let saved: Value = mutate(
        "/mcps",
        json!({
            "name": "External",
            "url": format!("{origin}/mcp"),
            "auth": "oauth",
            "allowPrivateNetwork": true,
        }),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
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
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }))
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
        let response = client
            .post(format!("{}/oauth/register", relay.app.url))
            .json(&json!({
                "client_name": "Public client",
                "redirect_uris": ["http://localhost:9999/callback"],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "registration {index}"
        );
    }
    relay.close().await;
}

fn stalled_mcp_upload(
    relay: &RelayedInstallation,
    token: &str,
) -> (
    tokio::sync::mpsc::Sender<String>,
    tokio::task::JoinHandle<reqwest::Response>,
) {
    let (sender, receiver) = tokio::sync::mpsc::channel::<String>(2);
    let body = reqwest::Body::wrap_stream(futures_util::stream::unfold(
        receiver,
        |mut receiver| async move {
            receiver
                .recv()
                .await
                .map(|chunk| (Ok::<_, std::io::Error>(chunk), receiver))
        },
    ));
    sender.try_send("{".into()).unwrap();
    let request = relay
        .app
        .client
        .post(format!("{}/mcp", relay.app.url))
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(body);
    (
        sender,
        tokio::spawn(async move { request.send().await.unwrap() }),
    )
}

#[tokio::test]
async fn slow_mcp_uploads_have_a_body_deadline_and_release_their_admission() {
    use std::time::Duration;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let minted: Value = owner_post(
        &relay,
        &format!("/api/installations/{id}/tokens"),
        json!({ "label": "Stalled client", "scopes": ["read"] }),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let (_sender, response) = stalled_mcp_upload(&relay, minted["token"].as_str().unwrap());
    let response = tokio::time::timeout(Duration::from_secs(15), response)
        .await
        .expect("a slow MCP body must expire")
        .unwrap();
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(
        relay.get("/projects").send().await.unwrap().status(),
        StatusCode::OK
    );

    let response = relay
        .app
        .client
        .post(format!("{}/mcp", relay.app.url))
        .bearer_auth(minted["token"].as_str().unwrap())
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    relay.close().await;
}

#[tokio::test]
async fn slow_mcp_uploads_cannot_block_owner_access_even_after_revocation() {
    use std::time::Duration;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let tokens_path = format!("/api/installations/{id}/tokens");
    let minted: Value = owner_post(
        &relay,
        &tokens_path,
        json!({ "label": "Revoked upload", "scopes": ["read", "manage"] }),
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    let start_upload = || stalled_mcp_upload(&relay, minted["token"].as_str().unwrap());
    // A rejected upload proves that the authenticated uploads reached admission.
    // Keep 32 stalled bodies open without allowing them to consume owner capacity.
    let mut uploads: Vec<_> = (0..=leo_relay_protocol::MAX_IN_FLIGHT)
        .map(|_| start_upload())
        .collect();
    let rejected = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(index) = uploads.iter().position(|upload| upload.1.is_finished()) {
                let (_, response) = uploads.swap_remove(index);
                break response.await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("excess MCP uploads must be rejected without waiting for their bodies");
    assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        relay.get("/projects").send().await.unwrap().status(),
        StatusCode::OK
    );

    let revoked = relay
        .app
        .client
        .delete(format!(
            "{}{tokens_path}/{}",
            relay.app.url,
            minted["id"].as_str().unwrap()
        ))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);

    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "save_project",
            "arguments": {
                "name": "Rejected after revocation",
                "path": relay.root.path().to_str().unwrap(),
            },
        },
    })
    .to_string();
    assert_eq!(
        relay.get("/projects").send().await.unwrap().status(),
        StatusCode::OK
    );

    let mut revoked_uploads = 0;
    for (sender, response) in uploads {
        // Excess uploads can already have returned 429 and closed their bodies.
        let _ = sender.send(payload[1..].into()).await;
        drop(sender);
        match response.await.unwrap().status() {
            StatusCode::UNAUTHORIZED => revoked_uploads += 1,
            StatusCode::TOO_MANY_REQUESTS => {}
            status => panic!("unexpected upload response: {status}"),
        }
    }
    assert!(
        revoked_uploads > 0,
        "admitted uploads must be rejected after revocation"
    );

    let projects: Value = relay
        .get("/projects")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(projects, json!([]));
    relay.close().await;
}

async fn oauth_parameters(relay: &RelayedInstallation, redirect: &str) -> Value {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};

    let response = relay
        .app
        .client
        .post(format!("{}/oauth/register", relay.app.url))
        .json(&json!({
            "client_name": "Native client",
            "redirect_uris": [redirect],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let client: Value = response.json().await.unwrap();
    json!({
        "client_id": client["client_id"],
        "redirect_uri": redirect,
        "response_type": "code",
        "code_challenge_method": "S256",
        "code_challenge": URL_SAFE_NO_PAD.encode(Sha256::digest("a".repeat(43).as_bytes())),
        "scope": "read",
    })
}

async fn approved_code_request(relay: &RelayedInstallation, parameters: &Value) -> Value {
    let response = owner_post(
        relay,
        "/api/mcp/oauth/consent",
        json!({
            "parameters": parameters,
            "installationId": relay.session["installations"][0]["id"],
            "approved": true,
        }),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let consent: Value = response.json().await.unwrap();
    let redirect = url::Url::parse(consent["redirect"].as_str().unwrap()).unwrap();
    let code = redirect
        .query_pairs()
        .find(|(name, _)| name == "code")
        .unwrap()
        .1
        .into_owned();
    json!({
        "grant_type": "authorization_code",
        "client_id": parameters["client_id"],
        "redirect_uri": parameters["redirect_uri"],
        "code": code,
        "code_verifier": "a".repeat(43),
    })
}

#[tokio::test]
async fn native_loopback_ports_vary_but_other_redirects_and_code_bindings_stay_exact() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let parameters = oauth_parameters(&relay, "https://client.example/callback").await;
    for redirect in [
        "https://client.example/callback/extra",
        "https://client.example/callback-evil",
        "https://client.example.evil/callback",
        "https://client.example/callback?extra=1",
        "http://client.example/callback",
        "https://client.example:8443/callback",
    ] {
        let mut wrong = parameters.clone();
        wrong["redirect_uri"] = redirect.into();
        assert_eq!(
            owner_post(&relay, "/api/mcp/oauth/preview", wrong)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST,
            "{redirect}"
        );
    }
    let insecure = relay
        .app
        .client
        .post(format!("{}/oauth/register", relay.app.url))
        .json(&json!({ "redirect_uris": ["http://external.example/callback"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(insecure.status(), StatusCode::BAD_REQUEST);

    for host in ["127.0.0.1", "[::1]"] {
        let registered = format!("http://{host}:9999/callback");
        let mut parameters = oauth_parameters(&relay, &registered).await;
        parameters["redirect_uri"] = format!("http://{host}:54321/callback").into();
        let preview = owner_post(&relay, "/api/mcp/oauth/preview", parameters.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(
            preview.status(),
            StatusCode::OK,
            "Native loopback ports may vary"
        );
        assert_eq!(
            preview.json::<Value>().await.unwrap()["client"]["redirect_uri"],
            parameters["redirect_uri"]
        );
        let mut wrong_path = parameters.clone();
        wrong_path["redirect_uri"] = format!("http://{host}:54321/callback/extra").into();
        assert_eq!(
            owner_post(&relay, "/api/mcp/oauth/preview", wrong_path)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        let exchange = approved_code_request(&relay, &parameters).await;
        for (key, value) in [
            ("client_id", "another-client"),
            ("redirect_uri", registered.as_str()),
        ] {
            let mut wrong = exchange.clone();
            wrong[key] = value.into();
            let response = relay
                .app
                .client
                .post(format!("{}/oauth/token", relay.app.url))
                .form(&wrong)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"],
                "invalid_grant"
            );
        }
        let response = relay
            .app
            .client
            .post(format!("{}/oauth/token", relay.app.url))
            .form(&exchange)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let tokens: Value = response.json().await.unwrap();
        let tools = relay
            .app
            .client
            .post(format!("{}/mcp", relay.app.url))
            .bearer_auth(tokens["access_token"].as_str().unwrap())
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(tools.status(), StatusCode::OK);
    }
    relay.close().await;
}

#[tokio::test]
async fn replaying_an_authorization_code_revokes_its_access_and_rotated_refresh_family() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let parameters = oauth_parameters(&relay, "https://client.example/callback").await;
    sqlx_core::query::query("UPDATE mcp_clients SET created_at = now() - interval '31 days'")
        .execute(&relay.app.pool)
        .await
        .unwrap();
    let exchange = approved_code_request(&relay, &parameters).await;
    let endpoint = format!("{}/oauth/token", relay.app.url);
    let response = relay
        .app
        .client
        .post(&endpoint)
        .form(&exchange)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let original: Value = response.json().await.unwrap();
    let response = relay
        .app
        .client
        .post(&endpoint)
        .form(&json!({
            "grant_type": "refresh_token",
            "client_id": parameters["client_id"],
            "refresh_token": original["refresh_token"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let rotated: Value = response.json().await.unwrap();
    // Expired code tombstones must still detect a replay while their grant is
    // alive. This changes test time at its storage seam, not the verdict.
    sqlx_core::query::query("UPDATE mcp_codes SET expires_at = now() - interval '1 second'")
        .execute(&relay.app.pool)
        .await
        .unwrap();
    // Hourly maintenance must retain expired consumed code tombstones as well.
    leo_official_service::cleanup_expired(&relay.app.pool)
        .await
        .unwrap();
    // Another consent runs expiry cleanup without erasing a live grant's proof.
    let _pending = approved_code_request(&relay, &parameters).await;
    let replay = relay
        .app
        .client
        .post(&endpoint)
        .form(&exchange)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        replay.json::<Value>().await.unwrap()["error"],
        "invalid_grant"
    );
    for tokens in [&original, &rotated] {
        let response = relay
            .app
            .client
            .post(format!("{}/mcp", relay.app.url))
            .bearer_auth(tokens["access_token"].as_str().unwrap())
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "Code replay must revoke every token it issued"
        );
    }

    let refresh = relay
        .app
        .client
        .post(&endpoint)
        .form(&json!({
            "grant_type": "refresh_token",
            "client_id": parameters["client_id"],
            "refresh_token": rotated["refresh_token"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(refresh.status(), StatusCode::BAD_REQUEST);
    relay.close().await;
}

#[tokio::test]
async fn an_expired_unused_authorization_code_cannot_issue_tokens() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let parameters = oauth_parameters(&relay, "https://client.example/callback").await;
    let exchange = approved_code_request(&relay, &parameters).await;
    sqlx_core::query::query("UPDATE mcp_codes SET expires_at = now() - interval '1 second'")
        .execute(&relay.app.pool)
        .await
        .unwrap();
    let response = relay
        .app
        .client
        .post(format!("{}/oauth/token", relay.app.url))
        .form(&exchange)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"],
        "invalid_grant"
    );

    let id = relay.session["installations"][0]["id"].as_str().unwrap();
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
async fn a_member_cannot_consent_to_mcp_or_create_a_personal_token_for_the_shared_installation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let invitation = owner_post(
        &relay,
        &format!("/api/installations/{id}/sharing/invitations"),
        json!({
            "email": "mcp-member@example.test",
        }),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(invitation.status(), StatusCode::CREATED);

    let invitation: Value = invitation.json().await.unwrap();
    let (cookie, session) = common::login(&relay.app, "mcp-member@example.test").await;
    let member_post = |path: &str, value: Value| {
        relay
            .app
            .client
            .post(format!("{}{path}", relay.app.url))
            .header("cookie", &cookie)
            .header("origin", &relay.app.url)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&value)
    };
    let accepted = member_post(
        &format!(
            "/api/account/invitations/{}/accept",
            invitation["id"].as_str().unwrap()
        ),
        json!({}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(accepted.status(), StatusCode::NO_CONTENT);

    let parameters = oauth_parameters(&relay, "https://client.example/callback").await;
    let preview = member_post("/api/mcp/oauth/preview", parameters.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(preview.status(), StatusCode::OK);
    assert_eq!(
        preview.json::<Value>().await.unwrap()["installations"],
        json!([])
    );

    let forced = member_post(
        "/api/mcp/oauth/consent",
        json!({
            "parameters": parameters,
            "installationId": id,
            "approved": true,
        }),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(forced.status(), StatusCode::NOT_FOUND);

    let token = member_post(
        &format!("/api/installations/{id}/tokens"),
        json!({
            "label": "Forced member token",
            "scopes": ["read", "run", "manage"],
        }),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(token.status(), StatusCode::NOT_FOUND);

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
async fn maintenance_forgets_old_unused_clients_but_preserves_recent_registrations() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let old = oauth_parameters(&relay, "https://old.example/callback").await;
    let recent = oauth_parameters(&relay, "https://recent.example/callback").await;
    sqlx_core::query::query(
        "UPDATE mcp_clients SET created_at = now() - interval '31 days' WHERE id = $1",
    )
    .bind(old["client_id"].as_str().unwrap())
    .execute(&relay.app.pool)
    .await
    .unwrap();

    leo_official_service::cleanup_expired(&relay.app.pool)
        .await
        .unwrap();
    for (parameters, expected) in [(&old, StatusCode::BAD_REQUEST), (&recent, StatusCode::OK)] {
        let preview = owner_post(&relay, "/api/mcp/oauth/preview", parameters.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(preview.status(), expected);
    }
    // Fresh registrations still complete the same PKCE flow through the relay.
    let exchange = approved_code_request(&relay, &recent).await;
    let response = relay
        .app
        .client
        .post(format!("{}/oauth/token", relay.app.url))
        .form(&exchange)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let tokens: Value = response.json().await.unwrap();
    let response = relay
        .app
        .client
        .post(format!("{}/mcp", relay.app.url))
        .bearer_auth(tokens["access_token"].as_str().unwrap())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    relay.close().await;
}

#[tokio::test]
async fn consent_returns_a_client_error_if_maintenance_forgets_it_while_waiting_for_owner() {
    use sqlx_core::{query::query, query_as::query_as};
    use std::time::Duration;

    let relay = RelayedInstallation::with_pool_size(axum::Router::new(), 8).await;
    let parameters = oauth_parameters(&relay, "https://old.example/callback").await;
    query("UPDATE mcp_clients SET created_at = now() - interval '31 days'")
        .execute(&relay.app.pool)
        .await
        .unwrap();

    let mut owner_lock = relay.app.pool.begin().await.unwrap();
    let (pid,): (i32,) = query_as("SELECT pg_backend_pid()")
        .fetch_one(&mut *owner_lock)
        .await
        .unwrap();
    query("SELECT id FROM leo_accounts WHERE id = $1 FOR NO KEY UPDATE")
        .bind(relay.session["account"]["id"].as_str().unwrap())
        .execute(&mut *owner_lock)
        .await
        .unwrap();
    let request = owner_post(
        &relay,
        "/api/mcp/oauth/consent",
        json!({
            "parameters": parameters,
            "installationId": relay.session["installations"][0]["id"],
            "approved": true,
        }),
    );
    let consent = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (blocked,): (bool,) = query_as("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))")
                .bind(pid).fetch_one(&relay.app.pool).await.unwrap();
            if blocked { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("consent must reach the owner lock after reading client metadata");

    leo_official_service::cleanup_expired(&relay.app.pool)
        .await
        .unwrap();
    owner_lock.commit().await.unwrap();
    assert_eq!(consent.await.unwrap().status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn expired_grants_and_unused_codes_release_old_registrations_without_touching_the_installation()
 {
    use sqlx_core::query::query;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let used = oauth_parameters(&relay, "https://used.example/callback").await;
    let exchange = approved_code_request(&relay, &used).await;
    let response = relay
        .app
        .client
        .post(format!("{}/oauth/token", relay.app.url))
        .form(&exchange)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let tokens: Value = response.json().await.unwrap();
    let unused = oauth_parameters(&relay, "https://unused.example/callback").await;
    let _pending = approved_code_request(&relay, &unused).await;
    query("UPDATE mcp_clients SET created_at = now() - interval '31 days'")
        .execute(&relay.app.pool)
        .await
        .unwrap();
    query("UPDATE mcp_grants SET expires_at = now() - interval '1 second'")
        .execute(&relay.app.pool)
        .await
        .unwrap();
    query("UPDATE mcp_codes SET expires_at = now() - interval '1 second' WHERE grant_id IS NULL")
        .execute(&relay.app.pool)
        .await
        .unwrap();

    leo_official_service::cleanup_expired(&relay.app.pool)
        .await
        .unwrap();
    for parameters in [&used, &unused] {
        assert_eq!(
            owner_post(&relay, "/api/mcp/oauth/preview", parameters.clone())
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let response = relay
        .app
        .client
        .post(format!("{}/mcp", relay.app.url))
        .bearer_auth(tokens["access_token"].as_str().unwrap())
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = relay
        .app
        .client
        .post(format!("{}/oauth/token", relay.app.url))
        .form(&json!({
            "grant_type": "refresh_token",
            "client_id": used["client_id"],
            "refresh_token": tokens["refresh_token"],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn maintenance_skips_a_busy_old_client_and_retries_after_it_is_released() {
    use sqlx_core::query::query;
    use std::time::Duration;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let parameters = oauth_parameters(&relay, "https://busy.example/callback").await;
    query("UPDATE mcp_clients SET created_at = now() - interval '31 days'")
        .execute(&relay.app.pool)
        .await
        .unwrap();
    let mut client_lock = relay.app.pool.begin().await.unwrap();
    query("SELECT id FROM mcp_clients WHERE id = $1 FOR KEY SHARE")
        .bind(parameters["client_id"].as_str().unwrap())
        .execute(&mut *client_lock)
        .await
        .unwrap();

    tokio::time::timeout(
        Duration::from_secs(2),
        leo_official_service::cleanup_expired(&relay.app.pool),
    )
    .await
    .expect("a busy registration must not stall maintenance")
    .unwrap();
    assert_eq!(
        owner_post(&relay, "/api/mcp/oauth/preview", parameters.clone())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    client_lock.rollback().await.unwrap();

    leo_official_service::cleanup_expired(&relay.app.pool)
        .await
        .unwrap();
    assert_eq!(
        owner_post(&relay, "/api/mcp/oauth/preview", parameters)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    relay.close().await;
}
