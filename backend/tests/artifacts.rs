mod common;

use common::relay_fixture::router;

use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::post,
};
use common::{Credentials, RelayContext, eventually, request, send};
use leo_agent_manager::{
    artifacts::{self, file, sharing},
    config::{Config, MAIN_AGENT_ID, id},
    nodes::transport::Transport,
    project_workspaces,
    run_status::RunStatus,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::task::JoinHandle;

/// A running task whose run can publish artifacts through its MCP grant.
struct Fixture {
    _root: TempDir,
    s: Arc<Service>,
    run: String,
    token: String,
    /// The runner guest that serves exported files.
    server: JoinHandle<()>,
}

/// Serves the guest files the runner exports, keyed by their guest path.
fn guest_exports() -> Router {
    Router::new().fallback(post(|Json(value): Json<Value>| async move {
        match text(&value, "path") {
            path if path.starts_with("/tmp/remote") => {
                let length = match path {
                    "/tmp/remote-short.md" => Some(18),
                    "/tmp/remote-long.md" => Some(16),
                    "/tmp/remote-too-large.md" => Some(file::MAX_FILE + 1),
                    "/tmp/remote-unknown.md" => None,
                    "/tmp/remote-empty.md" => Some(0),
                    _ => Some(17),
                };
                let data = if path == "/tmp/remote-empty.md" {
                    ""
                } else {
                    "IyBGaXJzdCByZXZpc2lvbgo="
                };

                let transport = Arc::new(Transport::default());
                let node = id();
                let replying = transport.clone();
                let worker_node = node.clone();
                let worker = tokio::spawn(async move {
                    let command = replying.poll(&worker_node).await.unwrap();
                    replying
                        .reply(
                            &worker_node,
                            json!({
                                "id": command["id"],
                                "status": 200,
                                "length": length,
                                "data": data,
                                "done": true
                            }),
                        )
                        .await
                        .unwrap();
                });
                let mut response = transport
                    .request(
                        &node,
                        "POST",
                        &format!("/runs/{}/artifact", id()),
                        Vec::new(),
                    )
                    .await
                    .unwrap();
                worker.await.unwrap();

                // A streaming intermediary can remove the HTTP framing length.
                response.headers_mut().remove("content-length");
                if path == "/tmp/remote-malformed.md" {
                    response
                        .headers_mut()
                        .insert("x-leo-artifact-size", "unknown".parse().unwrap());
                }
                if path == "/tmp/remote-conflicting.md" {
                    response
                        .headers_mut()
                        .insert("content-length", "17".parse().unwrap());
                    response
                        .headers_mut()
                        .insert("x-leo-artifact-size", "16".parse().unwrap());
                }
                response
            }
            "/tmp/second.md" => "# Second revision\n".into_response(),
            "/tmp/preview.png" => include_bytes!("../../tests/fixtures/artifacts/thumbnail.png")
                .as_slice()
                .into_response(),
            "/tmp/truncated.md" => {
                let stream = futures_util::stream::iter([
                    Ok::<_, std::io::Error>(Bytes::from_static(b"partial")),
                    Err(std::io::Error::other("interrupted")),
                ]);
                ([("content-length", "100")], Body::from_stream(stream)).into_response()
            }
            "/tmp/slow.md" => {
                let stream = futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    Ok::<_, std::io::Error>(Bytes::from_static(b"# Late file"))
                });
                ([("content-length", "11")], Body::from_stream(stream)).into_response()
            }
            _ => "# First revision\n".into_response(),
        }
    }))
}

/// Starts a manual run of a new task and marks it running.
async fn start_run(s: &Arc<Service>, name: &str, prompt: &str) -> Value {
    let task = s
        .task(
            json!({ "name": name, "prompt": prompt, "agentId": MAIN_AGENT_ID }),
            None,
        )
        .await
        .unwrap();
    let run = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    s.store
        .patch_run(text(&run, "id"), json!({ "status": RunStatus::Running }))
        .await
        .unwrap();
    run
}

async fn run_token(s: &Arc<Service>, run: &Value) -> String {
    let config = s.mcps.run_configuration(s, run).await.unwrap();
    text(&config["env"], "LEO_MCP_RUN_TOKEN").to_owned()
}

