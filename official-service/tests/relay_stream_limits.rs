mod common;

use axum::{
    Router,
    body::{Body, Bytes},
    routing::get,
};
use common::RelayedInstallation;
use reqwest::StatusCode;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

struct Subscription {
    polls: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

fn fast_subscription(polls: Arc<AtomicUsize>, dropped: Arc<AtomicBool>) -> Router {
    Router::new().route(
        "/api/fixture/fast/stream",
        get({
            let polls = polls.clone();
            let dropped = dropped.clone();
            move || {
                let subscription = Subscription {
                    polls: polls.clone(),
                    dropped: dropped.clone(),
                };
                async move {
                    let stream = futures_util::stream::unfold(subscription, |subscription| async {
                        subscription.polls.fetch_add(1, Ordering::SeqCst);
                        Some((
                            Ok::<_, std::io::Error>(Bytes::from(
                                vec![b'x'; leo_relay_protocol::MAX_STREAM_CHUNK],
                            )),
                            subscription,
                        ))
                    });
                    (
                        [("content-type", "text/event-stream")],
                        Body::from_stream(stream),
                    )
                }
            }
        }),
    )
}

async fn wait_for_backpressure(polls: &AtomicUsize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut previous = 0;
        let mut stable = tokio::time::Instant::now();
        loop {
            let current = polls.load(Ordering::SeqCst);
            assert!(
                current < 256,
                "a stalled reader cannot buffer unbounded stream data"
            );
            if current != previous {
                previous = current;
                stable = tokio::time::Instant::now();
            }
            if current > 0 && stable.elapsed() > Duration::from_millis(150) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the remote subscription must pause under back-pressure");
}

#[tokio::test]
async fn a_slow_reader_backpressures_only_its_subscription_and_drop_cancels_it() {
    let polls = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let routes = fast_subscription(polls.clone(), dropped.clone());
    let relay = RelayedInstallation::new(routes).await;
    let response = relay
        .get("/fixture/fast/stream")
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // HTTP/TCP buffers may accept several chunks. Once they fill, the actual
    // installation body must stop being polled; memory cannot grow with time.
    wait_for_backpressure(&polls).await;
    assert_eq!(
        relay
            .get("/chats")
            .timeout(Duration::from_secs(1))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    drop(response);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("browser cancellation must drop the installation subscription");
    relay.close().await;
}

#[tokio::test]
async fn a_failed_stream_and_a_panicking_request_leave_other_streams_and_requests_alive() {
    let routes = Router::new()
        .route(
            "/api/fixture/broken/stream",
            get(|| async {
                let stream = futures_util::stream::iter([
                    Ok(Bytes::from_static(b"event: ping\ndata: {}\n\n")),
                    Err(std::io::Error::other("fixture stream failure")),
                ]);
                (
                    [("content-type", "text/event-stream")],
                    Body::from_stream(stream),
                )
            }),
        )
        .route("/api/fixture/panic", get(panicking_handler));
    let relay = RelayedInstallation::new(routes).await;
    let mut healthy = relay.get("/chats/stream").send().await.unwrap();
    healthy.chunk().await.unwrap().unwrap();
    let mut broken = relay.get("/fixture/broken/stream").send().await.unwrap();
    assert_eq!(broken.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Ok(Some(_)) = broken.chunk().await {}
    })
    .await
    .expect("failed streams must close independently");
    assert_eq!(
        relay.get("/fixture/panic").send().await.unwrap().status(),
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    let response = relay
        .app
        .client
        .post(format!("{}/chats", relay.base))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), healthy.chunk())
            .await
            .unwrap()
            .unwrap()
            .is_some()
    );
    drop(healthy);
    relay.close().await;
}

async fn panicking_handler() -> &'static str {
    panic!("fixture handler failure");
}

#[tokio::test]
async fn empty_body_chunks_do_not_consume_the_next_stream_credit() {
    let routes = Router::new().route(
        "/api/fixture/empty/stream",
        get(|| async {
            let stream = futures_util::stream::iter([
                Ok::<_, std::io::Error>(Bytes::new()),
                Ok(Bytes::from_static(b"event: ping\ndata: {}\n\n")),
            ]);
            (
                [("content-type", "text/event-stream")],
                Body::from_stream(stream),
            )
        }),
    );
    let relay = RelayedInstallation::new(routes).await;
    let mut stream = relay.get("/fixture/empty/stream").send().await.unwrap();
    assert_eq!(stream.status(), StatusCode::OK);
    let chunk = tokio::time::timeout(Duration::from_secs(1), stream.chunk())
        .await
        .expect("empty installation chunks must not stall live delivery")
        .unwrap()
        .unwrap();
    assert_eq!(chunk.as_ref(), b"event: ping\ndata: {}\n\n");
    drop(stream);
    relay.close().await;
}

#[tokio::test]
async fn finite_response_headers_do_not_extend_the_request_deadline_or_hold_its_slot() {
    let routes = Router::new().route(
        "/api/fixture/delayed-body",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            Body::from_stream(futures_util::stream::pending::<Result<Bytes, std::io::Error>>())
        }),
    );
    let relay = RelayedInstallation::new(routes).await;
    let requests: Vec<_> = (0..leo_relay_protocol::MAX_IN_FLIGHT)
        .map(|_| tokio::spawn(relay.get("/fixture/delayed-body").send()))
        .collect();
    for request in requests {
        assert_eq!(
            request.await.unwrap().unwrap().status(),
            StatusCode::GATEWAY_TIMEOUT
        );
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if relay.get("/chats").send().await.unwrap().status() == StatusCode::OK {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the original 30-second deadline must release timed-out finite requests");
    relay.close().await;
}

#[tokio::test]
async fn revocation_cancels_a_backpressured_subscription_without_waiting_for_the_browser() {
    let polls = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let relay = RelayedInstallation::new(fast_subscription(polls.clone(), dropped.clone())).await;
    let stalled = relay.get("/fixture/fast/stream").send().await.unwrap();
    assert_eq!(stalled.status(), StatusCode::OK);
    wait_for_backpressure(&polls).await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let account = relay.session["account"]["id"].as_str().unwrap();
    relay.app.relay.revoke_access(id, Some(account));
    tokio::time::timeout(Duration::from_secs(1), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("revocation must cancel the remote subscription without another browser read");

    // Keep the revoked browser response alive. The full stream capacity must
    // already be reusable, with eight slots reserved for ordinary requests.
    let mut healthy = Vec::new();
    for _ in 0..24 {
        let response = relay.get("/chats/stream").send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        healthy.push(response);
    }
    assert_eq!(
        relay.get("/chats/stream").send().await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    drop(healthy);
    drop(stalled);
    relay.close().await;
}
