//! Exercise the relay's real request and seed receiver without enabling host-native
//! disk management. Only the executor's backend and data-root context are injected.
use super::*;
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Notify, mpsc, oneshot};

#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
    dropped: CancellationToken,
}

struct Dropped(CancellationToken);

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[derive(Clone)]
struct FixtureState {
    seed: Arc<Gate>,
    controller: Arc<Gate>,
    seed_bytes: Vec<u8>,
    target: PathBuf,
    source: PathBuf,
    attempt: String,
    project: String,
    run: String,
    controller_calls: Arc<AtomicUsize>,
    replies: mpsc::UnboundedSender<Value>,
}

fn authenticated(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(token)
}

async fn seed_response(
    State(state): State<FixtureState>,
    headers: HeaderMap,
    axum::extract::Path(attempt): axum::extract::Path<String>,
    Json(selection): Json<Value>,
) -> std::result::Result<Body, StatusCode> {
    if !authenticated(&headers, "Bearer test-node")
        || attempt != state.attempt
        || selection != json!({ "kind": "project", "projectId": state.project })
    {
        return Err(StatusCode::FORBIDDEN);
    }
    // Withhold the completion frame after a real file has crossed the wire.
    // The receiver must retain its temporary tree until the gate is released.
    let split = state.seed_bytes.len() - b"{\"complete\":true}\n".len();
    let first = Bytes::copy_from_slice(&state.seed_bytes[..split]);
    let last = Bytes::copy_from_slice(&state.seed_bytes[split..]);
    let dropped = Dropped(state.seed.dropped.clone());
    let stream = futures_util::stream::unfold(
        (Some(first), Some(last), state.seed, dropped),
        |(mut first, mut last, gate, dropped)| async move {
            if let Some(bytes) = first.take() {
                return Some((Ok::<_, std::io::Error>(bytes), (first, last, gate, dropped)));
            }
            if let Some(bytes) = last.take() {
                gate.entered.notify_one();
                gate.release.notified().await;
                return Some((Ok(bytes), (first, last, gate, dropped)));
            }
            None
        },
    );
    Ok(Body::from_stream(stream))
}

async fn controller_response(
    State(state): State<FixtureState>,
    headers: HeaderMap,
    axum::extract::Path((attempt, project)): axum::extract::Path<(String, String)>,
    Json(body): Json<Value>,
) -> std::result::Result<Json<Value>, StatusCode> {
    if !authenticated(&headers, "Bearer test-controller")
        || attempt != state.attempt
        || project != state.project
        || body["runId"] != state.run
        || body["source"] != state.source.to_string_lossy().as_ref()
    {
        return Err(StatusCode::FORBIDDEN);
    }
    assert_eq!(
        tokio::fs::read(state.target.join("file")).await.unwrap(),
        b"project seed"
    );
    state.controller_calls.fetch_add(1, Ordering::SeqCst);
    let _dropped = Dropped(state.controller.dropped.clone());
    state.controller.entered.notify_one();
    state.controller.release.notified().await;
    Ok(Json(json!({ "ok": true })))
}

