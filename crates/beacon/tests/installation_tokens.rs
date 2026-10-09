mod common;

use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn rotating_a_machine_token_closes_old_streams_and_can_retry_a_lost_response() {
    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let dir = relay
        .installation
        .config
        .data_dir
        .join("installation-relay");
    let path = dir.join("identity.json");
    let mut identity: Value =
        serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    let id = identity["installationId"].as_str().unwrap().to_owned();
    let old = identity["token"].as_str().unwrap().to_owned();
    let token = "a".repeat(64);
    let rotate = |credential: &str, replacement: &str| {
        relay
            .app
            .client
            .post(format!("{}/api/relay/{id}/rotate-token", relay.app.url))
            .bearer_auth(credential)
            .json(&json!({ "token": replacement }))
    };
    let mut stream = relay.get("/chats/stream").send().await.unwrap();
    stream.chunk().await.unwrap().unwrap();
    assert_eq!(
        rotate("wrong", &token).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        rotate(&old, &token).send().await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Ok(Some(_)) = stream.chunk().await {}
    })
    .await
    .expect("rotation must end streams on the old tunnel");
    tokio::time::timeout(Duration::from_secs(3), &mut relay.connector)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        rotate(&old, &"b".repeat(64)).send().await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        relay
            .app
            .post(
                "/api/relay/device-claim/start",
                json!({
                    "name": "Old proof",
                    "protocol": 1,
                    "identity": identity,
                })
            )
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );

    identity["token"] = json!(token);
    tokio::fs::write(&path, serde_json::to_vec(&identity).unwrap())
        .await
        .unwrap();
    relay.connector = tokio::spawn(cairn_installation::relay::connect(
        dir,
        cairn_installation::http::router(relay.installation.clone())
            .await
            .unwrap(),
        relay.installation.clone(),
        relay.stop.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while relay.get("/chats").send().await.unwrap().status() != StatusCode::OK {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let rotate = relay
        .app
        .client
        .post(format!("{}/api/relay/{id}/rotate-token", relay.app.url))
        .bearer_auth(&old)
        .json(&json!({ "token": token }));
    assert_eq!(
        rotate.send().await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK,
        "retrying the acknowledged rotation must preserve the new tunnel"
    );
    relay.close().await;
}

#[tokio::test]
async fn the_machine_resumes_rotation_after_the_beacon_response_is_lost() {
    use axum::{
        Json,
        extract::State,
        http::HeaderMap,
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use std::{
        os::unix::fs::PermissionsExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    async fn forward(
        State((url, lost)): State<(String, Arc<AtomicBool>)>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> StatusCode {
        let response = reqwest::Client::new()
            .post(format!("{url}/rotate-token"))
            .header("authorization", &headers["authorization"])
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        if lost.swap(false, Ordering::SeqCst) {
            StatusCode::BAD_GATEWAY
        } else {
            StatusCode::NO_CONTENT
        }
    }

    async fn forward_policy(
        State((url, _)): State<(String, Arc<AtomicBool>)>,
        headers: HeaderMap,
    ) -> Response {
        let response = reqwest::Client::new()
            .get(format!("{url}/task-authors"))
            .header("authorization", &headers["authorization"])
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = response.json::<Value>().await.unwrap();

        (status, Json(body)).into_response()
    }

    let mut relay = RelayedInstallation::new(axum::Router::new()).await;
    let dir = relay
        .installation
        .config
        .data_dir
        .join("installation-relay");
    let path = dir.join("identity.json");
    let mut identity: Value =
        serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    let id = identity["installationId"].as_str().unwrap().to_owned();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://localhost:{}", listener.local_addr().unwrap().port());
    let router = axum::Router::new()
        .route("/api/relay/{installation}/rotate-token", post(forward))
        .route(
            "/api/relay/{installation}/task-authors",
            get(forward_policy),
        )
        .with_state((
            format!("{}/api/relay/{id}", relay.app.url),
            Arc::new(AtomicBool::new(true)),
        ));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    identity["origin"] = json!(proxy);
    tokio::fs::write(&path, serde_json::to_vec(&identity).unwrap())
        .await
        .unwrap();
    assert!(cairn_installation::relay::rotate_token(&dir).await.is_err());
    let after: Value = serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    assert!(
        after == identity,
        "a failed response must preserve the active identity"
    );
    let pending_path = dir.join("rotation.json");
    let pending: Value =
        serde_json::from_slice(&tokio::fs::read(&pending_path).await.unwrap()).unwrap();
    assert_eq!(
        tokio::fs::metadata(&pending_path)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let mut another_generation = identity.clone();
    another_generation["token"] = json!("b".repeat(64));
    tokio::fs::write(&path, serde_json::to_vec(&another_generation).unwrap())
        .await
        .unwrap();
    assert!(
        cairn_installation::relay::rotate_token(&dir).await.is_err(),
        "a stale pending proof must not overwrite another identity generation"
    );
    let preserved: Value = serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    assert!(preserved == another_generation);
    tokio::fs::write(&path, serde_json::to_vec(&identity).unwrap())
        .await
        .unwrap();
    cairn_installation::relay::rotate_token(&dir).await.unwrap();
    let mut updated: Value =
        serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
    assert!(
        updated == pending["identity"],
        "the retry must install the persisted replacement"
    );
    assert!(updated["token"] != identity["token"]);
    assert!(!pending_path.exists());
    assert_eq!(
        tokio::fs::metadata(&path)
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    tokio::time::timeout(Duration::from_secs(3), &mut relay.connector)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // Restore the direct origin after the response-loss adapter, then exercise the real connector.
    updated["origin"] = json!(relay.app.url);
    tokio::fs::write(&path, serde_json::to_vec(&updated).unwrap())
        .await
        .unwrap();
    relay.connector = tokio::spawn(cairn_installation::relay::connect(
        dir,
        cairn_installation::http::router(relay.installation.clone())
            .await
            .unwrap(),
        relay.installation.clone(),
        relay.stop.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while relay.get("/chats").send().await.unwrap().status() != StatusCode::OK {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    server.abort();
    relay.close().await;
}
