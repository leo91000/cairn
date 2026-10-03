mod common;

use axum::{Router, routing::get};
use common::RelayedInstallation;
use reqwest::StatusCode;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Semaphore,
};

/// Send headers only: an unavailable installation must reject before reading an upload.
async fn rejects_before_reading_body(relay: &RelayedInstallation) {
    let address = relay.app.url.strip_prefix("http://").unwrap();
    let path = relay.base.strip_prefix(&relay.app.url).unwrap();
    let csrf = relay.session["csrf"].as_str().unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let headers = format!(
        "POST {path}/chats HTTP/1.1\r\nHost: {address}\r\nOrigin: {}\r\nCookie: {}\r\nX-CSRF-Token: {csrf}\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n",
        relay.app.url, relay.cookie,
    );
    stream.write_all(headers.as_bytes()).await.unwrap();
    let mut status = [0_u8; 12];
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut status))
        .await
        .expect("relay must reject without waiting for the upload body")
        .unwrap();
    assert_eq!(&status, b"HTTP/1.1 503");
}

#[tokio::test]
async fn a_full_installation_rejects_uploads_and_preserves_pending_requests() {
    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let routes = Router::new().route(
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
    let mut requests = Vec::new();
    for _ in 0..leo_relay_protocol::MAX_IN_FLIGHT {
        requests.push(tokio::spawn(relay.get("/fixture/held").send()));
    }
    tokio::time::timeout(Duration::from_secs(5), started.acquire_many(32))
        .await
        .unwrap()
        .unwrap()
        .forget();

    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    // Abandon half of the browser requests; their installation work still occupies slots.
    for request in requests.drain(..16) {
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
    }
    rejects_before_reading_body(&relay).await;
    release.add_permits(32);
    for request in requests {
        let response = request.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "completed");
    }
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn an_offline_installation_rejects_before_reading_an_upload() {
    let relay = RelayedInstallation::new(Router::new()).await;
    relay.stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if relay.get("/chats").send().await.unwrap().status() == StatusCode::SERVICE_UNAVAILABLE
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    rejects_before_reading_body(&relay).await;
    relay.close().await;
}
