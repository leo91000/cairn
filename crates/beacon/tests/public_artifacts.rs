use common::MissionFixture;

mod common;

use common::RelayedInstallation;
use reqwest::StatusCode;
use serde_json::{Value, json};

async fn seeded_artifact(
    relay: &RelayedInstallation,
    bytes: &[u8],
    media_type: &str,
) -> (String, String) {
    let installation = &relay.installation;
    let task = installation
        .task_fixture(
            json!({
                "name": "Report",
                "prompt": "Make a report",
                "agentId": cairn_installation::config::MAIN_AGENT_ID,
            }),
            None,
        )
        .await
        .unwrap();
    let run = installation
        .enqueue(task["id"].as_str().unwrap(), "manual", None)
        .await
        .unwrap();
    let run = run["id"].as_str().unwrap().to_owned();
    let artifact = uuid::Uuid::new_v4().to_string();
    let directory = installation.config.data_dir.join("artifacts");
    tokio::fs::create_dir_all(&directory).await.unwrap();
    tokio::fs::write(directory.join(&artifact), bytes)
        .await
        .unwrap();
    let record: cairn_contracts::Deliverable = serde_json::from_value(json!({
        "id": artifact, "runId": run, "messageId": null,
        "key": "report", "version": 1, "group": "", "kind": "file", "size": bytes.len(), "createdAt": 1,
        "name": "report.html", "title": "Report", "mediaType": media_type,
        "visibility": "private", "previewStatus": "none",
        "url": format!("/api/runs/{run}/artifacts/{artifact}"),
    })).unwrap();
    installation
        .store
        .transaction(move |db| db.insert_artifact(&record))
        .await
        .unwrap();
    (run, artifact)
}

