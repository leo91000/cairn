mod common;

use common::{RelayedInstallation, seeded_artifact};
use reqwest::StatusCode;
use serde_json::{Value, json};

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
        .json(&json!({
            "label": "Artifact reader",
            "scopes": ["read"],
        }))
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
async fn anonymous_links_stay_revoked_while_conversation_purge_removes_artifacts() {
    use axum::{
        Json, Router,
        extract::{Path, State},
        routing::post,
    };
    use std::sync::Arc;
    use tokio::sync::{Notify, mpsc};

    #[derive(Clone)]
    struct PurgeGate {
        reached: mpsc::UnboundedSender<String>,
        release: Arc<Notify>,
    }

    async fn delete_disk(State(gate): State<PurgeGate>, Path(run): Path<String>) -> Json<Value> {
        gate.reached.send(run).unwrap();
        gate.release.notified().await;
        Json(json!({}))
    }

    let (reached, mut requests) = mpsc::unbounded_channel();
    let release = Arc::new(Notify::new());
    let runner = Router::new()
        .route("/disks/{run}/delete", post(delete_disk))
        .with_state(PurgeGate {
            reached,
            release: release.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let runner_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, runner).await.unwrap() });
    let relay = RelayedInstallation::with_runner_url(Router::new(), runner_url).await;
    let bytes = b"Public purge sentinel";
    let (run, artifact) = seeded_artifact(&relay, bytes, "text/plain").await;
    let file = relay
        .installation
        .config
        .data_dir
        .join("artifacts")
        .join(&artifact);
    let preview = file.with_file_name(format!("{artifact}.jpg"));
    tokio::fs::write(&preview, b"Preview purge sentinel")
        .await
        .unwrap();
    let mut chat = relay.installation.chat_create(json!({})).await.unwrap();
    chat["runId"] = run.clone().into();
    relay
        .installation
        .store
        .save_conversation_boundary(chat.clone())
        .await
        .unwrap();
    let shared = visibility(&relay, &run, &artifact, "public").await;
    let public = shared["publicUrl"].as_str().unwrap();
    let token = shared["publicToken"].as_str().unwrap().to_owned();

    // This client has neither the owner's session nor a public MCP grant.
    let anonymous = reqwest::Client::new();
    let response = anonymous.get(public).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().as_ref(), bytes);

    let chat_id = chat["id"].as_str().unwrap();
    relay.installation.chat_trash(chat_id, true).await.unwrap();
    assert!(
        relay
            .installation
            .store
            .read({
                let token = token.clone();
                move |db| db.public_artifact(&token)
            })
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        anonymous.get(public).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );

    let service = relay.installation.clone();
    let purging = tokio::spawn(async move { service.cleanup_conversations().await });
    let deleting = tokio::time::timeout(std::time::Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deleting, run);
    let current = relay
        .installation
        .store
        .conversation_boundary(chat_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current["lifecycle"], "purging");
    assert!(file.exists() && preview.exists());
    let retained = relay
        .installation
        .store
        .artifact(&run, &artifact)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained.visibility.as_deref(), Some("private"));
    assert!(retained.public_token.is_none() && retained.public_url.is_none());
    for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
        let response = anonymous.request(method, public).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            !response
                .text()
                .await
                .unwrap()
                .contains("Public purge sentinel")
        );
    }

    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(30), purging)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        relay
            .installation
            .store
            .conversation_boundary(chat_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        relay.installation.store.run(&run).await.unwrap_err().status,
        404
    );
    assert!(
        relay
            .installation
            .store
            .artifacts(Some(&run))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        relay
            .installation
            .store
            .read(move |db| db.public_artifact(&token))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        relay
            .installation
            .store
            .read(|db| Ok(!db.0.prepare("PRAGMA foreign_key_check")?.exists([])?))
            .await
            .unwrap()
    );
    assert!(!file.exists() && !preview.exists());
    assert_eq!(
        anonymous.get(public).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    relay.close().await;
    server.abort();
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
                json!({
                    "runId": run,
                })
            } else {
                json!({
                    "runId": run,
                    "artifactId": artifact,
                })
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
    use axum::{
        Json, Router,
        extract::{OriginalUri, State},
        http::HeaderMap,
        routing::post,
    };
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
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        Json(input): Json<Value>,
    ) -> Json<bool> {
        let read_check = uri.path().ends_with("/mcp-authorization");
        if read_check && gate.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            gate.reached.notify_one();
            gate.release.notified().await;
        }
        let allowed = gate
            .client
            .post(format!("{}{}", gate.target, uri.path()))
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
                target: relay.app.url.clone(),
                calls: calls.clone(),
                reached: reached.clone(),
                release: release.clone(),
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            // The shared authority also checks the public read scope through
            // the plugin endpoint. Forward both real Beacon decisions, holding
            // only the second artifact-read check after result preparation.
            let plugin_path = format!("/api/relay/{installation}/plugin-mcp-authorization");
            let router = Router::new()
                .route(&path, post(check))
                .route(&plugin_path, post(check))
                .with_state(gate);
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
                json!({
                    "runId": run,
                })
            } else {
                json!({
                    "runId": run,
                    "artifactId": artifact,
                })
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
async fn private_files_larger_than_a_relay_frame_stream_for_reads_downloads_ranges_and_head() {
    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let bytes = (0..cairn_protocol::MAX_BODY + 100_000)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let (run, artifact) = seeded_artifact(&relay, &bytes, "video/mp4").await;
    let file = format!("/runs/{run}/artifacts/{artifact}");

    for route in [file.clone(), format!("{file}?download=1")] {
        let response = relay.get(&route).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{route}");
        assert_eq!(response.headers()["content-type"], "video/mp4");
        assert_eq!(
            response.headers()["content-disposition"],
            "attachment; filename*=UTF-8''report.html"
        );
        assert_eq!(response.bytes().await.unwrap().as_ref(), bytes.as_slice());
    }

    // Deliverable files keep their stored bytes, length and byte ranges.
    let response = relay
        .get(&file)
        .header("accept-encoding", "gzip, br")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("content-encoding"));
    assert_eq!(
        response.headers()["content-length"],
        bytes.len().to_string()
    );
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    assert_eq!(response.bytes().await.unwrap().as_ref(), bytes.as_slice());

    let tail = cairn_protocol::MAX_BODY - 10;
    let response = relay
        .get(&file)
        .header("range", format!("bytes={tail}-"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers()["content-range"],
        format!("bytes {tail}-{}/{}", bytes.len() - 1, bytes.len())
    );
    assert_eq!(response.bytes().await.unwrap().as_ref(), &bytes[tail..]);

    let response = relay
        .app
        .client
        .head(format!("{}{file}", relay.base))
        .header("cookie", &relay.cookie)
        .header("origin", &relay.app.url)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-length"],
        bytes.len().to_string()
    );
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    assert!(response.bytes().await.unwrap().is_empty());

    let metadata: Value = relay
        .get(&format!("{file}?metadata=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metadata["id"], artifact);
    assert_eq!(metadata["size"], bytes.len());
    relay.close().await;
}

/// A download whose reader never reads: its tiny receive window stalls the
/// transfer once the response head has arrived.
async fn stalled_download(relay: &RelayedInstallation, route: &str) -> tokio::net::TcpStream {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let url = url::Url::parse(&format!("{}{route}", relay.base)).unwrap();
    let mut connected = None;
    for address in url.socket_addrs(|| None).unwrap() {
        let socket = if address.is_ipv4() {
            tokio::net::TcpSocket::new_v4()
        } else {
            tokio::net::TcpSocket::new_v6()
        }
        .unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        if let Ok(stream) = socket.connect(address).await {
            connected = Some(stream);
            break;
        }
    }
    let mut stream = connected.expect("the Beacon fixture must accept a connection");
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\nAccept-Encoding: gzip, br\r\nConnection: close\r\n\r\n",
        url.path(),
        url.authority(),
        relay.cookie
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut status = [0; 12];
    stream.read_exact(&mut status).await.unwrap();
    assert_eq!(&status, b"HTTP/1.1 200");
    stream
}

#[tokio::test]
async fn private_files_wait_for_file_capacity_without_live_streams_and_expire_when_idle() {
    use std::time::Duration;
    use tokio::io::AsyncReadExt;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    // Larger than loopback socket buffers, so unread transfers stay in flight.
    let bytes = vec![b'x'; 100_000_000];
    let (run, artifact) = seeded_artifact(&relay, &bytes, "image/svg+xml").await;
    let file = format!("/runs/{run}/artifacts/{artifact}");

    // Fill the account's six concurrent file transfers on the Beacon.
    let mut downloads = Vec::new();
    for _ in 0..6 {
        downloads.push(stalled_download(&relay, &file).await);
    }

    let mut live = Vec::new();
    for _ in 0..cairn_protocol::MAX_STREAMS_PER_ACCOUNT {
        let response = relay.get("/chats/stream").send().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "Files must leave the account's live-stream allowance intact"
        );
        live.push(response);
    }
    assert_eq!(
        relay.get("/chats").send().await.unwrap().status(),
        StatusCode::OK
    );

    // A gallery's next original waits for a file transfer instead of failing.
    let mut next = tokio::spawn(relay.get(&file).send());
    assert!(
        tokio::time::timeout(Duration::from_secs(2), &mut next)
            .await
            .is_err()
    );
    drop(downloads.pop());
    let next = tokio::time::timeout(Duration::from_secs(10), next)
        .await
        .expect("A finished transfer must admit the waiting file")
        .unwrap()
        .unwrap();
    assert_eq!(next.status(), StatusCode::OK);
    drop(next);
    drop(live);

    // Never read meanwhile: the tunnel's file deadline must end the transfer.
    tokio::time::sleep(Duration::from_secs(31)).await;
    let mut received = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(15),
        downloads[0].read_to_end(&mut received),
    )
    .await
    .expect("An idle private file must release its transfer")
    .ok();
    assert!(
        received.len() < bytes.len(),
        "An idle private file must end incomplete, never as a complete body"
    );
    let head = String::from_utf8_lossy(&received[..received.len().min(2048)]).to_lowercase();
    assert!(head.contains(&format!("content-length: {}", bytes.len())));
    assert!(!head.contains("content-encoding"));

    drop(downloads);
    let response = relay.get(&file).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().len(), bytes.len());
    relay.close().await;
}

#[tokio::test]
async fn private_files_wait_for_a_tunnel_stream_slot_instead_of_failing() {
    use std::time::Duration;

    let relay = RelayedInstallation::new(axum::Router::new()).await;
    let (run, artifact) = seeded_artifact(&relay, b"original", "image/png").await;
    let cookies = common::stream_accounts(&relay).await;
    let mut live = Vec::new();
    for index in 0..cairn_protocol::MAX_STREAMS {
        let response = relay
            .app
            .client
            .get(format!("{}/chats/stream", relay.base))
            .header(
                "cookie",
                &cookies[index / cairn_protocol::MAX_STREAMS_PER_ACCOUNT],
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        live.push(response);
    }

    let mut file = tokio::spawn(
        relay
            .get(&format!("/runs/{run}/artifacts/{artifact}"))
            .send(),
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), &mut file)
            .await
            .is_err()
    );
    drop(live.pop());
    let response = tokio::time::timeout(Duration::from_secs(10), file)
        .await
        .expect("A released stream slot must admit the waiting file")
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().as_ref(), b"original");
    drop(live);
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
