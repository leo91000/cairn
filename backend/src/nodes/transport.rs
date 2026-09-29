//! Bounded outbound RPC. Node identities can answer only their own outstanding calls.
use crate::{
    error::{Error, Result},
    http::App,
    service::Service,
    validation::text,
};
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, mpsc, oneshot};

type Head = (u16, Option<u64>, String);

struct Frame {
    bytes: Bytes,
    done: bool,
}

struct Call {
    node: String,
    command: Value,
    head: Option<oneshot::Sender<Head>>,
    body: mpsc::Sender<Frame>,
    sequence: u64,
    streaming: bool,
    length: Option<u64>,
}

#[derive(Default)]
struct StateData {
    calls: HashMap<String, Call>,
    pending: HashMap<String, VecDeque<String>>,
}

#[derive(Default)]
pub struct Transport {
    state: Mutex<StateData>,
    changed: Notify,
}

struct Pending {
    hub: Arc<Transport>,
    id: String,
}

impl Drop for Pending {
    fn drop(&mut self) {
        let mut state = self.hub.state.lock().unwrap();
        if let Some(call) = state.calls.remove(&self.id)
            && let Some(queue) = state.pending.get_mut(&call.node)
        {
            queue.retain(|id| id != &self.id);
        }
    }
}

impl Transport {
    pub async fn request(
        self: &Arc<Self>,
        node: &str,
        method: &str,
        path: &str,
        body: Vec<u8>,
    ) -> Result<Response> {
        let id = crate::config::id();
        let (head_tx, head_rx) = oneshot::channel();
        let (body_tx, body_rx) = mpsc::channel(8);
        {
            let mut state = self.state.lock().unwrap();
            if state.calls.values().filter(|c| c.node == node).count() >= 64 {
                return Err(Error::unavailable("Node transport is busy."));
            }
            state.calls.insert(
                id.clone(),
                Call {
                    node: node.into(),
                    command: json!({
                        "id": id,
                        "method": method,
                        "path": path,
                        "body": STANDARD.encode(body),
                        "streamBody": true
                    }),
                    head: Some(head_tx),
                    body: body_tx,
                    sequence: 0,
                    streaming: false,
                    length: None,
                },
            );
            state
                .pending
                .entry(node.into())
                .or_default()
                .push_back(id.clone());
        }
        let pending = Pending {
            hub: self.clone(),
            id,
        };
        self.changed.notify_waiters();
        let (status, length, kind) = tokio::time::timeout(
            Duration::from_secs(if path.ends_with("/restore") {
                120
            } else if path.starts_with("/prepare/") || path.ends_with("snapshot") {
                300
            } else {
                30
            }),
            head_rx,
        )
        .await
        .map_err(|_| Error::unavailable("Node did not acknowledge execution."))?
        .map_err(|_| Error::unavailable("Node disconnected."))?;
        let stream = futures_util::stream::try_unfold(
            (body_rx, pending, false),
            |(mut rx, guard, done)| async move {
                if done {
                    return Ok(None);
                }
                match tokio::time::timeout(Duration::from_secs(60), rx.recv()).await {
                    Ok(Some(frame)) => Ok(Some((frame.bytes, (rx, guard, frame.done)))),
                    // Only an explicit end frame completes a response. A dropped upload
                    // must fail even when the response has no Content-Length.
                    Ok(None) => Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "Node response incomplete",
                    )),
                    Err(_) => Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "Node output interrupted",
                    )),
                }
            },
        );
        let mut response = Response::builder().status(status);
        if let Some(length) = length {
            response = response.header("content-length", length);
        }
        if !kind.is_empty() {
            response = response.header("content-type", kind);
        }
        response
            .body(Body::from_stream(stream))
            .map_err(Error::internal)
    }

    pub async fn poll(&self, node: &str) -> Result<Value> {
        let wait = async {
            loop {
                let changed = self.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                {
                    let mut state = self.state.lock().unwrap();
                    while let Some(id) = state.pending.entry(node.into()).or_default().pop_front() {
                        if let Some(call) = state.calls.get(&id) {
                            return Ok(call.command.clone());
                        }
                    }
                }
                changed.await;
            }
        };
        tokio::time::timeout(Duration::from_secs(20), wait)
            .await
            .unwrap_or(Ok(Value::Null))
    }

    pub async fn reply(&self, node: &str, value: Value) -> Result<Value> {
        let id = text(&value, "id");
        let bytes = STANDARD
            .decode(text(&value, "data"))
            .map_err(|_| Error::bad("Invalid node response."))?;
        if bytes.len() > 65536 {
            return Err(Error::bad("Node frame exceeds limit."));
        }
        let sequence = value["sequence"].as_u64().unwrap_or(0);
        let sender = {
            let state = self.state.lock().unwrap();
            let call = state
                .calls
                .get(id)
                .filter(|c| c.node == node && !c.streaming)
                .ok_or_else(|| Error::conflict("Execution request no longer exists."))?;
            if sequence < call.sequence {
                return Ok(json!({"ack": sequence}));
            }
            if sequence != call.sequence {
                return Err(Error::conflict("Node response out of order."));
            }
            call.body.clone()
        };
        // Acquire capacity before acknowledging. Header, body and sequence publish
        // synchronously so cancellation or a concurrent retry cannot lose a frame.
        let permit = tokio::time::timeout(Duration::from_secs(20), sender.reserve_owned())
            .await
            .map_err(|_| Error::unavailable("Execution reader is stalled."))?
            .map_err(|_| Error::conflict("Execution reader closed."))?;
        let mut state = self.state.lock().unwrap();
        let call = state
            .calls
            .get_mut(id)
            .filter(|c| c.node == node && !c.streaming)
            .ok_or_else(|| Error::conflict("Execution request no longer exists."))?;
        if sequence < call.sequence {
            return Ok(json!({"ack": sequence}));
        }
        if sequence != call.sequence {
            return Err(Error::conflict("Node response out of order."));
        }
        if call.head.is_some() {
            let status = value["status"]
                .as_u64()
                .filter(|v| (200..=599).contains(v))
                .ok_or_else(|| Error::bad("Invalid response status."))?
                as u16;
            let kind = text(&value, "contentType").to_owned();
            if kind.len() > 128 || kind.contains(['\r', '\n']) {
                return Err(Error::bad("Invalid content type."));
            }
            let _ = call
                .head
                .take()
                .unwrap()
                .send((status, value["length"].as_u64(), kind));
            call.length = value["length"].as_u64();
        }
        let done = value["done"] == true;
        if !bytes.is_empty() || done {
            permit.send(Frame {
                bytes: Bytes::from(bytes),
                done,
            });
        }
        call.sequence += 1;
        if value["done"] == true {
            state.calls.remove(id);
        }
        Ok(json!({"ack": sequence}))
    }

    /// One authenticated HTTP upload per bulk response, with bounded backpressure.
    /// Legacy JSON frames remain usable by nodes that ignore streamBody.
    async fn stream(self: &Arc<Self>, node: &str, id: &str, body: Body) -> Result<()> {
        let (sender, length) = {
            let mut state = self.state.lock().unwrap();
            let call = state
                .calls
                .get_mut(id)
                .filter(|call| {
                    call.node == node
                        && !call.streaming
                        && call.sequence == 1
                        && call.head.is_none()
                })
                .ok_or_else(|| Error::conflict("Execution stream no longer available."))?;
            call.streaming = true;
            (call.body.clone(), call.length)
        };
        // Disconnects, cancellation and errors remove the call; the reader then
        // sees an incomplete response unless we sent the explicit final frame.
        let _pending = Pending {
            hub: self.clone(),
            id: id.to_owned(),
        };
        let mut body = body.into_data_stream();
        let mut received = 0u64;
        loop {
            let next = tokio::select! {
                _ = sender.closed() => return Err(Error::conflict("Execution reader closed.")),
                next = tokio::time::timeout(Duration::from_secs(60),body.next()) => next.map_err(|_|Error::unavailable("Node upload stalled."))?,
            };
            let Some(bytes) = next else { break };
            let bytes = bytes.map_err(|_| Error::unavailable("Node upload interrupted."))?;
            received = received
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| Error::bad("Response too large."))?;
            if length.is_some_and(|length| received > length) {
                return Err(Error::bad("Node response exceeds its declared length."));
            }
            for chunk in bytes.chunks(65536) {
                tokio::time::timeout(
                    Duration::from_secs(20),
                    sender.send(Frame {
                        bytes: Bytes::copy_from_slice(chunk),
                        done: false,
                    }),
                )
                .await
                .map_err(|_| Error::unavailable("Execution reader is stalled."))?
                .map_err(|_| Error::conflict("Execution reader closed."))?;
            }
        }
        if length.is_some_and(|length| received != length) {
            return Err(Error::bad("Node response was truncated."));
        }
        tokio::time::timeout(
            Duration::from_secs(20),
            sender.send(Frame {
                bytes: Bytes::new(),
                done: true,
            }),
        )
        .await
        .map_err(|_| Error::unavailable("Execution reader is stalled."))?
        .map_err(|_| Error::conflict("Execution reader closed."))?;
        Ok(())
    }
}

