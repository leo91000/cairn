mod common;

use common::{Fixture, RelayedInstallation};
use serde_json::json;
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn fixture_client_stays_on_its_bound_loopback_address() {
    let app = Fixture::new().await;
    let port = reqwest::Url::parse(&app.url).unwrap().port().unwrap();
    let unrelated = tokio::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port))
        .await
        .unwrap();
    let unrelated_server = tokio::spawn(async move {
        let (mut connection, _) = unrelated.accept().await.unwrap();
        connection.read_exact(&mut [0]).await.unwrap();
        // Dropping the unrelated service with unread HTTP bytes resets the request.
    });

    let response = app
        .post(
            "/api/account/email-code",
            json!({ "email": "fixture@example.test" }),
        )
        .await;
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    assert_eq!(app.mail.0.lock().unwrap().len(), 1);

    unrelated_server.abort();
    app.close().await;
}

#[tokio::test]
async fn fixture_client_preserves_other_localhost_url_ports() {
    let app = Fixture::new().await;
    let mut other = Fixture::new().await;
    other.client = app.client.clone();

    let response = other
        .post(
            "/api/account/email-code",
            json!({ "email": "other-fixture@example.test" }),
        )
        .await;
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    assert!(app.mail.0.lock().unwrap().is_empty());
    assert_eq!(other.mail.0.lock().unwrap().len(), 1);

    other.close().await;
    app.close().await;
}

#[tokio::test]
async fn relay_fixture_keeps_internal_clients_on_its_bound_loopback_address() {
    let app = Fixture::new().await;
    let port = reqwest::Url::parse(&app.url).unwrap().port().unwrap();
    let unrelated = tokio::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, port))
        .await
        .unwrap();
    let unrelated_server = tokio::spawn(async move {
        let router = axum::Router::new().fallback(|| async { reqwest::StatusCode::IM_A_TEAPOT });
        axum::serve(unrelated, router).await.unwrap();
    });

    let relay = RelayedInstallation::with_app(axum::Router::new(), String::new(), app).await;
    let response = relay.get("/chats").send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    unrelated_server.abort();
    relay.close().await;
}