async fn visibility(relay: &RelayedInstallation, run: &str, artifact: &str, value: &str) -> Value {
    let response = relay
        .app
        .client
        .put(format!(
            "{}/runs/{run}/artifacts/{artifact}/visibility",
            relay.base
        ))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&json!({ "visibility": value }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn reader_grant(relay: &RelayedInstallation) -> Value {
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    let response = relay
        .app
        .client
        .post(format!(
            "{}/api/installations/{installation}/tokens",
            relay.app.url
        ))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
        .json(&json!({"label": "Artifact reader", "scopes": ["read"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json().await.unwrap()
}

async fn invalidate_reader(relay: &RelayedInstallation, grant: &Value, invalidation: &str) {
    match invalidation {
        "revoke" => {
            let installation = relay.session["installations"][0]["id"].as_str().unwrap();
            let response = relay
                .app
                .client
                .delete(format!(
                    "{}/api/installations/{installation}/tokens/{}",
                    relay.app.url,
                    grant["id"].as_str().unwrap()
                ))
                .header("cookie", &relay.cookie)
                .header("origin", &relay.app.url)
                .header("x-csrf-token", relay.session["csrf"].as_str().unwrap())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        "expire" => {
            use sha2::{Digest, Sha256};
            let changed = sqlx_core::query::query(
                "UPDATE mcp_tokens SET expires_at = now() - interval '1 second' WHERE digest = $1",
            )
            .bind(hex::encode(Sha256::digest(
                grant["token"].as_str().unwrap().as_bytes(),
            )))
            .execute(&relay.app.pool)
            .await
            .unwrap();
            assert_eq!(changed.rows_affected(), 1);
        }
        _ => assert_eq!(invalidation, "none"),
    }
}

fn artifact_call(
    relay: &RelayedInstallation,
    token: &str,
    operation: &str,
    input: Value,
) -> reqwest::RequestBuilder {
    relay
        .app
        .client
        .post(format!("{}/mcp", relay.app.url))
        .bearer_auth(token)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "execute",
                "arguments": {
                    "code": format!("return await cairn.call({operation:?}, input);"),
                    "input": input,
                },
            },
        }))
}

#[tokio::test]
async fn admitted_artifact_reads_recheck_public_authorization() {
    use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
    for operation in ["list_run_artifacts", "read_run_artifact"] {
        for invalidation in ["none", "revoke", "expire"] {
            let relay = RelayedInstallation::new(axum::Router::new()).await;
            let (run, artifact) =
                seeded_artifact(&relay, b"Private artifact sentinel", "text/plain").await;
            let grant = reader_grant(&relay).await;
            let token = grant["token"].as_str().unwrap();

            // Admission is proven by the real code worker starting. Hold its
            // invocation until the owner has changed the originating grant.
            let ready = relay.root.path().join("artifact-reader-ready");
            let resume = relay.root.path().join("artifact-reader-resume");
            let fifo = std::ffi::CString::new(resume.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
            let mut release = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&resume)
                .unwrap();
            let worker = std::env::var_os("CAIRN_TEST_CODE_WORKER")
                .map(std::path::PathBuf::from)
                .expect("Use the credential-isolating backend launcher");
            let wrapper = relay.root.path().join("artifact-code-worker");
            let quote = |path: &std::path::Path| {
                format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
            };
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nset -eu\n: > {}\nIFS= read -r release < {}\nexec {} \"$@\"\n",
                    quote(&ready),
                    quote(&resume),
                    quote(&worker),
                ),
            )
            .unwrap();
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
            relay.installation.code_mode.use_worker(wrapper);
            let input = if operation == "list_run_artifacts" {
                json!({"runId": run})
            } else {
                json!({"runId": run, "artifactId": artifact})
            };
            let request = artifact_call(&relay, token, operation, input);
            let pending = tokio::spawn(async move { request.send().await.unwrap() });
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while !ready.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            invalidate_reader(&relay, &grant, invalidation).await;
            std::io::Write::write_all(&mut release, b"continue\n").unwrap();
            let response = pending.await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let response: Value = response.json().await.unwrap();
            if invalidation == "none" {
                assert_ne!(response["result"]["isError"], true, "{response}");
                let value = &response["result"]["structuredContent"]["result"];
                if operation == "list_run_artifacts" {
                    assert_eq!(value["artifacts"][0]["id"], artifact, "{response}");
                } else {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(value["data"].as_str().unwrap())
                        .unwrap();
                    assert_eq!(bytes, b"Private artifact sentinel");
                }
            } else {
                assert_eq!(
                    response["result"]["isError"], true,
                    "{operation}/{invalidation}: {response}"
                );
                assert!(
                    response.to_string().contains("MCP authorization"),
                    "{response}"
                );
                assert!(!response.to_string().contains("Private artifact sentinel"));
            }
            relay.close().await;
        }
    }
}

#[tokio::test]
async fn artifact_reads_recheck_public_authorization_after_preparation() {
    use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::sync::Notify;

    #[derive(Clone)]
    struct CheckGate {
        client: reqwest::Client,
        target: String,
        calls: Arc<AtomicUsize>,
        reached: Arc<Notify>,
        release: Arc<Notify>,
    }

    async fn check(
        State(gate): State<CheckGate>,
        headers: HeaderMap,
        Json(input): Json<Value>,
    ) -> Json<bool> {
        if gate.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            gate.reached.notify_one();
            gate.release.notified().await;
        }
        let allowed = gate
            .client
            .post(&gate.target)
            .header("authorization", headers["authorization"].to_str().unwrap())
            .json(&input)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<bool>()
            .await
            .unwrap();
        Json(allowed)
    }

    for operation in ["list_run_artifacts", "read_run_artifact"] {
        for invalidation in ["none", "revoke", "expire", "trash", "purge"] {
            let relay = RelayedInstallation::new(Router::new()).await;
            let (run, artifact) =
                seeded_artifact(&relay, b"Private artifact sentinel", "text/plain").await;
            let mut chat = relay.installation.chat_create(json!({})).await.unwrap();
            chat["runId"] = run.clone().into();
            relay
                .installation
                .store
                .save_conversation_boundary(chat.clone())
                .await
                .unwrap();
            let worker = std::env::var_os("CAIRN_TEST_CODE_WORKER")
                .map(std::path::PathBuf::from)
                .expect("Use the credential-isolating backend launcher");
            relay.installation.code_mode.use_worker(worker);
            let grant = reader_grant(&relay).await;
            let installation = relay.session["installations"][0]["id"].as_str().unwrap();
            let path = format!("/api/relay/{installation}/mcp-authorization");
            let calls = Arc::new(AtomicUsize::new(0));
            let reached = Arc::new(Notify::new());
            let release = Arc::new(Notify::new());
            let gate = CheckGate {
                client: relay.app.client.clone(),
                target: format!("{}{path}", relay.app.url),
                calls: calls.clone(),
                reached: reached.clone(),
                release: release.clone(),
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let router = Router::new().route(&path, post(check)).with_state(gate);
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

            // Observe the real Beacon decision without replacing its grant
            // authority. The second check is held until owner invalidation.
            let identity = relay
                .installation
                .config
                .data_dir
                .join("installation-relay/identity.json");
            let original = tokio::fs::read(&identity).await.unwrap();
            let mut redirected: Value = serde_json::from_slice(&original).unwrap();
            redirected["origin"] = origin.into();
            tokio::fs::write(&identity, serde_json::to_vec(&redirected).unwrap())
                .await
                .unwrap();
            let input = if operation == "list_run_artifacts" {
                json!({"runId": run})
            } else {
                json!({"runId": run, "artifactId": artifact})
            };
            let request = artifact_call(&relay, grant["token"].as_str().unwrap(), operation, input);
            let mut pending = tokio::spawn(async move { request.send().await.unwrap() });
            let finished_early = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                tokio::select! {
                    _ = reached.notified() => false,
                    _ = &mut pending => true,
                }
            })
            .await
            .unwrap();
            if finished_early {
                tokio::fs::write(&identity, &original).await.unwrap();
                server.abort();
                relay.close().await;
                panic!("{operation} completed without its final authorization check");
            }
            if matches!(invalidation, "trash" | "purge") {
                let chat_id = chat["id"].as_str().unwrap();
                relay.installation.chat_trash(chat_id, true).await.unwrap();
                if invalidation == "purge" {
                    cairn_installation::conversation_deletion::purge(
                        &relay.installation,
                        chat.clone(),
                    )
                    .await
                    .unwrap();
                }
            } else {
                invalidate_reader(&relay, &grant, invalidation).await;
            }
            release.notify_one();
            let response = pending.await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let response: Value = response.json().await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert_eq!(
                response["result"]["isError"] == true,
                invalidation != "none",
                "{operation}/{invalidation}: {response}"
            );
            if matches!(invalidation, "revoke" | "expire") {
                assert!(
                    response.to_string().contains("MCP authorization"),
                    "{response}"
                );
            }
            tokio::fs::write(&identity, &original).await.unwrap();
            server.abort();
            relay.close().await;
        }
    }
}

