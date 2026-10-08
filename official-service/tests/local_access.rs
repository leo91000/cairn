mod common;

use common::RelayedInstallation;
use leo_agent_manager::{auth::digest, config::now};
use reqwest::StatusCode;
use serde_json::json;

#[tokio::test]
async fn installation_browser_routes_refuse_local_credentials_and_forged_identity_headers() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    // Persist the old record shapes without preserving their credential issuer.
    // The hash is the Node scrypt fixture for legacy-password-long-enough / legacy-salt.
    let session = "old-local-session";
    let personal = "old-local-personal-token";
    let csrf = "old-local-csrf";
    let expires = now() + 600_000;
    let grant = json!({
        "clientId": "personal",
        "resource": format!("{}/mcp", relay.installation.config.public_url),
        "scopes": ["read", "manage"],
        "label": "Legacy",
        "family": "old-local-grant",
        "expiresAt": expires,
        "used": false,
    });
    for (key, value, expiry) in [
        (
            "admin".to_owned(),
            json!({
                "salt": "legacy-salt",
                "hash": "f135651144674b54d6faf9dc217c43ae1a296eeba56adda5fc4032b5c8bf95fc7407c283bb5f865d21dbb97eea03f46ab3a13c5520065ebc8955ffc5a72b4c8b",
            }),
            None,
        ),
        (
            format!("session:{}", digest(session)),
            json!({
                "csrf": csrf,
                "createdAt": now(),
            }),
            Some(expires),
        ),
        (
            format!("access:{}", digest(personal)),
            grant.clone(),
            Some(expires),
        ),
        ("grant:old-local-grant".to_owned(), grant, Some(expires)),
    ] {
        relay
            .installation
            .store
            .set(&key, value, expiry)
            .await
            .unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = format!("http://{}", listener.local_addr().unwrap());
    let router = leo_agent_manager::http::router(relay.installation.clone())
        .await
        .unwrap();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    for (method, path) in [
        (reqwest::Method::GET, "/api/chats"),
        (reqwest::Method::GET, "/"),
        (reqwest::Method::GET, "/api/session"),
        (reqwest::Method::POST, "/api/setup"),
        (reqwest::Method::POST, "/api/login"),
        (reqwest::Method::GET, "/api/chats/stream"),
        (reqwest::Method::GET, "/api/public/artifacts/fixture"),
        (
            reqwest::Method::GET,
            "/.well-known/oauth-authorization-server",
        ),
        (reqwest::Method::POST, "/oauth/register"),
        (reqwest::Method::POST, "/mcp"),
    ] {
        let response = relay
            .app
            .client
            .request(method, format!("{local}{path}"))
            .header("cookie", format!("leo_session={session}"))
            .header("x-csrf-token", csrf)
            .header("authorization", format!("Bearer {personal}"))
            .header("x-leo-role", "owner")
            .header("x-leo-account-id", "fixture-owner")
            .json(&json!({
                "password": "legacy-password-long-enough",
                "setupToken": "old-local-setup-token",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    // Readiness and machine channels do not depend on a browser account.
    assert_eq!(
        relay
            .app
            .client
            .get(format!("{local}/health"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    server.abort();
    relay.close().await;
}
