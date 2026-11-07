mod common;

use axum::{Router, body::Body, routing::get};
use common::RelayedInstallation;
use reqwest::StatusCode;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[tokio::test]
async fn a_full_size_attachment_streams_without_the_buffered_response_limit() {
    let relay = RelayedInstallation::new(Router::new()).await;
    let chat = relay
        .installation
        .chat_create(serde_json::json!({}))
        .await
        .unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let bytes = vec![b'z'; cairn_installation::attachments::MAX_FILE];
    relay
        .installation
        .store_attachment(chat_id, &id, "large.bin", &bytes)
        .await
        .unwrap();
    let response = relay
        .get(&format!("/chats/{chat_id}/attachments/{id}/stream"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert_eq!(response.bytes().await.unwrap().as_ref(), bytes);
    relay.close().await;
}

#[tokio::test]
async fn a_stalled_file_without_a_length_ends_as_an_error_not_a_complete_body() {
    let routes = Router::new().route(
        "/api/runs/{run}/artifacts/{artifact}",
        get(|| async {
            let partial =
                futures_util::stream::once(async { Ok::<_, std::io::Error>(b"partial".to_vec()) });
            Body::from_stream(futures_util::StreamExt::chain(
                partial,
                futures_util::stream::pending(),
            ))
        }),
    );
    let relay = RelayedInstallation::new(routes).await;
    let run = uuid::Uuid::new_v4();
    let response = relay
        .get(&format!("/runs/{run}/artifacts/{}", uuid::Uuid::new_v4()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("content-length"));

    // The installation stops making progress, so the Beacon's file deadline ends
    // the transfer. Without a length, only an error can mark the body incomplete.
    let body = tokio::time::timeout(Duration::from_secs(45), response.bytes())
        .await
        .expect("The file deadline must end the transfer");
    assert!(body.is_err(), "A truncated file must not look complete");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn private_files_queue_within_the_request_deadline_behind_progressing_transfers() {
    // A file of unknown length streams; this one makes progress forever.
    let routes = Router::new().route(
        "/api/runs/{run}/artifacts/{artifact}",
        get(|| async {
            let progress = futures_util::stream::unfold((), |()| async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Some((Ok::<_, std::io::Error>(b"progress".to_vec()), ()))
            });
            Body::from_stream(progress)
        }),
    );
    let relay = RelayedInstallation::new(routes).await;
    let file = format!(
        "/runs/{}/artifacts/{}",
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    );
    let read = |mut response: reqwest::Response| {
        tokio::spawn(async move { while let Ok(Some(_)) = response.chunk().await {} })
    };

    // Fill the account's four concurrent file transfers on this installation.
    let mut transfers = Vec::new();
    for _ in 0..4 {
        let response = relay.get(&file).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        transfers.push(read(response));
    }

    // The next file queues for the account's share and is admitted when one ends.
    let mut next = tokio::spawn(relay.get(&file).send());
    assert!(
        tokio::time::timeout(Duration::from_secs(2), &mut next)
            .await
            .is_err()
    );
    transfers.pop().unwrap().abort();
    let admitted = tokio::time::timeout(Duration::from_secs(10), next)
        .await
        .expect("A finished transfer must admit the queued file")
        .unwrap()
        .unwrap();
    assert_eq!(admitted.status(), StatusCode::OK);
    transfers.push(read(admitted));

    // Progressing transfers are never cut, so the queue answers within its deadline.
    let started = tokio::time::Instant::now();
    let response = tokio::time::timeout(Duration::from_secs(60), relay.get(&file).send())
        .await
        .expect("A queued file must be answered within the request deadline")
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(started.elapsed() >= Duration::from_secs(25));
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );

    for transfer in transfers {
        transfer.abort();
    }
    relay.close().await;
}

#[tokio::test]
async fn an_error_on_a_file_route_stays_finite_without_a_length() {
    // Compression removes an error body's Content-Length, as this body has none.
    let routes = Router::new().route(
        "/api/runs/{run}/artifacts/{artifact}",
        get(|| async {
            let body = futures_util::stream::once(async {
                Ok::<_, std::io::Error>(
                    br#"{"error":"This file is no longer available."}"#.to_vec(),
                )
            });
            (StatusCode::NOT_FOUND, Body::from_stream(body))
        }),
    );
    let relay = RelayedInstallation::new(routes).await;
    let response = relay
        .get(&format!(
            "/runs/{}/artifacts/{}",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        !response.headers().contains_key("x-accel-buffering"),
        "An older Beacon cancels an unexpected stream, so errors must stay finite"
    );
    assert_eq!(
        response.text().await.unwrap(),
        r#"{"error":"This file is no longer available."}"#
    );
    relay.close().await;
}

#[tokio::test]
async fn an_oversized_response_fails_only_its_request() {
    let routes = Router::new().route(
        "/api/fixture/oversized",
        get(|| async { Body::from(vec![0_u8; cairn_protocol::MAX_BODY + 1]) }),
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