#[tokio::test]
async fn public_file_is_an_beacon_read_only_link_with_security_headers_revocation_and_offline_message()
 {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (run, artifact) = seeded_artifact(&relay, b"<h1>Report</h1>", "text/html").await;
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let id = relay.session["installations"][0]["id"].as_str().unwrap();
    let public = shared["publicUrl"].as_str().unwrap();
    assert!(
        public.starts_with(&format!(
            "{}/api/public/installations/{id}/artifacts/",
            relay.beacon_origin
        )),
        "The share URL must use the Beacon"
    );
    for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
        let response = relay
            .app
            .client
            .request(method.clone(), public)
            .header("origin", "https://recipient.example")
            .header("range", "bytes=4-9")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        for (name, value) in [
            ("access-control-allow-origin", "*"),
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("content-security-policy", "default-src 'none'; sandbox"),
            ("referrer-policy", "no-referrer"),
            ("x-robots-tag", "noindex, nofollow"),
        ] {
            assert_eq!(response.headers()[name], value);
        }
        assert!(!response.headers().contains_key("set-cookie"));
        assert!(
            !response
                .headers()
                .contains_key("access-control-allow-credentials")
        );
        assert_eq!(response.headers()["content-range"], "bytes 4-9/15");
        assert_eq!(response.headers()["content-length"], "6");
        assert_eq!(response.headers()["content-disposition"], "attachment");
        assert_eq!(
            response.text().await.unwrap(),
            if method == reqwest::Method::HEAD {
                ""
            } else {
                "Report"
            }
        );
    }

    assert_eq!(
        relay
            .app
            .client
            .get(format!("{}/runs/{run}/artifacts", relay.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    assert_eq!(
        relay
            .app
            .client
            .post(public)
            .body("changed")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );

    visibility(&relay, &run, &artifact, "private").await;
    assert_eq!(
        relay.app.client.get(public).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );

    let next = visibility(&relay, &run, &artifact, "public").await;
    assert_ne!(next["publicUrl"], shared["publicUrl"]);

    relay.stop.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
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
            if installations[0]["online"] == false {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let response = relay
        .app
        .client
        .get(next["publicUrl"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    assert!(!response.headers().contains_key("content-disposition"));
    assert_eq!(
        response.headers()["content-security-policy"],
        "default-src 'none'; sandbox"
    );
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("set-cookie"));

    let body = response.text().await.unwrap();
    assert!(
        body.contains("<h1>Installation offline</h1>"),
        "A recipient must understand why the file is unavailable"
    );
    assert!(!body.contains("relay-owner") && !body.contains("accountId"));
    relay.close().await;
}

#[tokio::test]
async fn public_files_larger_than_a_relay_frame_stream_without_truncation() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let bytes = vec![b'x'; cairn_protocol::MAX_BODY + 100_000];
    let (run, artifact) = seeded_artifact(&relay, &bytes, "application/octet-stream").await;
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let response = relay
        .app
        .client
        .get(shared["publicUrl"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().as_ref(), bytes.as_slice());
    relay.close().await;
}

#[tokio::test]
async fn public_svg_is_downloaded_as_an_attachment() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (run, artifact) = seeded_artifact(&relay, b"<svg></svg>", "image/svg+xml").await;
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let response = relay
        .app
        .client
        .get(shared["publicUrl"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-disposition"], "attachment");
    assert_eq!(response.headers()["content-length"], "11");
    assert_eq!(response.text().await.unwrap(), "<svg></svg>");
    relay.close().await;
}

#[tokio::test]
async fn stalled_public_downloads_cannot_exhaust_live_streams_and_expire_without_another_read() {
    use std::time::Duration;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (run, artifact) =
        seeded_artifact(&relay, &vec![b'x'; 12_000_000], "application/octet-stream").await;
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let public = shared["publicUrl"].as_str().unwrap();
    let mut downloads = Vec::new();
    for _ in 0..4 {
        let response = relay.app.client.get(public).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        downloads.push(response);
    }

    assert_eq!(
        relay.app.client.get(public).send().await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "Public downloads must have their own small capacity pool"
    );

    let cookies = common::stream_accounts(&relay).await;
    let mut live = Vec::new();
    for index in 0..24 {
        let response = relay
            .app
            .client
            .get(format!("{}/chats/stream", relay.base))
            .header("cookie", &cookies[index / 8])
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "All live-stream slots must remain available"
        );
        live.push(response);
    }

    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    // Keep the stalled responses alive and never poll their bodies. Expiry must
    // be driven by the tunnel, not by a downstream HTTP body read.
    tokio::time::sleep(Duration::from_secs(30)).await;

    let replacement = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let response = relay.app.client.get(public).send().await.unwrap();
            if response.status() == StatusCode::OK {
                break response;
            }
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    })
    .await
    .expect("Idle public downloads must release their slots");
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    drop(replacement);
    drop(downloads);
    drop(live);
    relay.close().await;
}

#[tokio::test]
async fn public_download_rate_limit_uses_the_peer_and_preserves_account_access() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (run, artifact) = seeded_artifact(&relay, b"Report", "text/plain").await;
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let public = shared["publicUrl"].as_str().unwrap();
    for _ in 0..30 {
        let response = relay.app.client.get(public).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "Report");
    }
    let limited = relay
        .app
        .client
        .get(public)
        .header("x-forwarded-for", "198.51.100.1")
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        limited.headers()["content-security-policy"],
        "default-src 'none'; sandbox"
    );
    assert!(!limited.headers().contains_key("set-cookie"));
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );
    relay.close().await;
}

