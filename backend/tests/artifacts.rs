use axum::response::IntoResponse;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
    routing::post,
};
use leo_agent_manager::{
    artifacts::{self, file},
    config::{Config, MAIN_AGENT_ID, id},
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

async fn fixture() -> (
    TempDir,
    Arc<Service>,
    Value,
    String,
    tokio::task::JoinHandle<()>,
) {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("home/.codex")).unwrap();
    std::fs::write(root.path().join("home/.codex/leo-managed-auth"), "1").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().fallback(post(|axum::Json(value): axum::Json<Value>| async move {
        match text(&value, "path") {
            "/tmp/second.md" => "# Second revision\n".into_response(),
            "/tmp/preview.png" => include_bytes!("../../tests/fixtures/artifacts/thumbnail.png")
                .as_slice()
                .into_response(),
            "/tmp/truncated.md" => {
                let stream = futures_util::stream::iter([
                    Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"partial")),
                    Err(std::io::Error::other("interrupted")),
                ]);
                ([("content-length", "100")], Body::from_stream(stream)).into_response()
            }
            "/tmp/slow.md" => {
                let stream = futures_util::stream::once(async {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"# Late file"))
                });
                ([("content-length", "11")], Body::from_stream(stream)).into_response()
            }
            _ => "# First revision\n".into_response(),
        }
    }));
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let s = Service::new(Config {
        data_dir: root.path().join("data"),
        home: root.path().join("home"),
        workspace_roots: vec![root.path().to_owned()],
        public_url: "http://localhost:4310".into(),
        host: "127.0.0.1".into(),
        port: 0,
        setup_token: "test-setup".into(),
        codex_bin: "codex".into(),
        claude_bin: "claude".into(),
        gh_bin: "gh".into(),
        concurrency: 1,
        logger: false,
        worker_enabled: false,
        runner_url: url,
    })
    .await
    .unwrap();
    let task = s
        .task(
            json!({
                "name": "Artifacts",
                "prompt": "Create a report",
                "agentId": MAIN_AGENT_ID
            }),
            None,
        )
        .await
        .unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    s.store
        .patch_run(
            text(&run, "id"),
            json!({
                "status": "running"
            }),
        )
        .await
        .unwrap();
    s.store
        .set(
            &format!("run-checkpoint:{}", text(&run, "id")),
            json!({
                "runnerId": id()
            }),
            None,
        )
        .await
        .unwrap();
    let config = s.mcps.run_configuration(&s, &run).await.unwrap();
    let token = text(&config["env"], "LEO_MCP_RUN_TOKEN").to_owned();
    (root, s, run, token, server)
}