async fn ordinary_response(
    State(state): State<FixtureState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, StatusCode> {
    if !authenticated(&headers, "Bearer test-controller") {
        return Err(StatusCode::FORBIDDEN);
    }
    let _dropped = Dropped(state.controller.dropped.clone());
    state.controller.entered.notify_one();
    state.controller.release.notified().await;
    Ok(Json(json!({ "ok": true })))
}

async fn reply(
    State(state): State<FixtureState>,
    headers: HeaderMap,
    Json(frame): Json<Value>,
) -> StatusCode {
    if !authenticated(&headers, "Bearer test-node") {
        return StatusCode::UNAUTHORIZED;
    }
    state.replies.send(frame).unwrap();
    StatusCode::OK
}

struct Fixture {
    _root: tempfile::TempDir,
    state: FixtureState,
    url: url::Url,
    executor: Arc<Executor>,
    replies: mpsc::UnboundedReceiver<Value>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        assert!(
            !crate::native::enabled(),
            "use the isolated backend test launcher"
        );
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("node");
        let (attempt, project, run) = (
            crate::config::id(),
            crate::config::id(),
            crate::config::id(),
        );
        let source = root
            .path()
            .join("master/runs")
            .join(&run)
            .join("workspace")
            .join(&project);
        let target = data
            .join("runs")
            .join(&run)
            .join("workspace")
            .join(&project);
        tokio::fs::create_dir_all(&source).await.unwrap();
        tokio::fs::write(source.join("file"), b"project seed")
            .await
            .unwrap();
        let mut seed_bytes = Vec::new();
        crate::nodes::files::send(&source, &mut seed_bytes)
            .await
            .unwrap();
        tokio::fs::create_dir_all(data.join("runner-plans"))
            .await
            .unwrap();
        tokio::fs::write(
            data.join("runner-plans").join(format!("{attempt}.json")),
            serde_json::to_vec(&json!({ "guestDataRoot": root.path().join("master") })).unwrap(),
        )
        .await
        .unwrap();
        let (replies_tx, replies) = mpsc::unbounded_channel();
        let state = FixtureState {
            seed: Arc::default(),
            controller: Arc::default(),
            seed_bytes,
            target,
            source,
            attempt,
            project,
            run,
            controller_calls: Arc::default(),
            replies: replies_tx,
        };
        let app = Router::new()
            .route(
                "/internal/node-workspace/{attempt}/seed",
                post(seed_response),
            )
            .route(
                "/runs/{attempt}/projects/{project}",
                post(controller_response),
            )
            .route("/storage-policy", post(ordinary_response))
            .route("/internal/nodes/reply", post(reply))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            _root: root,
            state,
            url,
            executor: Arc::new(Executor::for_project_import(data)),
            replies,
            server,
        }
    }

    async fn start(
        &self,
        path: String,
        body: Value,
    ) -> (tokio::task::JoinHandle<Result<()>>, tokio::time::Instant) {
        let (url, executor) = (self.url.clone(), self.executor.clone());
        let (started_tx, started_rx) = oneshot::channel();
        let operation = tokio::spawn(async move {
            let client = reqwest::Client::new();
            let controller = crate::nodes::local_client().unwrap();
            let master = Master {
                client: &client,
                controller: &controller,
                url: &url,
                token: "test-node",
            };
            let command = json!({
                "id": crate::config::id(), "method": "POST", "path": path,
                "body": STANDARD.encode(serde_json::to_vec(&body).unwrap()),
            });
            started_tx.send(tokio::time::Instant::now()).unwrap();
            forward(&master, &url, "test-controller", &command, &executor).await
        });
        (operation, ready(started_rx).await.unwrap())
    }

    async fn import(&self) -> (tokio::task::JoinHandle<Result<()>>, tokio::time::Instant) {
        self.start(
            format!(
                "/runs/{}/projects/{}",
                self.state.attempt, self.state.project
            ),
            json!({ "runId": self.state.run, "source": self.state.source }),
        )
        .await
    }

    async fn receiving_seed(&self) {
        ready(self.state.seed.entered.notified()).await;
        // Observe the receiver's actual staging file, not only server readiness.
        // The stream cannot complete until the test releases its gate.
        ready(async {
            loop {
                if let Ok(mut entries) =
                    tokio::fs::read_dir(self.state.target.parent().unwrap()).await
                {
                    while let Some(entry) = entries.next_entry().await.unwrap() {
                        if tokio::fs::read(entry.path().join("file"))
                            .await
                            .is_ok_and(|bytes| bytes == b"project seed")
                        {
                            return;
                        }
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn ready<F: std::future::Future>(operation: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), operation)
        .await
        .expect("relay fixture did not reach the expected boundary")
}

async fn advance(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}

async fn advance_before(started: tokio::time::Instant, elapsed: Duration) {
    let remaining = (started + elapsed)
        .checked_duration_since(tokio::time::Instant::now())
        .expect("fixture readiness exceeded the controlled deadline");
    advance(remaining).await;
}

#[tokio::test]
async fn native_import_relay_includes_seed_reception_and_controller_in_one_deadline() {
    let mut fixture = Fixture::new().await;
    let (operation, _) = fixture.import().await;
    fixture.receiving_seed().await;
    advance(Duration::from_secs(160)).await;
    assert!(!operation.is_finished());
    assert_eq!(fixture.state.controller_calls.load(Ordering::SeqCst), 0);
    assert!(!fixture.state.target.exists());
    fixture.state.seed.release.notify_one();
    ready(fixture.state.controller.entered.notified()).await;
    advance(Duration::from_secs(100)).await;
    assert!(!operation.is_finished());
    fixture.state.controller.release.notify_one();
    ready(operation).await.unwrap().unwrap();
    let head = ready(fixture.replies.recv()).await.unwrap();
    assert_eq!(head["status"], 200);
    assert_eq!(head["sequence"], 0);
    let chunk = ready(fixture.replies.recv()).await.unwrap();
    assert_eq!(
        STANDARD.decode(chunk["data"].as_str().unwrap()).unwrap(),
        br#"{"ok":true}"#
    );
    assert_eq!(ready(fixture.replies.recv()).await.unwrap()["done"], true);
}

#[tokio::test]
async fn native_import_relay_timeout_cancels_seed_and_removes_partial_staging() {
    let mut fixture = Fixture::new().await;
    let (operation, started) = fixture.import().await;
    fixture.receiving_seed().await;
    advance_before(
        started,
        crate::nodes::PROJECT_OPEN_RELAY_TIMEOUT - Duration::from_secs(1),
    )
    .await;
    assert!(!operation.is_finished());
    advance(Duration::from_secs(1)).await;
    ready(operation).await.unwrap().unwrap();
    let frame = ready(fixture.replies.recv()).await.unwrap();
    assert_eq!(frame["status"], 504);
    assert_eq!(frame["done"], true);
    assert_eq!(frame["sequence"], 0);
    assert!(fixture.replies.try_recv().is_err());
    ready(fixture.state.seed.dropped.cancelled()).await;
    assert_eq!(fixture.state.controller_calls.load(Ordering::SeqCst), 0);
    assert!(!fixture.state.target.exists());
    assert!(
        tokio::fs::read_dir(fixture.state.target.parent().unwrap())
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn native_import_relay_controller_uses_remaining_budget_and_control_stays_at_25_seconds() {
    let mut fixture = Fixture::new().await;
    let (operation, started) = fixture.import().await;
    fixture.receiving_seed().await;
    advance(Duration::from_secs(160)).await;
    fixture.state.seed.release.notify_one();
    ready(fixture.state.controller.entered.notified()).await;
    advance_before(
        started,
        crate::nodes::PROJECT_OPEN_RELAY_TIMEOUT - Duration::from_secs(1),
    )
    .await;
    assert!(!operation.is_finished());
    advance(Duration::from_secs(1)).await;
    ready(operation).await.unwrap().unwrap();
    assert_eq!(ready(fixture.replies.recv()).await.unwrap()["status"], 504);
    ready(fixture.state.controller.dropped.cancelled()).await;
    assert_eq!(fixture.state.controller_calls.load(Ordering::SeqCst), 1);
    assert!(fixture.state.target.join("file").exists());

    let mut ordinary = Fixture::new().await;
    let (operation, started) = ordinary.start("/storage-policy".into(), json!({})).await;
    ready(ordinary.state.controller.entered.notified()).await;
    advance_before(started, Duration::from_secs(24)).await;
    assert!(!operation.is_finished());
    advance(Duration::from_secs(1)).await;
    ready(operation).await.unwrap().unwrap();
    assert_eq!(ready(ordinary.replies.recv()).await.unwrap()["status"], 503);
    ready(ordinary.state.controller.dropped.cancelled()).await;
}
