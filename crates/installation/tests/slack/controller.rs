//! A synthetic controller running the real native chat adapter and a fake Codex
//! process. It supplies no VM isolation proof; it never fabricates completed work.
use axum::{
    Json, Router,
    body::Body,
    extract::{FromRequest, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use cairn_installation::{chat_process, config::Config};
use serde_json::{Value, json};
use std::{collections::HashMap, convert::Infallible, sync::Arc};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Attempt {
    done: watch::Receiver<Option<i32>>,
    logs: watch::Receiver<String>,
    stop: CancellationToken,
}

pub struct Controller {
    config: Config,
    attempts: Mutex<HashMap<String, Attempt>>,
    pub plans: Mutex<Vec<Value>>,
    budget: Mutex<Value>,
    stop: CancellationToken,
}

impl Controller {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config,
            attempts: Mutex::new(HashMap::new()),
            plans: Mutex::new(Vec::new()),
            stop: CancellationToken::new(),
            budget: Mutex::new(
                json!({"slots": 2, "limits": {"cpu": 2, "memoryMiB": 8192, "diskMiB": 65536}}),
            ),
        })
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new().fallback(any(handle)).with_state(self.clone())
    }

    pub fn shutdown(&self) {
        self.stop.cancel();
    }

    pub async fn stopped(&self) -> bool {
        self.attempts
            .lock()
            .await
            .values()
            .any(|attempt| attempt.stop.is_cancelled() && attempt.done.borrow().is_some())
    }
}

async fn handle(State(state): State<Arc<Controller>>, request: Request) -> Response {
    let path = request.uri().path();
    if path == "/health" {
        return Json(json!({
            "status": "ok", "sharedResources": true,
            "budget": state.budget.lock().await.clone(),
            "runtimeId": "fixture", "runtimes": ["fixture"],
            "capabilities": { "os": "linux", "arch": "x86_64", "kvm": true, "fuse": true,
                "cpu": 4, "memoryMiB": 16384, "diskMiB": 131072 }
        }))
        .into_response();
    }
    if path == "/node-budget" {
        let Json(budget) = Json::<Value>::from_request(request, &()).await.unwrap();
        *state.budget.lock().await = budget;
        return Json(json!({})).into_response();
    }
    if path.ends_with("/lease") || path.starts_with("/prepare/") {
        return Json(json!({})).into_response();
    }
    if path.ends_with("/administrative-pause") {
        return Json(json!({"administrativePausedMs": 0})).into_response();
    }
    if !path.starts_with("/runs/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let attempt = path.split('/').nth(2).unwrap().to_owned();
    if request.method() == "DELETE" && path.split('/').count() == 3 {
        let Some(mut attempt) = state.attempts.lock().await.get(&attempt).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        // The real runner also cleans up completed attempts. Only an active
        // adapter needs cancellation; a successful reply is not a Stop proof.
        if attempt.done.borrow().is_none() {
            attempt.stop.cancel();
            let stopped = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if attempt.done.borrow().is_some() {
                        return true;
                    }
                    if attempt.done.changed().await.is_err() {
                        return false;
                    }
                }
            })
            .await;
            if !matches!(stopped, Ok(true)) {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
        return Json(json!({})).into_response();
    }
    if path.ends_with("/logs") || path.ends_with("/wait") {
        let Some(mut attempt) = state.attempts.lock().await.get(&attempt).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if path.ends_with("/logs") {
            // Deliver real adapter events while the provider is still running.
            // Buffering until completion prevents a native wait from observing
            // its acknowledged input during the turn that schedules it.
            let stream = futures_util::stream::unfold(
                (attempt.logs, 0),
                |(mut logs, mut offset)| async move {
                    loop {
                        let chunk = {
                            let current = logs.borrow_and_update();
                            let chunk = current[offset..].to_owned();
                            offset = current.len();
                            chunk
                        };
                        if !chunk.is_empty() {
                            return Some((Ok::<_, Infallible>(chunk), (logs, offset)));
                        }
                        if logs.changed().await.is_err() {
                            return None;
                        }
                    }
                },
            );
            return Body::from_stream(stream).into_response();
        }
        let result = loop {
            if let Some(result) = *attempt.done.borrow() {
                break result;
            }
            if attempt.done.changed().await.is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
        return Json(json!({ "StatusCode": result })).into_response();
    }
    // Only the actual start endpoint launches a process. This fixture does not
    // implement VM snapshots/artifacts or project exports; reject their paths.
    if request.method() != "POST" || path.split('/').count() != 3 {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = state
        .config
        .data_dir
        .join("runner-plans")
        .join(format!("{attempt}.json"));
    let plan: Value = serde_json::from_slice(&tokio::fs::read(path).await.unwrap()).unwrap();
    state.plans.lock().await.push(plan.clone());
    let (done, receiver) = watch::channel(None);
    let (logs, output) = watch::channel(String::new());
    let stop = state.stop.child_token();
    assert!(
        state
            .attempts
            .lock()
            .await
            .insert(
                attempt,
                Attempt {
                    done: receiver,
                    logs: output,
                    stop: stop.clone(),
                }
            )
            .is_none(),
        "a read must not launch twice"
    );
    let config = state.config.clone();
    tokio::spawn(async move {
        let mounts = plan["imports"].as_array().unwrap();
        let source = |target: &str| {
            mounts.iter().find(|m| m["target"] == target).unwrap()["source"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let home = std::path::PathBuf::from(source("/home/node")).join(".codex");
        tokio::fs::create_dir_all(&home).await.unwrap();
        let mut chat = plan["chat"].clone();
        chat["inputDirectory"] = source("/run/cairn-chat").into();
        let cwd = chat["cwd"].as_str().unwrap().to_owned();
        let (events, mut received) = tokio::sync::mpsc::channel(128);
        let collected = logs.clone();
        let output = tokio::spawn(async move {
            while let Some(event) = received.recv().await {
                let frame = json!({
                    "type": "output",
                    "stderr": false,
                    "data": STANDARD.encode(format!("{event}\n")),
                });
                collected.send_modify(|output| output.push_str(&format!("{frame}\n")));
            }
        });
        let result = chat_process::run(&config, &home, chat, events, stop).await;
        output.await.unwrap();
        if let Err(error) = &result {
            eprintln!(
                "fixture adapter failed: {}; binary={} exists={}; cwd={} exists={}",
                error.message,
                config.codex_bin,
                std::path::Path::new(&config.codex_bin).exists(),
                cwd,
                std::path::Path::new(&cwd).exists()
            );
            let event = json!({ "type": "fixture.error", "message": error.message });
            let frame = json!({
                "type": "output",
                "stderr": false,
                "data": STANDARD.encode(format!("{event}\n")),
            });
            logs.send_modify(|output| output.push_str(&format!("{frame}\n")));
        }
        let _ = done.send(Some(if result.is_ok() { 0 } else { 1 }));
    });
    Json(json!({})).into_response()
}