#[tokio::test]
async fn publication_is_idempotent_versioned_and_readable_after_restart_without_a_vm() {
    let (_root, s, run, token, server) = fixture().await;
    let args = json!({
        "path": "/tmp/report.md",
        "title": "Release report",
        "key": "report",
        "group": "Release"
    });
    let (first, retry) = tokio::join!(
        s.artifacts.publish(&s, &token, &args),
        s.artifacts.publish(&s, &token, &args)
    );
    let first = first.unwrap();
    assert_eq!(first["id"], retry.unwrap()["id"]);
    let mut args = args;
    args["path"] = "/tmp/second.md".into();
    let second = s.artifacts.publish(&s, &token, &args).await.unwrap();
    assert_eq!(second["version"], 2);
    assert_ne!(first["id"], second["id"]);
    assert_eq!(second["kind"], "markdown");
    assert_eq!(
        artifacts::list(&s, text(&run, "id")).await.unwrap().len(),
        2
    );
    let run_id = text(&run, "id").to_owned();
    let events = s
        .store
        .read(move |db| db.events(&run_id, 0, 100))
        .await
        .unwrap();
    assert_eq!(events.iter().filter(|e| e["type"] == "artifact").count(), 2);
    server.abort();
    s.store
        .patch_run(
            text(&run, "id"),
            json!({
                "status": "succeeded"
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        s.artifacts
            .publish(&s, &token, &args)
            .await
            .unwrap_err()
            .status,
        401
    );
    let orphan = s.config.data_dir.join("artifacts").join(id());
    std::fs::write(&orphan, "orphan").unwrap();
    let old = std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH);
    std::fs::File::open(&orphan)
        .unwrap()
        .set_times(old)
        .unwrap();
    std::fs::File::open(s.config.data_dir.join("artifacts").join(text(&first, "id")))
        .unwrap()
        .set_times(old)
        .unwrap();
    let restarted = Service::new(s.config.clone()).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while orphan.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let response = artifacts::http(
        &restarted,
        text(&run, "id"),
        Some(text(&first, "id")),
        Request::builder().body(Body::empty()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        "# First revision\n"
    );
    let response = artifacts::http(
        &restarted,
        text(&run, "id"),
        Some(text(&second, "id")),
        Request::builder()
            .header("range", "bytes=2-7")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()["content-range"], "bytes 2-7/18");
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        "Second"
    );
    let response = artifacts::http(
        &restarted,
        text(&run, "id"),
        Some(text(&second, "id")),
        Request::builder()
            .header("range", "bytes=999-")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 416);
    assert_eq!(response.headers()["content-range"], "bytes */18");
}

#[tokio::test]
async fn publication_checks_grants_and_downloads_require_authentication_and_correct_run() {
    let (_root, s, run, token, server) = fixture().await;
    let args = json!({
        "path": "/tmp/report.md",
        "title": "Report",
        "key": "report"
    });
    assert_eq!(
        s.artifacts
            .publish(&s, "wrong-token", &args)
            .await
            .unwrap_err()
            .status,
        401
    );
    let item = s.artifacts.publish(&s, &token, &args).await.unwrap();
    let app = leo_agent_manager::http::router(s.clone()).await.unwrap();
    for path in [
        format!("/api/runs/{}/artifacts", text(&run, "id")),
        text(&item, "url").to_owned(),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "localhost:4310")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }
    let task = s
        .task(
            json!({
                "name": "Other",
                "prompt": "Other",
                "agentId": MAIN_AGENT_ID
            }),
            None,
        )
        .await
        .unwrap();
    let other = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    assert_eq!(
        artifacts::http(
            &s,
            text(&other, "id"),
            Some(text(&item, "id")),
            Request::new(Body::empty())
        )
        .await
        .unwrap_err()
        .status,
        404
    );
    s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
    assert_eq!(
        s.artifacts
            .publish(&s, &token, &args)
            .await
            .unwrap_err()
            .status,
        401
    );
    server.abort();
}

#[tokio::test]
async fn guest_export_rejects_symlinks_devices_and_parent_paths_and_snapshots_original_bytes() {
    let root = TempDir::new().unwrap();
    let file = root.path().join("report.txt");
    std::fs::write(&file, "original").unwrap();
    let (snapshot, size) = file::snapshot(&file, root.path()).await.unwrap();
    std::fs::write(&file, "changed").unwrap();
    assert_eq!(std::fs::read(snapshot.path()).unwrap(), b"original");
    assert_eq!(size, 8);
    std::os::unix::fs::symlink(&file, root.path().join("link")).unwrap();
    std::os::unix::fs::symlink(root.path(), root.path().join("dir-link")).unwrap();
    for path in [
        root.path().join("link"),
        root.path().join("dir-link/report.txt"),
        root.path().join("../report.txt"),
        root.path().to_owned(),
        Path::new("/etc/passwd").to_owned(),
        Path::new("/dev/zero").to_owned(),
    ] {
        assert!(
            file::open_export(&path, root.path()).is_err(),
            "{}",
            path.display()
        );
    }
}

#[test]
fn media_detection_and_byte_ranges_do_not_trust_file_extensions() {
    assert_eq!(
        file::classify("malicious.png", b"<script>alert(1)</script>"),
        ("file", "application/octet-stream")
    );
    assert_eq!(
        file::classify("report.html", b"<html>test</html>"),
        ("file", "application/octet-stream")
    );
    assert_eq!(
        file::classify("image.png", b"\x89PNG\r\n\x1a\n"),
        ("image", "image/png")
    );
    assert_eq!(
        file::classify("video.mp4", b"\0\0\0\x18ftypisom"),
        ("video", "video/mp4")
    );
    assert_eq!(file::range(Some("bytes=-3"), 10).unwrap(), Some((7, 9)));
    assert_eq!(file::range(Some("bytes=3-"), 10).unwrap(), Some((3, 9)));
    assert_eq!(file::range(Some("bytes=0-999"), 10).unwrap(), Some((0, 9)));
    for range in [
        "bytes=4-2",
        "bytes=0-1,3-4",
        "bytes=-0",
        "bytes=999-",
        "nope",
    ] {
        assert!(file::range(Some(range), 10).is_err());
    }
}