impl Fixture {
    async fn new() -> Self {
        let root = TempDir::new().unwrap();
        common::managed_codex_home(&root.path().join("home"));
        let (url, server) = common::serve_locally(guest_exports()).await;
        let s = Service::new(Config {
            runner_url: url,
            ..common::config(root.path())
        })
        .await
        .unwrap();
        common::relay_fixture::claimed(&s).await.unwrap();
        let run = start_run(&s, "Artifacts", "Create a report").await;
        let run_id = text(&run, "id").to_owned();
        common::set_checkpoint(&s.store, &run_id, json!({ "runnerId": id() })).await;
        let token = run_token(&s, &run).await;
        Self {
            _root: root,
            s,
            run: run_id,
            token,
            server,
        }
    }

    async fn publish(&self, args: &Value) -> leo_agent_manager::error::Result<Value> {
        self.s.artifacts.publish(&self.s, &self.token, args).await
    }

    async fn listed(&self) -> Vec<Value> {
        artifacts::list(&self.s, &self.run).await.unwrap()
    }
}

#[tokio::test]
async fn publication_through_node_channel_survives_missing_content_length() {
    let f = Fixture::new().await;
    let artifact = f
        .publish(&json!({
            "path": "/tmp/remote.md",
            "title": "Remote report",
            "key": "remote-report"
        }))
        .await
        .expect("A complete node export must publish without HTTP Content-Length");
    assert_eq!(artifact["size"], 17);
    let response = download(&f.s, &f.run, &artifact, None).await;
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        "# First revision\n"
    );
    f.server.abort();
}

#[tokio::test]
async fn node_artifact_size_validation_rejects_invalid_and_incomplete_exports() {
    let f = Fixture::new().await;
    for (path, status, message) in [
        (
            "/tmp/remote-short.md",
            503,
            "Artifact transfer was incomplete. Retry publication.",
        ),
        (
            "/tmp/remote-long.md",
            400,
            "Artifact changed during transfer. Finish writing it before publishing.",
        ),
        (
            "/tmp/remote-too-large.md",
            413,
            "Artifact exceeds the 512 MiB file limit.",
        ),
        (
            "/tmp/remote-unknown.md",
            400,
            "Artifact size is unknown. The export must declare its snapshot size.",
        ),
        (
            "/tmp/remote-malformed.md",
            400,
            "Invalid artifact snapshot size header.",
        ),
        (
            "/tmp/remote-conflicting.md",
            400,
            "Artifact size headers disagree.",
        ),
    ] {
        let error = f
            .publish(&json!({
                "path": path,
                "title": "Remote report",
                "key": "remote-report"
            }))
            .await
            .unwrap_err();
        assert_eq!(error.status, status, "{path}: {error}");
        assert_eq!(error.message, message, "{path}");
        assert!(f.listed().await.is_empty());

        let directory = f.s.config.data_dir.join("artifacts");
        if directory.is_dir() {
            assert_eq!(std::fs::read_dir(directory).unwrap().count(), 0, "{path}");
        }
    }
    f.server.abort();
}

#[tokio::test]
async fn empty_node_artifact_is_published_without_http_length() {
    let f = Fixture::new().await;
    let artifact = f
        .publish(&json!({
            "path": "/tmp/remote-empty.md",
            "title": "Empty report",
            "key": "empty-report"
        }))
        .await
        .unwrap();
    assert_eq!(artifact["size"], 0);
    let response = download(&f.s, &f.run, &artifact, None).await;
    assert!(
        to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty()
    );
    f.server.abort();
}

/// Downloads an artifact of `run` through the authenticated route handler.
async fn download(
    s: &Service,
    run: &str,
    artifact: &Value,
    range: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::builder();
    if let Some(range) = range {
        request = request.header("range", range);
    }
    artifacts::http(
        s,
        run,
        Some(text(artifact, "id")),
        request.body(Body::empty()).unwrap(),
    )
    .await
    .unwrap()
}

async fn body(response: axum::response::Response) -> Bytes {
    to_bytes(response.into_body(), 1024).await.unwrap()
}

