mod common;

use common::{Fixture, MissionFixture, RelayedInstallation, login};
use reqwest::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn history_compression_is_negotiated_and_preserved_through_the_relay() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let task = relay.installation.task_fixture(json!({
        "name": "Compressed history", "prompt": "Test", "agentId": cairn_installation::config::MAIN_AGENT_ID,
    }), None).await.unwrap();
    let run = relay
        .installation
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let run_id = run["id"].as_str().unwrap().to_owned();
    let id = run_id.clone();
    relay
        .installation
        .store
        .transaction(move |db| {
            for _ in 0..200 {
                db.event(
                    &id,
                    &cairn_contracts::EventKind::Output,
                    &"history 🦊 ".repeat(500),
                    None,
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let id = run_id.clone();
    let first = relay
        .installation
        .store
        .read(move |db| Ok(db.event_page(&id, 0, 1)?[0].id))
        .await
        .unwrap();
    let path = format!(
        "/runs/{run_id}/history?before={}&history=v1:{run_id}:{first}",
        i64::MAX
    );
    let response = relay
        .get(&path)
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-encoding"], "gzip");
    assert_eq!(response.headers()["x-cairn-transport"], "relay");
    let encoded = response.bytes().await.unwrap();
    assert!(encoded.len() < 256 * 1024);
    let mut decoded = String::new();
    std::io::Read::read_to_string(
        &mut flate2::read::GzDecoder::new(encoded.as_ref()),
        &mut decoded,
    )
    .unwrap();
    let page: Value = serde_json::from_str(&decoded).unwrap();
    assert!(decoded.len() > 256 * 1024);
    assert!(page["events"].as_array().unwrap().len() >= 200);
    let plain = relay
        .get(&path)
        .header("accept-encoding", "identity")
        .send()
        .await
        .unwrap();
    assert!(!plain.headers().contains_key("content-encoding"));
    assert_eq!(plain.json::<Value>().await.unwrap(), page);
    let stream = relay
        .get(&format!("/runs/{run_id}/stream?window=1"))
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert!(!stream.headers().contains_key("content-encoding"));
    drop(stream);
    relay.close().await;
}

#[tokio::test]
#[ignore = "local endpoint measurement; run alone with --ignored --nocapture --test-threads=1"]
async fn history_endpoint_measurement() {
    use rand::{RngCore, SeedableRng};
    use std::time::Instant;
    for workload in ["repetitive", "mixed", "noise"] {
        let relay = RelayedInstallation::new(axum::Router::new()).await;
        let task = relay.installation.task_fixture(json!({
            "name":"History measurement","prompt":"Test","agentId":cairn_installation::config::MAIN_AGENT_ID,
        }), None).await.unwrap();
        let run = relay
            .installation
            .enqueue(task["id"].as_str().unwrap(), "manual", None)
            .await
            .unwrap();
        let run_id = run["id"].as_str().unwrap().to_owned();
        let id = run_id.clone();
        relay.installation.store.transaction(move |db| {
            let mut rng = rand::rngs::StdRng::seed_from_u64(391);
            for n in 0..1500 {
                let output = if workload == "noise" || workload == "mixed" && n % 10 == 0 {
                    let mut bytes = [0; 2048];
                    rng.fill_bytes(&mut bytes);
                    bytes.iter().map(|byte| char::from(b'!' + byte % 94)).collect()
                } else { "src/module/example.rs\n".repeat(80) };
                db.event(&id, &cairn_contracts::EventKind::ItemCompleted, "tool", Some(&json!({
                    "item":{"id":format!("tool-{n}"),"type":"command_execution","command":"rg --files","aggregated_output":output},
                })))?;
            }
            Ok(())
        }).await.unwrap();
        let id = run_id.clone();
        let first = relay
            .installation
            .store
            .read(move |db| Ok(db.event_page(&id, 0, 1)?[0].id))
            .await
            .unwrap();
        let request = |before| {
            let path =
                format!("/runs/{run_id}/history?before={before}&history=v1:{run_id}:{first}");
            let pending = relay.get(&path).header("accept-encoding", "gzip");
            async move {
                let started = Instant::now();
                let response = pending.send().await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let gzip = response
                    .headers()
                    .get("content-encoding")
                    .is_some_and(|v| v == "gzip");
                let bytes = response.bytes().await.unwrap();
                let encoded = bytes.len();
                let decoded = if gzip {
                    let mut plain = Vec::new();
                    std::io::Read::read_to_end(
                        &mut flate2::read::GzDecoder::new(bytes.as_ref()),
                        &mut plain,
                    )
                    .unwrap();
                    plain
                } else {
                    bytes.to_vec()
                };
                let page: Value = serde_json::from_slice(&decoded).unwrap();
                (page, encoded, decoded.len(), started.elapsed().as_micros())
            }
        };
        let cpu = || {
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
            unsafe {
                assert_eq!(libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()), 0);
                let usage = usage.assume_init();
                (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) * 1_000_000
                    + usage.ru_utime.tv_usec
                    + usage.ru_stime.tv_usec
            }
        };
        let (opening, encoded, decoded, cold_us) = request(i64::MAX).await;
        let mut warm = Vec::new();
        let warm_cpu_start = cpu();
        for _ in 0..12 {
            warm.push(request(i64::MAX).await.3);
        }
        let warm_cpu = cpu() - warm_cpu_start;
        warm.sort();
        let history_cpu_start = cpu();
        let started = Instant::now();
        let mut before = i64::MAX;
        let mut pages = 0;
        let mut transferred = 0;
        loop {
            let (page, bytes, _, _) = request(before).await;
            pages += 1;
            transferred += bytes;
            let oldest = page["oldest"].as_i64().unwrap();
            assert!(oldest < before);
            before = oldest;
            if page["hasOlder"] == false {
                break;
            }
        }
        println!(
            "HISTORY_ENDPOINT {}",
            json!({
                "label":std::env::var("CAIRN_HISTORY_MEASUREMENT_LABEL").unwrap_or_else(|_|"candidate".into()),
                "sourceRoot":env!("CARGO_MANIFEST_DIR"),
                "forwardsEncoding":cairn_protocol::request_header("accept-encoding"),
                "workload":workload,"rows":1500,"openingEvents":opening["events"].as_array().unwrap().len(),
                "openingTransferredBytes":encoded,"openingDecodedBytes":decoded,
                "coldUs":cold_us,"warmP50Us":warm[6],"warmP95Us":warm[11],
                "historyPages":pages,"historyTransferredBytes":transferred,"historyUs":started.elapsed().as_micros(),
                "warm12CpuUs":warm_cpu,"historyCpuUs":cpu()-history_cpu_start,
                "conditions":"loopback HTTP through real Beacon/installation relay; first-read cold, warm OS cache; no simulated network latency"
            })
        );
        relay.close().await;
    }
}

#[tokio::test]
async fn additional_nodes_enroll_directly_and_keep_their_channel_outside_the_relay() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let router = cairn_installation::http::router(relay.installation.clone())
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let direct_url = format!("http://{}", listener.local_addr().unwrap());
    let manager_url = "https://manager.vpn.example";
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let response = relay
        .app
        .client
        .post(format!("{}/nodes/enrollments", relay.base))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&json!({
            "name": "Direct execution node",
            "managerUrl": manager_url,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let invitation: Value = response.json().await.unwrap();
    assert_eq!(invitation["managerUrl"], manager_url);

    let response = relay
        .app
        .client
        .post(format!("{direct_url}/internal/nodes/enroll"))
        .header("host", "manager.vpn.example")
        .json(&json!({
            "code": invitation["code"],
            "name": "Direct execution node",
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
    let identity: Value = response.json().await.unwrap();

    let installation_url = relay.base.trim_end_matches("/api");
    for route in [
        "/internal/nodes/heartbeat".to_owned(),
        format!("/internal/node-restore/{}", "a".repeat(64)),
    ] {
        // Even a node bearer credential and owner session cannot relay machine
        // traffic. Neither the real path nor an API-prefixed lookalike is routed.
        for prefix in [installation_url, relay.base.as_str()] {
            let response = relay
                .app
                .client
                .get(format!("{prefix}{route}"))
                .header("cookie", &relay.cookie)
                .bearer_auth(identity["token"].as_str().unwrap())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }

    relay.stop.cancel();
    let heartbeat = relay
        .app
        .client
        .post(format!("{direct_url}/internal/nodes/heartbeat"))
        .header("host", "manager.vpn.example")
        .bearer_auth(identity["token"].as_str().unwrap())
        .json(&json!({ "runtimeId": "development", "nodeProtocol": 2 }))
        .send()
        .await
        .unwrap();
    assert_eq!(heartbeat.status(), StatusCode::OK);

    server.abort();
    relay.close().await;
}

#[tokio::test]
async fn owner_renames_a_relayed_installation_without_changing_its_identity() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let url = format!("{}/api/installations/{id}", app.url);
    let rename = |cookie: &str, csrf: &str, name: &str| {
        app.client
            .patch(&url)
            .header("origin", &app.url)
            .header("cookie", cookie)
            .header("x-csrf-token", csrf)
            .json(&json!({ "name": name }))
    };
    let csrf = relay.session["csrf"].as_str().unwrap();

    assert_eq!(
        rename(&relay.cookie, "", "Forbidden")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    let (foreign, session) = login(app, "foreign@example.test").await;
    assert_eq!(
        rename(&foreign, session["csrf"].as_str().unwrap(), "Foreign")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    for invalid in [" ".to_owned(), "x".repeat(101), "Invalid\nname".to_owned()] {
        assert_eq!(
            rename(&relay.cookie, csrf, &invalid)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }

    let response = rename(&relay.cookie, csrf, "  Home installation  ")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let session: Value = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", &relay.cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(session["installations"][0]["name"], "Home installation");
    assert_eq!(session["installations"][0]["id"], id);

    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );

    relay.close().await;
}

#[tokio::test]
async fn owner_claims_an_installation_with_a_single_use_code() {
    let app = Fixture::new().await;
    let (cookie, session) = login(&app, "owner@example.test").await;
    let response = app
        .client
        .post(format!("{}/api/installations/claim-code", app.url))
        .header("origin", &app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let claim: Value = response.json().await.unwrap();
    assert_eq!(claim["expiresIn"], 600);
    let claim_body = json!({
        "code": claim["code"],
        "name": "My installation",
        "protocol": cairn_protocol::PROTOCOL_VERSION,
    });
    let response = app
        .client
        .post(format!("{}/api/relay/claim", app.url))
        .json(&claim_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let identity: Value = response.json().await.unwrap();
    assert!(identity["token"].as_str().unwrap().len() >= 32);
    assert_eq!(
        app.client
            .post(format!("{}/api/relay/claim", app.url))
            .json(&claim_body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let session: Value = app
        .client
        .get(format!("{}/api/account/session", app.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        session["installations"][0]["id"],
        identity["installationId"]
    );
    assert_eq!(session["installations"][0]["name"], "My installation");
    app.close().await;
}

#[tokio::test]
async fn owner_uses_the_real_installation_api_over_an_outbound_relay() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let app = &relay.app;
    let cookie = relay.cookie.clone();
    let session = relay.session.clone();
    let base = &relay.base;
    // Owner-only routes prove the connector uses the trusted owner context,
    // regardless of identity-looking headers supplied by a browser.
    assert_eq!(
        app.client
            .get(format!("{base}/accounts"))
            .header("cookie", &cookie)
            .header("x-cairn-role", "member")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.client
            .get(format!("{base}/chats"))
            .header("x-cairn-role", "owner")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.client
            .post(format!("{base}/chats"))
            .header("cookie", &cookie)
            .header("origin", &app.url)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.client
            .get(format!("{base}/session"))
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let request = |path: String, body: Value| {
        app.client
            .post(path)
            .header("origin", &app.url)
            .header("cookie", &cookie)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&body)
    };
    let response = request(format!("{base}/chats"), json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chat: Value = response.json().await.unwrap();
    let chat_id = chat["id"].as_str().unwrap();
    let response = request(
        format!("{base}/chats/{chat_id}/messages"),
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "text": "Relayed message",
        }),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let detail: Value = app
        .client
        .get(format!("{base}/chats/{chat_id}"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["messages"][0]["text"], "Relayed message");
    let attachment_path = format!(
        "{base}/chats/{chat_id}/attachments/{}?name=hostile.svg",
        uuid::Uuid::new_v4()
    );
    let response = app
        .client
        .put(&attachment_path)
        .header("origin", &app.url)
        .header("cookie", &cookie)
        .header("x-csrf-token", session["csrf"].as_str().unwrap())
        .body("<svg onload='alert(1)'/>")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .client
        .get(&attachment_path)
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(
        response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );

    let (other_cookie, _) = login(app, "other@example.test").await;
    assert_eq!(
        app.client
            .get(format!("{base}/chats"))
            .header("cookie", other_cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    relay.close().await;
}
