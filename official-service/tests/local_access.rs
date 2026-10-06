mod common;
#[path = "../../backend/examples/support/legacy_auth.rs"]
mod legacy_auth;

use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::json;

#[tokio::test]
async fn installation_browser_routes_refuse_local_credentials_and_forged_identity_headers() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let legacy = legacy_auth::Auth::new(
        relay.installation.store.clone(),
        relay.installation.config.public_url.clone(),
    );
    // Simulate credentials left by an installation upgraded from local access.
    legacy.setup("legacy-password-long-enough").await.unwrap();
    let session = legacy.session().await.unwrap();
    let personal = legacy
        .personal("Legacy", vec!["read", "manage"])
        .await
        .unwrap();
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
            .header(
                "cookie",
                format!("leo_session={}", session["value"].as_str().unwrap()),
            )
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .header(
                "authorization",
                format!("Bearer {}", personal["token"].as_str().unwrap()),
            )
            .header("x-leo-role", "owner")
            .header("x-leo-account-id", "fixture-owner")
            .json(&json!({}))
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