fn anonymous_get(path: &str) -> Request<Body> {
    request("GET", path).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn publication_is_idempotent_versioned_and_readable_after_restart_without_a_vm() {
    let f = Fixture::new().await;
    let s = &f.s;
    let mut args = json!({
        "path": "/tmp/report.md",
        "title": "Release report",
        "key": "report",
        "group": "Release",
    });
    let (first, retry) = tokio::join!(f.publish(&args), f.publish(&args));
    let first = first.unwrap();
    assert_eq!(first["id"], retry.unwrap()["id"]);
    args["path"] = "/tmp/second.md".into();
    let second = f.publish(&args).await.unwrap();
    assert_eq!(second["version"], 2);
    assert_ne!(first["id"], second["id"]);
    assert_eq!(second["kind"], "markdown");
    assert_eq!(f.listed().await.len(), 2);
    let run_id = f.run.clone();
    let events = s
        .store
        .read(move |db| db.events(&run_id, 0, 100))
        .await
        .unwrap();
    assert_eq!(events.iter().filter(|e| e["type"] == "artifact").count(), 2);
    f.server.abort();
    s.store
        .patch_run(&f.run, json!({ "status": RunStatus::Succeeded }))
        .await
        .unwrap();
    assert_eq!(f.publish(&args).await.unwrap_err().status, 401);
    let artifacts = s.config.data_dir.join("artifacts");
    let orphan = artifacts.join(id());
    std::fs::write(&orphan, "orphan").unwrap();
    let old = std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH);
    for path in [&orphan, &artifacts.join(text(&first, "id"))] {
        std::fs::File::open(path).unwrap().set_times(old).unwrap();
    }
    let restarted = Service::new(s.config.clone()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while orphan.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let response = download(&restarted, &f.run, &first, None).await;
    assert_eq!(body(response).await, "# First revision\n");
    let response = download(&restarted, &f.run, &second, Some("bytes=2-7")).await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()["content-range"], "bytes 2-7/18");
    assert_eq!(body(response).await, "Second");
    let response = download(&restarted, &f.run, &second, Some("bytes=999-")).await;
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()["content-range"], "bytes */18");
}