#[tokio::test]
async fn interrupted_transfers_and_revoked_in_flight_grants_do_not_publish_partial_files() {
    let (_root, s, run, token, server) = fixture().await;
    let args = json!({
        "path": "/tmp/truncated.md",
        "title": "Report",
        "key": "report"
    });
    assert!(s.artifacts.publish(&s, &token, &args).await.is_err());
    assert!(
        artifacts::list(&s, text(&run, "id"))
            .await
            .unwrap()
            .is_empty()
    );
    let args = json!({
        "path": "/tmp/slow.md",
        "title": "Report",
        "key": "report"
    });
    let revoke = async {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
    };
    let (published, ()) = tokio::join!(s.artifacts.publish(&s, &token, &args), revoke);
    assert_eq!(published.unwrap_err().status, 401);
    assert!(
        artifacts::list(&s, text(&run, "id"))
            .await
            .unwrap()
            .is_empty()
    );
    if s.config.data_dir.join("artifacts").is_dir() {
        assert_eq!(
            std::fs::read_dir(s.config.data_dir.join("artifacts"))
                .unwrap()
                .count(),
            0
        );
    }
    server.abort();
}

#[tokio::test]
async fn preview_is_prepared_asynchronously_or_reports_missing_optional_tools_without_losing_original()
 {
    let (_root, s, run, token, server) = fixture().await;
    let artifact = s
        .artifacts
        .publish(
            &s,
            &token,
            &json!({
                "path": "/tmp/preview.png",
                "title": "Preview",
                "key": "preview"
            }),
        )
        .await
        .unwrap();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let item = s
                .store
                .kv(&format!(
                    "artifact:{}:{}",
                    text(&run, "id"),
                    text(&artifact, "id")
                ))
                .await
                .unwrap()
                .unwrap();
            if item["previewStatus"] != "pending" {
                break item;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let tools = std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert_eq!(
        completed["previewStatus"],
        if tools { "ready" } else { "unavailable" }
    );
    if tools {
        assert_eq!(completed["width"], 64);
        assert_eq!(completed["height"], 64);
    }
    assert!(
        s.config
            .data_dir
            .join("artifacts")
            .join(text(&artifact, "id"))
            .is_file()
    );
    server.abort();
}

#[tokio::test]
async fn public_links_are_scoped_revocable_and_survive_restart_without_exposing_private_routes() {
    let (_root, s, run, token, server) = fixture().await;
    let item = s
        .artifacts
        .publish(
            &s,
            &token,
            &json!({
                "path": "/tmp/report.md",
                "title": "Report",
                "key": "public-test"
            }),
        )
        .await
        .unwrap();
    assert_eq!(item["visibility"], "private");
    assert!(item["publicUrl"].is_null());
    let session = s.auth.session().await.unwrap();
    let app = leo_agent_manager::http::router(s.clone()).await.unwrap();
    let endpoint = format!("{}/visibility", text(&item, "url"));
    for (cookie, csrf, expected) in [(false, false, 401), (true, false, 403), (true, true, 200)] {
        let mut request = Request::builder()
            .method("PUT")
            .uri(&endpoint)
            .header("host", "localhost:4310");
        if cookie {
            request = request.header("cookie", format!("leo_session={}", text(&session, "value")));
        }
        if csrf {
            request = request.header("x-csrf-token", text(&session, "csrf"));
        }
        let response = app
            .clone()
            .oneshot(
                request
                    .body(Body::from(r#"{"visibility":"public"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    let shared = artifacts::list(&s, text(&run, "id"))
        .await
        .unwrap()
        .pop()
        .unwrap();
    let url = url::Url::parse(text(&shared, "publicUrl")).unwrap();
    let public = url.path().to_owned();
    for (path, status) in [
        (text(&item, "url").to_owned(), 401),
        (format!("{}/../", text(&item, "url")), 401),
        (format!("/api/runs/{}/artifacts", text(&run, "id")), 401),
        (format!("/api/public/artifacts/{}", id()), 404),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "localhost:4310")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    for method in ["GET", "HEAD"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(&public)
                    .header("host", "localhost:4310")
                    .header("origin", "https://recipient.example")
                    .header("range", "bytes=2-6")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 206);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            response.headers()["content-security-policy"],
            "default-src 'none'; sandbox"
        );
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(
            bytes.as_ref(),
            if method == "HEAD" {
                b"".as_slice()
            } else {
                b"First".as_slice()
            }
        );
    }
    let restarted = Service::new(s.config.clone()).await.unwrap();
    let router = leo_agent_manager::http::router(restarted.clone())
        .await
        .unwrap();
    let request = || {
        Request::builder()
            .uri(&public)
            .header("host", "localhost:4310")
            .body(Body::empty())
            .unwrap()
    };
    assert_eq!(
        router.clone().oneshot(request()).await.unwrap().status(),
        200
    );
    let disabled = artifacts::sharing::for_agent(
        &s,
        &token,
        &json!({
            "artifactId": item["id"],
            "visibility": "private"
        }),
    )
    .await
    .unwrap();
    assert!(disabled["publicUrl"].is_null());
    assert_eq!(
        router.clone().oneshot(request()).await.unwrap().status(),
        404
    );
    let enabled = artifacts::sharing::for_agent(
        &s,
        &token,
        &json!({
            "artifactId": item["id"],
            "visibility": "public"
        }),
    )
    .await
    .unwrap();
    assert_ne!(enabled["publicUrl"], shared["publicUrl"]);
    assert_eq!(router.oneshot(request()).await.unwrap().status(), 404);
    assert_eq!(
        artifacts::sharing::for_agent(
            &s,
            "invalid",
            &json!({
                "artifactId": item["id"],
                "visibility": "public"
            })
        )
        .await
        .unwrap_err()
        .status,
        401
    );
    assert_eq!(
        artifacts::sharing::for_agent(
            &s,
            &token,
            &json!({
                "artifactId": id(),
                "visibility": "public"
            })
        )
        .await
        .unwrap_err()
        .status,
        404
    );
    assert!(
        artifacts::sharing::for_agent(
            &s,
            &token,
            &json!({
                "artifactId": item["id"],
                "visibility": "invalid"
            })
        )
        .await
        .is_err()
    );
    // A grant for another active run cannot change a known artifact ID.
    let other_task = s
        .task(
            json!({
                "name": "Other",
                "prompt": "Other task",
                "agentId": MAIN_AGENT_ID
            }),
            None,
        )
        .await
        .unwrap();
    let other_run = s
        .enqueue(text(&other_task, "id"), "manual", None)
        .await
        .unwrap();
    s.store
        .patch_run(
            text(&other_run, "id"),
            json!({
                "status": "running"
            }),
        )
        .await
        .unwrap();
    let other_config = s.mcps.run_configuration(&s, &other_run).await.unwrap();
    let other_token = text(&other_config["env"], "LEO_MCP_RUN_TOKEN");
    assert_eq!(
        artifacts::sharing::for_agent(
            &s,
            other_token,
            &json!({
                "artifactId": item["id"],
                "visibility": "private"
            })
        )
        .await
        .unwrap_err()
        .status,
        404
    );
    assert_eq!(
        artifacts::sharing::set(
            &s,
            text(&run, "id"),
            text(&item, "id"),
            "private",
            Some(other_token)
        )
        .await
        .unwrap_err()
        .status,
        403
    );
    s.mcps.revoke_run(&s, text(&run, "id")).await.unwrap();
    assert_eq!(
        artifacts::sharing::for_agent(
            &s,
            &token,
            &json!({
                "artifactId": item["id"],
                "visibility": "public"
            })
        )
        .await
        .unwrap_err()
        .status,
        401
    );
    server.abort();
}

#[tokio::test]
async fn agent_publication_is_explicit_per_version_and_idempotent() {
    let (_root, s, run, token, server) = fixture().await;
    let mut args = json!({
        "path": "/tmp/report.md",
        "title": "Public report",
        "key": "report",
        "visibility": "public"
    });
    let first = s.artifacts.publish(&s, &token, &args).await.unwrap();
    assert_eq!(first["visibility"], "public");
    let again = s.artifacts.publish(&s, &token, &args).await.unwrap();
    assert_eq!(again["publicUrl"], first["publicUrl"]);
    args["path"] = "/tmp/second.md".into();
    args.as_object_mut().unwrap().remove("visibility");
    let second = s.artifacts.publish(&s, &token, &args).await.unwrap();
    assert_eq!(second["version"], 2);
    assert_eq!(second["visibility"], "private");
    args["visibility"] = "public".into();
    let public = s.artifacts.publish(&s, &token, &args).await.unwrap();
    assert_eq!(public["id"], second["id"]);
    assert_ne!(public["publicUrl"], first["publicUrl"]);
    assert_eq!(
        artifacts::list(&s, text(&run, "id")).await.unwrap().len(),
        2
    );
    let rpc = leo_agent_manager::project_workspaces::rpc(
        &s,
        &token,
        "tools/call",
        &json!({
            "name": "set_artifact_visibility",
            "arguments": {
                "artifactId": public["id"],
                "visibility": "private"
            }
        }),
    )
    .await
    .unwrap();
    assert_eq!(rpc["structuredContent"]["visibility"], "private");
    server.abort();
}
