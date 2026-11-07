mod common;

use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::atomic::Ordering, time::Duration};

/// The node gateway, run in-process like its systemd unit on a free port.
struct Gateway {
    origin: String,
    statistics: std::sync::Arc<cairn_node_gateway::Statistics>,
    stop: tokio_util::sync::CancellationToken,
    _root: tempfile::TempDir,
}

impl Gateway {
    async fn start(beacon: &str, installation: &str, pin: &str, direct: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = cairn_node_gateway::Config {
            beacon: beacon.to_owned(),
            installation_id: installation.to_owned(),
            pin: pin.to_owned(),
            port: 0,
            direct,
        };
        let gateway = cairn_node_gateway::Gateway::bind(config, root.path())
            .await
            .unwrap();
        assert!(root.path().join("socket/gateway.sock").exists());
        let origin = format!("http://{}", gateway.local_addr().unwrap());
        let statistics = gateway.statistics();
        let stop = tokio_util::sync::CancellationToken::new();
        tokio::spawn(gateway.serve(stop.clone()));
        Self {
            origin,
            statistics,
            stop,
            _root: root,
        }
    }

    fn carried(&self) -> (u64, u64) {
        (
            self.statistics.direct.load(Ordering::Relaxed),
            self.statistics.relay.load(Ordering::Relaxed),
        )
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.stop.cancel();
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

/// Gateway and manager node logs, shown with a failing test.
fn logs() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("cairn_node_gateway=debug,cairn_installation::relay::nodes=debug")
        .with_test_writer()
        .try_init();
}

/// Every request is bounded, so a stalled path fails here instead of hanging.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap()
}

/// The pin at the end of a Beacon install command.
fn pin(enrollment: &Value) -> String {
    let command = enrollment["installCommand"].as_str().unwrap();
    let pin = command.rsplit(' ').next().unwrap().to_owned();
    assert!(cairn_protocol::nodes::valid_pin(&pin), "{command}");
    pin
}

/// A relayed installation whose manager also echoes large bodies on a node route.
async fn echoing_installation() -> (RelayedInstallation, String) {
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
    (relay, installation)
}

