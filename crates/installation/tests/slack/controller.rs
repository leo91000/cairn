//! A synthetic controller running the real native chat adapter and a fake Codex
//! process. It supplies no VM isolation proof; it never fabricates completed work.
use axum::{
    Json, Router,
    extract::{FromRequest, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use cairn_installation::{chat_process, config::Config};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

type Completion = watch::Receiver<Option<(String, i32)>>;

pub struct Controller {
    config: Config,
    attempts: Mutex<HashMap<String, Completion>>,
    pub plans: Mutex<Vec<Value>>,
    budget: Mutex<Value>,
}

impl Controller {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config,
            attempts: Mutex::new(HashMap::new()),
            plans: Mutex::new(Vec::new()),
            budget: Mutex::new(
                json!({"slots": 2, "limits": {"cpu": 2, "memoryMiB": 8192, "diskMiB": 65536}}),
            ),
        })
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new().fallback(any(handle)).with_state(self.clone())
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
    if path.ends_with("/lease") || path.starts_with("/prepare/") || request.method() == "DELETE" {
        return Json(json!({})).into_response();
    }
    if path.ends_with("/administrative-pause") {
        return Json(json!({"administrativePausedMs": 0})).into_response();
    }
    if !path.starts_with("/runs/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let attempt = path.split('/').nth(2).unwrap().to_owned();
    if path.ends_with("/logs") || path.ends_with("/wait") {
        let Some(mut done) = state.attempts.lock().await.get(&attempt).cloned() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let result = loop {
            if let Some(result) = done.borrow().clone() {
                break result;
            }
            if done.changed().await.is_err() {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };
        if path.ends_with("/wait") {
            return Json(json!({ "StatusCode": result.1 })).into_response();
        }
        let frame = json!({"type": "output", "stderr": false, "data": STANDARD.encode(result.0)});
        return format!("{frame}\n").into_response();
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
    assert!(
        state
            .attempts
            .lock()
            .await
            .insert(attempt, receiver)
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
        let output = tokio::spawn(async move {
            let mut lines = String::new();
            while let Some(event) = received.recv().await {
                lines.push_str(&format!("{event}\n"));
            }
            lines
        });
        let result =
            chat_process::run(&config, &home, chat, events, CancellationToken::new()).await;
        let mut output = output.await.unwrap();
        if let Err(error) = &result {
            eprintln!(
                "fixture adapter failed: {}; binary={} exists={}; cwd={} exists={}",
                error.message,
                config.codex_bin,
                std::path::Path::new(&config.codex_bin).exists(),
                cwd,
                std::path::Path::new(&cwd).exists()
            );
            output.push_str(&format!(
                "{}\n",
                json!({"type": "fixture.error", "message": error.message})
            ));
        }
        let _ = done.send(Some((output, if result.is_ok() { 0 } else { 1 })));
    });
    Json(json!({})).into_response()
}
