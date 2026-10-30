mod common;

use common::RelayedInstallation;
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

async fn peer(
    relay: &RelayedInstallation,
    hello: Value,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let identity: serde_json::Value = serde_json::from_slice(
        &tokio::fs::read(
            relay
                .installation
                .config
                .data_dir
                .join("installation-relay/identity.json"),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let url = format!(
        "{}/api/relay/{}/connect",
        relay.app.url.replace("http:", "ws:"),
        identity["installationId"].as_str().unwrap()
    );
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", identity["token"].as_str().unwrap())
            .parse()
            .unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
        .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
        .await
        .unwrap();
    socket
}

#[tokio::test]
async fn incompatible_protocol_requires_update() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    relay.stop.cancel();
    tokio::time::timeout(Duration::from_secs(3), async {
        while relay.get("/chats").send().await.unwrap().status() != StatusCode::SERVICE_UNAVAILABLE
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    for hello in [
        json!({ "type": "hello", "version": 1 }),
        json!({ "type": "hello", "version": 4 }),
        json!({ "type": "hello", "version": 99 }),
        json!({ "type": "hello", "versions": [5, 4, 3, 2, 1] }),
    ] {
        let mut incompatible = peer(&relay, hello).await;
        let message = tokio::time::timeout(Duration::from_secs(1), incompatible.next())
            .await
            .unwrap();
        let Some(Ok(Message::Close(Some(close)))) = message else {
            panic!("expected protocol refusal")
        };
        assert_eq!(
            close.code,
            tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Protocol
        );
        assert!(close.reason.contains("Update required"));
    }
    let installations: serde_json::Value = relay
        .app
        .client
        .get(format!("{}/api/installations", relay.app.url))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(installations[0]["updateRequired"], true);
    assert_eq!(installations[0]["online"], false);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::UPGRADE_REQUIRED
    );

    relay.close().await;
}

#[tokio::test]
async fn refusing_an_outdated_second_peer_keeps_the_current_tunnel_available() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let mut outdated = peer(&relay, json!({ "type": "hello", "version": 4 })).await;
    let Some(Ok(Message::Close(Some(close)))) = outdated.next().await else {
        panic!("expected protocol refusal")
    };
    assert!(close.reason.contains("Update required"));
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    let installations: Value = relay
        .app
        .client
        .get(format!("{}/api/installations", relay.app.url))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(installations[0]["updateRequired"], false);
    assert_eq!(installations[0]["online"], true);
    relay.close().await;
}