/// Enrolls through `gateway`, then checks heartbeats, a disk-sized transfer
/// and that only node routes answer. Returns the node token.
async fn exercise(relay: &RelayedInstallation, gateway: &Gateway) -> String {
    let invitation = create_enrollment(relay, json!({ "name": "Office" })).await;
    let client = client();
    let response = client
        .post(format!("{}/internal/nodes/enroll", gateway.origin))
        .json(&json!({
            "code": invitation["code"],
            "name": "Office",
            "protocol": 1,
            "runtimeId": "development",
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
    let token = node["token"].as_str().unwrap().to_owned();

    // Several requests reuse one local connection, and so one channel.
    for _ in 0..3 {
        let heartbeat = client
            .post(format!("{}/internal/nodes/heartbeat", gateway.origin))
            .bearer_auth(&token)
            .json(&json!({"runtimeId": "development", "nodeProtocol": 2}))
            .send()
            .await
            .unwrap();
        assert_eq!(heartbeat.status(), StatusCode::OK);
    }
    let unauthenticated = client
        .post(format!("{}/internal/nodes/heartbeat", gateway.origin))
        .json(&json!({"runtimeId": "development", "nodeProtocol": 2}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    // Disk-sized bodies cross many messages in both directions.
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
        tokio::time::timeout(
            Duration::from_secs(60),
            tokio::io::AsyncReadExt::read_to_string(&mut stream, &mut response),
        )
        .await
        .unwrap_or_else(|_| panic!("{route}: no answer"))
        .unwrap();
        assert!(response.starts_with("HTTP/1.1 404"), "{route}: {response}");
    }
    token
}

#[tokio::test]
async fn a_node_without_a_manager_address_connects_directly_after_beacon_introduces_it() {
    logs();
    let (relay, installation) = echoing_installation().await;

    let native = create_enrollment(&relay, json!({ "name": "Desktop", "backend": "native" })).await;
    assert_eq!(native["route"], "beacon");
    assert_eq!(native["managerUrl"], Value::Null);
    let pin = pin(&native);
    assert_eq!(
        native["installCommand"],
        format!(
            "curl --fail --silent --show-error '{}/install/node.sh' | sudo bash -s -- '{installation}' native {pin}",
            relay.beacon_origin
        )
    );
    // The pin is the manager's node certificate, kept across enrollments.
    let stored: Value = serde_json::from_slice(
        &std::fs::read(
            relay
                .installation
                .config
                .data_dir
                .join("installation-relay/node-certificate.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let certificate = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        stored["certificate"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(pin, hex::encode(Sha256::digest(&certificate)));
    let again = create_enrollment(&relay, json!({ "name": "Again", "backend": "native" })).await;
    assert_eq!(pin, self::pin(&again));
    // A direct address stays available and keeps the node off Beacon.
    let direct = create_enrollment(
        &relay,
        json!({ "name": "VPN", "managerUrl": "https://manager.vpn.example" }),
    )
    .await;
    assert_eq!(direct["route"], "direct");
    assert_eq!(direct["managerUrl"], "https://manager.vpn.example");

    let gateway = Gateway::start(&relay.app.url, &installation, &pin, true).await;
    // Connections use Beacon until the direct connection is up.
    let client = client();
    tokio::time::timeout(Duration::from_secs(60), async {
        while gateway.carried().0 == 0 {
            let _ = client
                .post(format!("{}/internal/nodes/heartbeat", gateway.origin))
                .timeout(Duration::from_secs(5))
                .json(&json!({"runtimeId": "development", "nodeProtocol": 2}))
                .send()
                .await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("direct connection");
    let (_, relayed) = gateway.carried();
    exercise(&relay, &gateway).await;
    // Every connection after that went direct: Beacon carried none.
    assert_eq!(gateway.carried().1, relayed);

    drop(gateway);
    relay.close().await;
}

#[tokio::test]
async fn without_a_direct_connection_beacon_carries_tls_to_the_pinned_manager() {
    logs();
    let (relay, installation) = echoing_installation().await;
    let pin =
        pin(&create_enrollment(&relay, json!({ "name": "Office", "backend": "native" })).await);

    let gateway = Gateway::start(&relay.app.url, &installation, &pin, false).await;
    let token = exercise(&relay, &gateway).await;
    assert_eq!(gateway.carried().0, 0);
    assert!(gateway.carried().1 > 0);

    // Another certificate is refused on both paths: Beacon cannot pose as
    // the manager, and the node is told why.
    let impostor = Gateway::start(&relay.app.url, &installation, &"0".repeat(64), true).await;
    let refused = client()
        .post(format!("{}/internal/nodes/heartbeat", impostor.origin))
        .bearer_auth(&token)
        .json(&json!({"runtimeId": "development", "nodeProtocol": 2}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_GATEWAY);
    let refused: Value = refused.json().await.unwrap();
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("certificate does not match"),
        "{refused}"
    );
    assert_eq!(impostor.carried(), (0, 0));

    // Channels end with the installation's tunnel; the node is told why.
    relay.stop.cancel();
    let client = client();
    let offline = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = client
                .post(format!("{}/internal/nodes/heartbeat", gateway.origin))
                .bearer_auth(&token)
                .json(&json!({"runtimeId": "development", "nodeProtocol": 2}))
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

    drop((gateway, impostor));
    relay.close().await;
}

#[tokio::test]
async fn beacon_accepts_node_channels_and_offers_only_for_current_installations() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let identity = identity(&relay);
    let installation = identity["installationId"].as_str().unwrap();
    let ws = relay.app.url.replace("http:", "ws:");

    // An unknown installation has no tunnel to carry the channel or the offer.
    let unknown = uuid::Uuid::new_v4();
    let refused =
        tokio_tungstenite::connect_async(format!("{ws}/api/relay/{unknown}/nodes/connect"))
            .await
            .unwrap_err();
    let tokio_tungstenite::tungstenite::Error::Http(response) = refused else {
        panic!("expected an HTTP refusal, got {refused:?}");
    };
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let offer = relay
        .app
        .client
        .post(format!(
            "{}/api/relay/{unknown}/nodes/direct",
            relay.app.url
        ))
        .json(&json!({ "sdp": "v=0" }))
        .send()
        .await
        .unwrap();
    assert_eq!(offer.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Oversized offers never reach the installation.
    let oversized = relay
        .app
        .client
        .post(format!(
            "{}/api/relay/{installation}/nodes/direct",
            relay.app.url
        ))
        .json(&json!({ "sdp": "a".repeat(cairn_protocol::nodes::MAX_SDP + 1) }))
        .send()
        .await
        .unwrap();
    assert!(oversized.status().is_client_error());
    // An invalid offer gets no answer, so the node uses Beacon.
    let invalid = relay
        .app
        .client
        .post(format!(
            "{}/api/relay/{installation}/nodes/direct",
            relay.app.url
        ))
        .json(&json!({ "sdp": "v=0" }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::SERVICE_UNAVAILABLE);
    let ice: Value = relay
        .app
        .client
        .get(format!(
            "{}/api/relay/{installation}/nodes/direct",
            relay.app.url
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        ice["iceServers"][0]["urls"][0]
            .as_str()
            .unwrap()
            .starts_with("stun:")
    );

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
async fn beacon_serves_the_node_gateway_with_its_checksum_in_the_bootstrap() {
    let get = |router: axum::Router, path: &'static str| async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}{path}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let response = reqwest::get(url).await.unwrap();
        let status = response.status();
        let cache = response.headers()["cache-control"]
            .to_str()
            .unwrap()
            .to_owned();
        let body = response.bytes().await.unwrap();
        server.abort();
        (status, cache, body)
    };
    let binary = b"\x7fELF node gateway".to_vec();
    let router =
        cairn_beacon::installer::gateway_router("https://cairn.example", Some(binary.clone()));
    let (status, cache, script) = get(router.clone(), "/install/node.sh").await;
    assert_eq!(status, 200);
    assert_eq!(cache, "no-store");
    let script = String::from_utf8(script.to_vec()).unwrap();
    assert!(script.starts_with("#!/usr/bin/env bash"));
    assert!(script.contains("CAIRN_BEACON_ORIGIN='https://cairn.example'"));
    assert!(script.contains(&format!(
        "CAIRN_GATEWAY_SHA256={}",
        hex::encode(Sha256::digest(&binary))
    )));
    assert!(!script.contains("__CAIRN_"));
    let (status, _, served) = get(router, "/install/node-gateway").await;
    assert_eq!(status, 200);
    assert_eq!(served.as_ref(), binary.as_slice());

    // A Beacon without the gateway says so instead of installing half a node.
    let router = cairn_beacon::installer::gateway_router("https://cairn.example", None);
    let (status, _, script) = get(router, "/install/node.sh").await;
    assert_eq!(status, 200);
    let script = String::from_utf8(script.to_vec()).unwrap();
    assert!(script.contains("does not provide the node gateway"));
    assert!(script.contains("exit 1"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_direct_channels_keep_their_responses_when_streams_are_reused() {
    logs();
    let (relay, installation) = echoing_installation().await;

    let pin =
        pin(&create_enrollment(&relay, json!({ "name": "Desktop", "backend": "native" })).await);
    let gateway = Gateway::start(&relay.app.url, &installation, &pin, true).await;
    // Connections use Beacon until the direct connection is up.
    let client = client();
    tokio::time::timeout(Duration::from_secs(60), async {
        while gateway.carried().0 == 0 {
            let _ = client
                .post(format!("{}/internal/nodes/heartbeat", gateway.origin))
                .timeout(Duration::from_secs(5))
                .json(&json!({"runtimeId": "development", "nodeProtocol": 2}))
                .send()
                .await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("direct connection");
    let (_, relayed) = gateway.carried();

    // A production gateway serves concurrent TCP connections. Closing each
    // response repeatedly reuses the finite set of SCTP wire stream IDs.
    for _ in 0..16 {
        let mut requests = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let address = gateway.origin.trim_start_matches("http://").to_owned();
            requests.spawn(async move {
                let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
                let request = b"GET /internal/nodes/../execution/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
                tokio::io::AsyncWriteExt::write_all(&mut stream, request)
                    .await
                    .unwrap();
                let mut response = String::new();
                tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::io::AsyncReadExt::read_to_string(&mut stream, &mut response),
                )
                .await
                .expect("a concurrent direct channel must answer")
                .unwrap();
                assert!(response.starts_with("HTTP/1.1 404"), "expected traversal rejection, got {response:?}");
            });
        }
        while let Some(result) = requests.join_next().await {
            result.unwrap();
        }
    }
    // Every connection after that went direct: Beacon carried none.
    assert_eq!(gateway.carried().1, relayed);

    drop(gateway);
    relay.close().await;
}