#[tokio::test]
async fn only_owner_controls_public_links_and_member_removal_preserves_them() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let cookies = common::stream_accounts(&relay).await;
    let cookie = &cookies[1];
    let session: Value = relay
        .app
        .client
        .get(format!("{}/api/account/session", relay.app.url))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let (run, artifact) = seeded_artifact(&relay, b"Report", "text/plain").await;
    let path = format!("{}/runs/{run}/artifacts/{artifact}/visibility", relay.base);
    for value in ["public", "private"] {
        let response = relay
            .app
            .client
            .put(&path)
            .header("cookie", cookie)
            .header("origin", &relay.app.url)
            .header("x-csrf-token", session["csrf"].as_str().unwrap())
            .json(&json!({ "visibility": value }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let public = shared["publicUrl"].as_str().unwrap();
    let installation = relay.session["installations"][0]["id"].as_str().unwrap();
    let removed = relay
        .app
        .authenticated(
            &relay.cookie,
            &relay.session,
            reqwest::Method::DELETE,
            &format!(
                "/api/installations/{installation}/sharing/members/{}",
                session["account"]["id"].as_str().unwrap()
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        relay
            .app
            .client
            .get(public)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "Report"
    );
    visibility(&relay, &run, &artifact, "private").await;
    assert_eq!(
        relay.app.client.get(public).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    relay.close().await;
}