#[tokio::test]
async fn publication_checks_grants_and_downloads_require_authentication_and_correct_run() {
    let f = Fixture::new().await;
    let s = &f.s;
    let args = json!({ "path": "/tmp/report.md", "title": "Report", "key": "report" });
    assert_eq!(
        s.artifacts
            .publish(s, "wrong-token", &args)
            .await
            .unwrap_err()
            .status,
        401
    );
    let item = f.publish(&args).await.unwrap();
    let app = router(s.clone()).await.unwrap();
    for path in [
        format!("/api/runs/{}/artifacts", f.run),
        text(&item, "url").to_owned(),
    ] {
        assert_eq!(
            send(&app, anonymous_get(&path)).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let task = s
        .task(
            json!({ "name": "Other", "prompt": "Other", "agentId": MAIN_AGENT_ID }),
            None,
        )
        .await
        .unwrap();
    let other = s.enqueue(text(&task, "id"), "manual", None).await.unwrap();
    assert_eq!(
        artifacts::http(
            s,
            text(&other, "id"),
            Some(text(&item, "id")),
            Request::new(Body::empty())
        )
        .await
        .unwrap_err()
        .status,
        404
    );
    s.mcps.revoke_run(s, &f.run).await.unwrap();
    assert_eq!(f.publish(&args).await.unwrap_err().status, 401);
    f.server.abort();
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
    let f = Fixture::new().await;
    let s = &f.s;
    let truncated = json!({ "path": "/tmp/truncated.md", "title": "Report", "key": "report" });
    assert!(f.publish(&truncated).await.is_err());
    assert!(f.listed().await.is_empty());
    let slow = json!({ "path": "/tmp/slow.md", "title": "Report", "key": "report" });
    let revoke = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.mcps.revoke_run(s, &f.run).await.unwrap();
    };
    let (published, ()) = tokio::join!(f.publish(&slow), revoke);
    assert_eq!(published.unwrap_err().status, 401);
    assert!(f.listed().await.is_empty());
    let artifacts = s.config.data_dir.join("artifacts");
    if artifacts.is_dir() {
        assert_eq!(std::fs::read_dir(artifacts).unwrap().count(), 0);
    }
    f.server.abort();
}

#[tokio::test]
async fn rejected_metadata_commit_removes_the_durable_unpublished_file() {
    let f = Fixture::new().await;
    let database = rusqlite::Connection::open(f.s.config.data_dir.join("manager.db")).unwrap();
    database
        .execute_batch(
            "CREATE TRIGGER reject_artifact BEFORE INSERT ON kv
             WHEN NEW.key LIKE 'artifact:%'
             BEGIN SELECT RAISE(ABORT, 'rejected artifact commit'); END;",
        )
        .unwrap();

    let error = f
        .publish(&json!({ "path": "/tmp/report.md", "title": "Report", "key": "report" }))
        .await
        .unwrap_err();
    assert_eq!(error.status, 409);
    assert!(f.listed().await.is_empty());
    assert_eq!(
        std::fs::read_dir(f.s.config.data_dir.join("artifacts"))
            .unwrap()
            .count(),
        0
    );
    f.server.abort();
}

#[tokio::test]
async fn preview_is_prepared_asynchronously_or_reports_missing_optional_tools_without_losing_original()
 {
    let f = Fixture::new().await;
    let s = &f.s;
    let artifact = f
        .publish(&json!({ "path": "/tmp/preview.png", "title": "Preview", "key": "preview" }))
        .await
        .unwrap();
    let key = format!("artifact:{}:{}", f.run, text(&artifact, "id"));
    let completed = eventually(
        Duration::from_secs(30),
        Duration::from_millis(50),
        async || {
            let item = s.store.kv(&key).await.unwrap().unwrap();
            (item["previewStatus"] != "pending").then_some(item)
        },
    )
    .await;
    let tools = std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
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
    f.server.abort();
}

/// Makes `item` public through the owner route, which requires the trusted owner context.
async fn share_from_the_owner_route(app: &Router, s: &Service, item: &Value) {
    let session = RelayContext::new(&common::relay_fixture::context(s).await);
    let endpoint = format!("{}/visibility", text(item, "url"));
    for (credentials, expected) in [
        (Credentials::Anonymous, StatusCode::UNAUTHORIZED),
        (Credentials::Cookie(&session), StatusCode::UNAUTHORIZED),
        (Credentials::Owner(&session), StatusCode::OK),
    ] {
        let request = credentials
            .apply(request("PUT", &endpoint))
            .body(Body::from(r#"{"visibility":"public"}"#))
            .unwrap();
        assert_eq!(send(app, request).await.status(), expected);
    }
}

/// A public link opens only its artifact: private routes still need a session.
async fn assert_public_link_is_scoped(app: &Router, run: &str, item: &Value, public: &str) {
    let url = text(item, "url");
    for (path, status) in [
        (url.to_owned(), StatusCode::UNAUTHORIZED),
        (format!("{url}/../"), StatusCode::UNAUTHORIZED),
        (
            format!("/api/runs/{run}/artifacts"),
            StatusCode::UNAUTHORIZED,
        ),
        (
            format!(
                "/api/public/installations/00000000-0000-4000-8000-000000000055/artifacts/{}",
                id()
            ),
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(send(app, anonymous_get(&path)).await.status(), status);
    }
    for method in ["GET", "HEAD"] {
        let range = request(method, public)
            .header("origin", "https://recipient.example")
            .header("range", "bytes=2-6")
            .body(Body::empty())
            .unwrap();
        let response = send(app, range).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            response.headers()["content-security-policy"],
            "default-src 'none'; sandbox"
        );
        assert_eq!(response.headers()["access-control-allow-origin"], "*");
        let expected = if method == "HEAD" {
            b"".as_slice()
        } else {
            b"First".as_slice()
        };
        assert_eq!(body(response).await.as_ref(), expected);
    }
}

#[tokio::test]
async fn public_links_are_scoped_revocable_and_survive_restart_without_exposing_private_routes() {
    let f = Fixture::new().await;
    let s = &f.s;
    let item = f
        .publish(&json!({ "path": "/tmp/report.md", "title": "Report", "key": "public-test" }))
        .await
        .unwrap();
    assert_eq!(item["visibility"], "private");
    assert!(item["publicUrl"].is_null());
    let app = router(s.clone()).await.unwrap();
    share_from_the_owner_route(&app, s, &item).await;
    let shared = f.listed().await.pop().unwrap();
    let public = url::Url::parse(text(&shared, "publicUrl"))
        .unwrap()
        .path()
        .to_owned();
    assert_public_link_is_scoped(&app, &f.run, &item, &public).await;

    let restarted = Service::new(s.config.clone()).await.unwrap();
    let router = router(restarted.clone()).await.unwrap();
    let public_status = async || send(&router, anonymous_get(&public)).await.status();
    assert_eq!(public_status().await, StatusCode::OK);
    let visibility = |artifact: &Value, visibility: &str| json!({ "artifactId": artifact["id"], "visibility": visibility });
    let disabled = sharing::for_agent(s, &f.token, &visibility(&item, "private"))
        .await
        .unwrap();
    assert!(disabled["publicUrl"].is_null());
    assert_eq!(public_status().await, StatusCode::NOT_FOUND);
    let enabled = sharing::for_agent(s, &f.token, &visibility(&item, "public"))
        .await
        .unwrap();
    assert_ne!(enabled["publicUrl"], shared["publicUrl"]);
    assert_eq!(public_status().await, StatusCode::NOT_FOUND);
    assert_eq!(
        sharing::for_agent(s, "invalid", &visibility(&item, "public"))
            .await
            .unwrap_err()
            .status,
        401
    );
    assert_eq!(
        sharing::for_agent(s, &f.token, &visibility(&json!({ "id": id() }), "public"))
            .await
            .unwrap_err()
            .status,
        404
    );
    assert!(
        sharing::for_agent(s, &f.token, &visibility(&item, "invalid"))
            .await
            .is_err()
    );
    // A grant for another active run cannot change a known artifact ID.
    let other_run = start_run(s, "Other", "Other task").await;
    let other_token = run_token(s, &other_run).await;
    assert_eq!(
        sharing::for_agent(s, &other_token, &visibility(&item, "private"))
            .await
            .unwrap_err()
            .status,
        404
    );
    assert_eq!(
        sharing::set(s, &f.run, text(&item, "id"), "private", Some(&other_token))
            .await
            .unwrap_err()
            .status,
        403
    );
    s.mcps.revoke_run(s, &f.run).await.unwrap();
    assert_eq!(
        sharing::for_agent(s, &f.token, &visibility(&item, "public"))
            .await
            .unwrap_err()
            .status,
        401
    );
    f.server.abort();
}

#[tokio::test]
async fn agent_publication_is_explicit_per_version_and_idempotent() {
    let f = Fixture::new().await;
    let mut args = json!({
        "path": "/tmp/report.md",
        "title": "Public report",
        "key": "report",
        "visibility": "public",
    });
    let first = f.publish(&args).await.unwrap();
    assert_eq!(first["visibility"], "public");
    let again = f.publish(&args).await.unwrap();
    assert_eq!(again["publicUrl"], first["publicUrl"]);
    args["path"] = "/tmp/second.md".into();
    args.as_object_mut().unwrap().remove("visibility");
    let second = f.publish(&args).await.unwrap();
    assert_eq!(second["version"], 2);
    assert_eq!(second["visibility"], "private");
    args["visibility"] = "public".into();
    let public = f.publish(&args).await.unwrap();
    assert_eq!(public["id"], second["id"]);
    assert_ne!(public["publicUrl"], first["publicUrl"]);
    assert_eq!(f.listed().await.len(), 2);
    let call = json!({
        "name": "set_artifact_visibility",
        "arguments": { "artifactId": public["id"], "visibility": "private" },
    });
    let rpc = project_workspaces::rpc(&f.s, &f.token, "tools/call", &call)
        .await
        .unwrap();
    assert_eq!(rpc["structuredContent"]["visibility"], "private");
    f.server.abort();
}
