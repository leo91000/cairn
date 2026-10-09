mod common;

use common::RelayedInstallation;
use futures_util::{SinkExt, StreamExt};
use cairn_protocol::{ApiResponse, Frame};
use reqwest::StatusCode;
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

async fn peer(
    relay: &RelayedInstallation,
    versions: Vec<u16>,
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
        .send(Message::Text(
            serde_json::to_string(&Frame::Hello { versions })
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    socket
}

#[tokio::test]
async fn incompatible_protocol_is_refused_and_v1_rejects_only_the_stream() {
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
    let mut incompatible = peer(&relay, vec![99]).await;
    let message = tokio::time::timeout(Duration::from_secs(1), incompatible.next())
        .await
        .unwrap();
    assert!(matches!(message, None | Some(Ok(Message::Close(_)))));
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
        StatusCode::SERVICE_UNAVAILABLE
    );

    let mut old = peer(&relay, vec![1]).await;
    let Some(Ok(Message::Text(welcome))) = old.next().await else {
        panic!("old peer must negotiate")
    };
    assert!(matches!(
        serde_json::from_str::<Frame>(&welcome).unwrap(),
        Frame::Welcome { version: 1 }
    ));
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
    assert_eq!(installations[0]["updateRequired"], false);
    assert_eq!(
        relay.get("/chats/stream").send().await.unwrap().status(),
        StatusCode::NOT_IMPLEMENTED
    );
    let finite = tokio::spawn(relay.get("/chats").send());
    let request = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match old.next().await.unwrap().unwrap() {
                Message::Ping(bytes) => old.send(Message::Pong(bytes)).await.unwrap(),
                Message::Text(text) => {
                    let Frame::Request(request) = serde_json::from_str(&text).unwrap() else {
                        panic!("v1 must never receive stream frames")
                    };
                    break request;
                }
                _ => panic!("unexpected v1 frame"),
            }
        }
    })
    .await
    .unwrap();
    old.send(Message::Text(
        serde_json::to_string(&Frame::Response(ApiResponse {
            id: request.id,
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: b"[]".to_vec(),
        }))
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
    let response = finite.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "[]");
    old.close(None).await.unwrap();
    relay.close().await;
}

#[tokio::test]
async fn a_new_installation_can_still_claim_against_a_v1_official_service() {
    use axum::{Json, Router, response::IntoResponse, routing::post};
    use serde_json::{Value, json};
    let app = Router::new().route(
        "/api/relay/claim",
        post(|Json(input): Json<Value>| async move {
            if input["protocol"] != 1 {
                return axum::http::StatusCode::CONFLICT.into_response();
            }
            (
                axum::http::StatusCode::CREATED,
                Json(json!({
                    "installationId": uuid::Uuid::new_v4().to_string(),
                    "token": "fixture-only",
                })),
            )
                .into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let result = cairn_installation::relay::claim(
        &url,
        &root.path().join("relay"),
        "fixture-code",
        "Compatible installation",
    )
    .await;
    server.abort();
    assert!(
        result.is_ok(),
        "claim should declare the minimum supported protocol; Hello negotiates the actual version"
    );
}