pub async fn stream(State(app): State<App>, request: Request) -> Result<axum::Json<Value>> {
    if request.method() != "POST" {
        return Err(Error::method_not_allowed("Method not allowed."));
    }
    let node = authenticate(&app.service, request.headers()).await?;
    let id = request
        .uri()
        .path()
        .trim_start_matches("/internal/nodes/stream/")
        .to_owned();
    crate::validation::uuid(&id)?;
    app.service
        .node_transport
        .stream(&node, &id, request.into_body())
        .await?;
    Ok(axum::Json(json!({"complete": true})))
}

pub async fn authenticate(s: &Service, headers: &axum::http::HeaderMap) -> Result<String> {
    let credential = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() == 43)
        .ok_or_else(|| Error::unauthorized("Invalid node identity."))?;
    let node = s
        .store
        .kv(&format!("node-token:{}", crate::auth::digest(credential)))
        .await?
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| Error::unauthorized("Invalid node identity."))?;
    if s.get("nodes", &node).await?["revoked"] == true {
        return Err(Error::unauthorized("Node revoked."));
    }
    Ok(node)
}

pub async fn proxy(State(app): State<App>, request: Request) -> Result<Response> {
    let s = &app.service;
    let token = crate::execution::secret(&s.config.data_dir, "runner-secret").await?;
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !crate::auth::safe_equal(supplied, &token) {
        return Err(Error::unauthorized("Invalid execution credential."));
    }
    let path = request
        .uri()
        .path()
        .trim_start_matches("/internal/execution/");
    let (node, path) = path
        .split_once('/')
        .ok_or_else(|| Error::bad("Missing execution path."))?;
    crate::validation::uuid(node)?;
    let record = s.get("nodes", node).await?;
    if record["revoked"] == true {
        return Err(Error::conflict("Node revoked."));
    }
    let (node, path, method) = (
        node.to_owned(),
        format!("/{path}"),
        request.method().as_str().to_owned(),
    );
    let limit = if path.ends_with("/restore") {
        super::snapshots::MAX_MANIFEST_BYTES
    } else {
        2_000_000
    };
    let body = axum::body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| Error::bad("Execution request too large."))?;
    s.node_transport
        .request(&node, &method, &path, body.to_vec())
        .await
}

/// All manager callers, including artifact and project operations, use the same destination.
pub async fn url(s: &Service, run: &str) -> Result<String> {
    let checkpoint = s
        .store
        .kv(&format!("run-checkpoint:{run}"))
        .await?
        .unwrap_or_default();
    match checkpoint["nodeId"]
        .as_str()
        .filter(|id| *id != super::LOCAL_NODE_ID)
    {
        Some(node) => Ok(format!(
            "{}/internal/execution/{node}",
            s.config.public_url.trim_end_matches('/')
        )),
        None => Ok(s.config.runner_url.clone()),
    }
}
