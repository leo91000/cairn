mod common;

use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

const GATEWAY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/nodes/gateway.py");

/// The node gateway, started like its systemd unit on a free loopback port.
struct Gateway {
    origin: String,
    _process: tokio::process::Child,
    _root: tempfile::TempDir,
}

impl Gateway {
    async fn start(beacon: &str, installation: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        std::fs::write(
            root.path().join("config.json"),
            json!({ "beacon": beacon, "installationId": installation, "port": port }).to_string(),
        )
        .unwrap();
        let mut process = tokio::process::Command::new("python3")
            .arg(GATEWAY)
            .arg("run")
            .env("CAIRN_GATEWAY_ROOT", root.path())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(process.stdout.take().unwrap()).lines();
        let ready = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(ready.contains("listening"), "{ready}");
        assert!(root.path().join("socket/gateway.sock").exists());
        Self {
            origin: format!("http://127.0.0.1:{port}"),
            _process: process,
            _root: root,
        }
    }
}

fn identity(relay: &RelayedInstallation) -> Value {
    serde_json::from_slice(
        &std::fs::read(
            relay
                .installation
                .config
                .data_dir
                .join("installation-relay/identity.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

async fn create_enrollment(relay: &RelayedInstallation, body: Value) -> Value {
    let response = relay
        .app
        .client
        .post(format!("{}/nodes/enrollments", relay.base))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

#[tokio::test]
async fn a_node_without_a_manager_address_enrolls_and_transfers_through_beacon() {
    // Node routes the manager serves; this one echoes large bodies.
    let echo = axum::Router::new()
        .route(
            "/internal/nodes/fixture-echo",
            axum::routing::post(|body: axum::body::Bytes| async move { body }),
        )
        .layer(axum::extract::DefaultBodyLimit::disable());
    let relay = RelayedInstallation::new(echo).await;
    let installation = identity(&relay)["installationId"]
        .as_str()
        .unwrap()
        .to_owned();

    let native = create_enrollment(&relay, json!({ "name": "Desktop", "backend": "native" })).await;
    assert_eq!(native["route"], "beacon");
    assert_eq!(native["managerUrl"], Value::Null);
    assert_eq!(
        native["installCommand"],
        format!(
            "curl --fail --silent --show-error '{}/install/node.sh' | sudo bash -s -- '{installation}' native",
            relay.app.url
        )
    );
    // A direct address stays available and keeps the node off Beacon.
    let direct = create_enrollment(
        &relay,
        json!({ "name": "VPN", "managerUrl": "https://manager.vpn.example" }),
    )
    .await;
    assert_eq!(direct["route"], "direct");
    assert_eq!(direct["managerUrl"], "https://manager.vpn.example");

    let invitation = create_enrollment(&relay, json!({ "name": "Office" })).await;
    let gateway = Gateway::start(&relay.app.url, &installation).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/internal/nodes/enroll", gateway.origin))
        .json(&json!({
            "code": invitation["code"],
            "name": "Office",
            "protocol": 1,
            "runtimeId": "fixture",
            "capabilities": {
                "os": "linux",
                "arch": "x86_64",
                "kvm": true,
                "cpu": 4,
                "memoryMiB": 8192,
                "diskMiB": 32768,
            },
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let node: Value = response.json().await.unwrap();
    assert_eq!(node["nodeId"], invitation["nodeId"]);
    let token = node["token"].as_str().unwrap();

    // Several requests reuse one local connection, and so one channel.
    for _ in 0..3 {
        let heartbeat = client
            .post(format!("{}/internal/nodes/heartbeat", gateway.origin))
            .bearer_auth(token)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(heartbeat.status(), StatusCode::OK);
    }
    let unauthenticated = client
        .post(format!("{}/internal/nodes/heartbeat", gateway.origin))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    // Disk-sized bodies cross many channel messages in both directions.
    let body: Vec<u8> = (0..8_000_000u32).map(|index| (index % 251) as u8).collect();
    let echoed = client
        .post(format!("{}/internal/nodes/fixture-echo", gateway.origin))
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(echoed.status(), StatusCode::OK);
    assert_eq!(echoed.bytes().await.unwrap().as_ref(), body.as_slice());

    // Only node routes answer: the API, the manager's own execution channel
    // and traversal stay unreachable even with an owner session.
    for route in [
        "/api/chats",
        "/internal/execution/health",
        "/internal/nodes/../execution/health",
        "/internal/nodes/%2e%2e/execution/health",
    ] {
        let mut stream =
            tokio::net::TcpStream::connect(gateway.origin.trim_start_matches("http://"))
                .await
                .unwrap();
        let request = format!(
            "GET {route} HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: {}\r\nConnection: close\r\n\r\n",
            relay.cookie
        );
        tokio::io::AsyncWriteExt::write_all(&mut stream, request.as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        tokio::io::AsyncReadExt::read_to_string(&mut stream, &mut response)
            .await
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 404"), "{route}: {response}");
    }

    // Channels end with the installation's tunnel; the node is told why.
    relay.stop.cancel();
    let offline = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = client
                .post(format!("{}/internal/nodes/heartbeat", gateway.origin))
                .bearer_auth(token)
                .json(&json!({}))
                .send()
                .await;
            if let Ok(response) = response
                && response.status() == StatusCode::SERVICE_UNAVAILABLE
            {
                break response.json::<Value>().await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        offline["error"]
            .as_str()
            .unwrap()
            .contains("offline or must be updated")
    );

    drop(gateway);
    relay.close().await;
}

#[tokio::test]
async fn beacon_accepts_node_channels_only_for_current_installations() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let identity = identity(&relay);
    let installation = identity["installationId"].as_str().unwrap();
    let ws = relay.app.url.replace("http:", "ws:");

    // An unknown installation has no tunnel to carry the channel.
    let unknown = uuid::Uuid::new_v4();
    let refused =
        tokio_tungstenite::connect_async(format!("{ws}/api/relay/{unknown}/nodes/connect"))
            .await
            .unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(response) = refused else {
        panic!("expected an HTTP refusal, got {refused:?}");
    };
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Only the installation's own identity may accept a channel.
    for token in ["", "wrong"] {
        let mut request =
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
                format!("{ws}/api/relay/{installation}/nodes/{unknown}/accept"),
            )
            .unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        let refused = tokio_tungstenite::connect_async(request).await.unwrap_err();
        let tokio_tungstenite::tungstenite::Error::Http(response) = refused else {
            panic!("expected an HTTP refusal, got {refused:?}");
        };
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(format!(
            "{ws}/api/relay/{installation}/nodes/{unknown}/accept"
        ))
        .unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", identity["token"].as_str().unwrap())
            .parse()
            .unwrap(),
    );
    let refused = tokio_tungstenite::connect_async(request).await.unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(response) = refused else {
        panic!("expected an HTTP refusal, got {refused:?}");
    };
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    relay.close().await;
}

#[tokio::test]
async fn beacon_serves_the_node_bootstrap_with_the_gateway_checksum() {
    let app = common::Fixture::new().await;
    let script = app
        .client
        .get(format!("{}/install/node.sh", app.url))
        .send()
        .await
        .unwrap();
    assert_eq!(script.status(), 200);
    assert_eq!(script.headers()["cache-control"], "no-store");
    let script = script.text().await.unwrap();
    assert!(script.starts_with("#!/usr/bin/env bash"));
    assert!(script.contains(&format!("CAIRN_BEACON_ORIGIN='{}'", app.url)));
    assert!(!script.contains("__CAIRN_"));

    let gateway = app
        .client
        .get(format!("{}/install/node-gateway.py", app.url))
        .send()
        .await
        .unwrap();
    assert_eq!(gateway.status(), 200);
    let gateway = gateway.bytes().await.unwrap();
    assert_eq!(gateway.as_ref(), std::fs::read(GATEWAY).unwrap().as_slice());
    let checksum = hex::encode(Sha256::digest(&gateway));
    assert!(script.contains(&format!("CAIRN_GATEWAY_SHA256={checksum}")));
    app.close().await;
}
