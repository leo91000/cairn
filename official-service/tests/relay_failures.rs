mod common;

use axum::{Router, body::Body, routing::get};
use common::RelayedInstallation;
use reqwest::StatusCode;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[tokio::test]
async fn an_oversized_response_fails_only_its_request() {
    let routes = Router::new().route(
        "/api/fixture/oversized",
        get(|| async { Body::from(vec![0_u8; leo_relay_protocol::MAX_BODY + 1]) }),
    );
    let relay = RelayedInstallation::new(routes).await;
    let response = relay.get("/fixture/oversized").send().await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn a_panicking_handler_leaves_another_in_flight_request_and_the_tunnel_alive() {
    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let routes = Router::new()
        .route(
            "/api/fixture/panic",
            get(|| async {
                panic!("fixture handler failure");
                #[allow(unreachable_code)]
                "unreachable"
            }),
        )
        .route(
            "/api/fixture/held",
            get({
                let started = started.clone();
                let release = release.clone();
                move || {
                    let started = started.clone();
                    let release = release.clone();
                    async move {
                        started.add_permits(1);
                        release.acquire().await.unwrap().forget();
                        "completed"
                    }
                }
            }),
        );
    let relay = RelayedInstallation::new(routes).await;
    let held = tokio::spawn(relay.get("/fixture/held").send());
    tokio::time::timeout(Duration::from_secs(5), started.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    assert_eq!(
        relay.get("/fixture/panic").send().await.unwrap().status(),
        StatusCode::BAD_GATEWAY
    );
    release.add_permits(1);
    let response = held.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "completed");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}
